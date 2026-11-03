//! Agent terminals a remote node hosts on its own Rex server, one session
//! per pane, and the jump that shows one in Rex.app. The node tracks each
//! session by the id it was created with: Rex's Claude Code hook renames a
//! session at the agent's first request.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{LivePaneInfo, Multiplexer};
use super::{CreatedWindow, RexBackend, SessionId, found, parallel_map, shell_block};

/// Reads the env file in `$1`, removes it, replaces the environment the
/// block inherited from the Rex server with it and runs the script in `$2`.
/// Only the variables Rex sets for the block itself stay. The block's command
/// line names the file, never a value in it. The file is a named pipe, which
/// macOS `sh` (bash 3.2) cannot source: `.` reads a file's stat size, zero
/// for a pipe.
const SOURCE_ENV: &str = "env=$(cat \"$1\") && rm -f \"$1\" && \
    for v in $(env | sed -n 's/^\\([A-Za-z_][A-Za-z0-9_]*\\)=.*/\\1/p'); do \
    case $v in REX_*|TERM|TERM_PROGRAM|TERM_PROGRAM_VERSION) ;; *) unset \"$v\" ;; esac; done; \
    eval \"$env\" && exec sh -c \"$2\"";

/// A jump to a session on a server reached over the tailnet; an unknown
/// session id takes the app about five seconds to refuse.
const REMOTE_SELECT_TIMEOUT: Duration = Duration::from_secs(10);

/// `server.status`, as far as hosting needs it.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerStatus {
    pub instance_id: String,
    #[serde(default)]
    tailscale: Option<Tailnet>,
}

#[derive(Debug, Clone, Deserialize)]
struct Tailnet {
    #[serde(default)]
    state: String,
    #[serde(default)]
    dns_name: Option<String>,
}

impl ServerStatus {
    /// The name Rex.app reaches the server by, while it serves the tailnet.
    pub fn tailnet_name(&self) -> Option<&str> {
        let tailnet = self.tailscale.as_ref()?;
        (tailnet.state == "running")
            .then_some(tailnet.dns_name.as_deref())
            .flatten()
            .filter(|name| !name.is_empty())
    }
}

/// A session on the server, by id and current label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedSession {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct CreatedSession {
    session_id: SessionId,
    initial_windows: Vec<CreatedWindow>,
}

#[derive(Debug, Deserialize)]
struct AppClients {
    #[serde(default)]
    clients: Vec<AppClient>,
}

#[derive(Debug, Deserialize)]
struct AppClient {
    client_id: String,
    #[serde(default)]
    connection_state: String,
    #[serde(default)]
    info: Option<AppInfo>,
}

#[derive(Debug, Deserialize)]
struct AppInfo {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    label: Option<String>,
}

/// The connected app client a jump switches: the one labeled `wanted`, else
/// the only one.
fn pick_app(clients: &AppClients, wanted: Option<&str>, server: &str) -> Result<String> {
    let apps: Vec<(&str, Option<&str>)> = clients
        .clients
        .iter()
        .filter(|client| client.connection_state != "lost")
        .filter_map(|client| {
            let info = client.info.as_ref().filter(|info| info.kind == "app")?;
            Some((client.client_id.as_str(), info.label.as_deref()))
        })
        .collect();
    if let Some(wanted) = wanted {
        return apps
            .iter()
            .find(|(_, label)| *label == Some(wanted))
            .map(|(id, _)| id.to_string())
            .ok_or_else(|| anyhow!("No Rex.app labeled '{wanted}' is connected to {server}"));
    }
    match apps.as_slice() {
        [] => bail!("Rex.app is not connected to {server}"),
        [(id, _)] => Ok(id.to_string()),
        several => {
            let labels: Vec<&str> = several
                .iter()
                .map(|(id, label)| label.unwrap_or(id))
                .collect();
            bail!(
                "{} Rex.app clients are connected to {server} ({}); set rex_client on the \
                 remote to the one to switch",
                several.len(),
                labels.join(", ")
            )
        }
    }
}

impl RexBackend {
    pub fn server_status(&self) -> Result<ServerStatus> {
        Ok(self.api("server.status", json!({}))?)
    }

    /// Every session on the server, by id and label as Rex reports them.
    pub fn hosted_sessions(&self) -> Result<Vec<HostedSession>> {
        Ok(self
            .raw_sessions()?
            .into_iter()
            .map(|session| HostedSession {
                id: session.session_id.0,
                label: session.label,
            })
            .collect())
    }

    /// Create session `label` with one window `window_label` running the
    /// login shell in `cwd` with the environment in `env_file`; returns the
    /// session and its block.
    pub fn create_hosted_session(
        &self,
        label: &str,
        window_label: &str,
        cwd: &Path,
        env_file: &Path,
    ) -> Result<(String, String)> {
        let shell = format!(
            "exec {} -l",
            crate::shell::shell_quote(&self.get_default_shell()?)
        );
        let created: CreatedSession = self
            .api(
                "session.create",
                json!({
                    "label": label,
                    "initial_windows": [{
                        "window_label": window_label,
                        "layout": sourcing(cwd, env_file, &shell),
                    }],
                }),
            )
            .with_context(|| format!("Failed to create Rex session {label}"))?;
        let window = created
            .initial_windows
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Rex created session {label} without a window"))?;
        let block = self.cache_first_block(window.block_ids, created.session_id.clone())?;
        Ok((created.session_id.0, block.0))
    }

    /// Destroy a session; one already gone is fine.
    pub fn destroy_hosted_session(&self, id: &str) -> Result<()> {
        self.destroy_session(&SessionId(id.to_string()))
    }

    /// Live terminal blocks of session `id`; none when it is gone.
    pub fn hosted_live_panes(&self, id: &str) -> Result<HashMap<String, LivePaneInfo>> {
        let Some(view) = found(self.view(&SessionId(id.to_string())))? else {
            return Ok(HashMap::new());
        };
        let blocks: Vec<_> = view.terminal_blocks().collect();
        let panes = parallel_map(&blocks, |(window, block)| {
            self.live_pane(&view, window, block)
                .map(|pane| pane.map(|pane| (block.as_str().to_string(), pane)))
        });
        Ok(panes
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Have the next `respawn_pane` source `env_file` before its command, so
    /// the variables in it reach that one program and nothing else.
    pub fn source_on_respawn(&self, env_file: PathBuf) {
        *self.respawn_env.lock().unwrap() = Some(env_file);
    }

    pub(super) fn respawn_layout(&self, cwd: &Path, script: &str) -> Result<Value> {
        match self.respawn_env.lock().unwrap().take() {
            Some(file) => Ok(sourcing(cwd, &file, script)),
            None => self.block_layout(cwd, Some(script)),
        }
    }

    /// A backend that runs `program` as `rex` against `server`.
    #[cfg(test)]
    pub fn with_program(program: &Path, server: &str) -> Self {
        Self {
            program: program.display().to_string(),
            caller: super::Caller::default(),
            registry: None,
            ..Self::for_server(server)
        }
    }

    /// Show session `id` of this server on Rex.app: the client labeled
    /// `client`, else the only app connected.
    pub fn select_on_app(&self, client: Option<&str>, id: &str) -> Result<()> {
        let server = self.server.as_deref().unwrap_or("the local Rex server");
        let clients: AppClients = self.api("client.list", json!({}))?;
        let app = pick_app(&clients, client, server)?;
        let session = format!("session_id={id}");
        self.exec_within(
            &["-C", &app, "do", "session.select", &session],
            None,
            Some(REMOTE_SELECT_TIMEOUT),
        )
        .map_err(|err| {
            if err.to_string().contains("(code=refused)") {
                anyhow!(super::REMOTE_CONTROL_FIX)
            } else {
                anyhow!("Rex.app did not show the session: {err}")
            }
        })?;
        Ok(())
    }
}

/// A block that runs `script` with the environment in `env_file`.
fn sourcing(cwd: &Path, env_file: &Path, script: &str) -> Value {
    shell_block(
        cwd,
        &[
            "sh",
            "-c",
            SOURCE_ENV,
            "sh",
            &env_file.to_string_lossy(),
            script,
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = include_str!("../testdata/rex_server_status_tailnet.json");

    fn clients(entries: &[(&str, &str, Option<&str>, &str)]) -> AppClients {
        let clients: Vec<Value> = entries
            .iter()
            .map(|(id, kind, label, state)| {
                json!({
                    "client_id": id,
                    "connection_state": state,
                    "info": { "kind": kind, "label": label },
                })
            })
            .collect();
        serde_json::from_value(json!({ "clients": clients })).unwrap()
    }

    #[test]
    fn the_tailnet_name_is_known_only_while_tailscale_runs() {
        let status: ServerStatus = serde_json::from_str(STATUS).unwrap();
        assert_eq!(status.tailnet_name(), Some("rex.tail4e96ed.ts.net"));
        assert_eq!(status.instance_id, "1272fa9c-28a6-4f19-b069-6a35abbfa704");
        let stopped: ServerStatus = serde_json::from_value(json!({
            "instance_id": "i",
            "tailscale": { "state": "disabled", "dns_name": "rex.example.ts.net" },
        }))
        .unwrap();
        assert_eq!(stopped.tailnet_name(), None);
        let local: ServerStatus = serde_json::from_value(json!({ "instance_id": "i" })).unwrap();
        assert_eq!(local.tailnet_name(), None);
    }

    #[test]
    fn a_jump_picks_the_only_connected_app_or_the_named_one() {
        let one = clients(&[
            ("client:app", "app", Some("rexbook"), "connected"),
            ("client:cli", "cli", None, "connected"),
            ("client:old", "app", Some("old"), "lost"),
        ]);
        assert_eq!(pick_app(&one, None, "h").unwrap(), "client:app");
        assert_eq!(pick_app(&one, Some("rexbook"), "h").unwrap(), "client:app");
        assert!(pick_app(&one, Some("old"), "h").is_err());

        let none = clients(&[("client:cli", "cli", None, "connected")]);
        let err = pick_app(&none, None, "rex.example.ts.net").unwrap_err();
        assert_eq!(
            err.to_string(),
            "Rex.app is not connected to rex.example.ts.net"
        );

        let two = clients(&[
            ("client:a", "app", Some("rexbook"), "connected"),
            ("client:b", "app", Some("studio"), "connected"),
        ]);
        let err = pick_app(&two, None, "h").unwrap_err().to_string();
        assert!(err.contains("rexbook, studio"), "{err}");
        assert_eq!(pick_app(&two, Some("studio"), "h").unwrap(), "client:b");
    }

    #[test]
    fn only_the_next_respawn_sources_the_env_file() {
        let rex = RexBackend::for_server("unix:///nonexistent/rex.sock");
        rex.source_on_respawn(PathBuf::from("/node/secrets/x1-0.env"));
        let first = rex.respawn_layout(Path::new("/w"), "agent").unwrap();
        assert_eq!(
            first["block"]["options"]["command"],
            json!([
                "sh",
                "-c",
                SOURCE_ENV,
                "sh",
                "/node/secrets/x1-0.env",
                "agent"
            ])
        );
        let second = rex.respawn_layout(Path::new("/w"), "agent").unwrap();
        assert_eq!(
            second["block"]["options"]["command"],
            json!(["sh", "-c", "agent"])
        );
    }
}
