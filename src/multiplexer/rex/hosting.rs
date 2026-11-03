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
use super::{
    CallError, ClientList, CreatedWindow, ProcessReport, RexBackend, SessionId, SessionView,
    WindowView, found, pane_info, shell_block,
};

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

/// Reads the sessions in `rex.args.sessions` in one `rex do`: for each, its
/// label and, for each terminal block in a tiled layer, the window, process
/// report and title. `rex do` drops error codes, so each session and block
/// carries the error Rex gave for it. Lua encodes an empty table as `{}`, so
/// the result keys every collection by id and drops empty `ancestors`.
const READ_SESSIONS: &str = r#"
local out = {}
for _, id in ipairs(rex.args.sessions) do
  local attached, err = rex.session.attach{session_id = id}
  if not attached then
    out[id] = {error = err}
  else
    local view = attached.view
    local blocks = {}
    for _, window in ipairs(view.windows or {}) do
      for _, layer in ipairs(window.layers or {}) do
        for _, block in ipairs(layer.kind == "tiled" and layer.blocks or {}) do
          if block.creator_name == "com.superlogical.terminal" then
            local call = {session_id = id, block_id = block.block_id, method = "process"}
            local entry = {window = {window_id = window.window_id, label = window.label}}
            entry.process, entry.error = rex.call("block.method", call)
            if entry.process and entry.process.last_exit == nil then
              if entry.process.ancestors and next(entry.process.ancestors) == nil then
                entry.process.ancestors = nil
              end
              call.method = "title"
              local title, failed = rex.call("block.method", call)
              entry.title, entry.error = title and title.title, failed
            end
            blocks[block.block_id] = entry
          end
        end
      end
    end
    out[id] = {session = {session_id = view.session_id, label = view.label}, blocks = blocks}
  end
end
return out
"#;

#[derive(Debug, Deserialize)]
struct SessionRead {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    session: Option<SessionView>,
    #[serde(default)]
    blocks: HashMap<String, BlockRead>,
}

#[derive(Debug, Deserialize)]
struct BlockRead {
    window: WindowView,
    #[serde(default)]
    process: Option<ProcessReport>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// What a read finds of a recorded session.
#[derive(Debug)]
pub enum HostedRead {
    /// Rex has no session with that id.
    Gone,
    /// Its live terminal blocks, by block id.
    Live(HashMap<String, LivePaneInfo>),
    Unreadable(anyhow::Error),
}

impl HostedRead {
    pub fn live(self) -> HashMap<String, LivePaneInfo> {
        match self {
            Self::Live(panes) => panes,
            Self::Gone | Self::Unreadable(_) => HashMap::new(),
        }
    }

    fn of(read: SessionRead) -> Self {
        let not_found = |error: &str| {
            matches!(
                CallError::from_stderr(&["do"], error),
                CallError::NotFound(_)
            )
        };
        if let Some(error) = read.error {
            return if not_found(&error) {
                Self::Gone
            } else {
                Self::Unreadable(anyhow!("{error}"))
            };
        }
        let Some(session) = read.session else {
            return Self::Unreadable(anyhow!("Rex returned no view"));
        };
        let mut live = HashMap::new();
        for (block, read) in read.blocks {
            match (read.process, read.error) {
                (_, Some(error)) if !not_found(&error) => {
                    return Self::Unreadable(anyhow!("{block}: {error}"));
                }
                (Some(process), _) => {
                    let title = read.title.unwrap_or_default();
                    if let Some(pane) = pane_info(&session, &read.window, &process, title) {
                        live.insert(block, pane);
                    }
                }
                (None, _) => {}
            }
        }
        Self::Live(live)
    }
}

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

/// The connected app client a jump switches: the one labeled `wanted`, else
/// the only one.
fn pick_app(clients: &ClientList, wanted: Option<&str>, server: &str) -> Result<String> {
    let apps: Vec<(&str, Option<&str>)> = clients
        .connected_apps()
        .map(|client| {
            let label = client.info.as_ref().and_then(|info| info.label.as_deref());
            (client.client_id.as_str(), label)
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

    /// Sessions `ids`, read with one `rex` process however many there are.
    /// A session that is gone or unreadable fails only its own entry.
    pub fn read_hosted(&self, ids: &[&str]) -> Result<HashMap<String, HostedRead>> {
        let mut reads = HashMap::new();
        if ids.is_empty() {
            return Ok(reads);
        }
        let output = self.run(
            &["do", "-e", READ_SESSIONS, "--args", "-", "--json"],
            Some(&json!({ "sessions": ids }).to_string()),
        )?;
        let mut found: HashMap<String, SessionRead> = Self::parse("session read", &output)?;
        for id in ids {
            let read = match found.remove(*id) {
                Some(read) => HostedRead::of(read),
                None => HostedRead::Unreadable(anyhow!("Rex did not read session {id}")),
            };
            reads.insert(id.to_string(), read);
        }
        Ok(reads)
    }

    /// Whether the server has session `id`.
    pub fn has_session(&self, id: &str) -> Result<bool> {
        Ok(found(self.view(&SessionId(id.to_string())))?.is_some())
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
        let clients: ClientList = self.api("client.list", json!({}))?;
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

    fn clients(entries: &[(&str, &str, Option<&str>, &str)]) -> ClientList {
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
    fn a_sourced_block_keeps_only_its_rex_variables_and_the_env_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x1-0.env");
        let path = std::env::var("PATH").unwrap();
        std::fs::write(
            &file,
            format!(
                "export PATH={}\nexport WR8_FROM_FILE='from file'\n",
                crate::shell::shell_quote(&path)
            ),
        )
        .unwrap();
        let output = std::process::Command::new("sh")
            .args(["-c", SOURCE_ENV, "sh", &file.to_string_lossy(), "env"])
            .env_clear()
            .env("PATH", &path)
            .env("WR8_FROM_SERVER", "server only")
            .env("REX_BLOCK", "block:b")
            .env("TERM", "xterm-256color")
            .output()
            .unwrap();
        let env = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{env}");
        for kept in [
            "WR8_FROM_FILE=from file",
            "REX_BLOCK=block:b",
            "TERM=xterm-256color",
        ] {
            assert!(env.lines().any(|line| line == kept), "{kept}: {env}");
        }
        assert!(!env.contains("WR8_FROM_SERVER"), "{env}");
        assert!(!file.exists());
    }

    fn app_labeled(id: &str, label: &str) -> Value {
        json!({
            "client_id": id,
            "connection_state": "connected",
            "info": { "kind": "app", "label": label },
        })
    }

    #[test]
    fn a_jump_selects_the_session_on_the_app_rex_client_names() {
        let apps = [
            app_labeled("client:a", "rexbook"),
            app_labeled("client:b", "studio"),
        ];
        let fake = super::super::tests::app_fake("client:a", &apps, "{}");
        fake.rex
            .select_on_app(Some("studio"), "session:hosted")
            .unwrap();
        let selects = fake.calls("session.select");
        let [select] = &selects[..] else {
            panic!("{selects:?}")
        };
        assert!(select.contains("client:b"), "{select}");
        assert!(select.contains("session:hosted"), "{select}");
        assert!(!select.contains("client:a"), "{select}");
    }

    #[test]
    fn a_refused_jump_names_the_remote_control_fix() {
        let fake = super::super::tests::app_fake(
            "client:a",
            &[app_labeled("client:a", "rexbook")],
            "!Error: Remote Control is off for this client (code=refused)",
        );
        let err = fake.rex.select_on_app(None, "session:hosted").unwrap_err();
        assert_eq!(err.to_string(), super::super::REMOTE_CONTROL_FIX);
        assert_eq!(fake.calls("session.select").len(), 1);
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
