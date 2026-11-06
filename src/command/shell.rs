use crate::bin_name::BIN;
use anyhow::{Result, bail};

use crate::remote::cache::WorkspaceRecord;

/// Open an independent native Rex shell session for a remote workspace.
pub fn run(name: &str, host: bool) -> Result<()> {
    if crate::remote::node::operation::in_session() {
        bail!("this pane is already on the server, in the workspace's worktree");
    }
    let record = named(name)?;
    let (config, _) = record.creator_config()?;
    crate::remote::shell::run(&config, &record, host)
}

fn named(name: &str) -> Result<WorkspaceRecord> {
    if let Some(record) = crate::remote::agents::find_target(name) {
        return Ok(record);
    }
    if crate::git::find_worktree(name).is_ok() {
        bail!(
            "'{name}' is a local worktree; `{BIN} shell` opens shells on remote servers. \
             Use `cd \"$({BIN} path {name})\"`"
        );
    }
    bail!("no remote workspace named '{name}'; `{BIN} list` shows this repository's workspaces")
}
