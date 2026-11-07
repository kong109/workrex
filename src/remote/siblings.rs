//! Workspaces that remote agents spawn on their node (see
//! `node::spawn`). The node runs them without the laptop; the laptop adopts
//! each one the next time it looks at the node, saving the native Rex placement.
//!
//! The record is the only acknowledgement. A running sibling the laptop has
//! no record of is adopted again, and one it cannot adopt (unknown project,
//! name taken) keeps running on the node until `workrex remote gc`.

use crate::bin_name::BIN;
use std::collections::HashSet;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};

use super::cache::{self, ExecutionRecord, WorkspaceRecord, WorkspaceState};
use super::node::observe::{ObservedWorkspace, Sibling};
use super::observe::Observation;
use crate::config::Config;
use crate::multiplexer::{create_backend, detect_backend};
use crate::workflow::WorkflowContext;

/// Adopt the running siblings in `observation` of `remote`. Problems go to stderr in
/// `foreground` commands and to the log in observers.
pub fn adopt(remote: &str, observation: &Observation, foreground: bool) {
    if let Err(err) = adopt_all(remote, observation, foreground) {
        report(
            foreground,
            &format!("could not adopt remote agents on '{remote}': {err:#}"),
        );
    }
}

fn adopt_all(remote: &str, observation: &Observation, foreground: bool) -> Result<()> {
    // Deletion uses the same lock, so no adoption can pass the tombstone
    // check and then recreate a record that another process has deleted.
    let _lock = lock(remote)?;
    // A workspace whose record went was removed or forgotten on purpose.
    // `remove --force` can forget one the node still has; `remote gc`
    // removes it.
    let running: Vec<(&ObservedWorkspace, &Sibling)> = observation
        .workspaces
        .iter()
        .filter(|ws| cache::forgotten_at(remote, &ws.ws_id).is_none())
        .filter_map(|ws| Some((ws, ws.sibling.as_ref()?)))
        .collect();
    if running.is_empty() {
        return Ok(());
    }
    let unsettled = |records: &[WorkspaceRecord]| -> Vec<(&ObservedWorkspace, &Sibling)> {
        running
            .iter()
            .copied()
            .filter(|(ws, _)| {
                !records.iter().any(|record| {
                    record.remote == remote && record.ws_id == ws.ws_id && !record.window_pending
                })
            })
            .collect()
    };
    let records = cache::all();
    for (ws, sibling) in unsettled(&records) {
        let key = format!("{remote}/{}", ws.ws_id);
        if refused().contains(&key) {
            continue;
        }
        let pending = records
            .iter()
            .find(|record| record.remote == remote && record.ws_id == ws.ws_id);
        // An ended execution has no live terminal; `workrex open` restarts it.
        if pending.is_some() && ws.ended.is_some() {
            continue;
        }
        let result = match pending {
            Some(record) => settle(record.clone()),
            None => adopt_one(remote, ws, sibling, &records, foreground),
        };
        if let Err(err) = result {
            refused().insert(key);
            report(
                foreground,
                &format!(
                    "not adopting remote agent '{}' on '{remote}': {err:#}",
                    sibling.identity.handle
                ),
            );
        }
    }
    Ok(())
}

fn adopt_one(
    remote: &str,
    ws: &ObservedWorkspace,
    sibling: &Sibling,
    records: &[WorkspaceRecord],
    foreground: bool,
) -> Result<()> {
    let identity = &sibling.identity;
    let Some(project_root) = project_root(&identity.project_id, records) else {
        bail!(
            "its repository has no other remote workspace on this machine; run `{BIN} open {}` \
             in the repository",
            identity.handle
        );
    };
    if !valid_pin(&sibling.pin) {
        bail!("the node reported an invalid binary name");
    }
    if let Some(record) = cache::claimed(&project_root, &identity.handle, &identity.branch) {
        bail!(
            "remote workspace '{}' on '{}' already uses that name or branch",
            record.handle,
            record.remote
        );
    }
    let parent = records
        .iter()
        .find(|parent| parent.remote == remote && parent.ws_id == sibling.origin.parent);
    let context = context(&project_root, parent)?;
    let window_name = crate::multiplexer::util::prefixed(&context.prefix, &identity.handle);
    if let Some(conflict) =
        super::lifecycle::local_conflict(&context, &identity.branch, &identity.handle, true, "")?
    {
        bail!("{conflict}");
    }
    let mut record = WorkspaceRecord::new(
        identity.ws_id.clone(),
        remote,
        identity.project_id.clone(),
        project_root,
        &identity.handle,
        &identity.branch,
        identity.base_sha.clone(),
        Some(sibling.origin.base_label.clone()),
        window_name,
        context.worktree_base_dir()?.join(&identity.handle),
    );
    record.local_config = match parent.and_then(|parent| parent.local_config.clone()) {
        Some(document) => Some(document),
        None => Some(crate::frozen_config::snapshot_document(
            &context.config,
            None,
        )?),
    };
    record.remote_worktree = Some(sibling.worktree.clone());
    record.state = WorkspaceState::Ready;
    record.layout = sibling.origin.layout.clone();
    record.execution = Some(ExecutionRecord {
        ex_id: ws.ex_id.clone(),
        panes: ws.panes.iter().map(|pane| pane.index).collect(),
        node_bin: sibling.pin.clone(),
        rex: ws.rex.clone(),
    });
    record.window_pending = true;
    record.save()?;
    let spec = super::RemoteSpec::resolve(&context.config, remote)?;
    super::observe::ensure_background(&spec, &record.project_root)?;
    report_info(
        foreground,
        &format!(
            "adopted remote agent '{}' on '{remote}', spawned by another agent there",
            identity.handle
        ),
    );
    if ws.ended.is_some() {
        return Ok(());
    }
    settle(record)
}

/// Native placement already exists on the server; adoption never changes the
/// app's selected session or creates a terminal on this machine.
fn settle(mut record: WorkspaceRecord) -> Result<()> {
    anyhow::ensure!(
        record
            .execution
            .as_ref()
            .is_some_and(|execution| execution.rex.is_some()),
        "remote workspace '{}' has no Rex session",
        record.handle
    );
    record.window_pending = false;
    record.save()
}

/// The repository of `project_id` on this machine: from the records, else
/// the current repository when it is that project.
fn project_root(project_id: &str, records: &[WorkspaceRecord]) -> Option<PathBuf> {
    if let Some(record) = records
        .iter()
        .find(|record| record.project_id == project_id)
    {
        return Some(record.project_root.clone());
    }
    let root = crate::util::canon_or_self(&crate::git::get_main_worktree_root().ok()?);
    (cache::project_id_of(&root) == project_id).then_some(root)
}

fn context(
    project_root: &std::path::Path,
    record: Option<&WorkspaceRecord>,
) -> Result<WorkflowContext> {
    let (config, location) = match record {
        Some(record) => record.creator_config()?,
        None => Config::load_with_location_from(project_root, None)?,
    };
    WorkflowContext::new_in(
        project_root,
        config,
        create_backend(detect_backend()),
        location,
    )
}

/// A node binary name as `install` makes them: `workrex-<version>` or
/// `workrex-dev-<hash>`.
fn valid_pin(name: &str) -> bool {
    crate::bin_name::pin_version(name).is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+'))
    })
}

/// Siblings this process gave up on, so observers do not retry them on
/// every frame.
fn refused() -> std::sync::MutexGuard<'static, HashSet<String>> {
    static REFUSED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    REFUSED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|err| err.into_inner())
}

/// Serializes adoption and record deletion across processes.
pub(super) fn lock(remote: &str) -> Result<File> {
    let file = cache::lock_file(remote, "adopt.lock")?;
    file.lock().context("Failed to lock adopt.lock")?;
    Ok(file)
}

fn report(foreground: bool, message: &str) {
    if foreground {
        eprintln!("{BIN}: {message}");
    } else {
        tracing::warn!("{message}");
    }
}

fn report_info(foreground: bool, message: &str) {
    if foreground {
        eprintln!("{BIN}: {message}");
    } else {
        tracing::info!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pins_look_like_installed_binaries() {
        assert!(valid_pin(&format!("{BIN}-0.1.80")));
        assert!(valid_pin(&format!("{BIN}-dev-ba7816bf8f01")));
        assert!(!valid_pin(&format!("{BIN}-")));
        assert!(!valid_pin(&format!("{BIN}-x;rm -rf ~")));
        assert!(!valid_pin(&format!("../{BIN}-1")));
        assert!(!valid_pin("sh"));
        assert!(!valid_pin("other-tool-0.1.278"));
    }
}
