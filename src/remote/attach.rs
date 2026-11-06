//! Select a remote Rex session in the connected application.

use anyhow::{Result, anyhow};
use std::time::Duration;

pub(super) const STABLE: Duration = Duration::from_secs(30);

pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64.checked_shl(attempt).unwrap_or(u64::MAX).min(30))
}

pub fn run(name: &str, ex_id: &str, pane: usize) -> Result<()> {
    let record = super::cache::by_execution(name, ex_id)
        .ok_or_else(|| anyhow!("execution '{ex_id}' is not recorded for '{name}'"))?;
    let (config, _) = record.creator_config()?;
    super::rex_jump::select_pane(&config, &record, pane)
}
