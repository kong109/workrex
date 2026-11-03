//! Statuses agents report to the multiplexer itself, such as Rex program
//! status, merged into hook state when it is read.
//!
//! Hooks own the agent state files and readers never write them. What
//! readers observe goes to a separate file per pane under `reported/` that
//! only readers write, so a reader cannot undo a hook's write.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent_identity::classify_agent_kind;
use crate::multiplexer::{AgentPane, AgentStatus, LivePaneInfo, Multiplexer, ReportedStatus};

use super::{AgentState, PaneKey, StateStore};

#[cfg(test)]
mod fake_mux;
#[cfg(test)]
pub(crate) mod tests;

/// The last status a pane reported, as a reader saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReportedRecord {
    pub pane_key: PaneKey,
    pub status: AgentStatus,
    pub since: u64,
    pub workdir: PathBuf,
    pub window_name: Option<String>,
    pub session_name: Option<String>,
}

/// Let a reported status win over the stored one. Returns whether the state
/// changed. An unchanged status keeps its timestamp.
pub(crate) fn apply(state: &mut AgentState, reported: ReportedStatus) -> bool {
    let (status, since) = match reported {
        ReportedStatus::Agent { status, since } => (Some(status), Some(since)),
        ReportedStatus::Idle => (None, None),
    };
    if state.status == status {
        return false;
    }
    state.status = status;
    state.status_ts = since;
    state.activity_ts = None;
    true
}

/// What panes reported during one reconcile pass.
pub(crate) struct Reports<'a> {
    mux: &'a dyn Multiplexer,
    instance: &'a str,
    hooked: HashSet<String>,
    shown: Vec<ReportedRecord>,
}

impl<'a> Reports<'a> {
    pub(crate) fn new(mux: &'a dyn Multiplexer, instance: &'a str) -> Self {
        Self {
            mux,
            instance,
            hooked: HashSet::new(),
            shown: Vec::new(),
        }
    }

    /// Merge what the live pane of a hook-registered agent reports into it.
    pub(crate) fn overlay(&mut self, state: &mut AgentState) {
        self.hooked.insert(state.pane_key.pane_id.clone());
        let Some(reported) = self.mux.reported_status(&state.pane_key.pane_id) else {
            return;
        };
        apply(state, reported);
        if let ReportedStatus::Agent { status, since } = reported {
            self.shown.push(record(state, status, since));
        }
    }

    /// Agents for live panes that report a status but have no live hook
    /// state. Only panes in a linked worktree of the current repository
    /// count, so a build tool reporting progress in another tab is no agent.
    pub(crate) fn adopt(
        &mut self,
        live_panes: &HashMap<String, LivePaneInfo>,
        boot_id: Option<&str>,
    ) -> Vec<AgentPane> {
        let mut candidates: Vec<_> = live_panes
            .iter()
            .filter(|(pane_id, _)| !self.hooked.contains(pane_id.as_str()))
            .filter_map(|(pane_id, live)| match self.mux.reported_status(pane_id)? {
                ReportedStatus::Agent { status, since } => Some((pane_id, live, status, since)),
                ReportedStatus::Idle => None,
            })
            .collect();
        if candidates.is_empty() {
            return Vec::new();
        }
        candidates.sort_by(|a, b| a.0.cmp(b.0));
        let worktrees = current_linked_worktrees();
        candidates
            .into_iter()
            .filter(|(_, live, _, _)| within(&worktrees, &live.working_dir))
            .map(|(pane_id, live, status, since)| {
                let state = self.adopted_state(pane_id, live, status, since, boot_id);
                self.shown.push(record(&state, status, since));
                let mut pane = state.to_agent_pane(
                    live.session.clone().unwrap_or_default(),
                    live.window.clone().unwrap_or_default(),
                );
                pane.window_index = live.window_index;
                pane.window_id = live.window_id.clone().unwrap_or_default();
                pane
            })
            .collect()
    }

    fn adopted_state(
        &self,
        pane_id: &str,
        live: &LivePaneInfo,
        status: AgentStatus,
        since: u64,
        boot_id: Option<&str>,
    ) -> AgentState {
        let command = live.current_command.clone().unwrap_or_default();
        AgentState {
            pane_key: PaneKey {
                backend: self.mux.name().to_string(),
                instance: self.instance.to_string(),
                pane_id: pane_id.to_string(),
            },
            workdir: live.working_dir.clone(),
            status: Some(status),
            status_ts: Some(since),
            activity_ts: None,
            pane_title: live.title.clone(),
            pane_pid: live.pid.unwrap_or_default(),
            agent_kind: classify_agent_kind(Some(&command), live.title.as_deref()),
            command,
            updated_ts: since,
            window_name: live.window.clone(),
            session_name: live.session.clone(),
            boot_id: boot_id.map(str::to_string),
            agent_session_id: None,
            prompt: None,
            remote: None,
        }
    }

    /// Save what this pass saw for readers that skip the live query, and
    /// drop the records of panes that are gone or report nothing now.
    pub(crate) fn persist(
        self,
        store: &StateStore,
        live_panes: &HashMap<String, LivePaneInfo>,
    ) -> Result<()> {
        let dir = store.reported_dir();
        let backend = self.mux.name();
        for (path, existing) in read_records(&dir)? {
            let key = &existing.pane_key;
            if key.backend != backend || key.instance != self.instance {
                continue;
            }
            let still_reporting = live_panes.contains_key(&key.pane_id)
                && matches!(
                    self.mux.reported_status(&key.pane_id),
                    Some(ReportedStatus::Agent { .. })
                );
            if !still_reporting {
                remove(&path)?;
            }
        }
        for record in self.shown {
            let path = dir.join(record.pane_key.to_filename());
            // Of two racing readers, the one that saw the later report wins.
            if read_record(&path)?
                .is_some_and(|current| current == record || current.since > record.since)
            {
                continue;
            }
            fs::create_dir_all(&dir)?;
            crate::util::write_atomic(&path, serde_json::to_string_pretty(&record)?.as_bytes())?;
        }
        Ok(())
    }
}

/// Hook state with the statuses readers last saw merged in, plus agents
/// that only reported, for readers like last-done that skip the live query.
pub(crate) fn with_reported(
    store: &StateStore,
    agents: Vec<AgentState>,
) -> Result<Vec<AgentState>> {
    let mut records: HashMap<PaneKey, ReportedRecord> = read_records(&store.reported_dir())?
        .into_iter()
        .map(|(_, record)| (record.pane_key.clone(), record))
        .collect();
    let mut merged: Vec<AgentState> = agents
        .into_iter()
        .map(|mut state| {
            if let Some(record) = records.remove(&state.pane_key) {
                let reported = ReportedStatus::Agent {
                    status: record.status,
                    since: record.since,
                };
                apply(&mut state, reported);
            }
            state
        })
        .collect();
    merged.extend(records.into_values().map(|record| AgentState {
        workdir: record.workdir,
        status: Some(record.status),
        status_ts: Some(record.since),
        activity_ts: None,
        pane_title: None,
        pane_pid: 0,
        command: String::new(),
        updated_ts: record.since,
        window_name: record.window_name,
        session_name: record.session_name,
        boot_id: None,
        agent_kind: None,
        agent_session_id: None,
        prompt: None,
        remote: None,
        pane_key: record.pane_key,
    }));
    Ok(merged)
}

fn record(state: &AgentState, status: AgentStatus, since: u64) -> ReportedRecord {
    ReportedRecord {
        pane_key: state.pane_key.clone(),
        status,
        since,
        workdir: state.workdir.clone(),
        window_name: state.window_name.clone(),
        session_name: state.session_name.clone(),
    }
}

fn read_records(dir: &Path) -> Result<Vec<(PathBuf, ReportedRecord)>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut records = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && let Some(record) = read_record(&path)?
        {
            records.push((path, record));
        }
    }
    Ok(records)
}

fn read_record(path: &Path) -> Result<Option<ReportedRecord>> {
    match fs::read(path) {
        Ok(content) => Ok(serde_json::from_slice(&content).ok()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn remove(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

/// Linked worktrees of the repository the command runs in, from one
/// `git worktree list`.
fn current_linked_worktrees() -> Vec<PathBuf> {
    let Ok(listing) = crate::git::list_worktrees_with_main_in(None) else {
        return Vec::new();
    };
    let main = resolved(&listing.main_path);
    listing
        .worktrees
        .iter()
        .map(|(path, _)| resolved(path))
        .filter(|path| *path != main)
        .collect()
}

fn within(worktrees: &[PathBuf], dir: &Path) -> bool {
    let dir = resolved(dir);
    worktrees.iter().any(|worktree| dir.starts_with(worktree))
}

fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
