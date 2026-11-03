//! Show a Rex-hosted remote agent in Rex.app. The app is a client of the
//! node's Rex server over the tailnet, so the jump selects the agent's
//! session there; nothing runs locally.

use anyhow::{Result, anyhow};

use super::cache::WorkspaceRecord;
use crate::config::Config;
use crate::multiplexer::rex::RexBackend;

/// Select the session of `record`'s lowest pane on Rex.app.
pub fn select(config: &Config, record: &WorkspaceRecord) -> Result<()> {
    let (server, session) = shown_session(record)?;
    let client = super::rex_client(config, &record.remote);
    server.select_on_app(client.as_deref(), session)
}

/// Whether the server reports the session `select` would show as gone, as
/// after it was closed in Rex.app. An observation can still call it alive
/// for a few seconds; a server that does not answer leaves it in doubt.
pub fn session_gone(record: &WorkspaceRecord) -> bool {
    shown_session(record)
        .and_then(|(server, session)| server.has_session(session))
        .is_ok_and(|exists| !exists)
}

fn shown_session(record: &WorkspaceRecord) -> Result<(RexBackend, &str)> {
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
    Ok((RexBackend::for_server(&placement.host_label), session))
}
