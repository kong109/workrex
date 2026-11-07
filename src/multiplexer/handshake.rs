//! Pane handshake mechanisms for shell startup synchronization.
//!
//! Different backends use different mechanisms to ensure a shell is ready
//! before sending commands to a pane.

use anyhow::{Context, Result, anyhow};
#[cfg(unix)]
use nix::sys::stat::Mode;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Trait for pane handshake mechanisms.
///
/// A handshake ensures the shell has started in a pane before sending commands.
/// Rex panes signal readiness through a private named pipe.
pub trait PaneHandshake: Send {
    /// Returns a full shell command string that wraps the shell to signal readiness.
    /// Formatted for direct shell evaluation (e.g., `sh -c "..."`).
    /// The shell receives this wrapper as one command.
    fn wrapper_command(&self, shell: &str) -> String;

    /// Returns the raw POSIX script body that signals readiness and exec's the shell.
    /// Does NOT include `sh -c` wrapping -- the backend decides how to invoke it.
    /// Used by the shared `setup_panes` implementation, where each backend wraps
    /// the script appropriately for its CLI.
    fn script_content(&self, shell: &str) -> String {
        // Default: delegate to wrapper_command (backwards compat)
        self.wrapper_command(shell)
    }

    /// Waits for the handshake signal, consuming the handshake object.
    fn wait(self: Box<Self>) -> Result<()>;
}

/// Timeout for waiting for pane readiness (seconds)
const HANDSHAKE_TIMEOUT_SECS: u64 = 5;

/// Unix named pipe used to synchronize Rex shell startup.
#[cfg(unix)]
pub struct UnixPipeHandshake {
    pipe_path: PathBuf,
}

#[cfg(unix)]
impl UnixPipeHandshake {
    /// Create a new pipe handshake.
    ///
    /// Creates a named pipe (FIFO) that the shell will write to when ready.
    pub fn new() -> Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();

        let pipe_path = std::env::temp_dir().join(format!("workrex_pipe_{}_{}", pid, nanos));

        // Create FIFO with 0o600 permissions (owner read/write only)
        let mode = Mode::S_IRUSR | Mode::S_IWUSR;
        nix::unistd::mkfifo(&pipe_path, mode).context("Failed to create named pipe")?;

        Ok(Self { pipe_path })
    }
}

#[cfg(unix)]
impl PaneHandshake for UnixPipeHandshake {
    fn wrapper_command(&self, shell: &str) -> String {
        let escaped_shell = super::util::escape_for_sh_c_inner_single_quote(shell);
        format!(
            "sh -c 'echo ready > {}; exec '\\''{}'\\'' -l'",
            self.pipe_path.display(),
            escaped_shell
        )
    }

    fn script_content(&self, shell: &str) -> String {
        format!(
            "echo ready > {}; exec '{}' -l",
            self.pipe_path.display(),
            shell
        )
    }

    fn wait(self: Box<Self>) -> Result<()> {
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::AsRawFd;

        const POLL_INTERVAL_MS: u64 = 50;

        // Open pipe for reading (non-blocking)
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&self.pipe_path)
            .context("Failed to open pipe for reading")?;

        let fd = file.as_raw_fd();
        let start = Instant::now();
        let timeout = Duration::from_secs(HANDSHAKE_TIMEOUT_SECS);

        loop {
            // Check if data available via poll()
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };

            let poll_timeout_ms = POLL_INTERVAL_MS as i32;
            let ret = unsafe { libc::poll(&mut pollfd, 1, poll_timeout_ms) };

            if ret > 0 && (pollfd.revents & libc::POLLIN) != 0 {
                // Data available - read and verify we got data
                let mut buf = [0u8; 64];
                let bytes_read =
                    unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                let _ = std::fs::remove_file(&self.pipe_path);

                if bytes_read > 0 {
                    return Ok(());
                } else {
                    // EOF (0) or error (-1) - writer closed without sending data
                    return Err(anyhow!("Pipe closed without receiving handshake signal"));
                }
            }

            if start.elapsed() >= timeout {
                let _ = std::fs::remove_file(&self.pipe_path);
                return Err(anyhow!(
                    "Pane handshake timed out after {}s - shell may have failed to start",
                    HANDSHAKE_TIMEOUT_SECS
                ));
            }

            // Continue polling
        }
    }
}

#[cfg(unix)]
impl Drop for UnixPipeHandshake {
    fn drop(&mut self) {
        // Clean up the pipe file if it still exists
        let _ = std::fs::remove_file(&self.pipe_path);
    }
}
