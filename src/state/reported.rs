//! Statuses agents report to the multiplexer itself, merged into stored state.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::agent_identity::classify_agent_kind;
use crate::multiplexer::{LivePaneInfo, Multiplexer, ReportedStatus};

use super::{AgentState, PaneKey};

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

/// Agents for live panes that report a status but that no hook registered.
/// Only panes in a linked git worktree count, so a build tool reporting
/// progress in some other tab does not become an agent.
pub(crate) fn adopt_reporting_panes(
    agents: &[AgentState],
    mux: &dyn Multiplexer,
    instance: &str,
    boot_id: Option<&str>,
    live_panes: &HashMap<String, LivePaneInfo>,
) -> Vec<AgentState> {
    let backend = mux.name();
    let known: HashSet<&str> = agents
        .iter()
        .filter(|state| state.pane_key.backend == backend && state.pane_key.instance == instance)
        .map(|state| state.pane_key.pane_id.as_str())
        .collect();
    let mut adopted: Vec<AgentState> = live_panes
        .iter()
        .filter(|(pane_id, _)| !known.contains(pane_id.as_str()))
        .filter_map(|(pane_id, live)| {
            let ReportedStatus::Agent { status, since } = mux.reported_status(pane_id)? else {
                return None;
            };
            in_linked_worktree(&live.working_dir).then(|| {
                let command = live.current_command.clone().unwrap_or_default();
                AgentState {
                    pane_key: PaneKey {
                        backend: backend.to_string(),
                        instance: instance.to_string(),
                        pane_id: pane_id.clone(),
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
            })
        })
        .collect();
    adopted.sort_by(|a, b| a.pane_key.pane_id.cmp(&b.pane_key.pane_id));
    adopted
}

fn in_linked_worktree(path: &Path) -> bool {
    crate::git::worktree_root_if_present(path)
        .ok()
        .flatten()
        .is_some_and(|root| crate::git::linked_worktree_admin_dir(&root).is_some())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::multiplexer::AgentStatus;

    pub(crate) fn hooked(pane_id: &str, status: AgentStatus, status_ts: u64) -> AgentState {
        AgentState {
            pane_key: PaneKey {
                backend: "rex".to_string(),
                instance: "unix:///tmp/rex.sock".to_string(),
                pane_id: pane_id.to_string(),
            },
            workdir: PathBuf::from("/repo/wt"),
            status: Some(status),
            status_ts: Some(status_ts),
            activity_ts: Some(status_ts),
            pane_title: None,
            pane_pid: 7,
            command: "claude".to_string(),
            updated_ts: status_ts,
            window_name: Some("wm-feat".to_string()),
            session_name: Some("main".to_string()),
            boot_id: None,
            agent_kind: Some("claude".to_string()),
            agent_session_id: None,
            prompt: None,
            remote: None,
        }
    }

    #[test]
    fn a_reported_status_replaces_a_stale_one_with_its_time() {
        let mut state = hooked("block:a", AgentStatus::Waiting, 100);
        let working = ReportedStatus::Agent {
            status: AgentStatus::Working,
            since: 200,
        };

        assert!(apply(&mut state, working));
        assert_eq!(state.status, Some(AgentStatus::Working));
        assert_eq!(state.status_ts, Some(200));
        assert_eq!(state.activity_ts(), Some(200));
    }

    #[test]
    fn an_unchanged_status_keeps_its_hook_timestamp() {
        let mut state = hooked("block:a", AgentStatus::Working, 100);
        let working = ReportedStatus::Agent {
            status: AgentStatus::Working,
            since: 200,
        };

        assert!(!apply(&mut state, working));
        assert_eq!(state.status_ts, Some(100));
    }

    #[test]
    fn idle_clears_the_hook_status() {
        let mut state = hooked("block:a", AgentStatus::Waiting, 100);

        assert!(apply(&mut state, ReportedStatus::Idle));
        assert_eq!(state.status, None);
        assert_eq!(state.status_ts, None);
    }
}
