//! Show a Rex-hosted remote agent in Rex.app. The app is a client of the
//! node's Rex server over the tailnet, so the jump selects the agent's
//! session there; nothing runs locally.

use anyhow::{Result, anyhow};

use super::cache::WorkspaceRecord;
use crate::config::Config;
use crate::multiplexer::rex::RexBackend;

/// Select the session of `record`'s lowest pane on Rex.app.
pub fn select(config: &Config, record: &WorkspaceRecord) -> Result<()> {
    let placement = record
        .execution
        .as_ref()
        .and_then(|execution| execution.rex.as_ref())
        .ok_or_else(|| anyhow!("'{}' is not hosted on Rex", record.handle))?;
    let session = placement
        .sessions
        .values()
        .next()
        .ok_or_else(|| anyhow!("'{}' has no Rex session yet", record.handle))?;
    let client = super::rex_client(config, &record.remote);
    RexBackend::for_server(&placement.host_label).select_on_app(client.as_deref(), session)
}
