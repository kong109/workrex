//! Shared stale-agent classification used by dashboard and sidebar.

use crate::multiplexer::AgentStatus;

/// Whether an agent is stale under the shared timeout and activity rules.
pub(crate) fn is_stale(
    activity_ts: Option<u64>,
    status: Option<AgentStatus>,
    now_secs: u64,
    stale_after_secs: u64,
    is_sleeping: bool,
    is_interrupted: bool,
) -> bool {
    if is_sleeping {
        return true;
    }

    // Waiting and Done stay actionable until focus clears them. An interruption
    // only overrides Working, since the interrupted set can lag a newer status.
    match status {
        Some(AgentStatus::Waiting | AgentStatus::Done) => return false,
        Some(AgentStatus::Working) if !is_interrupted => return false,
        _ => {}
    }

    activity_ts
        .map(|ts| now_secs.saturating_sub(ts) > stale_after_secs)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_threshold_strictly_after_the_boundary() {
        assert!(!is_stale(Some(100), None, 200, 100, false, false));
        assert!(is_stale(Some(100), None, 201, 100, false, false));
    }

    #[test]
    fn missing_activity_is_stale_unless_status_is_active() {
        assert!(is_stale(None, None, 100, 60, false, false));
        assert!(!is_stale(
            None,
            Some(AgentStatus::Working),
            100,
            60,
            false,
            false,
        ));
        assert!(!is_stale(
            None,
            Some(AgentStatus::Waiting),
            100,
            60,
            false,
            false,
        ));
    }

    #[test]
    fn unacknowledged_completion_does_not_age_out() {
        for activity_ts in [Some(100), None] {
            assert!(!is_stale(
                activity_ts,
                Some(AgentStatus::Done),
                10_000,
                60,
                false,
                false,
            ));
            assert!(is_stale(activity_ts, None, 10_000, 60, false, false));
        }
    }

    #[test]
    fn interruption_only_overrides_working() {
        let stale =
            |status, interrupted| is_stale(Some(100), status, 10_000, 60, false, interrupted);
        assert!(!stale(Some(AgentStatus::Waiting), true));
        assert!(!stale(Some(AgentStatus::Done), true));
        assert!(stale(Some(AgentStatus::Working), true));
        assert!(!stale(Some(AgentStatus::Working), false));
        assert!(stale(None, true));
        assert!(stale(None, false));
    }

    #[test]
    fn sleeping_and_interrupted_statuses_override_active_status() {
        assert!(is_stale(
            Some(100),
            Some(AgentStatus::Working),
            100,
            60,
            true,
            false,
        ));
        assert!(is_stale(
            Some(100),
            Some(AgentStatus::Working),
            201,
            100,
            false,
            true,
        ));
        assert!(!is_stale(
            Some(150),
            Some(AgentStatus::Working),
            201,
            100,
            false,
            true,
        ));
    }
}
