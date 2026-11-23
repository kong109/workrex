use std::collections::HashSet;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::bin_name::BIN;
use crate::config::Config;
use crate::git;
use crate::multiplexer::{
    AgentPane, AgentStatus, FIRST_HOOK_GRACE_SECS, Launch, PaneStatus, create_backend,
    detect_backend,
};
use crate::remote::{self, cache::WorkspaceRecord, node::observe::ObservedAgent};
use crate::state::{PaneKey, StateStore};
use crate::util;
use crate::workflow;

/// Resolve a worktree name to its path, trying local git first then the
/// agents `load_agents` returns.
///
/// Local resolution is preferred because it works even before an agent starts
/// (the worktree directory exists from `workrex add`). Global resolution requires
/// a running agent. Its errors lose their agent error kind: `wait` exits 1
/// on a bad target and keeps its own exit codes, such as 4 for stalled.
fn resolve_worktree_path(
    name: &str,
    load_agents: impl FnOnce() -> Result<Vec<AgentPane>>,
) -> Result<std::path::PathBuf> {
    // Try local git resolution first (supports waiting for unstarted agents)
    if git::is_git_repo().unwrap_or(false) {
        match git::find_worktree(name) {
            Ok((path, _branch)) => return Ok(path),
            Err(e) if e.downcast_ref::<git::WorktreeNotFound>().is_some() => {}
            Err(e) => return Err(e),
        }
    }

    // Fall back to global agent resolution
    let (path, _agents) = workflow::resolve_worktree_agents_from_snapshot(name, &load_agents()?)
        .map_err(|err| anyhow!("{err}"))?;
    Ok(path)
}

/// The repository's worktree roots, or just `wt_path` once it is gone.
///
/// `workrex merge` can remove the worktree between two git calls, leaving a
/// missing directory or a `.git` file whose metadata is gone. Any failure here
/// means the worktree is no longer a live checkout, so it stands alone.
fn current_worktree_roots_or_removed_self(wt_path: &std::path::Path) -> Vec<std::path::PathBuf> {
    let roots = || -> Result<Option<Vec<std::path::PathBuf>>> {
        if git::get_repo_root_if_present_in(Some(wt_path))?.is_none() {
            return Ok(None);
        }
        Ok(Some(
            git::list_worktrees_with_main_in(Some(wt_path))?.canonical_paths(),
        ))
    };
    match roots() {
        Ok(Some(roots)) => roots,
        Ok(None) | Err(_) => vec![util::canon_or_self(wt_path)],
    }
}

pub fn parse_status(s: &str) -> Result<AgentStatus> {
    match s {
        "working" => Ok(AgentStatus::Working),
        "waiting" => Ok(AgentStatus::Waiting),
        "done" => Ok(AgentStatus::Done),
        _ => Err(anyhow!(
            "Invalid status '{}'. Must be: working, waiting, done",
            s
        )),
    }
}

pub fn run(
    worktree_names: &[String],
    target_status: &str,
    timeout_secs: Option<u64>,
    any: bool,
) -> Result<()> {
    let target = parse_status(target_status)?;
    let mux = create_backend(detect_backend());
    let start = Instant::now();

    // Remote workspaces are watched through their node; the rest resolve
    // to paths upfront (local git first, then global agents).
    let mut remote_targets: Vec<(String, WorkspaceRecord)> = Vec::new();
    let mut worktree_paths = Vec::new();
    for name in worktree_names {
        match remote::agents::find_target(name) {
            Some(record) => remote_targets.push((name.clone(), record)),
            None => worktree_paths.push((
                name.clone(),
                resolve_worktree_path(name, || {
                    StateStore::new()?.load_reconciled_agents(mux.as_ref())
                })?,
            )),
        }
    }
    let config = if remote_targets.is_empty() {
        Config::default()
    } else {
        Config::load(None).unwrap_or_default()
    };
    let mut remotes: Vec<String> = remote_targets
        .iter()
        .map(|(_, record)| record.remote.clone())
        .collect();
    remotes.sort();
    remotes.dedup();
    remote::observe::spawn(&config, &remotes);

    let mut reached: HashSet<String> = HashSet::new();
    let mut seen_agent: HashSet<String> = HashSet::new();
    // A wait on remote workspaces only never touches the local server.
    let local = if worktree_paths.is_empty() {
        None
    } else {
        Some((StateStore::new()?, mux.instance_id()))
    };

    loop {
        // Check timeout
        if let Some(timeout) = timeout_secs
            && start.elapsed() > Duration::from_secs(timeout)
        {
            let remaining: Vec<_> = worktree_names
                .iter()
                .filter(|n| !reached.contains(n.as_str()))
                .collect();
            eprintln!(
                "Timeout waiting for: {}",
                remaining
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            std::process::exit(1);
        }

        // A stalled agent ends the wait only after this pass, so that with
        // `--any` a worktree that reached the target wins.
        let mut stalled: Option<&str> = None;
        for (name, record) in &remote_targets {
            if reached.contains(name) {
                continue;
            }
            match remote_progress(record, target) {
                RemoteProgress::Unknown => {}
                RemoteProgress::Running => {
                    seen_agent.insert(name.clone());
                }
                RemoteProgress::Stalled => {
                    seen_agent.insert(name.clone());
                    stalled.get_or_insert(name);
                }
                RemoteProgress::Reached => {
                    seen_agent.insert(name.clone());
                    let elapsed = util::format_elapsed_duration(start.elapsed());
                    eprintln!("{}: {} ({})", name, target_status, elapsed);
                    reached.insert(name.clone());
                    if any {
                        return Ok(());
                    }
                }
                RemoteProgress::Removed => {
                    let elapsed = util::format_elapsed_duration(start.elapsed());
                    eprintln!("{}: removed ({})", name, elapsed);
                    reached.insert(name.clone());
                    if any {
                        return Ok(());
                    }
                }
                RemoteProgress::Gone if seen_agent.contains(name) => {
                    eprintln!("{}: agent exited unexpectedly", name);
                    std::process::exit(3);
                }
                RemoteProgress::Gone => {}
            }
        }
        if reached.len() == worktree_names.len() {
            return Ok(());
        }
        let Some((store, instance)) = &local else {
            if let Some(name) = stalled {
                exit_stalled(name);
            }
            thread::sleep(Duration::from_secs(2));
            continue;
        };
        let pane_key = |pane_id: &str| PaneKey {
            backend: mux.name().to_string(),
            instance: instance.clone(),
            pane_id: pane_id.to_string(),
        };

        // Load current agent state
        let agent_panes = store.load_reconciled_agents(mux.as_ref())?;
        let now = remote::cache::now();

        for (name, wt_path) in &worktree_paths {
            if reached.contains(name) {
                continue;
            }

            let roots = current_worktree_roots_or_removed_self(wt_path);
            let matching = workflow::match_agents_to_worktree(&agent_panes, wt_path, &roots);

            if !matching.is_empty() {
                seen_agent.insert(name.clone());

                let sent = matching
                    .iter()
                    .filter_map(|a| {
                        let fence = store.sent_turn(&pane_key(&a.pane_id))?;
                        Some((a.pane_id.as_str(), fence))
                    })
                    .max_by_key(|(_, fence)| fence.sent_at);
                let fence = sent
                    .as_ref()
                    .map(|(_, fence)| fence)
                    .filter(|fence| !fence.lapsed(now));
                let has_target = matching
                    .iter()
                    .filter(|a| sent.as_ref().is_none_or(|(pane, _)| a.pane_id == *pane))
                    .any(|a| {
                        a.status == Some(target)
                            && fence.is_none_or(|fence| fence.admits(a.status, a.status_ts))
                    });
                if has_target {
                    let elapsed = util::format_elapsed_duration(start.elapsed());
                    eprintln!("{}: {} ({})", name, target_status, elapsed);
                    reached.insert(name.clone());

                    if any {
                        return Ok(());
                    }
                } else if matching
                    .iter()
                    .any(|a| a.pane_status(now) == Some(PaneStatus::Stalled))
                {
                    stalled.get_or_insert(name);
                }
            } else if seen_agent.contains(name) {
                // Agent was previously running but disappeared
                // Check if worktree still exists - if not, it was merged (success)
                if !wt_path.exists() {
                    let elapsed = util::format_elapsed_duration(start.elapsed());
                    eprintln!("{}: merged ({})", name, elapsed);
                    reached.insert(name.clone());

                    if any {
                        return Ok(());
                    }
                } else {
                    // Worktree exists but agent gone - crashed/exited unexpectedly
                    eprintln!("{}: agent exited unexpectedly", name);
                    std::process::exit(3);
                }
            }
            // If we haven't seen an agent yet and it's been > 10s, still wait --
            // the agent may not have started yet. The timeout flag handles the
            // overall deadline.
        }

        // Check if all have reached target
        if reached.len() == worktree_names.len() {
            return Ok(());
        }
        if let Some(name) = stalled {
            exit_stalled(name);
        }

        thread::sleep(Duration::from_secs(2));
    }
}

/// Exit for an agent that fired no status hook within the grace window:
/// it may wait at a prompt that only its screen shows.
fn exit_stalled(name: &str) -> ! {
    eprintln!(
        "{name}: stalled: no status hook {FIRST_HOOK_GRACE_SECS}s after launch. \
         Run `{BIN} capture {name}` to see its screen, and answer any prompt with \
         `{BIN} send {name}`. If it shows no prompt, its status hooks may not be \
         installed: run `{BIN} setup`"
    );
    std::process::exit(4);
}

/// Where a remote workspace's agents stand, from the latest observation.
enum RemoteProgress {
    /// The remote is not observed right now; keep waiting.
    Unknown,
    Running,
    /// An agent fired no status hook within the grace window.
    Stalled,
    Reached,
    /// No agent runs in the workspace.
    Gone,
    /// The workspace was removed.
    Removed,
}

fn remote_progress(record: &WorkspaceRecord, target: AgentStatus) -> RemoteProgress {
    let current = remote::cache::all()
        .into_iter()
        .find(|r| r.remote == record.remote && r.ws_id == record.ws_id && !r.cleanup_pending());
    let Some(current) = current else {
        return RemoteProgress::Removed;
    };
    let Some(execution) = &current.execution else {
        return RemoteProgress::Gone;
    };
    let Some(observation) = remote::observe::load_connected(&current.remote) else {
        return RemoteProgress::Unknown;
    };
    let Some(workspace) = observation.execution(&current.ws_id, &execution.ex_id) else {
        return RemoteProgress::Unknown;
    };
    // After a send, only the agent sent to counts, and until the fence
    // lapses only the turn the send started.
    let sent = remote::cache::sent_turn(&current.remote, &current.ws_id)
        .filter(|turn| turn.ex_id == execution.ex_id);
    let fence = sent
        .as_ref()
        .filter(|turn| !turn.lapsed(remote::cache::now()));
    let agents: Vec<_> = workspace
        .panes
        .iter()
        .filter(|pane| sent.as_ref().is_none_or(|turn| turn.pane == pane.index))
        .filter_map(|pane| pane.agent.as_ref())
        .collect();
    let reached = |agent: &ObservedAgent| {
        agent.status == Some(target) && fence.is_none_or(|turn| turn.admits(agent))
    };
    let stalled = |agent: &ObservedAgent| {
        agent.launched.is_some_and(|launch| {
            Launch {
                at: observation.local_ts(launch.at),
                ..launch
            }
            .stalled(remote::cache::now())
        })
    };
    if agents.iter().any(|agent| reached(agent)) {
        RemoteProgress::Reached
    } else if agents.iter().any(|agent| stalled(agent)) {
        RemoteProgress::Stalled
    } else if agents.is_empty() {
        RemoteProgress::Gone
    } else {
        RemoteProgress::Running
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn agent_at(path: &std::path::Path, pane_id: &str) -> AgentPane {
        serde_json::from_value(serde_json::json!({
            "session": "test",
            "window_name": "wr-feature",
            "pane_id": pane_id,
            "path": path,
        }))
        .unwrap()
    }

    /// `wait` exits 4 for a stalled agent, so a target it cannot resolve
    /// must not carry the agent error kind that `main` maps to exit 4 or 5.
    #[test]
    fn an_unresolvable_wait_target_is_a_plain_error() {
        let temp = tempfile::tempdir().unwrap();
        let panes = vec![
            agent_at(&temp.path().join("alpha/feature"), "%1"),
            agent_at(&temp.path().join("beta/feature"), "%2"),
        ];
        for name in ["nosuch:feature", "absent:wr-nothing-here"] {
            let err = resolve_worktree_path(name, || Ok(panes.clone())).unwrap_err();
            assert_eq!(crate::agent_error::kind_of(&err), None, "{err}");
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn worktree_removed_mid_poll_stands_alone() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let wt = temp.path().join("wt");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "--initial-branch=main"]);
        git(&repo, &["commit", "--allow-empty", "-m", "init"]);
        git(
            &repo,
            &["worktree", "add", "-b", "feature", wt.to_str().unwrap()],
        );

        let live = current_worktree_roots_or_removed_self(&wt);
        assert!(live.contains(&util::canon_or_self(&repo)), "{live:?}");
        assert!(live.contains(&util::canon_or_self(&wt)), "{live:?}");

        // `workrex merge` has deleted the worktree metadata but not yet the
        // checkout: its `.git` file points at a missing directory.
        std::fs::remove_dir_all(repo.join(".git/worktrees")).unwrap();
        assert_eq!(
            current_worktree_roots_or_removed_self(&wt),
            vec![util::canon_or_self(&wt)]
        );

        std::fs::remove_dir_all(&wt).unwrap();
        assert_eq!(current_worktree_roots_or_removed_self(&wt), vec![wt]);
    }
}
