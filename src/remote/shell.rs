//! Open an interactive remote shell and select its native session in Rex.app.
//! The pre-RPC cleanup guard cancels failed or lost requests. Once the app
//! has shown the session, the shell belongs to the remote execution.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crate::bin_name::BIN;
use anyhow::{Context, Result, anyhow, bail};

use super::RemoteSpec;
use super::cache::WorkspaceRecord;
use super::client::{Node, error_code};
use super::install::Pin;
use super::node::shell::{ShellRequest, ShellResponse};
use super::protocol::{self, code};
use super::transport::Transport;
use crate::config::Config;

pub fn run(config: &Config, record: &WorkspaceRecord, host: bool) -> Result<()> {
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        bail!("{BIN} shell needs a terminal");
    }
    let spec = RemoteSpec::resolve(config, &record.remote)?;
    let node = Node::connect(&spec).with_context(|| {
        format!(
            "Cannot reach '{}' to open a shell in '{}'",
            spec.name, record.handle
        )
    })?;
    let ex_id = record
        .execution
        .as_ref()
        .ok_or_else(|| anyhow!("run `{BIN} open {}` first", record.handle))?
        .ex_id
        .clone();
    let slot = super::node::shell::new_slot()?;
    let signals = ShellSignals::new()?;
    let mut cleanup = ShellCleanup {
        spec: spec.clone(),
        pin: node.pin.name.clone(),
        ws_id: record.ws_id.clone(),
        ex_id: ex_id.clone(),
        slot: slot.clone(),
        armed: true,
    };
    let request = ShellRequest::Open {
        ws_id: record.ws_id.clone(),
        ex_id,
        slot,
        host,
    };
    let opened: Result<ShellResponse> = node
        .call(protocol::SHELL, &request, false)
        .map_err(|err| refusal(err, record));
    if signals.canceled.load(Ordering::SeqCst) {
        return Ok(());
    }
    let opened = opened?;
    let ShellResponse::Opened {
        ex_id,
        slot,
        container,
        working_dir,
        server,
        session_id,
    } = opened
    else {
        bail!("'{}' answered an open request with {opened:?}", spec.name);
    };
    tracing::info!(remote = %spec.name, ws_id = %record.ws_id, %ex_id, %slot, container, %working_dir, "opened remote shell");
    anyhow::ensure!(
        ex_id == cleanup.ex_id && slot == cleanup.slot,
        "the node returned a different shell identity"
    );
    let client = super::rex_client(config, &record.remote);
    crate::multiplexer::rex::RexBackend::for_server(&server)
        .select_on_app(client.as_deref(), &session_id)?;
    cleanup.armed = false;
    Ok(())
}

/// Capture termination while the open RPC may still be creating its session.
struct ShellSignals {
    canceled: Arc<AtomicBool>,
    handlers: Vec<signal_hook::SigId>,
}

impl ShellSignals {
    fn new() -> Result<Self> {
        let mut guard = Self {
            canceled: Arc::new(AtomicBool::new(false)),
            handlers: Vec::new(),
        };
        for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGINT] {
            guard
                .handlers
                .push(signal_hook::flag::register(signal, guard.canceled.clone())?);
        }
        Ok(guard)
    }
}

impl Drop for ShellSignals {
    fn drop(&mut self) {
        for handler in self.handlers.drain(..) {
            signal_hook::low_level::unregister(handler);
        }
    }
}

/// Own the shell before its open RPC, including an error or a lost response.
struct ShellCleanup {
    spec: RemoteSpec,
    pin: String,
    ws_id: String,
    ex_id: String,
    slot: String,
    armed: bool,
}

impl Drop for ShellCleanup {
    fn drop(&mut self) {
        if self.armed {
            close(
                &self.spec,
                self.pin.clone(),
                self.ws_id.clone(),
                self.ex_id.clone(),
                self.slot.clone(),
            );
        }
    }
}

/// Cancel an unclaimed shell, best effort; execution cleanup remains its owner.
fn close(spec: &RemoteSpec, pin: String, ws_id: String, ex_id: String, slot: String) {
    // A terminal that went away hangs up its foreground process group, ssh
    // included; ssh keeps an inherited ignored SIGHUP.
    // SAFETY: installs SIG_IGN using plain signal(2) for the cleanup RPC.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }
    let result = Transport::open(spec).and_then(|transport| {
        let node = Node {
            spec: spec.clone(),
            transport,
            pin: Pin::installed(pin),
        };
        let request = ShellRequest::Close { ws_id, ex_id, slot };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            let result = node.call::<_, ShellResponse>(protocol::SHELL, &request, false);
            if result
                .as_ref()
                .err()
                .is_some_and(|error| error_code(error) == Some(code::BUSY))
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
                continue;
            }
            break result;
        }
    });
    if let Err(err) = result {
        tracing::warn!(remote = %spec.name, "failed to close the remote shell: {err:#}");
    }
}

/// An open request's error, with the command that fixes a refusal.
fn refusal(err: anyhow::Error, record: &WorkspaceRecord) -> anyhow::Error {
    match error_code(&err) {
        Some(code::NOT_FOUND) | Some(code::NO_WORKTREE) => {
            anyhow!("{err}; run `{BIN} open {}` first", record.handle)
        }
        _ => err.context(format!(
            "Failed to open a shell in '{}' on '{}'",
            record.handle, record.remote
        )),
    }
}
