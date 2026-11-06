//! Daemon lifecycle management: spawn, kill, signal, health checks.

use anyhow::{Result, anyhow};
use std::path::PathBuf;
use std::time::Duration;

use crate::multiplexer::{Multiplexer, create_backend, detect_backend};

use super::daemon;

/// Ensure the daemon is running, spawning it if needed. Returns the socket path.
pub(super) fn ensure_daemon_running() -> Result<PathBuf> {
    let mux = create_backend(detect_backend());
    let instance_id = mux.instance_id();
    let sock_path = daemon::socket_path(&instance_id);

    if std::os::unix::net::UnixStream::connect(&sock_path).is_ok() {
        return Ok(sock_path);
    }

    // Stale socket from a crashed daemon
    let _ = std::fs::remove_file(&sock_path);
    spawn_daemon()?;
    if !wait_for_socket(&instance_id, Duration::from_secs(2)) {
        return Err(anyhow!("Sidebar daemon failed to start"));
    }
    Ok(sock_path)
}

/// Spawn the sidebar daemon as a detached background process.
fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe)
        .arg("_sidebar-daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

/// Wait for the daemon's Unix socket to appear.
fn wait_for_socket(instance_id: &str, timeout: Duration) -> bool {
    let path = daemon::socket_path(instance_id);
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Wake the Rex sidebar daemon for an immediate observation.
pub(super) fn signal_daemon_for(mux: &dyn Multiplexer) {
    if let Some(host) = mux.sidebar() {
        host.refresh();
    }
}
