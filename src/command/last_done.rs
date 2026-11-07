use std::cmp::Reverse;

use anyhow::Result;
use tracing::debug;

use crate::multiplexer::rex::AppRefused;
use crate::multiplexer::{AgentStatus, create_backend, detect_backend};
use crate::state::{AgentState, LastDoneCycleState, PaneKey, StateStore};

/// Switch to the agent that most recently completed or is waiting for input.
///
/// Finds all agents with "done" or "waiting" status from the StateStore and
/// switches to the one with the most recent timestamp. Cycles through matching
/// agents on repeated invocations using persisted cycle state.
pub fn run() -> Result<()> {
    let mux = create_backend(detect_backend());
    let store = StateStore::new()?;

    // Read agent state directly from disk without validating against Rex.
    // This avoids O(n) Rex queries. Dead panes are handled during switch.
    let agents = crate::state::reported::with_reported(&store, store.list_all_agents()?)?;

    // Filter to done/waiting agents for current backend/instance
    let backend_name = mux.name();
    let instance_id = mux.instance_id();
    let mut done_agents: Vec<_> = agents
        .into_iter()
        .filter(|a| {
            matches!(
                a.status,
                Some(AgentStatus::Done) | Some(AgentStatus::Waiting)
            ) && a.pane_key.backend == backend_name
                && a.pane_key.instance == instance_id
        })
        .collect();

    debug!(count = done_agents.len(), "done/waiting agents");

    if done_agents.is_empty() {
        println!("No completed or waiting agents found");
        return Ok(());
    }

    // Sort by status_ts descending (most recent first), with updated_ts as
    // tiebreaker for agents that changed status in the same second.
    sort_by_recency(&mut done_agents);

    let head_ts = done_agents[0].status_ts;

    let current_pane = mux.active_pane_id();
    debug!(current_pane = ?current_pane, "active pane");

    // Build PaneKey for current pane (for cycle state comparison)
    let current_key = current_pane.map(|id| PaneKey {
        backend: backend_name.to_string(),
        instance: instance_id.clone(),
        pane_id: id,
    });

    // Load saved cycle state and determine if we're continuing a cycle
    let saved_cycle = store.load_settings().ok().and_then(|s| s.last_done_cycle);
    let is_cycling = is_cycle_active(current_key.as_ref(), saved_cycle.as_ref(), head_ts);
    debug!(is_cycling, saved_target = ?saved_cycle.as_ref().map(|s| &s.target.pane_id), "cycle state");

    let current_idx = current_key
        .as_ref()
        .and_then(|key| done_agents.iter().position(|a| a.pane_key == *key));

    let target_idx = pick_target(current_idx, done_agents.len(), is_cycling);
    debug!(current_idx = ?current_idx, target_idx, "target selection");

    let jump = first_reachable(&done_agents, target_idx, |agent| {
        let pane_id = &agent.pane_key.pane_id;
        debug!(
            pane_id,
            status = ?agent.status,
            status_ts = ?agent.status_ts,
            "trying agent"
        );
        mux.switch_to_pane(pane_id, agent.window_name.as_deref())
    });
    // Persist cycle state so next invocation can continue the cycle
    if let Jump::To(agent) = jump {
        save_cycle_state(&store, &agent.pane_key, head_ts);
    }
    if let Some(message) = jump.message() {
        println!("{message}");
    }
    Ok(())
}

enum Jump<'a> {
    To(&'a AgentState),
    Refused,
    NoneLive,
}

impl Jump<'_> {
    /// A refused jump says nothing: the backend already printed the fix.
    fn message(&self) -> Option<&'static str> {
        match self {
            Jump::NoneLive => Some("No active completed or waiting agents found"),
            Jump::To(_) | Jump::Refused => None,
        }
    }
}

/// Tries agents in order from `start`, skipping dead panes. A refused jump
/// ends the search: the app would refuse every other pane too.
fn first_reachable(
    agents: &[AgentState],
    start: usize,
    mut switch: impl FnMut(&AgentState) -> Result<()>,
) -> Jump<'_> {
    for i in 0..agents.len() {
        let agent = &agents[(start + i) % agents.len()];
        match switch(agent) {
            Ok(()) => return Jump::To(agent),
            Err(e) if e.is::<AppRefused>() => return Jump::Refused,
            Err(e) => {
                debug!(pane_id = agent.pane_key.pane_id, error = %e, "pane dead, trying next")
            }
        }
    }
    Jump::NoneLive
}

/// Check if the user is in an active last-done cycle.
///
/// A cycle is active when:
/// 1. We have saved cycle state
/// 2. The current pane matches the target we last navigated to
/// 3. The head of the sorted list hasn't changed (no new completions)
fn is_cycle_active(
    current_key: Option<&PaneKey>,
    saved: Option<&LastDoneCycleState>,
    head_ts: Option<u64>,
) -> bool {
    match (current_key, saved) {
        (Some(key), Some(state)) => state.target == *key && state.head_ts == head_ts,
        _ => false,
    }
}

/// Pick the target index in the sorted list.
/// - If cycling (last-done brought us here): advance to next
/// - Otherwise (fresh invocation): go to most recent (index 0)
fn pick_target(current_idx: Option<usize>, len: usize, is_cycling: bool) -> usize {
    if is_cycling && let Some(idx) = current_idx {
        return (idx + 1) % len;
    }
    0
}

/// Sort agents by recency: most recent status change first, with updated_ts
/// as tiebreaker for deterministic ordering.
fn sort_by_recency(agents: &mut [AgentState]) {
    agents.sort_by_key(|a| {
        (
            Reverse(a.status_ts),
            Reverse(a.updated_ts),
            Reverse(a.pane_key.pane_id.clone()),
        )
    });
}

/// Save cycle state after a successful switch.
fn save_cycle_state(store: &StateStore, target: &PaneKey, head_ts: Option<u64>) {
    if let Ok(mut settings) = store.load_settings() {
        settings.last_done_cycle = Some(LastDoneCycleState {
            target: target.clone(),
            head_ts,
        });
        let _ = store.save_settings(&settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn make_agent(
        pane_id: &str,
        status: AgentStatus,
        status_ts: u64,
        updated_ts: u64,
    ) -> AgentState {
        AgentState {
            pane_key: PaneKey {
                backend: "Rex".to_string(),
                instance: "default".to_string(),
                pane_id: pane_id.to_string(),
            },
            workdir: PathBuf::from("/tmp"),
            status: Some(status),
            status_ts: Some(status_ts),
            activity_ts: Some(status_ts),
            pane_title: None,
            pane_pid: 1000,
            command: "node".to_string(),
            updated_ts,
            window_name: Some("wr-test".to_string()),
            session_name: Some("main".to_string()),
            boot_id: None,
            agent_kind: None,
            agent_session_id: None,
            prompt: None,
            remote: None,
        }
    }

    fn make_key(pane_id: &str) -> PaneKey {
        PaneKey {
            backend: "Rex".to_string(),
            instance: "default".to_string(),
            pane_id: pane_id.to_string(),
        }
    }

    fn make_cycle_state(pane_id: &str, head_ts: u64) -> LastDoneCycleState {
        LastDoneCycleState {
            target: make_key(pane_id),
            head_ts: Some(head_ts),
        }
    }

    // -- jump tests --

    #[test]
    fn a_refused_jump_stops_without_trying_other_agents() {
        let agents = [
            make_agent("%1", AgentStatus::Done, 2, 2),
            make_agent("%2", AgentStatus::Done, 1, 1),
        ];
        let mut tried = Vec::new();
        let jump = first_reachable(&agents, 0, |agent| {
            tried.push(agent.pane_key.pane_id.clone());
            Err(AppRefused.into())
        });
        assert!(matches!(jump, Jump::Refused));
        assert_eq!(jump.message(), None);
        assert_eq!(tried, ["%1"]);
    }

    #[test]
    fn dead_panes_are_skipped_until_one_is_reached() {
        let agents = [
            make_agent("%1", AgentStatus::Done, 2, 2),
            make_agent("%2", AgentStatus::Done, 1, 1),
        ];
        let jump = first_reachable(&agents, 0, |agent| match agent.pane_key.pane_id.as_str() {
            "%1" => Err(anyhow::anyhow!("pane dead")),
            _ => Ok(()),
        });
        assert!(matches!(jump, Jump::To(agent) if agent.pane_key.pane_id == "%2"));
        let none = first_reachable(&agents, 1, |_| Err(anyhow::anyhow!("pane dead")));
        assert!(matches!(none, Jump::NoneLive));
    }

    // -- sort tests --

    #[test]
    fn test_sort_by_recency_orders_most_recent_first() {
        let mut agents = vec![
            make_agent("%1", AgentStatus::Done, 100, 100),
            make_agent("%2", AgentStatus::Done, 300, 300),
            make_agent("%3", AgentStatus::Done, 200, 200),
        ];
        sort_by_recency(&mut agents);
        assert_eq!(agents[0].pane_key.pane_id, "%2");
        assert_eq!(agents[1].pane_key.pane_id, "%3");
        assert_eq!(agents[2].pane_key.pane_id, "%1");
    }

    #[test]
    fn test_sort_uses_updated_ts_as_tiebreaker() {
        let mut agents = vec![
            make_agent("%1", AgentStatus::Done, 100, 200),
            make_agent("%2", AgentStatus::Done, 100, 300),
        ];
        sort_by_recency(&mut agents);
        assert_eq!(agents[0].pane_key.pane_id, "%2");
        assert_eq!(agents[1].pane_key.pane_id, "%1");
    }

    #[test]
    fn test_sort_none_status_ts_goes_last() {
        let mut agents = vec![
            AgentState {
                status_ts: None,
                ..make_agent("%1", AgentStatus::Done, 0, 100)
            },
            make_agent("%2", AgentStatus::Done, 50, 50),
        ];
        sort_by_recency(&mut agents);
        assert_eq!(agents[0].pane_key.pane_id, "%2");
        assert_eq!(agents[1].pane_key.pane_id, "%1");
    }

    #[test]
    fn test_sort_mixed_done_and_waiting() {
        let mut agents = vec![
            make_agent("%1", AgentStatus::Done, 100, 100),
            make_agent("%2", AgentStatus::Waiting, 200, 200),
            make_agent("%3", AgentStatus::Done, 300, 300),
        ];
        sort_by_recency(&mut agents);
        assert_eq!(agents[0].pane_key.pane_id, "%3");
        assert_eq!(agents[1].pane_key.pane_id, "%2");
        assert_eq!(agents[2].pane_key.pane_id, "%1");
    }

    // -- cycle detection tests --

    #[test]
    fn test_not_cycling_without_saved_state() {
        let key = make_key("%1");
        assert!(!is_cycle_active(Some(&key), None, Some(300)));
    }

    #[test]
    fn test_not_cycling_when_on_different_pane() {
        let current = make_key("%2");
        let saved = make_cycle_state("%1", 300);
        assert!(!is_cycle_active(Some(&current), Some(&saved), Some(300)));
    }

    #[test]
    fn test_cycling_when_on_saved_target_and_head_unchanged() {
        let current = make_key("%1");
        let saved = make_cycle_state("%1", 300);
        assert!(is_cycle_active(Some(&current), Some(&saved), Some(300)));
    }

    #[test]
    fn test_not_cycling_when_head_changed() {
        // A new agent finished (head_ts changed from 300 to 400)
        let current = make_key("%1");
        let saved = make_cycle_state("%1", 300);
        assert!(!is_cycle_active(Some(&current), Some(&saved), Some(400)));
    }

    #[test]
    fn test_not_cycling_without_current_pane() {
        let saved = make_cycle_state("%1", 300);
        assert!(!is_cycle_active(None, Some(&saved), Some(300)));
    }

    // -- target selection tests --

    #[test]
    fn test_fresh_invocation_goes_to_most_recent() {
        assert_eq!(pick_target(None, 3, false), 0);
        assert_eq!(pick_target(Some(1), 3, false), 0);
        assert_eq!(pick_target(Some(2), 3, false), 0);
    }

    #[test]
    fn test_fresh_on_most_recent_still_goes_to_most_recent() {
        // Already on idx 0 but not cycling - go to idx 0 (switch to it / no-op)
        assert_eq!(pick_target(Some(0), 3, false), 0);
    }

    #[test]
    fn test_cycling_advances_to_next() {
        assert_eq!(pick_target(Some(0), 3, true), 1);
        assert_eq!(pick_target(Some(1), 3, true), 2);
    }

    #[test]
    fn test_cycling_wraps_around() {
        assert_eq!(pick_target(Some(2), 3, true), 0);
    }

    #[test]
    fn test_cycling_single_agent() {
        assert_eq!(pick_target(Some(0), 1, true), 0);
    }

    #[test]
    fn test_cycling_without_current_idx_resets() {
        // Cycling flag set but pane no longer in list (agent status changed)
        assert_eq!(pick_target(None, 3, true), 0);
    }
}
