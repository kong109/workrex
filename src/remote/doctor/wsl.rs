//! Fixes `remote doctor` names when the remote runs in WSL: Windows OpenSSH
//! answers on port 22, and the distro stops agents unless systemd keeps them.

use std::fmt;
use std::process::{Command, Stdio};
use std::time::Duration;

use super::super::transport::{SshFailure, SshFailureKind, run_bounded};
use super::{Level, Row};

const KEYSCAN_TIMEOUT: Duration = Duration::from_secs(8);

/// Where `ssh -G` sends a configured host.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Destination {
    user: String,
    hostname: String,
    port: String,
}

impl Destination {
    pub(super) fn resolve(host: &str) -> Option<Self> {
        let mut cmd = Command::new("ssh");
        cmd.args(["-G", host]);
        let output = run_bounded(cmd, Stdio::null(), Some(KEYSCAN_TIMEOUT)).ok()??;
        Self::parse(&output.stdout_text())
    }

    fn parse(config: &str) -> Option<Self> {
        let value = |key: &str| {
            config.lines().find_map(|line| {
                let (name, value) = line.split_once(' ')?;
                (name == key).then(|| value.trim().to_string())
            })
        };
        Some(Self {
            user: value("user")?,
            hostname: value("hostname")?,
            port: value("port")?,
        })
    }

    /// The server's version banner, such as `SSH-2.0-OpenSSH_for_Windows_9.5`.
    fn banner(&self) -> Option<String> {
        let mut cmd = Command::new("ssh-keyscan");
        cmd.args(["-T", "5", "-p", &self.port, &self.hostname]);
        let output = run_bounded(cmd, Stdio::null(), Some(KEYSCAN_TIMEOUT)).ok()??;
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .find_map(|line| {
                line.split_whitespace()
                    .find(|word| word.starts_with("SSH-"))
            })
            .map(str::to_string)
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{} port {}", self.user, self.hostname, self.port)
    }
}

/// The failed `ssh` row for `remotes.<name>.host = host`.
pub(super) fn ssh_row(name: &str, host: &str, err: &anyhow::Error) -> Row {
    let destination = Destination::resolve(host);
    let auth = err
        .downcast_ref::<SshFailure>()
        .is_some_and(|failure| failure.kind == SshFailureKind::Auth);
    let banner = destination
        .as_ref()
        .filter(|_| auth)
        .and_then(Destination::banner);
    failed_ssh_row(
        name,
        host,
        &format!("{err:#}"),
        auth,
        destination.as_ref(),
        banner.as_deref(),
    )
}

fn failed_ssh_row(
    name: &str,
    host: &str,
    message: &str,
    auth: bool,
    destination: Option<&Destination>,
    banner: Option<&str>,
) -> Row {
    let detail = match (destination, message.strip_prefix(&format!("ssh {host}:"))) {
        (Some(destination), Some(reason)) => format!("ssh {host} ({destination}):{reason}"),
        _ => message.to_string(),
    };
    let row = Row::new("ssh", Level::Fail, detail);
    let Some(destination) = destination else {
        return row;
    };
    let Destination { hostname, port, .. } = destination;
    if auth && banner.is_some_and(|banner| banner.contains("OpenSSH_for_Windows")) {
        row.fix(format!(
            "port {port} on {hostname} is Windows OpenSSH. If the remote is WSL, point \
             remotes.{name}.host at an ~/.ssh/config alias with WSL's port (Port 2222)"
        ))
    } else if ["connection refused", "timed out", "no response within"]
        .iter()
        .any(|unreachable| message.contains(unreachable))
    {
        row.fix(format!(
            "nothing answers on {hostname}:{port}. For WSL, start the distro on Windows \
             (`wsl.exe`) and check that `systemctl status ssh.socket` lists port {port}"
        ))
    } else {
        row
    }
}

/// Whether `uname -r` names a WSL kernel, such as `6.6.87.2-microsoft-standard-WSL2`.
fn is_wsl(kernel: &str) -> bool {
    kernel.to_ascii_lowercase().contains("microsoft")
}

/// `row` for the linger fact, with the WSL fix when the kernel is WSL's.
pub(super) fn linger_row(row: Row, kernel: &str, linger: &str, user: &str) -> Row {
    if !is_wsl(kernel) {
        return row;
    }
    match linger {
        "no" => Row {
            detail: format!(
                "{}; WSL also stops them when the distro shuts down",
                row.detail
            ),
            ..row
        },
        "unavailable" => row.fix(format!(
            "enable systemd in WSL: add `[boot]` `systemd=true` to /etc/wsl.conf, run \
             `wsl.exe --shutdown` on Windows, then `sudo loginctl enable-linger {user}`"
        )),
        _ => row,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn winbox(port: &str) -> Destination {
        Destination {
            user: "cicero".into(),
            hostname: "winbox".into(),
            port: port.into(),
        }
    }

    fn linger(level: Level, detail: &str) -> Row {
        Row::new("linger", level, detail).fix("sudo loginctl enable-linger cicero")
    }

    const WSL: &str = "6.18.54.1-microsoft-standard-WSL2";

    #[test]
    fn ssh_failure_row_shows_resolved_destination() {
        let config = "user cicero\nhostname winbox\nport 2222\nidentityfile ~/.ssh/id\n";
        let destination = Destination::parse(config).unwrap();
        assert_eq!(destination, winbox("2222"));
        let row = failed_ssh_row(
            "winbox",
            "winbox-wsl-ssh",
            "ssh winbox-wsl-ssh: host key is not trusted",
            false,
            Some(&destination),
            None,
        );
        assert_eq!(
            row.detail,
            "ssh winbox-wsl-ssh (cicero@winbox port 2222): host key is not trusted"
        );
        assert_eq!(row.fix, None);
    }

    #[test]
    fn wsl_auth_on_windows_sshd_names_the_port() {
        let row = failed_ssh_row(
            "winbox",
            "cicero@winbox",
            "ssh cicero@winbox: authentication failed",
            true,
            Some(&winbox("22")),
            Some("SSH-2.0-OpenSSH_for_Windows_9.5"),
        );
        assert_eq!(
            row.fix.as_deref(),
            Some(
                "port 22 on winbox is Windows OpenSSH. If the remote is WSL, point \
                 remotes.winbox.host at an ~/.ssh/config alias with WSL's port (Port 2222)"
            )
        );
    }

    #[test]
    fn auth_failure_on_linux_sshd_keeps_the_key_advice_alone() {
        let row = failed_ssh_row(
            "winbox",
            "winbox-wsl-ssh",
            "ssh winbox-wsl-ssh: authentication failed",
            true,
            Some(&winbox("2222")),
            Some("SSH-2.0-OpenSSH_9.6p1 Ubuntu-3ubuntu13.20"),
        );
        assert_eq!(row.fix, None);
    }

    #[test]
    fn wsl_refused_names_wsl_start_and_ssh_socket() {
        for reason in [
            "connection refused. Is sshd running",
            "connection timed out",
        ] {
            let row = failed_ssh_row(
                "winbox",
                "winbox-wsl-ssh",
                &format!("ssh winbox-wsl-ssh: {reason}"),
                false,
                Some(&winbox("2222")),
                None,
            );
            assert_eq!(
                row.fix.as_deref(),
                Some(
                    "nothing answers on winbox:2222. For WSL, start the distro on Windows \
                     (`wsl.exe`) and check that `systemctl status ssh.socket` lists port 2222"
                ),
                "{reason}"
            );
        }
    }

    #[test]
    fn an_unresolved_host_keeps_the_ssh_message() {
        let row = failed_ssh_row(
            "winbox",
            "nowhere",
            "ssh nowhere: cannot resolve the host name",
            false,
            None,
            None,
        );
        assert_eq!(row.detail, "ssh nowhere: cannot resolve the host name");
        assert_eq!(row.fix, None);
    }

    #[test]
    fn linger_unavailable_on_wsl_names_systemd() {
        let row = linger_row(
            Row::new("linger", Level::Warn, "loginctl not found; cannot check"),
            WSL,
            "unavailable",
            "cicero",
        );
        assert_eq!(row.level, Level::Warn);
        assert_eq!(
            row.fix.as_deref(),
            Some(
                "enable systemd in WSL: add `[boot]` `systemd=true` to /etc/wsl.conf, run \
                 `wsl.exe --shutdown` on Windows, then `sudo loginctl enable-linger cicero`"
            )
        );
    }

    #[test]
    fn linger_no_on_wsl_keeps_enable_linger() {
        let row = linger_row(
            linger(
                Level::Fail,
                "disabled for cicero: agents stop when your last ssh session ends",
            ),
            WSL,
            "no",
            "cicero",
        );
        assert_eq!(row.level, Level::Fail);
        assert_eq!(
            row.detail,
            "disabled for cicero: agents stop when your last ssh session ends; WSL also \
             stops them when the distro shuts down"
        );
        assert_eq!(
            row.fix.as_deref(),
            Some("sudo loginctl enable-linger cicero")
        );
    }

    #[test]
    fn non_wsl_rows_unchanged() {
        for (kernel, fact) in [
            ("6.8.0-45-generic", "no"),
            ("6.8.0-45-generic", "unavailable"),
            (WSL, "yes"),
        ] {
            let row = linger_row(linger(Level::Warn, "as before"), kernel, fact, "cicero");
            assert_eq!(row.detail, "as before");
            assert_eq!(
                row.fix.as_deref(),
                Some("sudo loginctl enable-linger cicero")
            );
        }
    }
}
