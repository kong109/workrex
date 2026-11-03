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
        // ssh-keyscan prints the `# host:port SSH-...` comment on stdout in
        // newer OpenSSH and on stderr in older releases.
        [&output.stdout, &output.stderr]
            .into_iter()
            .find_map(|stream| banner_in(&String::from_utf8_lossy(stream)))
    }
}

fn banner_in(keyscan: &str) -> Option<String> {
    keyscan.lines().find_map(|line| {
        line.strip_prefix('#')?
            .split_whitespace()
            .find(|word| word.starts_with("SSH-"))
            .map(str::to_string)
    })
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
/// `systemd` is whether `/run/systemd/system` exists, the test sd_booted(3) uses.
pub(super) fn linger_row(row: Row, kernel: &str, linger: &str, systemd: bool, user: &str) -> Row {
    if !is_wsl(kernel) {
        return row;
    }
    if !systemd {
        return Row {
            detail: "systemd is not running, so nothing keeps agents alive".into(),
            ..row
        }
        .fix(enable_systemd(user));
    }
    match linger {
        "no" => Row {
            detail: format!(
                "{}; WSL also stops them when the distro shuts down",
                row.detail
            ),
            ..row
        },
        // logind fails `show-user` for a user with neither linger nor a session.
        "" => Row {
            level: Level::Fail,
            detail: format!(
                "disabled for {user}: logind has no linger or session for {user}, and WSL \
                 stops agents when the distro shuts down"
            ),
            ..row
        },
        _ => row,
    }
}

fn enable_systemd(user: &str) -> String {
    format!(
        "enable systemd in WSL: add `[boot]` `systemd=true` to /etc/wsl.conf, run \
         `wsl.exe --shutdown` on Windows, then `sudo loginctl enable-linger {user}`"
    )
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
    fn banner_comes_from_the_keyscan_comment() {
        let keyscan = "# winbox:22 SSH-2.0-OpenSSH_for_Windows_9.5\n\
                       winbox ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA\n";
        assert_eq!(
            banner_in(keyscan).as_deref(),
            Some("SSH-2.0-OpenSSH_for_Windows_9.5")
        );
        assert_eq!(banner_in("winbox ssh-rsa SSH-not-a-banner\n"), None);
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

    const ENABLE_SYSTEMD: &str = "enable systemd in WSL: add `[boot]` `systemd=true` to \
                                  /etc/wsl.conf, run `wsl.exe --shutdown` on Windows, then \
                                  `sudo loginctl enable-linger cicero`";

    #[test]
    fn wsl_without_systemd_names_systemd() {
        for (fact, row) in [
            ("", linger(Level::Warn, "unknown linger state \"\"")),
            (
                "unavailable",
                Row::new("linger", Level::Warn, "loginctl not found; cannot check"),
            ),
        ] {
            let row = linger_row(row, WSL, fact, false, "cicero");
            assert_eq!(row.level, Level::Warn, "{fact:?}");
            assert_eq!(
                row.detail,
                "systemd is not running, so nothing keeps agents alive"
            );
            assert_eq!(row.fix.as_deref(), Some(ENABLE_SYSTEMD), "{fact:?}");
        }
    }

    #[test]
    fn wsl_with_systemd_and_linger_is_ok() {
        let row = linger_row(
            Row::new("linger", Level::Ok, "enabled for cicero"),
            WSL,
            "yes",
            true,
            "cicero",
        );
        assert_eq!(row.level, Level::Ok);
        assert_eq!(row.detail, "enabled for cicero");
        assert_eq!(row.fix, None);
    }

    #[test]
    fn wsl_with_systemd_and_no_session_keeps_enable_linger() {
        let row = linger_row(
            linger(Level::Warn, "unknown linger state \"\""),
            WSL,
            "",
            true,
            "cicero",
        );
        assert_eq!(row.level, Level::Fail);
        assert_eq!(
            row.detail,
            "disabled for cicero: logind has no linger or session for cicero, and WSL \
             stops agents when the distro shuts down"
        );
        assert_eq!(
            row.fix.as_deref(),
            Some("sudo loginctl enable-linger cicero")
        );
    }

    #[test]
    fn wsl_with_systemd_and_a_session_keeps_enable_linger() {
        let row = linger_row(
            linger(
                Level::Fail,
                "disabled for cicero: agents stop when your last ssh session ends",
            ),
            WSL,
            "no",
            true,
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
        for fact in ["no", "unavailable", ""] {
            for systemd in [true, false] {
                let row = linger_row(
                    linger(Level::Warn, "as before"),
                    "6.8.0-45-generic",
                    fact,
                    systemd,
                    "cicero",
                );
                assert_eq!(row.level, Level::Warn);
                assert_eq!(row.detail, "as before");
                assert_eq!(
                    row.fix.as_deref(),
                    Some("sudo loginctl enable-linger cicero")
                );
            }
        }
    }
}
