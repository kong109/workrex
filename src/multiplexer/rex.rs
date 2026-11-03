use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use crate::config::SplitDirection;

use super::Multiplexer;
use super::agent::shell_quote;
use super::types::*;
use super::util;

pub(crate) mod events;
pub(crate) mod sidebar_api;

const SHELL_FLAVOR: &str = "com.superlogical.terminal.shell";

macro_rules! rex_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
        #[serde(transparent)]
        struct $name(String);

        impl $name {
            fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

rex_id!(SessionId);
rex_id!(WindowId);
rex_id!(BlockId);

impl BlockId {
    fn parse(pane_id: &str) -> Result<Self> {
        if pane_id.starts_with("block:") {
            Ok(Self(pane_id.to_string()))
        } else {
            Err(anyhow!("'{pane_id}' is not a Rex block ID"))
        }
    }
}

#[derive(Debug, Deserialize)]
struct SessionList {
    sessions: Vec<SessionSummary>,
}

#[derive(Debug, Deserialize)]
struct SessionSummary {
    session_id: SessionId,
    #[serde(default)]
    label: String,
}

#[derive(Debug, Deserialize)]
struct SessionView {
    session_id: SessionId,
    #[serde(default)]
    label: String,
    #[serde(default)]
    active_window_id: Option<WindowId>,
    #[serde(default)]
    windows: Vec<WindowView>,
}

#[derive(Debug, Deserialize)]
struct WindowView {
    window_id: WindowId,
    #[serde(default)]
    label: String,
    #[serde(default)]
    focused_block_id: Option<BlockId>,
    #[serde(default)]
    layers: Vec<LayerView>,
}

#[derive(Debug, Deserialize)]
struct LayerView {
    #[serde(default)]
    blocks: Vec<BlockView>,
}

#[derive(Debug, Deserialize)]
struct BlockView {
    block_id: BlockId,
}

impl WindowView {
    fn block_ids(&self) -> impl Iterator<Item = &BlockId> {
        self.layers
            .iter()
            .flat_map(|layer| layer.blocks.iter().map(|block| &block.block_id))
    }

    fn contains(&self, block: &BlockId) -> bool {
        self.block_ids().any(|id| id == block)
    }
}

impl SessionView {
    fn window_of(&self, block: &BlockId) -> Option<&WindowView> {
        self.windows.iter().find(|window| window.contains(block))
    }

    fn window(&self, id: &WindowId) -> Option<&WindowView> {
        self.windows.iter().find(|window| &window.window_id == id)
    }

    fn windows_labeled<'a>(&'a self, label: &'a str) -> impl Iterator<Item = &'a WindowView> {
        self.windows
            .iter()
            .filter(move |window| window.label == label)
    }

    fn window_after(&self, id: &WindowId) -> Option<&WindowId> {
        let index = self.windows.iter().position(|w| &w.window_id == id)?;
        self.windows.get(index + 1).map(|w| &w.window_id)
    }

    fn active_window(&self) -> Option<&WindowView> {
        self.window(self.active_window_id.as_ref()?)
    }
}

#[derive(Debug, Deserialize)]
struct Whoami {
    endpoint: String,
    #[serde(rename = "instance_id")]
    boot_id: String,
}

#[derive(Debug, Deserialize)]
struct ProcessReport {
    #[serde(default)]
    child: Option<ProcessEntry>,
    #[serde(default)]
    foreground: Option<ProcessEntry>,
    #[serde(default)]
    ancestors: Vec<ProcessEntry>,
}

#[derive(Debug, Deserialize)]
struct ProcessEntry {
    pid: u32,
    name: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

impl ProcessReport {
    fn working_dir(&self) -> PathBuf {
        self.foreground
            .iter()
            .chain(&self.ancestors)
            .chain(&self.child)
            .find_map(|process| process.cwd.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Deserialize)]
struct TitleReport {
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
struct FormatReport {
    content: String,
}

#[derive(Debug, Deserialize)]
struct SizeReport {
    columns: u32,
    rows: u32,
}

#[derive(Debug, Deserialize)]
struct CreatedWindow {
    window_id: WindowId,
    block_ids: Vec<BlockId>,
}

#[derive(Debug, Deserialize)]
struct CreatedSession {
    #[serde(default)]
    initial_windows: Vec<CreatedWindow>,
}

#[derive(Debug, Deserialize)]
struct CreatedSplit {
    block_ids: Vec<BlockId>,
}

#[derive(Debug, Deserialize)]
struct Resolution {
    results: Vec<ResolvedQuery>,
}

#[derive(Debug, Deserialize)]
struct ResolvedQuery {
    status: String,
    #[serde(default)]
    target: Option<ResolvedTarget>,
}

#[derive(Debug, Deserialize)]
struct ResolvedTarget {
    session_id: SessionId,
    #[serde(default)]
    window_id: Option<WindowId>,
}

#[derive(Debug, Deserialize)]
struct ClientList {
    #[serde(default)]
    clients: Vec<ClientEntry>,
}

#[derive(Debug, Deserialize)]
struct ClientEntry {
    #[serde(default)]
    info: Option<ClientInfo>,
}

#[derive(Debug, Deserialize)]
struct ClientInfo {
    #[serde(default)]
    kind: String,
}

#[derive(Debug)]
enum CallError {
    NotFound(String),
    Failed(anyhow::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::NotFound(message) => write!(f, "{message}"),
            CallError::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for CallError {}

impl CallError {
    fn from_stderr(args: &[&str], stderr: &str) -> Self {
        let stderr = stderr.trim();
        if stderr.contains("code=not_found") || stderr.contains("code=session_not_found") {
            CallError::NotFound(stderr.to_string())
        } else {
            CallError::Failed(anyhow!("rex {} failed: {}", args.join(" "), stderr))
        }
    }
}

type CallResult<T> = std::result::Result<T, CallError>;

fn found<T>(result: CallResult<T>) -> Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(CallError::NotFound(_)) => Ok(None),
        Err(CallError::Failed(error)) => Err(error),
    }
}

/// A terminal block that runs `command` directly. Rex's default goes through
/// `/usr/bin/login` on macOS, which resets `HOME` and `PATH`, so the block
/// asks for no login wrapper and the command starts the login shell itself,
/// as tmux does with its default shell.
fn shell_block(cwd: &Path, command: &[&str]) -> Value {
    json!({
        "block": {
            "flavor": SHELL_FLAVOR,
            "options": {
                "cwd": cwd.to_string_lossy(),
                "shell": "none",
                "login": false,
                "command": command,
            },
        },
    })
}

/// Input for `send_key`: tmux key names become Rex key names, and a single
/// character is typed as text so it is not reinterpreted by a keyboard layout.
#[derive(Debug, PartialEq, Eq)]
enum KeyInput {
    Text(String),
    Key(String),
}

fn key_input(key: &str) -> KeyInput {
    if key.chars().count() == 1 {
        return KeyInput::Text(key.to_string());
    }
    if let Some(rest) = key.strip_prefix("C-") {
        return KeyInput::Key(format!("ctrl+{}", rest.to_lowercase()));
    }
    if let Some(rest) = key.strip_prefix("M-") {
        return KeyInput::Key(format!("alt+{}", rest.to_lowercase()));
    }
    let name = match key {
        "BSpace" => "backspace",
        "PPage" | "PageUp" => "page_up",
        "NPage" | "PageDown" => "page_down",
        other => return KeyInput::Key(other.to_lowercase()),
    };
    KeyInput::Key(name.to_string())
}

fn anchor_ratio(percentage: Option<u8>, size: Option<u16>, total: Option<u32>) -> f64 {
    let new_share = match (percentage, size, total) {
        (Some(percent), _, _) => f64::from(percent) / 100.0,
        (None, Some(size), Some(total)) if total > 0 => f64::from(size) / f64::from(total),
        _ => 0.5,
    };
    (1.0 - new_share).clamp(0.05, 0.95)
}

fn parallel_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    if items.len() <= 1 {
        return items.iter().map(&f).collect();
    }
    thread::scope(|scope| {
        let handles: Vec<_> = items.iter().map(|item| scope.spawn(|| f(item))).collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("rex worker thread panicked"))
            .collect()
    })
}

pub fn in_rex_pane() -> bool {
    ["REX_BLOCK", "REX_SESSION"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| !value.is_empty()))
}

#[derive(Debug, Default)]
pub struct RexBackend {
    server: Option<String>,
    endpoint: OnceLock<String>,
    block_sessions: Mutex<HashMap<BlockId, SessionId>>,
}

impl RexBackend {
    pub fn new() -> Self {
        Self {
            server: std::env::var("REX_SERVER")
                .ok()
                .filter(|server| !server.trim().is_empty()),
            ..Self::default()
        }
    }

    pub fn for_server(address: &str) -> Self {
        Self {
            server: Some(address.to_string()),
            ..Self::default()
        }
    }

    fn program() -> String {
        std::env::var_os("REX_BIN_DIR")
            .map(|dir| PathBuf::from(dir).join("rex"))
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "rex".to_string())
    }

    /// `rex block call -b` fails with "block target not found" when the
    /// inherited `REX_SESSION` names a different session than the block's.
    const INHERITED_SCOPE_VARS: [&'static str; 2] = ["REX_SESSION", "REX_BLOCK"];

    fn global_args(&self) -> Vec<&str> {
        let mut args = vec!["--autostart=false"];
        if let Some(server) = &self.server {
            args.extend(["-S", server.as_str()]);
        }
        args
    }

    fn run(&self, args: &[&str], stdin: Option<&str>) -> CallResult<String> {
        let program = Self::program();
        let mut command = Command::new(&program);
        for var in Self::INHERITED_SCOPE_VARS {
            command.env_remove(var);
        }
        command
            .args(self.global_args())
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command
            .spawn()
            .with_context(|| format!("Failed to run {program}"))
            .map_err(CallError::Failed)?;
        if let Some(input) = stdin
            && let Some(mut pipe) = child.stdin.take()
        {
            pipe.write_all(input.as_bytes())
                .context("Failed to write rex input")
                .map_err(CallError::Failed)?;
        }
        let output = child
            .wait_with_output()
            .context("Failed to wait for rex")
            .map_err(CallError::Failed)?;

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(CallError::from_stderr(
                args,
                &String::from_utf8_lossy(&output.stderr),
            ))
        }
    }

    fn block_layout(&self, cwd: &Path, script: Option<&str>) -> Result<Value> {
        Ok(match script {
            Some(script) => shell_block(cwd, &["sh", "-c", script]),
            None => shell_block(cwd, &[&self.get_default_shell()?, "-l"]),
        })
    }

    fn parse<T: DeserializeOwned>(what: &str, output: &str) -> CallResult<T> {
        serde_json::from_str(output)
            .with_context(|| format!("Failed to parse rex {what} output"))
            .map_err(CallError::Failed)
    }

    fn api<T: DeserializeOwned>(&self, method: &str, args: Value) -> CallResult<T> {
        let output = self.run(&["api", "call", method, "-"], Some(&args.to_string()))?;
        Self::parse(method, &output)
    }

    fn block_call<T: DeserializeOwned>(
        &self,
        block: &BlockId,
        method: &str,
        args: Value,
    ) -> CallResult<T> {
        let output = self.run(
            &["block", "call", "-b", block.as_str(), method, "-"],
            Some(&args.to_string()),
        )?;
        Self::parse(method, &output)
    }

    fn whoami(&self) -> Result<Whoami> {
        let output = self.run(&["whoami", "--json"], None)?;
        Ok(Self::parse("whoami", &output)?)
    }

    fn sessions(&self) -> Result<Vec<SessionSummary>> {
        Ok(self.api::<SessionList>("session.list", json!({}))?.sessions)
    }

    fn view(&self, session: &SessionId) -> CallResult<SessionView> {
        self.api("session.view", json!({ "session_id": session.as_str() }))
    }

    fn views(&self) -> Result<Vec<SessionView>> {
        let sessions = self.sessions()?;
        let views = parallel_map(&sessions, |session| found(self.view(&session.session_id)));
        Ok(views
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    fn sessions_labeled(&self, label: &str) -> Result<Vec<SessionId>> {
        Ok(self
            .sessions()?
            .into_iter()
            .filter(|session| session.label == label)
            .map(|session| session.session_id)
            .collect())
    }

    fn env_session() -> Option<SessionId> {
        std::env::var("REX_SESSION")
            .ok()
            .filter(|id| id.starts_with("session:"))
            .map(SessionId)
    }

    fn env_block() -> Option<BlockId> {
        std::env::var("REX_BLOCK")
            .ok()
            .and_then(|id| BlockId::parse(&id).ok())
    }

    fn current_view(&self) -> Result<Option<SessionView>> {
        match Self::env_session() {
            Some(session) => found(self.view(&session)),
            None => Ok(None),
        }
    }

    fn current_or_only_view(&self) -> Result<SessionView> {
        if let Some(view) = self.current_view()? {
            return Ok(view);
        }
        let mut sessions = self.sessions()?;
        if sessions.len() == 1 {
            let session = sessions.remove(0).session_id;
            return Ok(self.view(&session)?);
        }
        Err(anyhow!(
            "Cannot tell which Rex session to use. Run workmux inside a Rex pane \
             or set REX_SESSION."
        ))
    }

    fn resolve(&self, kind: &str, id: &str) -> Result<Option<ResolvedTarget>> {
        let resolution: Resolution = self.api(
            "target.resolve",
            json!({ "kinds": [kind], "queries": [id] }),
        )?;
        Ok(resolution
            .results
            .into_iter()
            .next()
            .filter(|result| result.status == "resolved")
            .and_then(|result| result.target))
    }

    /// The session that holds `block`. Blocks never change sessions, so the
    /// answer is cached for the life of the backend.
    fn session_of(&self, block: &BlockId) -> Result<Option<SessionId>> {
        if let Some(session) = self.block_sessions.lock().unwrap().get(block) {
            return Ok(Some(session.clone()));
        }
        let Some(target) = self.resolve("block", block.as_str())? else {
            return Ok(None);
        };
        self.block_sessions
            .lock()
            .unwrap()
            .insert(block.clone(), target.session_id.clone());
        Ok(Some(target.session_id))
    }

    fn require_session_of(&self, block: &BlockId) -> Result<SessionId> {
        self.session_of(block)?
            .ok_or_else(|| anyhow!("Rex block {} not found", block.as_str()))
    }

    fn block_session_call(&self, method: &str, block: &BlockId, mut args: Value) -> Result<Value> {
        let session = self.require_session_of(block)?;
        args["session_id"] = json!(session.as_str());
        Ok(self.api(method, args)?)
    }

    fn cache_first_block(&self, blocks: Vec<BlockId>, session: SessionId) -> Result<BlockId> {
        let block = blocks
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Rex created no block"))?;
        self.block_sessions
            .lock()
            .unwrap()
            .insert(block.clone(), session);
        Ok(block)
    }

    /// Rex opens new windows at the end of the tab bar.
    fn move_window_after(
        &self,
        view: &SessionView,
        window: &WindowId,
        after: &WindowId,
    ) -> Result<()> {
        let Some(next) = view.window_after(after) else {
            return Ok(());
        };
        self.api::<Value>(
            "session.move_window",
            json!({
                "session_id": view.session_id.as_str(),
                "window_id": window.as_str(),
                "before_window_id": next.as_str(),
            }),
        )
        .context("Failed to place the new Rex window")?;
        Ok(())
    }

    fn view_of_window(&self, window: &WindowId) -> Result<Option<SessionView>> {
        if let Some(view) = self.current_view()?
            && view.window(window).is_some()
        {
            return Ok(Some(view));
        }
        match self.resolve("window", window.as_str())? {
            Some(target) => found(self.view(&target.session_id)),
            None => Ok(None),
        }
    }

    fn target_windows(&self, target: &WindowTarget) -> Result<Vec<(SessionId, WindowId)>> {
        if let Some(id) = &target.window_id {
            let window = WindowId(id.clone());
            return Ok(self
                .view_of_window(&window)?
                .map(|view| vec![(view.session_id, window)])
                .unwrap_or_default());
        }
        let views = match target.parent_session() {
            Some(parent) => self
                .sessions_labeled(parent)?
                .iter()
                .filter_map(|session| found(self.view(session)).transpose())
                .collect::<Result<Vec<_>>>()?,
            None => vec![self.current_or_only_view()?],
        };
        Ok(views
            .iter()
            .flat_map(|view| {
                view.windows_labeled(&target.full_name)
                    .map(|window| (view.session_id.clone(), window.window_id.clone()))
            })
            .collect())
    }

    fn named_windows(&self, full_name: &str) -> Result<Vec<(SessionId, WindowId)>> {
        self.target_windows(&WindowTarget::new(full_name.to_string(), None))
    }

    fn close_window(&self, session: &SessionId, window: &WindowId) -> Result<()> {
        found(self.api::<Value>(
            "session.close_window",
            json!({ "session_id": session.as_str(), "window_id": window.as_str() }),
        ))?;
        Ok(())
    }

    fn focus_window(&self, session: &SessionId, window: &WindowId) -> Result<()> {
        self.api::<Value>(
            "session.focus_window",
            json!({ "session_id": session.as_str(), "window_id": window.as_str() }),
        )?;
        Ok(())
    }

    fn destroy_session(&self, session: &SessionId) -> Result<()> {
        found(self.api::<Value>("session.destroy", json!({ "session_id": session.as_str() })))?;
        Ok(())
    }

    fn shell_api_cmd(&self, method: &str, args: Value) -> String {
        self.shell_rex_cmd(&["api", "call", method, &args.to_string()])
    }

    fn shell_rex_cmd(&self, args: &[&str]) -> String {
        let mut words = vec!["env".to_string()];
        for var in Self::INHERITED_SCOPE_VARS {
            words.extend(["-u".to_string(), var.to_string()]);
        }
        words.push(shell_quote(&Self::program()));
        words.extend(
            self.global_args()
                .iter()
                .chain(args)
                .map(|arg| shell_quote(arg)),
        );
        format!("{} >/dev/null 2>&1 || true", words.join(" "))
    }

    fn shell_close_windows_cmd(&self, windows: &[(SessionId, WindowId)]) -> String {
        windows
            .iter()
            .map(|(session, window)| {
                self.shell_api_cmd(
                    "session.close_window",
                    json!({ "session_id": session.as_str(), "window_id": window.as_str() }),
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn shell_destroy_cmd(&self, session: &SessionId) -> String {
        self.shell_api_cmd("session.destroy", json!({ "session_id": session.as_str() }))
    }

    fn schedule(delay: Duration, commands: &str) -> Result<()> {
        if commands.is_empty() {
            return Ok(());
        }
        util::run_detached_sh_c(&format!("sleep {:.3}; {commands}", delay.as_secs_f64()))
    }

    fn live_pane(
        &self,
        session: &SessionView,
        window: &WindowView,
        block: &BlockId,
    ) -> Result<Option<LivePaneInfo>> {
        let Some(process) = found(self.block_call::<ProcessReport>(block, "process", json!({})))?
        else {
            return Ok(None);
        };
        let title = found(self.block_call::<TitleReport>(block, "title", json!({})))?
            .map(|report| report.title)
            .unwrap_or_default();
        Ok(Some(LivePaneInfo {
            pid: process.child.as_ref().map(|child| child.pid),
            current_command: process.foreground.as_ref().map(|fg| fg.name.clone()),
            working_dir: process.working_dir(),
            title: (!title.is_empty()).then_some(title),
            session: Some(session.label.clone()),
            window: Some(window.label.clone()),
            session_id: Some(session.session_id.as_str().to_string()),
            window_id: Some(window.window_id.as_str().to_string()),
            window_index: None,
        }))
    }

    fn app_attached(&self) -> Result<bool> {
        let clients: ClientList = self.api("client.list", json!({}))?;
        Ok(clients
            .clients
            .iter()
            .any(|client| client.info.as_ref().is_some_and(|info| info.kind == "app")))
    }

    fn select_session(&self, session: &SessionId) -> Result<()> {
        if !self.app_attached()? {
            return Ok(());
        }
        let selector = format!("session_id={}", session.as_str());
        self.run(&["do", "session.select", &selector], None)?;
        Ok(())
    }

    fn shell_select_session_cmd(&self, session: &SessionId) -> String {
        let selector = format!("session_id={}", session.as_str());
        self.shell_rex_cmd(&["do", "session.select", &selector])
    }

    fn first_session_labeled(&self, label: &str) -> Result<SessionId> {
        self.sessions_labeled(label)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Rex session '{label}' not found"))
    }
}

impl Multiplexer for RexBackend {
    fn name(&self) -> &'static str {
        "rex"
    }

    fn is_running(&self) -> Result<bool> {
        Ok(self.run(&["api", "call", "session.list"], None).is_ok())
    }

    fn current_window_id(&self) -> Result<Option<String>> {
        let (Some(view), Some(block)) = (self.current_view()?, Self::env_block()) else {
            return Ok(None);
        };
        Ok(view
            .window_of(&block)
            .map(|window| window.window_id.as_str().to_string()))
    }

    fn rightmost_window_id(&self) -> Result<Option<String>> {
        Ok(self.current_view()?.and_then(|view| {
            view.windows
                .last()
                .map(|w| w.window_id.as_str().to_string())
        }))
    }

    fn current_session_id(&self) -> Result<Option<String>> {
        Ok(Self::env_session().map(|session| session.0))
    }

    fn shell_close_window_by_id_guard_cmd(&self, id: &str) -> Result<String> {
        let window = WindowId(id.to_string());
        Ok(match self.view_of_window(&window)? {
            Some(view) => self.shell_close_windows_cmd(&[(view.session_id, window)]),
            None => "true".to_string(),
        })
    }

    fn shell_close_session_by_id_guard_cmd(
        &self,
        id: &str,
        _preferred_session: Option<&str>,
    ) -> Result<String> {
        Ok(self.shell_destroy_cmd(&SessionId(id.to_string())))
    }

    fn current_pane_id(&self) -> Option<String> {
        Self::env_block().map(|block| block.0)
    }

    fn active_pane_id(&self) -> Option<String> {
        let view = self.current_or_only_view().ok()?;
        view.active_window()?
            .focused_block_id
            .as_ref()
            .map(|block| block.as_str().to_string())
    }

    fn get_client_active_pane_path(&self) -> Result<PathBuf> {
        let block = match Self::env_block() {
            Some(block) => block,
            None => BlockId::parse(
                &self
                    .active_pane_id()
                    .ok_or_else(|| anyhow!("No active Rex pane found"))?,
            )?,
        };
        let process: ProcessReport = self.block_call(&block, "process", json!({}))?;
        let path = process.working_dir();
        if path.as_os_str().is_empty() {
            return Err(anyhow!("Rex reported no working directory for the pane"));
        }
        Ok(path)
    }

    fn create_window(&self, params: CreateWindowParams) -> Result<String> {
        let full_name = util::prefixed(params.prefix, params.name);
        let after = params.after_window.map(|id| WindowId(id.to_string()));
        let view = match &after {
            Some(window) => self
                .view_of_window(window)?
                .ok_or_else(|| anyhow!("Rex window {} not found", window.as_str()))?,
            None => self.current_or_only_view()?,
        };

        let created: CreatedWindow = self
            .api(
                "session.new_window",
                json!({
                    "session_id": view.session_id.as_str(),
                    "window_label": full_name,
                    "focus": false,
                    "layout": self.block_layout(params.cwd, None)?,
                }),
            )
            .context("Failed to create Rex window")?;

        if let Some(after) = &after {
            self.move_window_after(&view, &created.window_id, after)?;
        }

        Ok(self
            .cache_first_block(created.block_ids, view.session_id)?
            .0)
    }

    fn create_session(&self, params: CreateSessionParams) -> Result<String> {
        let full_name = util::prefixed(params.prefix, params.name);
        let mut window = json!({ "layout": self.block_layout(params.cwd, None)? });
        if let Some(label) = params.initial_window_name {
            window["window_label"] = json!(label);
        }
        let created: CreatedSession = self
            .api(
                "session.create",
                json!({ "label": full_name, "initial_windows": [window] }),
            )
            .context("Failed to create Rex session")?;
        created
            .initial_windows
            .into_iter()
            .next()
            .and_then(|window| window.block_ids.into_iter().next())
            .map(|block| block.0)
            .ok_or_else(|| anyhow!("Rex created a session without a block"))
    }

    fn create_window_in_session(&self, params: CreateWindowInSessionParams) -> Result<String> {
        let session = self.first_session_labeled(params.session_name)?;
        let mut args = json!({
            "session_id": session.as_str(),
            "focus": false,
            "layout": self.block_layout(params.cwd, None)?,
        });
        if let Some(label) = params.name {
            args["window_label"] = json!(label);
        }
        let created: CreatedWindow = self
            .api("session.new_window", args)
            .context("Failed to create window in Rex session")?;
        created
            .block_ids
            .into_iter()
            .next()
            .map(|block| block.0)
            .ok_or_else(|| anyhow!("Rex created a window without a block"))
    }

    fn switch_to_session(&self, prefix: &str, name: &str) -> Result<()> {
        let session = self.first_session_labeled(&util::prefixed(prefix, name))?;
        self.select_session(&session)
    }

    fn session_exists(&self, full_name: &str) -> Result<bool> {
        Ok(!self.sessions_labeled(full_name)?.is_empty())
    }

    fn kill_session(&self, full_name: &str) -> Result<()> {
        for session in self.sessions_labeled(full_name)? {
            self.destroy_session(&session)?;
        }
        Ok(())
    }

    fn kill_window(&self, full_name: &str) -> Result<()> {
        for (session, window) in self.named_windows(full_name)? {
            self.close_window(&session, &window)?;
        }
        Ok(())
    }

    fn kill_window_target(&self, target: &WindowTarget) -> Result<()> {
        for (session, window) in self.target_windows(target)? {
            self.close_window(&session, &window)?;
        }
        Ok(())
    }

    fn rename_window(&self, old_full_name: &str, new_full_name: &str) -> Result<()> {
        let windows = self.named_windows(old_full_name)?;
        let (session, window) = windows
            .first()
            .ok_or_else(|| anyhow!("Window '{old_full_name}' not found"))?;
        self.api::<Value>(
            "session.set_window_label",
            json!({
                "session_id": session.as_str(),
                "window_id": window.as_str(),
                "label": new_full_name,
            }),
        )?;
        Ok(())
    }

    fn rename_window_at_pane(&self, pane_id: &str, new_name: &str) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        let target = self
            .resolve("block", block.as_str())?
            .ok_or_else(|| anyhow!("Rex block {pane_id} not found"))?;
        let window = target
            .window_id
            .ok_or_else(|| anyhow!("Rex block {pane_id} is not in a window"))?;
        self.api::<Value>(
            "session.set_window_label",
            json!({
                "session_id": target.session_id.as_str(),
                "window_id": window.as_str(),
                "label": new_name,
            }),
        )?;
        Ok(())
    }

    fn rename_session(&self, old_full_name: &str, new_full_name: &str) -> Result<()> {
        let session = self.first_session_labeled(old_full_name)?;
        self.api::<Value>(
            "session.set_label",
            json!({ "session_id": session.as_str(), "label": new_full_name }),
        )?;
        Ok(())
    }

    fn schedule_window_close(&self, full_name: &str, delay: Duration) -> Result<()> {
        let windows = self.named_windows(full_name)?;
        Self::schedule(delay, &self.shell_close_windows_cmd(&windows))
    }

    fn schedule_window_target_close(&self, target: &WindowTarget, delay: Duration) -> Result<()> {
        let windows = self.target_windows(target)?;
        Self::schedule(delay, &self.shell_close_windows_cmd(&windows))
    }

    fn schedule_session_close(&self, full_name: &str, delay: Duration) -> Result<()> {
        let commands = self
            .sessions_labeled(full_name)?
            .iter()
            .map(|session| self.shell_destroy_cmd(session))
            .collect::<Vec<_>>()
            .join("; ");
        Self::schedule(delay, &commands)
    }

    fn run_deferred_script(&self, script: &str) -> Result<()> {
        util::run_detached_sh_c(script)
    }

    fn shell_select_window_cmd(&self, full_name: &str) -> Result<String> {
        let (session, window) = self
            .named_windows(full_name)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Window '{full_name}' not found"))?;
        Ok(self.shell_api_cmd(
            "session.focus_window",
            json!({ "session_id": session.as_str(), "window_id": window.as_str() }),
        ))
    }

    fn shell_kill_window_cmd(&self, full_name: &str) -> Result<String> {
        let windows = self.named_windows(full_name)?;
        if windows.is_empty() {
            return Err(anyhow!("Window '{full_name}' not found"));
        }
        Ok(self.shell_close_windows_cmd(&windows))
    }

    fn shell_kill_window_target_cmd(&self, target: &WindowTarget) -> Result<String> {
        let windows = self.target_windows(target)?;
        if windows.is_empty() {
            return Err(anyhow!("Window '{}' not found", target.full_name));
        }
        Ok(self.shell_close_windows_cmd(&windows))
    }

    fn shell_switch_session_cmd(&self, full_name: &str) -> Result<String> {
        Ok(self.shell_select_session_cmd(&self.first_session_labeled(full_name)?))
    }

    fn shell_kill_session_cmd(&self, full_name: &str) -> Result<String> {
        Ok(self.shell_destroy_cmd(&self.first_session_labeled(full_name)?))
    }

    fn select_window(&self, prefix: &str, name: &str) -> Result<()> {
        let full_name = util::prefixed(prefix, name);
        let (session, window) = self
            .named_windows(&full_name)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Window '{full_name}' not found"))?;
        self.focus_window(&session, &window)
    }

    fn select_window_target(&self, target: &WindowTarget) -> Result<()> {
        let (session, window) = self
            .target_windows(target)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Window '{}' not found", target.full_name))?;
        self.focus_window(&session, &window)
    }

    fn window_target_exists(&self, target: &WindowTarget) -> Result<bool> {
        Ok(!self.target_windows(target)?.is_empty())
    }

    fn current_window_name(&self) -> Result<Option<String>> {
        let (Some(view), Some(block)) = (self.current_view()?, Self::env_block()) else {
            return Ok(None);
        };
        Ok(view.window_of(&block).map(|window| window.label.clone()))
    }

    fn get_all_window_names(&self) -> Result<HashSet<String>> {
        let views = match self.current_view()? {
            Some(view) => vec![view],
            None => self.views()?,
        };
        Ok(views
            .iter()
            .flat_map(|view| view.windows.iter().map(|w| w.label.clone()))
            .collect())
    }

    fn get_window_names_in_session(&self, session_name: &str) -> Result<HashSet<String>> {
        let mut names = HashSet::new();
        for session in self.sessions_labeled(session_name)? {
            if let Some(view) = found(self.view(&session))? {
                names.extend(view.windows.into_iter().map(|w| w.label));
            }
        }
        Ok(names)
    }

    fn get_all_windows_with_sessions(&self) -> Result<HashSet<(String, String)>> {
        Ok(self
            .views()?
            .into_iter()
            .flat_map(|view| {
                let session = view.label;
                view.windows
                    .into_iter()
                    .map(move |window| (window.label, session.clone()))
            })
            .collect())
    }

    fn get_all_session_names(&self) -> Result<HashSet<String>> {
        Ok(self
            .sessions()?
            .into_iter()
            .map(|session| session.label)
            .collect())
    }

    fn wait_until_session_closed(&self, full_session_name: &str) -> Result<()> {
        println!("Waiting for session '{}' to close...", full_session_name);
        while self.is_running()? && self.session_exists(full_session_name)? {
            thread::sleep(Duration::from_millis(500));
        }
        Ok(())
    }

    fn select_pane(&self, pane_id: &str) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        self.block_session_call(
            "session.focus_block",
            &block,
            json!({ "block_id": block.as_str() }),
        )?;
        Ok(())
    }

    fn zoom_pane(&self, pane_id: &str) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        self.block_session_call(
            "session.zoom_block",
            &block,
            json!({ "block_id": block.as_str(), "zoom": true }),
        )?;
        Ok(())
    }

    fn switch_to_pane(&self, pane_id: &str, _window_hint: Option<&str>) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        let session = self.require_session_of(&block)?;
        if Self::env_session().as_ref() != Some(&session) {
            self.select_session(&session)?;
        }
        self.select_pane(pane_id)
    }

    fn kill_pane(&self, pane_id: &str) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        let Some(session) = self.session_of(&block)? else {
            return Ok(());
        };
        found(self.api::<Value>(
            "block.close",
            json!({ "session_id": session.as_str(), "block_id": block.as_str() }),
        ))?;
        Ok(())
    }

    fn respawn_pane(&self, pane_id: &str, cwd: &Path, cmd: Option<&str>) -> Result<String> {
        // Rex cannot restart a block's program. Start the replacement beside
        // the old block, then close the old one so the new block takes its
        // whole space in the layout.
        let old = BlockId::parse(pane_id)?;
        let session = self.require_session_of(&old)?;
        let split: CreatedSplit = self
            .api(
                "session.new_split",
                json!({
                    "session_id": session.as_str(),
                    "anchor_id": old.as_str(),
                    "direction": "horizontal",
                    "side": "after",
                    "layout": self.block_layout(cwd, cmd)?,
                }),
            )
            .context("Failed to start the replacement Rex block")?;
        let new = self.cache_first_block(split.block_ids, session.clone())?;
        found(self.api::<Value>(
            "block.close",
            json!({ "session_id": session.as_str(), "block_id": old.as_str() }),
        ))?;
        Ok(new.0)
    }

    fn set_pane_name(&self, pane_id: &str, name: &str) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        self.block_session_call(
            "session.set_block_label",
            &block,
            json!({ "block_id": block.as_str(), "label": name }),
        )?;
        Ok(())
    }

    fn capture_pane(&self, pane_id: &str, lines: u16) -> Option<String> {
        let block = BlockId::parse(pane_id).ok()?;
        let report: FormatReport = self
            .block_call(&block, "format", json!({ "format": "text", "trim": true }))
            .ok()?;
        Some(util::tail_lines(&report.content, lines))
    }

    fn send_text_fragment(&self, pane_id: &str, text: &str) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        self.block_call::<Value>(&block, "write", json!({ "data": text }))
            .context("Failed to send text to Rex pane")?;
        Ok(())
    }

    fn send_enter(&self, pane_id: &str) -> Result<()> {
        self.send_text_fragment(pane_id, "\r")
    }

    fn send_key(&self, pane_id: &str, key: &str) -> Result<()> {
        match key_input(key) {
            KeyInput::Text(text) => self.send_text_fragment(pane_id, &text),
            KeyInput::Key(name) => {
                let block = BlockId::parse(pane_id)?;
                self.run(&["send-key", "-b", block.as_str(), &name], None)
                    .context("Failed to send key to Rex pane")?;
                Ok(())
            }
        }
    }

    fn paste_text(&self, pane_id: &str, content: &str) -> Result<()> {
        // Unlike `write`, a paste event is bracketed when the program enabled
        // bracketed paste.
        let block = BlockId::parse(pane_id)?;
        self.block_session_call(
            "block.event",
            &block,
            json!({
                "block_id": block.as_str(),
                "event": { "type": "paste", "payload": { "text": content } },
            }),
        )
        .context("Failed to paste into Rex pane")?;
        Ok(())
    }

    fn set_status(&self, _pane_id: &str, _icon: &str, _auto_clear_on_focus: bool) -> Result<()> {
        Ok(())
    }

    fn clear_status(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn ensure_status_format(&self, _pane_id: &str) -> Result<()> {
        Ok(())
    }

    fn split_pane(
        &self,
        target_pane_id: &str,
        direction: &SplitDirection,
        cwd: &Path,
        size: Option<u16>,
        percentage: Option<u8>,
        command: Option<&str>,
    ) -> Result<String> {
        let direction = match direction {
            SplitDirection::Horizontal => "horizontal",
            SplitDirection::Vertical => "vertical",
            SplitDirection::Stacked => {
                return Err(anyhow!(
                    "split: stacked is only supported by the Zellij backend"
                ));
            }
        };
        let anchor = BlockId::parse(target_pane_id)?;
        let session = self.require_session_of(&anchor)?;

        let total = match (percentage, size) {
            (None, Some(_)) => {
                let report: SizeReport = self.block_call(&anchor, "size", json!({}))?;
                Some(if direction == "horizontal" {
                    report.columns
                } else {
                    report.rows
                })
            }
            _ => None,
        };

        let split: CreatedSplit = self
            .api(
                "session.new_split",
                json!({
                    "session_id": session.as_str(),
                    "anchor_id": anchor.as_str(),
                    "direction": direction,
                    "side": "after",
                    "ratio": anchor_ratio(percentage, size, total),
                    "layout": self.block_layout(cwd, command)?,
                }),
            )
            .context("Failed to split Rex pane")?;
        Ok(self.cache_first_block(split.block_ids, session)?.0)
    }

    fn current_session(&self) -> Option<String> {
        self.current_view().ok().flatten().map(|view| view.label)
    }

    fn get_all_window_names_all_sessions(&self) -> Result<HashSet<String>> {
        Ok(self
            .views()?
            .into_iter()
            .flat_map(|view| view.windows.into_iter().map(|window| window.label))
            .collect())
    }

    fn instance_id(&self) -> String {
        self.resolve_instance_id()
            .unwrap_or_else(|_| "default".to_string())
    }

    fn resolve_instance_id(&self) -> Result<String> {
        if let Some(server) = &self.server {
            return Ok(server.clone());
        }
        if let Some(endpoint) = self.endpoint.get() {
            return Ok(endpoint.clone());
        }
        let endpoint = self
            .whoami()
            .context("failed to resolve the Rex server address")?
            .endpoint;
        Ok(self.endpoint.get_or_init(|| endpoint).clone())
    }

    fn get_live_pane_info(&self, pane_id: &str) -> Result<Option<LivePaneInfo>> {
        let Ok(block) = BlockId::parse(pane_id) else {
            return Ok(None);
        };
        let Some(target) = self.resolve("block", block.as_str())? else {
            return Ok(None);
        };
        let Some(view) = found(self.view(&target.session_id))? else {
            return Ok(None);
        };
        let Some(window) = view.window_of(&block) else {
            return Ok(None);
        };
        self.live_pane(&view, window, &block)
    }

    fn get_all_live_pane_info(&self) -> Result<HashMap<String, LivePaneInfo>> {
        let views = self.views()?;
        let blocks: Vec<(&SessionView, &WindowView, &BlockId)> = views
            .iter()
            .flat_map(|view| {
                view.windows
                    .iter()
                    .flat_map(move |window| window.block_ids().map(move |b| (view, window, b)))
            })
            .collect();
        let panes = parallel_map(&blocks, |(view, window, block)| {
            self.live_pane(view, window, block)
                .map(|pane| pane.map(|pane| (block.as_str().to_string(), pane)))
        });
        Ok(panes
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    fn server_boot_id(&self) -> Result<Option<String>> {
        Ok(Some(self.whoami()?.boot_id))
    }

    fn sidebar(&self) -> Option<&dyn super::SidebarHost> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: &str = include_str!("testdata/rex_session_view.json");
    const PROCESS: &str = include_str!("testdata/rex_process.json");
    const SESSION_LIST: &str = include_str!("testdata/rex_session_list.json");
    const RESOLVE: &str = include_str!("testdata/rex_target_resolve.json");

    fn view() -> SessionView {
        serde_json::from_str(VIEW).unwrap()
    }

    #[test]
    fn session_view_keeps_tab_order_and_blocks_per_window() {
        let view = view();
        let labels: Vec<_> = view.windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["main", "wm-feat-a", "logs"]);
        let editor = &view.windows[1];
        assert_eq!(editor.block_ids().count(), 2);
        assert_eq!(
            view.active_window().unwrap().window_id,
            view.windows[0].window_id
        );
    }

    #[test]
    fn session_view_finds_a_blocks_window_and_its_successor() {
        let view = view();
        let second_block = view.windows[1].block_ids().nth(1).unwrap().clone();
        let window = view.window_of(&second_block).unwrap();
        assert_eq!(window.label, "wm-feat-a");
        assert_eq!(
            view.window_after(&view.windows[0].window_id),
            Some(&view.windows[1].window_id)
        );
        assert_eq!(view.window_after(&view.windows[2].window_id), None);
        assert_eq!(view.windows_labeled("wm-feat-a").count(), 1);
    }

    #[test]
    fn process_report_maps_child_pid_foreground_command_and_cwd() {
        let report: ProcessReport = serde_json::from_str(PROCESS).unwrap();
        assert_eq!(report.child.as_ref().unwrap().pid, 85152);
        assert_eq!(report.foreground.as_ref().unwrap().name, "sleep");
        assert_eq!(report.working_dir(), PathBuf::from("/private/tmp"));
    }

    #[test]
    fn process_report_falls_back_to_ancestor_cwd() {
        let report: ProcessReport = serde_json::from_str(
            r#"{"last_exit":null,
                "child":{"pid":7,"name":"sh"},
                "foreground":{"pid":9,"name":"claude"},
                "ancestors":[{"pid":8,"name":"zsh","cwd":"/work"}]}"#,
        )
        .unwrap();
        assert_eq!(report.working_dir(), PathBuf::from("/work"));
    }

    #[test]
    fn session_list_and_resolution_parse() {
        let list: SessionList = serde_json::from_str(SESSION_LIST).unwrap();
        assert_eq!(list.sessions.len(), 3);
        assert_eq!(list.sessions[2].label, "fixture");

        let resolution: Resolution = serde_json::from_str(RESOLVE).unwrap();
        assert_eq!(resolution.results[0].status, "resolved");
        assert!(
            resolution.results[0]
                .target
                .as_ref()
                .unwrap()
                .session_id
                .as_str()
                .starts_with("session:")
        );
        assert_eq!(resolution.results[1].status, "not_found");
        assert!(resolution.results[1].target.is_none());
    }

    #[test]
    fn not_found_errors_are_told_apart_from_failures() {
        let missing = CallError::from_stderr(
            &["block", "call"],
            "Error: stream response \"control-2\" failed: block not found: block:x \
             (status=404 code=not_found)",
        );
        assert!(matches!(missing, CallError::NotFound(_)));
        let session = CallError::from_stderr(
            &["api", "call"],
            "Error: ... session session:x not found (status=404 code=session_not_found)",
        );
        assert!(matches!(session, CallError::NotFound(_)));
        let down = CallError::from_stderr(
            &["api", "call"],
            "Error: no Rex server is running at /tmp/x.sock, and autostart is disabled",
        );
        assert!(matches!(down, CallError::Failed(_)));
        assert!(found::<()>(Err(missing)).unwrap().is_none());
        assert!(found::<()>(Err(down)).is_err());
    }

    #[test]
    fn pane_ids_must_be_full_block_ids() {
        assert!(BlockId::parse("block:01a19e59-43d8-72fd-860c-3a1d6f183cc3").is_ok());
        assert!(BlockId::parse("%3").is_err());
        assert!(BlockId::parse("3cc3").is_err());
    }

    #[test]
    fn tmux_key_names_map_to_rex_keys() {
        assert_eq!(key_input("q"), KeyInput::Text("q".into()));
        assert_eq!(key_input("Enter"), KeyInput::Key("enter".into()));
        assert_eq!(key_input("Escape"), KeyInput::Key("escape".into()));
        assert_eq!(key_input("BSpace"), KeyInput::Key("backspace".into()));
        assert_eq!(key_input("C-c"), KeyInput::Key("ctrl+c".into()));
        assert_eq!(key_input("M-x"), KeyInput::Key("alt+x".into()));
        assert_eq!(key_input("Up"), KeyInput::Key("up".into()));
    }

    #[test]
    fn anchor_ratio_leaves_the_new_block_its_share() {
        assert!((anchor_ratio(Some(30), None, None) - 0.7).abs() < 1e-9);
        assert!((anchor_ratio(None, Some(20), Some(80)) - 0.75).abs() < 1e-9);
        assert!((anchor_ratio(None, None, None) - 0.5).abs() < 1e-9);
        assert!((anchor_ratio(Some(100), None, None) - 0.05).abs() < 1e-9);
    }

    #[test]
    fn blocks_skip_the_login_wrapper() {
        let block = shell_block(Path::new("/work"), &["sh", "-c", "echo ready > /p"]);
        assert_eq!(block["block"]["flavor"], SHELL_FLAVOR);
        assert_eq!(
            block["block"]["options"],
            json!({
                "cwd": "/work",
                "shell": "none",
                "login": false,
                "command": ["sh", "-c", "echo ready > /p"],
            })
        );
    }

    #[test]
    fn deferred_commands_quote_arguments_and_target_the_server() {
        let rex = RexBackend::for_server("unix:///tmp/a b.sock");
        let command = rex.shell_api_cmd(
            "session.close_window",
            json!({ "session_id": "session:1", "window_id": "window:2" }),
        );
        let words = shlex::split(&command).unwrap();
        assert_eq!(&words[..5], ["env", "-u", "REX_SESSION", "-u", "REX_BLOCK"]);
        assert_eq!(
            &words[6..13],
            [
                "--autostart=false",
                "-S",
                "unix:///tmp/a b.sock",
                "api",
                "call",
                "session.close_window",
                r#"{"session_id":"session:1","window_id":"window:2"}"#,
            ]
        );
        assert!(command.ends_with(">/dev/null 2>&1 || true"));
    }

    #[test]
    fn instance_is_the_configured_server_address() {
        let rex = RexBackend::for_server("unix:///tmp/wr.sock");
        assert_eq!(rex.resolve_instance_id().unwrap(), "unix:///tmp/wr.sock");
    }
}
