//! Where an execution's terminals live: a session per pane on the node tmux
//! server, or on the node's own Rex server, which Rex.app shows directly.
//! The host is fixed when the execution starts and recorded on it
//! (`Execution::rex`), so verbs never read it from config.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

use tempfile::TempPath;

use super::execution::{kill_execution, new_session, session_alive, set_session_env, tmux_socket};
use super::{Execution, NodeRoot, refuse, secrets};
use crate::config::RemoteTerminal;
use crate::multiplexer::rex::{HostedSession, RexBackend};
use crate::multiplexer::{LivePaneInfo, Multiplexer, tmux::TmuxBackend};
use crate::remote::protocol::{RexPlacement, Secrets, code, session_name};

pub enum SessionHost {
    Tmux { socket: PathBuf, mux: TmuxBackend },
    Rex(Box<RexHost>),
}

pub struct RexHost {
    pub mux: RexBackend,
    /// Prefix of this root's session labels: every node root of the user
    /// shares the one Rex server.
    tag: String,
    pub host_label: String,
    pub instance_id: String,
}

impl SessionHost {
    pub fn tmux(root: &NodeRoot) -> Result<Self> {
        let socket = tmux_socket(root)?;
        Ok(Self::Tmux {
            mux: TmuxBackend::for_socket(&socket.to_string_lossy()),
            socket,
        })
    }

    /// The host `execution` was started on.
    pub fn for_execution(root: &NodeRoot, execution: &Execution) -> Result<Self> {
        match execution.rex {
            Some(_) => Ok(Self::Rex(Box::new(RexHost::open(root, false)?))),
            None => Self::tmux(root),
        }
    }

    /// The host a new execution starts on. Rex needs the server on the
    /// tailnet, where Rex.app reaches it.
    pub fn open_for_start(root: &NodeRoot, terminal: RemoteTerminal) -> Result<Self> {
        match terminal {
            RemoteTerminal::Tmux => Self::tmux(root),
            RemoteTerminal::Rex => {
                let host = RexHost::open(root, true)?;
                crate::util::write_atomic(&hosted_marker(root), b"")?;
                Ok(Self::Rex(Box::new(host)))
            }
        }
    }

    pub fn mux(&self) -> &dyn Multiplexer {
        match self {
            Self::Tmux { mux, .. } => mux,
            Self::Rex(rex) => &rex.mux,
        }
    }

    pub fn placement(&self) -> Option<RexPlacement> {
        match self {
            Self::Tmux { .. } => None,
            Self::Rex(rex) => Some(RexPlacement {
                host_label: rex.host_label.clone(),
                instance_id: rex.instance_id.clone(),
                sessions: Default::default(),
            }),
        }
    }

    /// Whether pane `index` of `execution` has its session.
    pub fn pane_alive(&self, execution: &Execution, index: usize) -> bool {
        match self {
            Self::Tmux { socket, .. } => {
                session_alive(socket, &session_name(&execution.ex_id, index))
            }
            Self::Rex(rex) => rex.pane_alive(execution, index),
        }
    }

    /// End pane `index`'s session, if any, before it starts again.
    pub fn kill_pane(&self, execution: &Execution, index: usize) {
        match self {
            Self::Tmux { socket, .. } => {
                let name = session_name(&execution.ex_id, index);
                let _ = super::execution::tmux(socket)
                    .args(["kill-session", "-t", &format!("={name}")])
                    .output();
            }
            Self::Rex(rex) => rex.kill_where(|session| {
                rex.recorded(execution, index) == Some(session.id.as_str())
                    || rex.untagged(&session.label) == Some(&session_name(&execution.ex_id, index))
            }),
        }
    }

    /// Start pane `spec.name`'s session running the login shell, with the
    /// workspace's config, spawn variables and secrets in its environment.
    pub fn create(&self, root: &NodeRoot, spec: &NewSession) -> Result<Created> {
        match self {
            Self::Tmux { socket, .. } => {
                let pane_id = new_session(
                    socket,
                    spec.conf,
                    spec.name,
                    spec.cwd,
                    spec.config_path,
                    spec.spawn_env,
                    None,
                )?;
                if !spec.secrets.0.is_empty() {
                    set_session_env(root, socket, spec.name, spec.secrets)?;
                }
                Ok(Created {
                    pane_id,
                    rex_session: None,
                    _env: Vec::new(),
                })
            }
            Self::Rex(rex) => {
                // The shell flavor takes no environment, so each block sources
                // it from its own read-once file.
                let env = block_env(spec);
                let shell_env = secrets::export_file(root, &format!("{}.shell", spec.name), &env)?;
                let label = rex.label(spec.name);
                let (session, block) = rex.mux.create_hosted_session(
                    &label,
                    spec.window_label,
                    spec.cwd,
                    &shell_env,
                )?;
                let agent_env = secrets::export_file(root, spec.name, &env)?;
                rex.mux.source_on_respawn(agent_env.to_path_buf());
                Ok(Created {
                    pane_id: block,
                    rex_session: Some(session),
                    _env: vec![shell_env, agent_env],
                })
            }
        }
    }

    /// End every session of `execution`, live or not.
    pub fn kill_execution(&self, root: &NodeRoot, execution: &Execution) {
        match self {
            Self::Tmux { socket, .. } => kill_execution(root, socket, &execution.ex_id),
            Self::Rex(rex) => rex.kill_execution(execution),
        }
    }
}

pub struct NewSession<'a> {
    /// The tmux session name, `<ex_id>-<index>`.
    pub name: &'a str,
    /// Rex window label: the workspace handle, which the Rex.app tab shows.
    pub window_label: &'a str,
    pub conf: &'a Path,
    pub cwd: &'a Path,
    pub config_path: &'a Path,
    pub spawn_env: Option<(&'a Path, &'a str)>,
    pub secrets: &'a Secrets,
}

/// A started session. Holds the env files Rex blocks read until the pane's
/// program has started.
pub struct Created {
    pub pane_id: String,
    pub rex_session: Option<String>,
    _env: Vec<TempPath>,
}

/// What a tmux session of `spec` gets through `new_session` and
/// `set_session_env`.
fn session_env(spec: &NewSession) -> Secrets {
    let mut env = spec.secrets.clone();
    env.0.insert("COLORTERM".into(), "truecolor".into());
    env.0.insert(
        crate::frozen_config::FROZEN_CONFIG_ENV.into(),
        spec.config_path.to_string_lossy().into_owned(),
    );
    if let Some((root, ws_id)) = spec.spawn_env {
        env.0.insert(
            super::spawn::SESSION_ROOT_ENV.into(),
            root.to_string_lossy().into_owned(),
        );
        env.0
            .insert(super::spawn::SESSION_WORKSPACE_ENV.into(), ws_id.into());
    }
    env
}

/// The environment of a Rex block of `spec`: the node's own, as a tmux
/// server the node starts inherits it, under `session_env`. Rex blocks would
/// otherwise run with the Rex server's environment, which differs from the
/// node's; on WSL it carries the Windows PATH, whose drvfs directories make
/// shell startup files that search PATH take a second.
fn block_env(spec: &NewSession) -> Secrets {
    let mut env = Secrets(
        std::env::vars()
            .filter(|(name, value)| {
                !super::execution::SESSION_EXCLUDED_ENV.contains(&name.as_str())
                    && !block_own_env(name)
                    && crate::config::is_valid_env_name(name)
                    && secrets::representable(value)
            })
            .collect(),
    );
    env.0.extend(session_env(spec).0);
    env
}

/// Variables Rex sets for each block, which `SOURCE_ENV` keeps.
fn block_own_env(name: &str) -> bool {
    name.starts_with("REX_") || matches!(name, "TERM" | "TERM_PROGRAM" | "TERM_PROGRAM_VERSION")
}

/// End `execution`'s sessions on its own host. A Rex server that does not
/// answer has nothing running for it, so it is skipped with a warning.
pub fn stop_execution(root: &NodeRoot, execution: &Execution) -> Result<()> {
    match SessionHost::for_execution(root, execution) {
        Ok(host) => host.kill_execution(root, execution),
        Err(err) if execution.rex.is_some() => {
            tracing::warn!(ex_id = %execution.ex_id, "Rex server unavailable: {err:#}");
        }
        Err(err) => return Err(err),
    }
    Ok(())
}

impl RexHost {
    /// The node's own Rex server. It is never autostarted: a server started
    /// here would serve no tailnet, so Rex.app could not show its sessions.
    /// Only a new execution needs the tailnet; stopping one does not.
    pub fn open(root: &NodeRoot, for_start: bool) -> Result<Self> {
        let mux = RexBackend::new();
        let status = mux.server_status().map_err(|err| {
            refuse(
                code::REX_UNAVAILABLE,
                format!(
                    "the node's Rex server does not answer ({err:#}); start it with \
                     `rex server run --tailscale` on the node"
                ),
            )
        })?;
        let host_label = match status.tailnet_name() {
            Some(name) => name.to_string(),
            // The loopback test node stands in for a tailnet server with
            // its own address.
            None if std::env::var_os(crate::remote::transport::LOOPBACK_ENV)
                .is_some_and(|v| v == "1") =>
            {
                std::env::var("REX_SERVER")
                    .map_err(|_| anyhow!("loopback Rex hosting needs REX_SERVER"))?
            }
            None if !for_start => String::new(),
            None => {
                return Err(refuse(
                    code::REX_UNAVAILABLE,
                    "the node's Rex server is not on the tailnet, so Rex.app cannot reach it; \
                     run it with `rex server run --tailscale`",
                ));
            }
        };
        Ok(Self {
            mux,
            tag: root_tag(&root.path),
            host_label,
            instance_id: status.instance_id,
        })
    }

    /// The label of the session tmux would name `name`.
    fn label(&self, name: &str) -> String {
        format!("{}{name}", self.tag)
    }

    /// `label` without this root's tag; `None` for another root's or a
    /// session workrex did not create.
    pub fn untagged<'a>(&self, label: &'a str) -> Option<&'a str> {
        label.strip_prefix(&self.tag)
    }

    /// This root's sessions, labels untagged.
    pub fn sessions(&self) -> Result<Vec<HostedSession>> {
        Ok(self
            .mux
            .hosted_sessions()?
            .into_iter()
            .filter_map(|session| {
                Some(HostedSession {
                    label: self.untagged(&session.label)?.to_string(),
                    id: session.id,
                })
            })
            .collect())
    }

    fn recorded<'a>(&self, execution: &'a Execution, index: usize) -> Option<&'a str> {
        let placement = execution.rex.as_ref()?;
        if placement.instance_id != self.instance_id {
            return None;
        }
        placement.sessions.get(&index).map(String::as_str)
    }

    fn pane_alive(&self, execution: &Execution, index: usize) -> bool {
        self.recorded(execution, index).is_some_and(|id| {
            self.mux
                .hosted_live_panes(id)
                .is_ok_and(|panes| !panes.is_empty())
        })
    }

    /// Live blocks of `execution`'s panes, each named as its tmux session
    /// would be, so the observer reads both hosts alike.
    pub fn live_panes(&self, execution: &Execution) -> HashMap<String, LivePaneInfo> {
        let mut live = HashMap::new();
        let Some(placement) = &execution.rex else {
            return live;
        };
        if placement.instance_id != self.instance_id {
            return live;
        }
        for (index, id) in &placement.sessions {
            let panes = self.mux.hosted_live_panes(id).unwrap_or_else(|err| {
                tracing::debug!(session = %id, "rex session unreadable: {err:#}");
                HashMap::new()
            });
            for (block, mut pane) in panes {
                pane.session = Some(session_name(&execution.ex_id, *index));
                live.insert(block, pane);
            }
        }
        live
    }

    /// Destroy the sessions `matches` selects.
    fn kill_where(&self, matches: impl Fn(&HostedSession) -> bool) {
        let sessions = match self.mux.hosted_sessions() {
            Ok(sessions) => sessions,
            Err(err) => {
                tracing::warn!("failed to list Rex sessions: {err:#}");
                return;
            }
        };
        for session in sessions.iter().filter(|session| matches(session)) {
            if let Err(err) = self.mux.destroy_hosted_session(&session.id) {
                tracing::warn!(session = %session.id, "failed to destroy: {err:#}");
            }
        }
    }

    /// Every recorded session and every session labeled for the execution,
    /// which also sweeps duplicates a crashed start left.
    fn kill_execution(&self, execution: &Execution) {
        let prefix = format!("{}-", execution.ex_id);
        let recorded: Vec<&str> = execution
            .rex
            .iter()
            .filter(|placement| placement.instance_id == self.instance_id)
            .flat_map(|placement| placement.sessions.values().map(String::as_str))
            .collect();
        self.kill_where(|session| {
            recorded.contains(&session.id.as_str())
                || self
                    .untagged(&session.label)
                    .is_some_and(|label| label.starts_with(&prefix))
        });
    }
}

/// Present once the root started an execution on Rex: only then do gc and
/// the observer look at the Rex server, which tmux-only nodes leave alone.
pub fn hosted_marker(root: &NodeRoot) -> PathBuf {
    root.path.join("run").join("rex-hosted")
}

/// `wr<8 hex>.`, from the digest of the root's path.
fn root_tag(root: &Path) -> String {
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    let hex: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    format!("wr{hex}.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::node::{Execution, ExecutionState, PaneProgress};
    use std::collections::BTreeMap;

    const TAG: &str = "wr0000beef.";
    const CREATED: &str = r#"{"session_id":"session:new","initial_windows":[{"window_id":"window:w","block_ids":["block:b0"],"revision":1}]}"#;
    const VIEW: &str = include_str!("../../multiplexer/testdata/rex_session_view.json");
    const RUNNING: &str = include_str!("../../multiplexer/testdata/rex_process.json");
    const EXITED: &str = include_str!("../../multiplexer/testdata/rex_process_exited.json");

    /// A fake `rex` that logs every call with its stdin and answers by the
    /// first pattern the call matches.
    struct Fake {
        dir: tempfile::TempDir,
        host: SessionHost,
    }

    impl Fake {
        fn new(cases: &[(&str, &str)]) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            // Shell builtins only: the Nix build sandbox has no `cat`.
            let mut arms = String::new();
            for (pattern, reply) in cases {
                let reply = reply.replace('\'', "'\\''");
                arms.push_str(&format!("  *{pattern}*) printf '%s\\n' '{reply}';;\n"));
            }
            let program = dir.path().join("rex");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\ninput=\nwhile IFS= read -r line || [ -n \"$line\" ]; do \
                     input=\"$input$line\"; done\ncall=\"$* $input\"\n\
                     echo \"$call\" >> '{}'\ncase \"$call\" in\n\
                     {arms}  *server.version*) echo '{{\"rex_version\":\"t\",\"api_revision\":9}}';;\n\
                     \x20 *) echo \"unexpected: $call\" >&2; exit 1;;\nesac\n",
                    dir.path().join("calls").display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            let server = format!("unix://{}/s.sock", dir.path().display());
            let host = SessionHost::Rex(Box::new(RexHost {
                mux: RexBackend::with_program(&program, &server),
                tag: TAG.into(),
                host_label: "rex.example.ts.net".into(),
                instance_id: "instance-1".into(),
            }));
            Self { dir, host }
        }

        fn rex(&self) -> &RexHost {
            match &self.host {
                SessionHost::Rex(rex) => rex,
                SessionHost::Tmux { .. } => unreachable!(),
            }
        }

        fn root(&self) -> NodeRoot {
            NodeRoot {
                path: self.dir.path().join("root"),
            }
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    fn execution(sessions: &[(usize, &str)], instance: &str) -> Execution {
        Execution {
            ex_id: "x0123456789ab".into(),
            state: ExecutionState::Running,
            panes: sessions
                .iter()
                .map(|(index, _)| PaneProgress { index: *index })
                .collect(),
            boot_id: None,
            pin: None,
            continues: false,
            agent_kind: None,
            closed_shells: Default::default(),
            rex: Some(RexPlacement {
                host_label: "rex.example.ts.net".into(),
                instance_id: instance.into(),
                sessions: sessions
                    .iter()
                    .map(|(index, id)| (*index, id.to_string()))
                    .collect(),
            }),
        }
    }

    #[test]
    fn a_rex_pane_gets_a_tagged_session_and_its_secrets_only_through_the_env_file() {
        let fake = Fake::new(&[
            ("session.create", CREATED),
            ("session.new_split", r#"{"block_ids":["block:agent"]}"#),
            ("block.close", "{}"),
        ]);
        let root = fake.root();
        std::fs::create_dir_all(&root.path).unwrap();
        let secrets = Secrets(BTreeMap::from([(
            "WR8_SECRET".to_string(),
            "s3cr3t value".to_string(),
        )]));
        let config_path = root.path.join("config.json");
        let created = fake
            .host
            .create(
                &root,
                &NewSession {
                    name: "x0123456789ab-0",
                    window_label: "feat-r",
                    conf: Path::new("/unused"),
                    cwd: Path::new("/work"),
                    config_path: &config_path,
                    spawn_env: Some((&root.path, "w0123456789ab")),
                    secrets: &secrets,
                },
            )
            .unwrap();
        assert_eq!(created.pane_id, "block:b0");
        assert_eq!(created.rex_session.as_deref(), Some("session:new"));
        let respawned = fake
            .rex()
            .mux
            .respawn_pane(&created.pane_id, Path::new("/work"), Some("handshake"))
            .unwrap();
        assert_eq!(respawned, "block:agent");

        let [shell_path, agent_path] = [0, 1].map(|i| created._env[i].to_path_buf());
        for path in [&shell_path, &agent_path] {
            let env = std::fs::read_to_string(path).unwrap();
            assert!(env.contains("export WR8_SECRET='s3cr3t value'\n"), "{env}");
            assert!(env.contains("export COLORTERM=truecolor\n"), "{env}");
            assert!(env.contains(&format!("{}=", crate::frozen_config::FROZEN_CONFIG_ENV)));
            // The node's own environment, which a tmux session inherits.
            let path_line = format!(
                "export PATH={}\n",
                crate::shell::shell_quote(&std::env::var("PATH").unwrap())
            );
            assert!(env.contains(&path_line), "{env}");
        }

        let calls = fake.calls();
        let create = calls.iter().find(|c| c.contains("session.create")).unwrap();
        assert!(create.contains(&format!("\"label\":\"{TAG}x0123456789ab-0\"")));
        assert!(create.contains("\"window_label\":\"feat-r\""));
        assert!(create.contains(&*shell_path.to_string_lossy()), "{create}");
        let split = calls.iter().find(|c| c.contains("new_split")).unwrap();
        assert!(split.contains(&*agent_path.to_string_lossy()), "{split}");
        assert!(split.contains("handshake"));
        for call in &calls {
            assert!(call.starts_with("--autostart=false -S unix://"), "{call}");
            assert!(!call.contains("s3cr3t"), "{call}");
        }
    }

    #[test]
    fn killing_a_rex_execution_destroys_only_its_own_sessions() {
        let list = serde_json::json!({ "sessions": [
            { "session_id": "session:renamed", "label": "the agent renamed me" },
            { "session_id": "session:dup", "label": format!("{TAG}x0123456789ab-0") },
            { "session_id": "session:shell", "label": format!("{TAG}x0123456789ab-shell1") },
            { "session_id": "session:other-ex", "label": format!("{TAG}x999999999999-0") },
            { "session_id": "session:other-root", "label": "wrffffffff.x0123456789ab-0" },
            { "session_id": "session:notes", "label": "notes" },
        ]});
        let fake = Fake::new(&[
            ("session.list", &list.to_string()),
            ("session.destroy", "{}"),
        ]);
        fake.host.kill_execution(
            &fake.root(),
            &execution(&[(0, "session:renamed")], "instance-1"),
        );
        let destroyed: Vec<String> = fake
            .calls()
            .into_iter()
            .filter(|call| call.contains("session.destroy"))
            .collect();
        assert_eq!(destroyed.len(), 3, "{destroyed:?}");
        for id in ["renamed", "dup", "shell"] {
            assert!(
                destroyed
                    .iter()
                    .any(|call| call.contains(&format!("\"session:{id}\""))),
                "{id}: {destroyed:?}"
            );
        }
    }

    #[test]
    fn a_block_keeps_only_the_variables_rex_sets_for_it() {
        for name in [
            "REX_BLOCK",
            "REX_SERVER",
            "TERM",
            "TERM_PROGRAM",
            "TERM_PROGRAM_VERSION",
        ] {
            assert!(block_own_env(name), "{name}");
        }
        for name in ["PATH", "TERMINFO", "DISPLAY"] {
            assert!(!block_own_env(name), "{name}");
        }
        let root = Path::new("/node");
        let env = block_env(&NewSession {
            name: "x0123456789ab-0",
            window_label: "feat-r",
            conf: Path::new("/unused"),
            cwd: Path::new("/work"),
            config_path: Path::new("/node/config.json"),
            spawn_env: Some((root, "w0123456789ab")),
            secrets: &Secrets::default(),
        });
        assert_eq!(
            env.0
                .get(super::super::spawn::SESSION_ROOT_ENV)
                .map(String::as_str),
            Some("/node")
        );
        assert!(
            env.0.keys().all(|name| !block_own_env(name)),
            "{:?}",
            env.0.keys()
        );
    }

    #[test]
    fn root_tags_tell_node_roots_apart() {
        let a = root_tag(Path::new("/home/u/.workrex-node"));
        assert_eq!(a, root_tag(Path::new("/home/u/.workrex-node")));
        assert_ne!(a, root_tag(Path::new("/home/u/.workrex-node-2")));
        assert!(
            a.starts_with("wr") && a.ends_with('.') && a.len() == 11,
            "{a}"
        );
    }

    #[test]
    fn a_rex_pane_is_alive_while_its_program_runs_on_the_same_server() {
        let running = Fake::new(&[
            ("session.view", VIEW),
            ("process", RUNNING),
            ("title", r#"{"title":"agent"}"#),
        ]);
        let ex = execution(&[(0, "session:s")], "instance-1");
        assert!(running.host.pane_alive(&ex, 0));
        assert!(!running.host.pane_alive(&ex, 1));
        let live = running.rex().live_panes(&ex);
        assert!(!live.is_empty());
        assert!(live.iter().all(|(id, pane)| id.starts_with("block:")
            && pane.session.as_deref() == Some("x0123456789ab-0")));
        // A replaced server does not have the recorded sessions.
        let replaced = execution(&[(0, "session:s")], "instance-0");
        assert!(!running.host.pane_alive(&replaced, 0));
        assert!(running.rex().live_panes(&replaced).is_empty());

        let exited = Fake::new(&[("session.view", VIEW), ("process", EXITED)]);
        assert!(!exited.host.pane_alive(&ex, 0));
        assert!(exited.rex().live_panes(&ex).is_empty());
    }
}
