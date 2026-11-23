use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::multiplexer::AgentStatus;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFence {
    pub status: Option<AgentStatus>,
    pub status_ts: Option<u64>,
    pub sent_at: u64,
}

impl TurnFence {
    pub const LAPSE: Duration = Duration::from_secs(120);

    pub fn admits(&self, status: Option<AgentStatus>, status_ts: Option<u64>) -> bool {
        let Some(baseline) = self.status_ts else {
            return status.is_some();
        };
        status_ts.is_some_and(|ts| match self.status {
            Some(AgentStatus::Working) => ts >= baseline,
            _ => ts > baseline,
        })
    }

    pub fn lapsed(&self, now: u64) -> bool {
        now >= self.sent_at + Self::LAPSE.as_secs()
    }
}

pub fn same_second(status: Option<AgentStatus>, status_ts: Option<u64>, now: u64) -> bool {
    matches!(status, Some(AgentStatus::Done | AgentStatus::Waiting))
        && status_ts.is_some_and(|ts| ts >= now)
}

pub fn sleep_to_next_second() {
    let into = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    std::thread::sleep(Duration::from_nanos(u64::from(1_000_000_000 - into)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fence_admits_only_later_states() {
        let fence = |status| TurnFence {
            status,
            status_ts: status.map(|_| 100),
            sent_at: 1000,
        };
        let done = fence(Some(AgentStatus::Done));
        let working = Some(AgentStatus::Working);
        assert!(!done.admits(Some(AgentStatus::Done), Some(100)));
        assert!(!done.admits(Some(AgentStatus::Done), Some(90)));
        assert!(done.admits(working, Some(101)));
        assert!(done.admits(Some(AgentStatus::Done), Some(101)));
        assert!(!done.admits(None, None));
        assert!(!fence(Some(AgentStatus::Waiting)).admits(Some(AgentStatus::Done), Some(99)));

        let busy = fence(working);
        assert!(busy.admits(working, Some(100)));
        assert!(busy.admits(Some(AgentStatus::Done), Some(104)));
        assert!(!busy.admits(Some(AgentStatus::Done), Some(99)));

        let first = fence(None);
        assert!(first.admits(Some(AgentStatus::Done), Some(1)));
        assert!(!first.admits(None, None));

        assert!(!done.lapsed(1119));
        assert!(done.lapsed(1120));
    }

    #[test]
    fn only_done_or_waiting_states_of_this_second_wait_for_the_next() {
        assert!(same_second(Some(AgentStatus::Done), Some(7), 7));
        assert!(same_second(Some(AgentStatus::Waiting), Some(8), 7));
        assert!(!same_second(Some(AgentStatus::Done), Some(6), 7));
        assert!(!same_second(Some(AgentStatus::Working), Some(7), 7));
        assert!(!same_second(None, None, 7));
    }
}
