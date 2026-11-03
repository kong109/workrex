//! Where an execution's terminals live: a session per pane on the node tmux
//! server, or on the node's own Rex server, which Rex.app shows directly.
//! The host is fixed when the execution starts and recorded on it
//! (`Execution::rex`), so verbs never read it from config.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Result, anyhow};
use sha2::{Digest, Sha256};

use tempfile::TempPath;

use super::execution::{kill_execution, new_session, session_alive, set_session_env, tmux_socket};
use super::{Execution, NodeRoot, refuse, secrets};
use crate::config::RemoteTerminal;
use crate::multiplexer::rex::{HostedRead, HostedSession, RexBackend};
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
    /// The sessions a start's opening read listed, which its first kill
    /// uses instead of listing them again. Only that start's own sessions,
    /// created after the read, are missing from it.
    listed: Mutex<Option<Vec<HostedSession>>>,
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
                let marker = hosted_marker(root);
                if let Some(run) = marker.parent() {
                    crate::remote::transport::ensure_private_dir(run, unsafe { libc::getuid() })?;
                }
                crate::util::write_atomic(&marker, b"")?;
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
    pub fn kill_pane(&self, root: &NodeRoot, execution: &Execution, index: usize) {
        match self {
            Self::Tmux { socket, .. } => {
                let name = session_name(&execution.ex_id, index);
                let _ = super::execution::tmux(socket)
                    .args(["kill-session", "-t", &format!("={name}")])
                    .output();
            }
            Self::Rex(rex) => {
                let name = session_name(&execution.ex_id, index);
                let own: Vec<&str> = rex.recorded(execution, index).into_iter().collect();
                rex.kill_owned(root, execution, &own, |label| label == name);
            }
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
            Self::Rex(rex) => rex.kill_execution(root, execution),
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
        Self::open_with(RexBackend::new(), root, for_start)
    }

    fn open_with(mux: RexBackend, root: &NodeRoot, for_start: bool) -> Result<Self> {
        let opened = mux.open_server(for_start).map_err(|err| {
            refuse(
                code::REX_UNAVAILABLE,
                format!(
                    "the node's Rex server does not answer ({err:#}); start it with \
                     `rex server run --tailscale` on the node"
                ),
            )
        })?;
        let status = opened.status;
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
            listed: Mutex::new(opened.sessions),
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

    /// This root's sessions, each named as its tmux session would be. A
    /// session one of `executions` recorded keeps that name after Rex
    /// renames it; any other is named by its label without the tag.
    pub fn sessions(&self, executions: &[&Execution]) -> Result<Vec<HostedSession>> {
        let recorded = self.recorded_names(executions);
        Ok(self
            .mux
            .hosted_sessions()?
            .into_iter()
            .filter_map(|session| {
                let label = match recorded.get(session.id.as_str()) {
                    Some(name) => name.clone(),
                    None => self.untagged(&session.label)?.to_string(),
                };
                Some(HostedSession {
                    label,
                    id: session.id,
                })
            })
            .collect())
    }

    /// The sessions `executions` recorded on this server, each by the name
    /// its tmux session would have.
    fn recorded_names<'a>(&self, executions: &[&'a Execution]) -> HashMap<&'a str, String> {
        executions
            .iter()
            .filter_map(|execution| {
                let placement = execution.rex.as_ref()?;
                (placement.instance_id == self.instance_id).then_some((execution, placement))
            })
            .flat_map(|(execution, placement)| {
                placement
                    .sessions
                    .iter()
                    .map(|(index, id)| (id.as_str(), session_name(&execution.ex_id, *index)))
            })
            .collect()
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
                .read_hosted(&[id])
                .is_ok_and(|mut reads| reads.remove(id).is_some_and(|read| !read.live().is_empty()))
        })
    }

    /// Live blocks of the panes of `executions`, each named as its tmux
    /// session would be, so the observer reads both hosts alike. One read
    /// covers every recorded session.
    pub fn live_panes(&self, executions: &[&Execution]) -> HashMap<String, LivePaneInfo> {
        let names = self.recorded_names(executions);
        let ids: Vec<&str> = names.keys().copied().collect();
        let reads = self.mux.read_hosted(&ids).unwrap_or_else(|err| {
            tracing::debug!("rex sessions unreadable: {err:#}");
            HashMap::new()
        });
        let mut live = HashMap::new();
        for (id, read) in reads {
            if let HostedRead::Unreadable(err) = &read {
                tracing::debug!(session = %id, "rex session unreadable: {err:#}");
            }
            for (block, mut pane) in read.live() {
                pane.session = names.get(id.as_str()).cloned();
                live.insert(block, pane);
            }
        }
        live
    }

    /// Destroy the sessions `matches` selects.
    fn kill_where(&self, matches: impl Fn(&HostedSession) -> bool) {
        let listed = self.listed.lock().unwrap().take();
        let sessions = match listed.map_or_else(|| self.mux.hosted_sessions(), Ok) {
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

    /// Every session `execution` recorded, and every session labeled for it
    /// that no execution records: duplicates a crashed start left.
    fn kill_execution(&self, root: &NodeRoot, execution: &Execution) {
        let prefix = format!("{}-", execution.ex_id);
        let own = self.recorded_ids(execution);
        self.kill_owned(root, execution, &own, |label| label.starts_with(&prefix));
    }

    /// Destroy the sessions in `own`, and the sessions whose untagged label
    /// `labeled` accepts that no execution on `root` records. A session
    /// another execution records is never destroyed, whatever its label.
    /// Rex keeps no creator on a session, so one created outside workrex
    /// with this root's tagged label passes for an orphan.
    fn kill_owned(
        &self,
        root: &NodeRoot,
        execution: &Execution,
        own: &[&str],
        labeled: impl Fn(&str) -> bool,
    ) {
        let executions: Vec<Execution> = root
            .workspaces()
            .into_iter()
            .filter_map(|workspace| workspace.execution)
            .collect();
        let recorded: HashSet<&str> = executions
            .iter()
            .chain([execution])
            .flat_map(|execution| self.recorded_ids(execution))
            .collect();
        self.kill_where(|session| {
            own.contains(&session.id.as_str())
                || (!recorded.contains(session.id.as_str())
                    && self.untagged(&session.label).is_some_and(&labeled))
        });
    }

    /// The sessions `execution` recorded on this server.
    fn recorded_ids<'a>(&self, execution: &'a Execution) -> Vec<&'a str> {
        execution
            .rex
            .iter()
            .filter(|placement| placement.instance_id == self.instance_id)
            .flat_map(|placement| placement.sessions.values().map(String::as_str))
            .collect()
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
    use crate::remote::node::{Execution, ExecutionState, PaneProgress, Workspace};
    use crate::remote::protocol::WorkspaceIdentity;
    use std::collections::BTreeMap;

    const TAG: &str = "wr0000beef.";
    const CREATED: &str = r#"{"session_id":"session:new","initial_windows":[{"window_id":"window:w","block_ids":["block:b0"],"revision":1}]}"#;
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
                // A reply starting with `!` fails with the rest on stderr.
                let (reply, out) = match reply.strip_prefix('!') {
                    Some(stderr) => (stderr, ">&2; exit 1"),
                    None => (*reply, ""),
                };
                let reply = reply.replace('\'', "'\\''");
                arms.push_str(&format!(
                    "  *{pattern}*) printf '%s\\n' '{reply}' {out};;\n"
                ));
            }
            let program = dir.path().join("rex");
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\ninput=\nwhile IFS= read -r line || [ -n \"$line\" ]; do \
                     input=\"$input$line\"; done\ncall=\"$* $input\"\n\
                     printf '%s\\036' \"$call\" >> '{}'\ncase \"$call\" in\n\
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
                listed: Mutex::new(None),
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

        /// Every `rex` process the fake ran, in order.
        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("calls"))
                .unwrap_or_default()
                .split_terminator('\u{1e}')
                .map(str::to_string)
                .collect()
        }

        /// What each process did: the method of an API call, `write` for a
        /// block write, and the script a `rex do` ran.
        fn steps(&self) -> Vec<String> {
            self.calls()
                .iter()
                .map(|call| {
                    let call = call.split_once("s.sock ").map_or(call.as_str(), |(_, c)| c);
                    let script = [("server.status", "open"), ("session.new_split", "respawn")]
                        .into_iter()
                        .find(|(needle, _)| call.starts_with("do -e") && call.contains(needle));
                    let words: Vec<&str> = call.split_whitespace().collect();
                    match (script, &words[..]) {
                        (Some((_, name)), _) => format!("do {name}"),
                        (None, ["api", "call", method, ..]) => method.to_string(),
                        (None, ["block", "call", "-b", _, method, ..]) => method.to_string(),
                        _ => call.to_string(),
                    }
                })
                .collect()
        }

        /// A host opened on the fake as `RexHost::open` opens one, with
        /// the fixed tag.
        fn open(&self, for_start: bool) -> Result<RexHost> {
            let host = RexHost::open_with(
                RexBackend::with_program(
                    &self.dir.path().join("rex"),
                    &format!("unix://{}/s.sock", self.dir.path().display()),
                ),
                &self.root(),
                for_start,
            )?;
            Ok(RexHost {
                tag: TAG.into(),
                ..host
            })
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

    /// The answer of the opening read: a server of `api_revision`, on the
    /// tailnet or not, with the sessions in `listed` by id and label.
    fn opened(api_revision: u32, tailnet: bool, listed: Option<&[(&str, &str)]>) -> String {
        let sessions = listed.map(|listed| {
            listed
                .iter()
                .map(|(id, label)| (id.to_string(), serde_json::json!(label)))
                .collect::<serde_json::Map<_, _>>()
        });
        serde_json::json!({
            "version": { "rex_version": "t", "api_revision": api_revision },
            "status": {
                "instance_id": "instance-1",
                "tailscale": {
                    "state": if tailnet { "running" } else { "disabled" },
                    "dns_name": "rex.example.ts.net",
                },
            },
            "sessions": sessions,
        })
        .to_string()
    }

    fn new_session<'a>(root: &'a NodeRoot, config_path: &'a Path) -> NewSession<'a> {
        NewSession {
            name: "x0123456789ab-0",
            window_label: "feat-r",
            conf: Path::new("/unused"),
            cwd: Path::new("/work"),
            config_path,
            spawn_env: Some((&root.path, "w0123456789ab")),
            secrets: &EMPTY,
        }
    }

    static EMPTY: Secrets = Secrets(BTreeMap::new());

    #[test]
    fn a_hosted_start_runs_four_rex_processes() {
        let fake = Fake::new(&[
            ("session.new_split", r#"{"block_ids":["block:agent"]}"#),
            ("server.status", &opened(9, true, Some(&[]))),
            ("session.create", CREATED),
            ("write", "{}"),
        ]);
        let root = fake.root();
        std::fs::create_dir_all(&root.path).unwrap();
        let host = SessionHost::Rex(Box::new(fake.open(true).unwrap()));
        let execution = execution(&[], "instance-1");
        host.kill_pane(&root, &execution, 0);
        let config_path = root.path.join("config.json");
        let created = host
            .create(&root, &new_session(&root, &config_path))
            .unwrap();
        let agent = host
            .mux()
            .respawn_pane(&created.pane_id, Path::new("/work"), Some("handshake"))
            .unwrap();
        host.mux().run_in_shell(&agent, " claude").unwrap();

        // The version, status and session list come from one read, the
        // replacement block and the close from another, and the command and
        // its Enter from one write.
        assert_eq!(
            fake.steps(),
            ["do open", "session.create", "do respawn", "write"],
            "{:#?}",
            fake.calls()
        );
        let calls = fake.calls();
        assert!(calls[0].contains(r#""sessions":true"#), "{}", calls[0]);
        assert!(calls[2].contains(r#""old":"block:b0""#), "{}", calls[2]);
        assert!(calls[3].contains("block:agent"), "{}", calls[3]);
        assert!(
            calls[3].ends_with(r#"{"data":" claude\r"}"#),
            "{}",
            calls[3]
        );
    }

    #[test]
    fn a_start_destroys_the_orphans_its_opening_read_listed_and_lists_again_after() {
        let orphan = format!("{TAG}x0123456789ab-0");
        let fake = Fake::new(&[
            (
                "server.status",
                &opened(9, true, Some(&[("session:orphan", &orphan)])),
            ),
            (
                "session.list",
                &serde_json::json!({ "sessions": [
                    { "session_id": "session:late", "label": orphan },
                ]})
                .to_string(),
            ),
            ("session.destroy", "{}"),
        ]);
        let root = fake.root();
        let host = SessionHost::Rex(Box::new(fake.open(true).unwrap()));
        let execution = execution(&[], "instance-1");
        host.kill_pane(&root, &execution, 0);
        host.kill_pane(&root, &execution, 0);
        assert_eq!(
            fake.steps(),
            [
                "do open",
                "session.destroy",
                "session.list",
                "session.destroy"
            ]
        );
        let calls = fake.calls();
        assert!(calls[1].contains(r#""session:orphan""#), "{}", calls[1]);
        assert!(calls[3].contains(r#""session:late""#), "{}", calls[3]);
    }

    #[test]
    fn only_a_start_lists_sessions_when_it_opens_the_server() {
        let fake = Fake::new(&[
            ("server.status", &opened(9, false, None)),
            ("session.list", r#"{"sessions":[]}"#),
        ]);
        let host = fake.open(false).unwrap();
        assert_eq!(host.host_label, "");
        assert_eq!(host.instance_id, "instance-1");
        assert!(fake.calls()[0].contains(r#""sessions":false"#));
        host.kill_where(|_| true);
        assert_eq!(fake.steps(), ["do open", "session.list"]);
    }

    #[test]
    fn a_start_needs_a_supported_server_on_the_tailnet() {
        let err = |fake: &Fake| fake.open(true).err().expect("open fails").to_string();

        let old = Fake::new(&[("server.status", &opened(3, true, Some(&[])))]);
        let message = err(&old);
        assert!(message.contains("does not answer"), "{message}");
        assert!(message.contains("unsupported Rex t"), "{message}");
        // The verdict holds for the process: no call reaches the server.
        assert!(old.rex().mux.hosted_sessions().is_err());
        assert_eq!(old.steps(), ["do open"]);

        // A server too old for the one-process read is named as unsupported.
        let older = Fake::new(&[
            ("server.status", "!Error: unknown command \"do\""),
            (
                "call?server.version",
                r#"{"rex_version":"t","api_revision":3}"#,
            ),
        ]);
        let message = err(&older);
        assert!(message.contains("unsupported Rex t"), "{message}");

        let off = Fake::new(&[("server.status", &opened(9, false, Some(&[])))]);
        let message = err(&off);
        assert!(message.contains("not on the tailnet"), "{message}");
    }

    #[test]
    fn a_failed_opening_read_names_its_step() {
        let status_failed = serde_json::json!({
            "version": { "rex_version": "t", "api_revision": 9 },
            "status_error": "stream response failed: boom (status=500 code=internal)",
        })
        .to_string();
        let fake = Fake::new(&[("server.status", &status_failed)]);
        let message = fake.open(true).err().unwrap().to_string();
        assert!(
            message.contains(
                "does not answer (rex server.status failed: stream response failed: boom"
            ),
            "{message}"
        );

        // A `rex do` that cannot reach the server is named, not quoted.
        let down = Fake::new(&[("server.status", "!Error: dial unix s.sock: connect refused")]);
        let message = format!("{:#}", down.open(true).err().unwrap());
        assert!(
            message.contains("rex do open failed: Error: dial unix s.sock: connect refused"),
            "{message}"
        );
        assert!(!message.contains("rex.call"), "{message}");
    }

    #[test]
    fn a_respawn_names_the_step_rex_refused() {
        let fake = Fake::new(&[
            ("session.create", CREATED),
            (
                "session.new_split",
                r#"{"split_error":"stream response failed: no room (status=409 code=conflict)"}"#,
            ),
        ]);
        let root = fake.root();
        std::fs::create_dir_all(&root.path).unwrap();
        let config_path = root.path.join("config.json");
        let created = fake
            .host
            .create(&root, &new_session(&root, &config_path))
            .unwrap();
        let err = fake
            .host
            .mux()
            .respawn_pane(&created.pane_id, Path::new("/work"), Some("handshake"))
            .unwrap_err();
        assert_eq!(
            format!("{err:#}"),
            "Failed to start the replacement Rex block: stream response failed: no room \
             (status=409 code=conflict)"
        );

        for (close_error, ok) in [
            (
                "block block:b0 not found in any window (status=404 code=not_found)",
                true,
            ),
            (
                "stream response failed: busy (status=500 code=internal)",
                false,
            ),
        ] {
            let reply =
                serde_json::json!({ "block_ids": ["block:agent"], "close_error": close_error });
            let fake = Fake::new(&[
                ("session.create", CREATED),
                ("session.new_split", &reply.to_string()),
            ]);
            let created = fake
                .host
                .create(&root, &new_session(&root, &config_path))
                .unwrap();
            let respawned = fake.host.mux().respawn_pane(
                &created.pane_id,
                Path::new("/work"),
                Some("handshake"),
            );
            assert_eq!(respawned.is_ok(), ok, "{close_error}: {respawned:?}");
        }
    }

    #[test]
    fn a_session_rex_renamed_keeps_the_name_its_execution_recorded() {
        let list = serde_json::json!({ "sessions": [
            { "session_id": "session:renamed", "label": "Fix the login bug" },
            { "session_id": "session:tagged", "label": format!("{TAG}x999999999999-0") },
            { "session_id": "session:notes", "label": "notes" },
        ]});
        let fake = Fake::new(&[("session.list", &list.to_string())]);
        let current = execution(&[(0, "session:renamed")], "instance-1");
        // An id recorded on an earlier run of the server names nothing now.
        let mut earlier = execution(&[(0, "session:notes")], "instance-0");
        earlier.ex_id = "x111111111111".into();
        let named = |id: &str, label: &str| HostedSession {
            id: id.into(),
            label: label.into(),
        };
        assert_eq!(
            fake.rex().sessions(&[&current, &earlier]).unwrap(),
            [
                named("session:renamed", "x0123456789ab-0"),
                named("session:tagged", "x999999999999-0"),
            ]
        );
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
    fn a_session_another_execution_records_survives_whatever_its_label() {
        // B's session carries A's label and A's carries B's, as after a swap.
        let list = serde_json::json!({ "sessions": [
            { "session_id": "session:a", "label": format!("{TAG}x999999999999-0") },
            { "session_id": "session:b", "label": format!("{TAG}x0123456789ab-0") },
            { "session_id": "session:pane1", "label": format!("{TAG}x0123456789ab-0") },
            { "session_id": "session:orphan", "label": format!("{TAG}x0123456789ab-0") },
        ]});
        let fake = Fake::new(&[
            ("session.list", &list.to_string()),
            ("session.destroy", "{}"),
        ]);
        let root = fake.root();
        let mut workspace = Workspace::new(
            &root,
            WorkspaceIdentity {
                ws_id: "w999999999999".into(),
                project_id: "repo-0123456789ab".into(),
                handle: "b".into(),
                branch: "b".into(),
                base_sha: "a".repeat(40),
            },
            crate::config::RemoteIsolation::None,
        );
        let mut b = execution(&[(0, "session:b")], "instance-1");
        b.ex_id = "x999999999999".into();
        workspace.execution = Some(b);
        root.save(&workspace).unwrap();
        let destroyed = || -> Vec<String> {
            fake.calls()
                .into_iter()
                .filter(|call| call.contains("session.destroy"))
                .collect()
        };

        let a = execution(&[(0, "session:a"), (1, "session:pane1")], "instance-1");
        let destroys = |id: &str| {
            let id = format!("\"session:{id}\"");
            destroyed().iter().filter(|call| call.contains(&id)).count()
        };
        fake.host.kill_pane(&root, &a, 0);
        assert_eq!(
            ["a", "b", "pane1", "orphan"].map(destroys),
            [1, 0, 0, 1],
            "{:?}",
            destroyed()
        );
        fake.host.kill_execution(&root, &a);
        assert_eq!(
            ["a", "b", "pane1", "orphan"].map(destroys),
            [2, 0, 1, 2],
            "{:?}",
            destroyed()
        );
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

    /// The answer of one session read: `blocks` maps each session to its
    /// blocks' process reports; a session left out is gone.
    fn read_reply(blocks: &[(&str, &[(&str, &str)])], gone: &[&str]) -> String {
        let mut reply = serde_json::Map::new();
        for (session, blocks) in blocks {
            let blocks: serde_json::Map<String, serde_json::Value> = blocks
                .iter()
                .map(|(block, process)| {
                    let read = serde_json::json!({
                        "window": { "window_id": "window:w", "label": "feat-r" },
                        "process": serde_json::from_str::<serde_json::Value>(process).unwrap(),
                        "title": "agent",
                    });
                    (block.to_string(), read)
                })
                .collect();
            reply.insert(
                session.to_string(),
                serde_json::json!({
                    "session": { "session_id": session, "label": "Fix the login bug" },
                    "blocks": blocks,
                }),
            );
        }
        for session in gone {
            let error = format!(
                "stream response \"control-1\" failed: session {session} not found \
                 (status=404 code=session_not_found)"
            );
            reply.insert(session.to_string(), serde_json::json!({ "error": error }));
        }
        serde_json::Value::Object(reply).to_string()
    }

    #[test]
    fn a_rex_pane_is_alive_while_its_program_runs_on_the_same_server() {
        let running = Fake::new(&[(
            "rex.session.attach",
            &read_reply(&[("session:s", &[("block:b", RUNNING)])], &[]),
        )]);
        let ex = execution(&[(0, "session:s")], "instance-1");
        assert!(running.host.pane_alive(&ex, 0));
        assert!(!running.host.pane_alive(&ex, 1));
        let live = running.rex().live_panes(&[&ex]);
        assert_eq!(
            live.keys().collect::<Vec<_>>(),
            ["block:b"],
            "{:?}",
            running.calls()
        );
        assert_eq!(live["block:b"].session.as_deref(), Some("x0123456789ab-0"));
        assert_eq!(live["block:b"].window.as_deref(), Some("feat-r"));
        // A replaced server does not have the recorded sessions.
        let replaced = execution(&[(0, "session:s")], "instance-0");
        assert!(!running.host.pane_alive(&replaced, 0));
        assert!(running.rex().live_panes(&[&replaced]).is_empty());

        let exited = Fake::new(&[(
            "rex.session.attach",
            &read_reply(&[("session:s", &[("block:b", EXITED)])], &[]),
        )]);
        assert!(!exited.host.pane_alive(&ex, 0));
        assert!(exited.rex().live_panes(&[&ex]).is_empty());
    }

    #[test]
    fn an_observation_reads_every_hosted_session_with_one_rex_process() {
        let ids = [
            "session:a",
            "session:b",
            "session:c",
            "session:d",
            "session:e",
        ];
        let blocks: Vec<[(&str, &str); 1]> = ids.iter().map(|id| [(*id, RUNNING)]).collect();
        let live: Vec<(&str, &[(&str, &str)])> = ids
            .iter()
            .zip(&blocks)
            .map(|(id, blocks)| (*id, &blocks[..]))
            .collect();
        let reply = read_reply(&live, &["session:gone"]);
        let rex_processes = |executions: &[Execution]| {
            let fake = Fake::new(&[("rex.session.attach", &reply)]);
            let executions: Vec<&Execution> = executions.iter().collect();
            let panes = fake.rex().live_panes(&executions);
            (fake.calls().len(), panes)
        };

        let (one, panes) = rex_processes(&[execution(&[(0, "session:a")], "instance-1")]);
        assert_eq!(panes.len(), 1);

        let mut second = execution(&[(0, "session:d"), (1, "session:e")], "instance-1");
        second.ex_id = "x999999999999".into();
        let (six, panes) = rex_processes(&[
            execution(
                &[(0, "session:a"), (1, "session:b"), (2, "session:c")],
                "instance-1",
            ),
            second,
            execution(&[(3, "session:gone")], "instance-1"),
        ]);
        assert_eq!(one, six, "rex processes for 1 and 6 recorded sessions");
        // The gone session is missing; the five others are read.
        assert_eq!(panes.len(), 5, "{panes:?}");
        assert_eq!(
            panes["session:e"].session.as_deref(),
            Some("x999999999999-1")
        );
    }
}
