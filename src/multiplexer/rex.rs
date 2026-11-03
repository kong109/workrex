use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::bin_name::BIN;
use crate::config::SplitDirection;

use super::Multiplexer;
use super::agent::shell_quote;
use super::types::*;
use super::util;

const SHELL_FLAVOR: &str = "com.superlogical.terminal.shell";
const TERMINAL_CREATOR: &str = "com.superlogical.terminal";

/// API revisions this backend was verified against: the macOS server
/// reports 8 and the Linux server 9, and 9 only adds methods.
const SUPPORTED_API_REVISIONS: RangeInclusive<u32> = 8..=9;

macro_rules! rex_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
rex_id!(ClientId);

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
    kind: String,
    #[serde(default)]
    blocks: Vec<BlockView>,
}

#[derive(Debug, Deserialize)]
struct BlockView {
    block_id: BlockId,
    #[serde(default)]
    creator_name: String,
}

impl BlockView {
    fn is_terminal(&self) -> bool {
        self.creator_name == TERMINAL_CREATOR
    }
}

impl WindowView {
    fn block_ids(&self) -> impl Iterator<Item = &BlockId> {
        self.layers
            .iter()
            .flat_map(|layer| layer.blocks.iter().map(|block| &block.block_id))
    }

    /// Agent panes: terminals in the tiled layout. Floating layers hold
    /// workmux's own dashboard and sidebar.
    fn terminal_block_ids(&self) -> impl Iterator<Item = &BlockId> {
        self.layers
            .iter()
            .filter(|layer| layer.kind == "tiled")
            .flat_map(|layer| layer.blocks.iter())
            .filter(|block| block.is_terminal())
            .map(|block| &block.block_id)
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

    fn terminal_blocks(&self) -> impl Iterator<Item = (&WindowView, &BlockId)> {
        self.windows.iter().flat_map(|window| {
            window
                .terminal_block_ids()
                .map(move |block| (window, block))
        })
    }
}

/// `server.version`. The schema leaves a revision out when it is 0.
#[derive(Debug, Deserialize)]
struct ServerVersion {
    rex_version: String,
    #[serde(default)]
    api_revision: u32,
    #[serde(default)]
    api_min_revision: u32,
}

impl ServerVersion {
    fn check(&self) -> Verdict {
        let (ours, theirs) = (
            &SUPPORTED_API_REVISIONS,
            self.api_min_revision..=self.api_revision,
        );
        if theirs.start() <= ours.end() && ours.start() <= theirs.end() {
            return Ok(());
        }
        Err(format!(
            "unsupported Rex {} (API revisions {} to {}), {BIN} supports Rex API revisions {} to {}",
            self.rex_version,
            theirs.start(),
            theirs.end(),
            ours.start(),
            ours.end(),
        ))
    }
}

/// Whether a server's API is supported, or the error naming why not.
type Verdict = std::result::Result<(), String>;

/// One verdict per server for the life of the process, keyed on the
/// canonical address so every spelling of one socket shares it.
static SERVER_VERDICTS: LazyLock<Mutex<HashMap<Option<String>, Verdict>>> =
    LazyLock::new(Default::default);

/// How long a command reuses a session list or view it already fetched. A
/// loop that polls for a change made outside workmux sees it within this time.
/// `WORKMUX_REX_READ_REUSE_MS` overrides it, so a test can count calls that
/// load cannot change.
const READ_REUSE: Duration = Duration::from_millis(250);

const READ_ONLY_API: [&str; 5] = [
    "session.list",
    "session.view",
    "target.resolve",
    "server.version",
    "client.list",
];
const READ_ONLY_BLOCK: [&str; 4] = ["process", "title", "size", "format"];

/// Calls that leave the server as it was. Any other call drops reused reads.
fn changes_nothing(args: &[&str]) -> bool {
    match args {
        ["api", "call", method, ..] => READ_ONLY_API.contains(method),
        ["block", "call", "-b", _, method, ..] => READ_ONLY_BLOCK.contains(method),
        ["whoami", ..] => true,
        _ => false,
    }
}

/// Session reads a command can reuse. `writes` counts the writes that have
/// returned and each forget, so a read that overlapped one is not stored.
#[derive(Debug, Default)]
struct Reads {
    writes: u64,
    answers: HashMap<String, (Instant, String)>,
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
    last_exit: Option<ExitReport>,
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

#[derive(Debug, Deserialize)]
struct ExitReport {}

impl ProcessReport {
    /// A block outlives its program, so an exited agent still has a block.
    fn has_exited(&self) -> bool {
        self.last_exit.is_some()
    }

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
    #[serde(default)]
    owner: Option<ClientId>,
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
    client_id: ClientId,
    #[serde(default)]
    info: Option<ClientInfo>,
    #[serde(default)]
    connection_state: String,
    #[serde(default)]
    session_ids: Option<Vec<SessionId>>,
}

impl ClientList {
    fn is_app_attached_to(&self, client: &ClientId, session: &SessionId) -> bool {
        self.clients.iter().any(|entry| {
            &entry.client_id == client
                && entry.connection_state != "lost"
                && entry.info.as_ref().is_some_and(|info| info.kind == "app")
                && entry.session_ids.iter().flatten().any(|id| id == session)
        })
    }
}

/// Whether `owner`, the client that holds the caller's block, is a connected
/// app attached to the caller's session. That is the one rule for which app a
/// jump may switch. Rex reports no session an app shows now, and an app keeps
/// its claim on a block after it moves on, so a true answer means the app
/// showed the caller's pane last, which is the closest Rex gets to "shows the
/// caller". Only an explicit jump asks; `add` and `open` never switch the app.
fn shows_caller(clients: &ClientList, owner: &ClientId, caller_session: &SessionId) -> bool {
    clients.is_app_attached_to(owner, caller_session)
}

#[derive(Debug, Deserialize)]
struct ClientInfo {
    #[serde(default)]
    kind: String,
}

/// Workmux's identity for a window it created. Rex windows carry only a
/// label, which an agent in the window can rename, so the ids live beside
/// the agent state, one file per window.
#[derive(Debug, Serialize, Deserialize)]
struct WindowRecord {
    token: String,
    primary: bool,
    session_id: SessionId,
    window_id: WindowId,
}

/// A recorded window that still exists, with its current labels.
struct LiveWindow {
    record: WindowRecord,
    window_label: String,
    session_label: String,
}

impl LiveWindow {
    fn target(self) -> OwnedWindowTarget {
        OwnedWindowTarget {
            target: WindowTarget::with_id(
                self.window_label,
                Some(self.session_label),
                self.record.window_id.0,
            ),
            is_primary: self.record.primary,
        }
    }
}

#[derive(Debug)]
enum CallError {
    NotFound(String),
    Unsupported(String),
    Failed(anyhow::Error),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::NotFound(message) | CallError::Unsupported(message) => {
                write!(f, "{message}")
            }
            CallError::Failed(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for CallError {}

impl CallError {
    fn from_stderr(args: &[&str], stderr: &str) -> Self {
        let stderr = stderr.trim();
        let gone_block =
            stderr.starts_with("Error: block target ") && stderr.ends_with(" not found");
        if gone_block
            || stderr.contains("code=not_found")
            || stderr.contains("code=session_not_found")
        {
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
        Err(CallError::Unsupported(message)) => Err(anyhow!(message)),
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
        "DC" => "delete",
        "PPage" | "PageUp" => "pageup",
        "NPage" | "PageDown" => "pagedown",
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

/// A pane is live while its program runs; Rex keeps the block after it exits.
fn pane_info(
    session: &SessionView,
    window: &WindowView,
    process: &ProcessReport,
    title: String,
) -> Option<LivePaneInfo> {
    if process.has_exited() {
        return None;
    }
    Some(LivePaneInfo {
        pid: process.child.as_ref().map(|child| child.pid),
        current_command: process.foreground.as_ref().map(|fg| fg.name.clone()),
        working_dir: process.working_dir(),
        title: (!title.is_empty()).then_some(title),
        session: Some(session.label.clone()),
        window: Some(window.label.clone()),
        session_id: Some(session.session_id.as_str().to_string()),
        window_id: Some(window.window_id.as_str().to_string()),
        window_index: None,
    })
}

/// Labels repeat, so a label lookup acts on one match: the caller's own
/// session or window when it matches, else the first.
fn prefer_current<T: PartialEq>(matches: Vec<T>, current: Option<&T>) -> Option<T> {
    let index = current
        .and_then(|current| matches.iter().position(|item| item == current))
        .unwrap_or(0);
    matches.into_iter().nth(index)
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

/// Never a real endpoint, so state written while the server is unreachable
/// cannot be mistaken for any server's.
const UNRESOLVED_INSTANCE: &str = "unresolved";

/// `unix://` addresses with the path percent-decoded and symlinks, `.` and
/// repeated slashes resolved, so every spelling of one socket compares equal.
fn canonical_unix_endpoint(address: &str) -> Option<String> {
    let path = address.strip_prefix("unix://")?;
    let path = PathBuf::from(
        percent_encoding::percent_decode_str(path)
            .decode_utf8_lossy()
            .into_owned(),
    );
    let path = path
        .canonicalize()
        .unwrap_or_else(|_| crate::util::normalize_path(&path));
    Some(format!("unix://{}", path.display()))
}

pub fn in_rex_terminal() -> bool {
    std::env::var("REX_BLOCK").is_ok_and(|value| !value.is_empty())
}

/// The Rex pane workmux runs in, from `REX_SESSION` and `REX_BLOCK`.
#[derive(Debug, Default)]
struct Caller {
    session: Option<SessionId>,
    block: Option<BlockId>,
}

impl Caller {
    fn from_env() -> Self {
        Self {
            session: std::env::var("REX_SESSION")
                .ok()
                .filter(|id| id.starts_with("session:"))
                .map(SessionId),
            block: std::env::var("REX_BLOCK")
                .ok()
                .and_then(|id| BlockId::parse(&id).ok()),
        }
    }
}

/// What became of asking Rex.app to show a session or window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppSwitch {
    Shown,
    /// The app turned the request down because Remote Control is off.
    Refused,
    /// No app owns the caller's pane, or the app could not be asked.
    NoApp,
}

impl AppSwitch {
    /// Prints the fix once per process, and never under a full-screen
    /// display such as the dashboard, which would swallow it.
    fn hint_if_refused(self, printed: &AtomicBool, out: &mut impl Write) {
        if self == AppSwitch::Refused
            && !crossterm::terminal::is_raw_mode_enabled().unwrap_or(false)
        {
            print_once(printed, out, REMOTE_CONTROL_FIX);
        }
    }
}

/// A healthy Rex.app answers `session.select` in 55 to 125 ms; one that
/// never answers would otherwise hold the command for Rex's 10 s default.
const APP_SELECT_TIMEOUT: Duration = Duration::from_secs(1);

const REMOTE_CONTROL_FIX: &str =
    "Rex.app did not switch: Remote Control is off. Turn it on in Rex Settings.";

/// The error a jump returns when Rex.app refused it. The backend has already
/// told the user how to fix it, so callers stay quiet.
#[derive(Debug)]
pub struct AppRefused;

impl std::fmt::Display for AppRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REMOTE_CONTROL_FIX)
    }
}

impl std::error::Error for AppRefused {}

/// Printed once per process, so a loop over panes repeats no hint.
static REMOTE_CONTROL_HINT: AtomicBool = AtomicBool::new(false);

fn print_once(printed: &AtomicBool, out: &mut impl Write, text: &str) {
    if !printed.swap(true, Ordering::Relaxed) {
        let _ = writeln!(out, "{text}");
    }
}

#[derive(Debug)]
pub struct RexBackend {
    program: String,
    reads: Mutex<Reads>,
    reuse: Duration,
    server: Option<String>,
    caller: Caller,
    endpoint: OnceLock<String>,
    block_sessions: Mutex<HashMap<BlockId, SessionId>>,
}

impl RexBackend {
    pub fn new() -> Self {
        Self::with_server(
            std::env::var("REX_SERVER")
                .ok()
                .filter(|server| !server.trim().is_empty()),
        )
    }

    pub fn for_server(address: &str) -> Self {
        Self::with_server(Some(address.to_string()))
    }

    fn with_server(server: Option<String>) -> Self {
        let program = std::env::var_os("REX_BIN_DIR")
            .map(|dir| PathBuf::from(dir).join("rex"))
            .filter(|path| path.is_file())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "rex".to_string());
        Self {
            program,
            reads: Mutex::default(),
            reuse: std::env::var("WORKMUX_REX_READ_REUSE_MS")
                .ok()
                .and_then(|ms| ms.parse().ok())
                .map_or(READ_REUSE, Duration::from_millis),
            server,
            caller: Caller::from_env(),
            endpoint: OnceLock::new(),
            block_sessions: Mutex::new(HashMap::new()),
        }
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
        self.ensure_supported()?;
        let [
            "api",
            "call",
            method @ ("session.list" | "session.view"),
            "-",
        ] = args
        else {
            return self.exec(args, stdin);
        };
        let key = format!("{method} {}", stdin.unwrap_or_default());
        let writes = {
            let reads = self.reads.lock().unwrap();
            if let Some((fetched, output)) = reads.answers.get(&key)
                && fetched.elapsed() < self.reuse
            {
                return Ok(output.clone());
            }
            reads.writes
        };
        let started = Instant::now();
        let output = self.exec(args, stdin)?;
        let mut reads = self.reads.lock().unwrap();
        if reads.writes == writes {
            reads.answers.insert(key, (started, output.clone()));
        }
        Ok(output)
    }

    /// Fails every call on a server whose API this backend does not speak,
    /// before a changed response shape surfaces as a parse error.
    fn ensure_supported(&self) -> CallResult<()> {
        let key = self
            .server
            .as_deref()
            .map(|server| canonical_unix_endpoint(server).unwrap_or_else(|| server.to_string()));
        let mut verdicts = SERVER_VERDICTS.lock().unwrap();
        let verdict = match verdicts.get(&key) {
            Some(verdict) => verdict.clone(),
            None => {
                let output = self.exec(&["api", "call", "server.version"], None)?;
                let verdict = Self::parse::<ServerVersion>("server.version", &output)?.check();
                verdicts.insert(key, verdict.clone());
                verdict
            }
        };
        verdict.map_err(CallError::Unsupported)
    }

    fn exec(&self, args: &[&str], stdin: Option<&str>) -> CallResult<String> {
        self.exec_within(args, stdin, None)
    }

    fn exec_within(
        &self,
        args: &[&str],
        stdin: Option<&str>,
        timeout: Option<Duration>,
    ) -> CallResult<String> {
        let result = self.spawn(args, stdin, timeout);
        if !changes_nothing(args) {
            self.forget_reads();
        }
        result
    }

    /// Drops every reused read, and keeps a read already under way from
    /// being stored.
    fn forget_reads(&self) {
        let mut reads = self.reads.lock().unwrap();
        reads.writes += 1;
        reads.answers.clear();
    }

    fn spawn(
        &self,
        args: &[&str],
        stdin: Option<&str>,
        timeout: Option<Duration>,
    ) -> CallResult<String> {
        let program = &self.program;
        let mut command = Command::new(program);
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
        if let Some(timeout) = timeout {
            let deadline = std::time::Instant::now() + timeout;
            while child
                .try_wait()
                .context("Failed to wait for rex")
                .map_err(CallError::Failed)?
                .is_none()
            {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CallError::Failed(anyhow!(
                        "rex {} gave no answer within {timeout:?}",
                        args.join(" ")
                    )));
                }
                thread::sleep(Duration::from_millis(5));
            }
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

    fn current_view(&self) -> Result<Option<SessionView>> {
        match &self.caller.session {
            Some(session) => found(self.view(session)),
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
            "Cannot tell which Rex session to use. Run {BIN} inside a Rex pane \
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

    fn target_window(&self, target: &WindowTarget) -> Result<Option<(SessionId, WindowId)>> {
        if let Some(id) = &target.window_id {
            let window = WindowId(id.clone());
            return Ok(self
                .view_of_window(&window)?
                .map(|view| (view.session_id, window)));
        }
        let views = match target.parent_session() {
            Some(parent) => self
                .sessions_labeled(parent)?
                .iter()
                .filter_map(|session| found(self.view(session)).transpose())
                .collect::<Result<Vec<_>>>()?,
            None => vec![self.current_or_only_view()?],
        };
        let matches = views
            .iter()
            .flat_map(|view| {
                view.windows_labeled(&target.full_name)
                    .map(|window| (view.session_id.clone(), window.window_id.clone()))
            })
            .collect();
        let current = self.caller.block.as_ref().and_then(|block| {
            views.iter().find_map(|view| {
                let window = view.window_of(block)?;
                Some((view.session_id.clone(), window.window_id.clone()))
            })
        });
        Ok(prefer_current(matches, current.as_ref()))
    }

    fn named_window(&self, full_name: &str) -> Result<Option<(SessionId, WindowId)>> {
        self.target_window(&WindowTarget::new(full_name.to_string(), None))
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
        words.push(shell_quote(&self.program));
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
        if process.has_exited() {
            return Ok(None);
        }
        let title = found(self.block_call::<TitleReport>(block, "title", json!({})))?
            .map(|report| report.title)
            .unwrap_or_default();
        Ok(pane_info(session, window, &process, title))
    }

    /// The Rex.app client that `shows_caller` allows a jump to switch.
    fn app_showing_caller(&self) -> Result<Option<ClientId>> {
        let (Some(session), Some(block)) = (&self.caller.session, &self.caller.block) else {
            return Ok(None);
        };
        let Some(owner) = found(self.block_call::<SizeReport>(block, "size", json!({})))?
            .and_then(|size| size.owner)
        else {
            return Ok(None);
        };
        let clients: ClientList = self.api("client.list", json!({}))?;
        Ok(shows_caller(&clients, &owner, session).then_some(owner))
    }

    /// Asks `app` to show `session`, at `window` if given. A refusal comes
    /// back as [`AppRefused`]. The call goes through `client.perform` rather
    /// than `rex -C <app> do`, whose error drops the documented
    /// `code=refused`.
    fn ask_app_to_show(
        &self,
        app: &ClientId,
        session: &SessionId,
        window: Option<&WindowId>,
        timeout: Duration,
    ) -> Result<()> {
        let mut args = json!({ "session_id": session.as_str() });
        if let Some(window) = window {
            args["window_id"] = json!(window.as_str());
        }
        let request = json!({
            "client_id": app.as_str(),
            "action": "session.select",
            "args": args,
            "timeout_ms": timeout.as_millis(),
        })
        .to_string();
        match self.ensure_supported().and_then(|()| {
            self.exec_within(
                &["api", "call", "client.perform", "-"],
                Some(&request),
                Some(timeout),
            )
        }) {
            Ok(_) => Ok(()),
            Err(error) if error.to_string().contains("code=refused") => Err(AppRefused.into()),
            Err(error) => Err(error.into()),
        }
    }

    /// Rex.app keeps its own shown tab, which `session.focus_window` and
    /// `session.focus_block` leave alone, so the app is asked to show it.
    fn show_on_app(&self, session: &SessionId, window: Option<&WindowId>) -> AppSwitch {
        let app = match self.app_showing_caller() {
            Ok(Some(app)) => app,
            Ok(None) => return AppSwitch::NoApp,
            Err(error) => {
                tracing::warn!(error = %format!("{error:#}"), "rex: app lookup failed");
                return AppSwitch::NoApp;
            }
        };
        match self.ask_app_to_show(&app, session, window, APP_SELECT_TIMEOUT) {
            Ok(()) => AppSwitch::Shown,
            Err(error) if error.is::<AppRefused>() => AppSwitch::Refused,
            Err(error) => {
                tracing::warn!(%error, "rex: app switch failed");
                AppSwitch::NoApp
            }
        }
    }

    /// Shows `pane_id` on the app and focuses it, writing the Remote Control
    /// fix to `hint` once per `printed` if the app refuses.
    fn jump_to_pane(
        &self,
        pane_id: &str,
        printed: &AtomicBool,
        hint: &mut impl Write,
    ) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        let target = self
            .resolve("block", block.as_str())?
            .ok_or_else(|| anyhow!("Rex block {pane_id} not found"))?;
        let switch = self.show_on_app(&target.session_id, target.window_id.as_ref());
        switch.hint_if_refused(printed, hint);
        if switch == AppSwitch::Refused {
            return Err(AppRefused.into());
        }
        self.select_pane(pane_id)
    }

    fn records_dir(&self) -> Result<PathBuf> {
        let instance = self.resolve_instance_id()?;
        Ok(crate::state::store::get_state_dir()?
            .join("rex-windows")
            .join(
                percent_encoding::utf8_percent_encode(
                    &instance,
                    percent_encoding::NON_ALPHANUMERIC,
                )
                .to_string(),
            ))
    }

    fn record_path(dir: &Path, window: &WindowId) -> PathBuf {
        let name = window.as_str().trim_start_matches("window:");
        dir.join(format!("{name}.json"))
    }

    fn write_record(&self, record: &WindowRecord) -> Result<()> {
        let dir = self.records_dir()?;
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        crate::util::write_atomic(
            &Self::record_path(&dir, &record.window_id),
            &serde_json::to_vec(record)?,
        )
    }

    fn read_records(&self) -> Result<(PathBuf, Vec<WindowRecord>)> {
        let dir = self.records_dir()?;
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((dir, Vec::new()));
            }
            Err(error) => return Err(error).context(format!("Failed to read {}", dir.display())),
        };
        let records = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                let content = std::fs::read(entry.path()).ok()?;
                serde_json::from_slice(&content).ok()
            })
            .collect();
        Ok((dir, records))
    }

    /// Recorded windows that still exist. Records of closed windows, or of a
    /// server that restarted and handed out new ids, are deleted.
    fn live_windows(&self) -> Result<Vec<LiveWindow>> {
        let (dir, records) = self.read_records()?;
        let sessions: Vec<SessionId> = records
            .iter()
            .map(|record| record.session_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let views: HashMap<SessionId, SessionView> =
            parallel_map(&sessions, |session| found(self.view(session)))
                .into_iter()
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .map(|view| (view.session_id.clone(), view))
                .collect();
        let mut live = Vec::new();
        for record in records {
            let window = views
                .get(&record.session_id)
                .and_then(|view| Some((view, view.window(&record.window_id)?)));
            match window {
                Some((view, window)) => live.push(LiveWindow {
                    window_label: window.label.clone(),
                    session_label: view.label.clone(),
                    record,
                }),
                None => {
                    let _ = std::fs::remove_file(Self::record_path(&dir, &record.window_id));
                }
            }
        }
        Ok(live)
    }

    fn labeled_session(&self, label: &str) -> Result<Option<SessionId>> {
        Ok(prefer_current(
            self.sessions_labeled(label)?,
            self.caller.session.as_ref(),
        ))
    }

    fn first_session_labeled(&self, label: &str) -> Result<SessionId> {
        self.labeled_session(label)?
            .ok_or_else(|| anyhow!("Rex session '{label}' not found"))
    }
}

impl Multiplexer for RexBackend {
    fn name(&self) -> &'static str {
        "rex"
    }

    fn is_running(&self) -> Result<bool> {
        // An unsupported server is running, so a command that acts on it
        // goes on to a call that names why workmux cannot use it.
        match self.run(&["api", "call", "session.list", "-"], Some("{}")) {
            Ok(_) | Err(CallError::Unsupported(_)) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    fn current_window_id(&self) -> Result<Option<String>> {
        let (Some(view), Some(block)) = (self.current_view()?, &self.caller.block) else {
            return Ok(None);
        };
        Ok(view
            .window_of(block)
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
        Ok(self
            .caller
            .session
            .as_ref()
            .map(|session| session.0.clone()))
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
        self.caller.block.as_ref().map(|block| block.0.clone())
    }

    /// The caller's pane while it is alive, else the pane the server has
    /// focused. `add` focuses its new window on the server without moving
    /// the app, so the server's focus is often a pane the user never saw.
    fn active_pane_id(&self) -> Option<String> {
        let view = self.current_or_only_view().ok()?;
        let block = match &self.caller.block {
            Some(caller) if view.window_of(caller).is_some() => caller,
            _ => view.active_window()?.focused_block_id.as_ref()?,
        };
        Some(block.as_str().to_string())
    }

    fn get_client_active_pane_path(&self) -> Result<PathBuf> {
        let block = match self.caller.block.clone() {
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

    fn supports_window_ownership(&self) -> bool {
        true
    }

    fn set_window_ownership(&self, pane_id: &str, token: &str, is_primary: bool) -> Result<()> {
        let is_window = pane_id.starts_with("window:");
        let target = if is_window {
            self.resolve("window", pane_id)?
        } else {
            self.resolve("block", BlockId::parse(pane_id)?.as_str())?
        };
        let target = target.ok_or_else(|| anyhow!("Rex target {pane_id} not found"))?;
        let window_id = if is_window {
            WindowId(pane_id.to_string())
        } else {
            target
                .window_id
                .ok_or_else(|| anyhow!("Rex block {pane_id} is not in a window"))?
        };
        self.write_record(&WindowRecord {
            token: token.to_string(),
            primary: is_primary,
            session_id: target.session_id,
            window_id,
        })
    }

    fn owned_window_targets(&self, token: &str) -> Result<Vec<OwnedWindowTarget>> {
        Ok(self
            .live_windows()?
            .into_iter()
            .filter(|window| window.record.token == token)
            .map(LiveWindow::target)
            .collect())
    }

    fn owned_window_tokens(&self) -> Result<HashSet<String>> {
        Ok(self
            .live_windows()?
            .into_iter()
            .map(|window| window.record.token)
            .collect())
    }

    fn window_ownership_records(&self) -> Result<Vec<WindowOwnershipRecord>> {
        let tokens: HashMap<WindowId, String> = self
            .read_records()?
            .1
            .into_iter()
            .map(|record| (record.window_id, record.token))
            .collect();
        let views = self.views()?;
        let windows: Vec<(&SessionView, &WindowView)> = views
            .iter()
            .flat_map(|view| view.windows.iter().map(move |window| (view, window)))
            .collect();
        parallel_map(&windows, |(view, window)| {
            let pane_path = match window.terminal_block_ids().next() {
                Some(block) => {
                    found(self.block_call::<ProcessReport>(block, "process", json!({})))?
                        .map(|process| process.working_dir())
                        .unwrap_or_default()
                }
                None => PathBuf::new(),
            };
            Ok(WindowOwnershipRecord {
                window_id: window.window_id.as_str().to_string(),
                window_name: window.label.clone(),
                session_name: view.label.clone(),
                token: tokens.get(&window.window_id).cloned(),
                pane_path,
            })
        })
        .into_iter()
        .collect()
    }

    fn switch_to_session(&self, prefix: &str, name: &str) -> Result<()> {
        self.first_session_labeled(&util::prefixed(prefix, name))?;
        Ok(())
    }

    fn session_exists(&self, full_name: &str) -> Result<bool> {
        Ok(!self.sessions_labeled(full_name)?.is_empty())
    }

    fn kill_session(&self, full_name: &str) -> Result<()> {
        if let Some(session) = self.labeled_session(full_name)? {
            self.destroy_session(&session)?;
        }
        Ok(())
    }

    fn kill_window(&self, full_name: &str) -> Result<()> {
        if let Some((session, window)) = self.named_window(full_name)? {
            self.close_window(&session, &window)?;
        }
        Ok(())
    }

    fn kill_window_target(&self, target: &WindowTarget) -> Result<()> {
        if let Some((session, window)) = self.target_window(target)? {
            self.close_window(&session, &window)?;
        }
        Ok(())
    }

    fn rename_window(&self, old_full_name: &str, new_full_name: &str) -> Result<()> {
        let (session, window) = self
            .named_window(old_full_name)?
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
        let window = self.named_window(full_name)?;
        Self::schedule(delay, &self.shell_close_windows_cmd(window.as_slice()))
    }

    fn schedule_window_target_close(&self, target: &WindowTarget, delay: Duration) -> Result<()> {
        let window = self.target_window(target)?;
        Self::schedule(delay, &self.shell_close_windows_cmd(window.as_slice()))
    }

    fn schedule_session_close(&self, full_name: &str, delay: Duration) -> Result<()> {
        let commands = self
            .labeled_session(full_name)?
            .map(|session| self.shell_destroy_cmd(&session))
            .unwrap_or_default();
        Self::schedule(delay, &commands)
    }

    fn run_deferred_script(&self, script: &str) -> Result<()> {
        util::run_detached_sh_c(script)
    }

    fn shell_select_window_cmd(&self, full_name: &str) -> Result<String> {
        let (session, window) = self
            .named_window(full_name)?
            .ok_or_else(|| anyhow!("Window '{full_name}' not found"))?;
        Ok(self.shell_api_cmd(
            "session.focus_window",
            json!({ "session_id": session.as_str(), "window_id": window.as_str() }),
        ))
    }

    fn shell_kill_window_cmd(&self, full_name: &str) -> Result<String> {
        let window = self
            .named_window(full_name)?
            .ok_or_else(|| anyhow!("Window '{full_name}' not found"))?;
        Ok(self.shell_close_windows_cmd(&[window]))
    }

    fn shell_kill_window_target_cmd(&self, target: &WindowTarget) -> Result<String> {
        let window = self
            .target_window(target)?
            .ok_or_else(|| anyhow!("Window '{}' not found", target.full_name))?;
        Ok(self.shell_close_windows_cmd(&[window]))
    }

    fn shell_switch_session_cmd(&self, full_name: &str) -> Result<String> {
        self.first_session_labeled(full_name)?;
        Ok("true".to_string())
    }

    fn shell_kill_session_cmd(&self, full_name: &str) -> Result<String> {
        Ok(self.shell_destroy_cmd(&self.first_session_labeled(full_name)?))
    }

    fn select_window(&self, prefix: &str, name: &str) -> Result<()> {
        let full_name = util::prefixed(prefix, name);
        let (session, window) = self
            .named_window(&full_name)?
            .ok_or_else(|| anyhow!("Window '{full_name}' not found"))?;
        self.focus_window(&session, &window)
    }

    fn select_window_target(&self, target: &WindowTarget) -> Result<()> {
        let (session, window) = self
            .target_window(target)?
            .ok_or_else(|| anyhow!("Window '{}' not found", target.full_name))?;
        self.focus_window(&session, &window)
    }

    fn window_target_exists(&self, target: &WindowTarget) -> Result<bool> {
        Ok(self.target_window(target)?.is_some())
    }

    fn current_window_name(&self) -> Result<Option<String>> {
        let (Some(view), Some(block)) = (self.current_view()?, &self.caller.block) else {
            return Ok(None);
        };
        Ok(view.window_of(block).map(|window| window.label.clone()))
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

    /// The one call that moves Rex.app. Rex does not say which session an
    /// app shows now, so `add` and `open` leave the app alone, but a jump is
    /// asked for from the pane the user is looking at, through the app that
    /// owns it. A refused jump fails, so the dashboard stays open, and only
    /// a command outside a full-screen display prints the fix.
    fn switch_to_pane(&self, pane_id: &str, _window_hint: Option<&str>) -> Result<()> {
        self.jump_to_pane(pane_id, &REMOTE_CONTROL_HINT, &mut std::io::stderr())
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
        // Rex cannot restart a block's program or report the command it
        // started with. Start the replacement beside the old block, then
        // close the old one so the new block takes its whole space.
        let cmd = cmd.ok_or_else(|| {
            anyhow!("Rex cannot rerun a block's original command; pass the command to respawn")
        })?;
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
                    "layout": self.block_layout(cwd, Some(cmd))?,
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
        if text.is_empty() {
            return Ok(());
        }
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
        self.resolve_instance_id().unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "rex: instance unresolved");
            UNRESOLVED_INSTANCE.to_string()
        })
    }

    /// The server's canonical address. The per-boot `whoami` instance id
    /// would orphan every agent record when the server restarts, and
    /// `host_id` is shared by every server the account runs on the machine.
    fn resolve_instance_id(&self) -> Result<String> {
        if let Some(endpoint) = self.endpoint.get() {
            return Ok(endpoint.clone());
        }
        let endpoint = match self.server.as_deref().and_then(canonical_unix_endpoint) {
            Some(endpoint) => endpoint,
            None => {
                let reported = self.whoami()?.endpoint;
                canonical_unix_endpoint(&reported).unwrap_or(reported)
            }
        };
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
        let Some((window, _)) = view.terminal_blocks().find(|(_, id)| **id == block) else {
            return Ok(None);
        };
        self.live_pane(&view, window, &block)
    }

    /// Runs only for a pane the batch read missed. A reused view would
    /// repeat the batch's answer, and a miss prunes the agent.
    fn validate_agent_alive(&self, state: &crate::state::AgentState) -> Result<bool> {
        self.forget_reads();
        let live_pane = self.get_live_pane_info(&state.pane_key.pane_id)?;
        Ok(super::pane_runs_agent(live_pane, state))
    }

    fn get_all_live_pane_info(&self) -> Result<HashMap<String, LivePaneInfo>> {
        let views = self.views()?;
        let blocks: Vec<(&SessionView, &WindowView, &BlockId)> = views
            .iter()
            .flat_map(|view| {
                view.terminal_blocks()
                    .map(move |(window, block)| (view, window, block))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: &str = include_str!("testdata/rex_session_view.json");
    const PROCESS: &str = include_str!("testdata/rex_process.json");
    const SESSION_LIST: &str = include_str!("testdata/rex_session_list.json");
    const RESOLVE: &str = include_str!("testdata/rex_target_resolve.json");
    const VIEW_WITH_ONBOARDING: &str = include_str!("testdata/rex_session_view_onboarding.json");
    const PROCESS_EXITED: &str = include_str!("testdata/rex_process_exited.json");
    const GONE_BLOCK_STDERR: &str = include_str!("testdata/rex_gone_block_stderr.txt");
    const VERSION_MAC: &str = include_str!("testdata/rex_server_version_mac.json");
    const VERSION_LINUX: &str = include_str!("testdata/rex_server_version_linux.json");
    const VIEW_WITH_FLOATING: &str = include_str!("testdata/rex_session_view_floating.json");
    const CLIENT_LIST: &str = include_str!("testdata/rex_client_list.json");

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
        let missing = CallError::from_stderr(&["block", "call"], GONE_BLOCK_STDERR);
        assert!(matches!(missing, CallError::NotFound(_)));
        let no_method = CallError::from_stderr(
            &["block", "call"],
            "Error: block 025b81 has no method \"process\"; methods: set_step, state",
        );
        assert!(matches!(no_method, CallError::Failed(_)));
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
        assert_eq!(key_input("DC"), KeyInput::Key("delete".into()));
        assert_eq!(key_input("PageUp"), KeyInput::Key("pageup".into()));
        assert_eq!(key_input("NPage"), KeyInput::Key("pagedown".into()));
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

    #[test]
    fn instance_ids_match_for_every_spelling_of_one_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("a b.sock");
        std::fs::write(&socket, "").unwrap();
        let canonical = format!("unix://{}", socket.canonicalize().unwrap().display());
        let parent = dir.path().display().to_string();
        for spelling in [
            format!("unix://{parent}//a b.sock"),
            format!("unix://{parent}/./a b.sock"),
            format!("unix://{parent}/a%20b.sock"),
        ] {
            let rex = RexBackend::for_server(&spelling);
            assert_eq!(rex.resolve_instance_id().unwrap(), canonical, "{spelling}");
        }
        assert_eq!(canonical_unix_endpoint("rex.tail4e96ed.ts.net"), None);
    }

    #[test]
    fn an_unreachable_server_is_not_keyed_as_default() {
        let rex = RexBackend {
            program: "/nonexistent/rex".to_string(),
            ..RexBackend::with_server(None)
        };
        assert!(rex.resolve_instance_id().is_err());
        assert_eq!(rex.instance_id(), UNRESOLVED_INSTANCE);
        assert_ne!(rex.instance_id(), "default");
    }

    #[test]
    fn mac_and_linux_servers_are_supported() {
        for fixture in [VERSION_MAC, VERSION_LINUX] {
            let version: ServerVersion = serde_json::from_str(fixture).unwrap();
            assert_eq!(version.check(), Ok(()), "{fixture}");
        }
        let newer: ServerVersion = serde_json::from_str(
            r#"{"rex_version":"0.0.2","api_min_revision":7,"api_revision":12}"#,
        )
        .unwrap();
        assert_eq!(newer.check(), Ok(()));
    }

    #[test]
    fn servers_outside_the_range_are_named_in_the_error() {
        let ancient: ServerVersion =
            serde_json::from_str(r#"{"rex_version":"0.0.1-old"}"#).unwrap();
        assert_eq!(
            ancient.check(),
            Err(format!(
                "unsupported Rex 0.0.1-old (API revisions 0 to 0), \
                 {BIN} supports Rex API revisions 8 to 9"
            ))
        );
        let future: ServerVersion = serde_json::from_str(
            r#"{"rex_version":"0.1.0","api_min_revision":10,"api_revision":11}"#,
        )
        .unwrap();
        assert!(
            future
                .check()
                .unwrap_err()
                .starts_with("unsupported Rex 0.1.0 (API revisions 10 to 11)")
        );
    }

    /// Writes a fake `rex` from a child process. On Linux, a script this
    /// process wrote can fail to start with ETXTBSY: a test on another thread
    /// that forks while the file is open for writing keeps a writable copy
    /// of its descriptor until that child execs.
    fn write_script(path: &Path, content: String) -> std::io::Result<()> {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "cat > \"$0\""])
            .arg(path)
            .stdin(Stdio::piped())
            .spawn()?;
        child.stdin.take().unwrap().write_all(content.as_bytes())?;
        let status = child.wait()?;
        assert!(
            status.success(),
            "writing {} failed: {status}",
            path.display()
        );
        Ok(())
    }

    #[test]
    fn an_unsupported_server_fails_every_call_after_one_version_query() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("calls");
        let program = dir.path().join("rex");
        write_script(
            &program,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\n\
                 case \"$*\" in *server.version*) echo '{{\"rex_version\":\"0.0.1-old\",\"api_revision\":3}}';;\n\
                 *) echo 'not json';; esac\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rex = RexBackend {
            program: program.display().to_string(),
            ..RexBackend::for_server(&format!("unix://{}/s.sock", dir.path().display()))
        };

        for _ in 0..2 {
            let error = rex.sessions().unwrap_err().to_string();
            assert!(error.starts_with("unsupported Rex 0.0.1-old"), "{error}");
        }
        let calls = std::fs::read_to_string(&log).unwrap();
        assert_eq!(calls.lines().count(), 1, "{calls}");
        assert!(calls.contains("api call server.version"));
    }

    #[test]
    fn the_pane_scan_probes_terminal_blocks_only() {
        let view: SessionView = serde_json::from_str(VIEW_WITH_ONBOARDING).unwrap();
        assert_eq!(view.windows.len(), 2);
        let probed: Vec<_> = view
            .terminal_blocks()
            .map(|(window, _)| window.label.as_str())
            .collect();
        assert_eq!(probed, ["wm-agent"]);
    }

    #[test]
    fn an_exited_program_reads_as_dead_while_its_block_remains() {
        let view = view();
        let window = &view.windows[1];
        let exited: ProcessReport = serde_json::from_str(PROCESS_EXITED).unwrap();
        assert!(pane_info(&view, window, &exited, String::new()).is_none());
        let running: ProcessReport = serde_json::from_str(PROCESS).unwrap();
        let pane = pane_info(&view, window, &running, "claude".into()).unwrap();
        assert_eq!(pane.pid, Some(85152));
        assert_eq!(pane.window.as_deref(), Some("wm-feat-a"));
    }

    #[test]
    fn the_pane_scan_skips_floating_layers() {
        let view: SessionView = serde_json::from_str(VIEW_WITH_FLOATING).unwrap();
        let window = &view.windows[0];
        assert_eq!(window.block_ids().count(), 2);
        let probed: Vec<_> = view.terminal_blocks().map(|(_, block)| block).collect();
        assert_eq!(probed.len(), 1);
        assert!(probed[0].as_str().ends_with("aaae97"));
    }

    #[test]
    fn label_lookups_act_on_one_match_preferring_the_callers_own() {
        let windows = vec!["a", "b", "c"];
        assert_eq!(prefer_current(windows.clone(), Some(&"b")), Some("b"));
        assert_eq!(prefer_current(windows.clone(), Some(&"z")), Some("a"));
        assert_eq!(prefer_current(windows, None), Some("a"));
        assert_eq!(prefer_current(Vec::<&str>::new(), Some(&"a")), None);
    }

    /// A `rex` that answers each call with the reply of the first pattern
    /// its arguments and stdin match, and logs every call.
    struct FakeRex {
        dir: tempfile::TempDir,
        rex: RexBackend,
    }

    impl FakeRex {
        fn new(cases: &[(&str, &str)]) -> Self {
            use std::os::unix::fs::PermissionsExt;
            let dir = tempfile::tempdir().unwrap();
            let mut arms = String::new();
            for (index, (pattern, reply)) in cases.iter().enumerate() {
                let file = dir.path().join(format!("reply{index}"));
                let arm = match reply.strip_prefix('!') {
                    _ if reply.starts_with('$') => reply[1..].to_string(),
                    Some(stderr) => {
                        std::fs::write(&file, stderr).unwrap();
                        format!("cat '{}' >&2; exit 1", file.display())
                    }
                    None => {
                        std::fs::write(&file, reply).unwrap();
                        format!("cat '{}'", file.display())
                    }
                };
                // A space would end the case pattern; `?` matches it.
                let pattern = pattern.replace(' ', "?");
                arms.push_str(&format!("  *{pattern}*) {arm};;\n"));
            }
            let program = dir.path().join("rex");
            write_script(
                &program,
                format!(
                    "#!/bin/sh\ndir=$(dirname \"$0\")\ncall=\"$* $(cat)\"\necho \"$call\" >> '{log}'\n\
                     case \"$call\" in\n\
                     {arms}\
                     \x20 *server.version*) echo '{{\"rex_version\":\"t\",\"api_revision\":8}}';;\n\
                     \x20 *) echo \"unexpected: $call\" >&2; exit 1;;\nesac\n",
                    log = dir.path().join("calls").display(),
                ),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
            let rex = RexBackend {
                program: program.display().to_string(),
                caller: Caller::default(),
                ..RexBackend::for_server(&format!("unix://{}/s.sock", dir.path().display()))
            };
            Self { dir, rex }
        }

        fn in_pane(mut self, session: &str, block: &str) -> Self {
            self.rex.caller = Caller {
                session: Some(SessionId(session.into())),
                block: Some(BlockId(block.into())),
            };
            self
        }

        fn calls(&self, needle: &str) -> Vec<String> {
            std::fs::read_to_string(self.dir.path().join("calls"))
                .unwrap_or_default()
                .lines()
                .filter(|line| line.contains(needle))
                .map(str::to_string)
                .collect()
        }
    }

    #[test]
    fn empty_text_is_not_sent() {
        let fake = FakeRex::new(&[]);
        fake.rex
            .send_text_fragment("block:01a19e59-43d8-72fd-860c-3a1d6f183cc3", "")
            .unwrap();
        assert!(fake.calls("write").is_empty());
    }

    const APP: &str = "client:app-1";
    const CALLER_SESSION: &str = "session:b";
    const CALLER_BLOCK: &str = "block:caller";

    fn client(id: &str, kind: &str, state: &str, sessions: &[&str]) -> Value {
        json!({
            "client_id": id,
            "info": { "kind": kind },
            "connection_state": state,
            "session_ids": sessions,
        })
    }

    fn app(id: &str, sessions: &[&str]) -> Value {
        client(id, "app", "connected", sessions)
    }

    fn owner(client: &str) -> String {
        json!({ "columns": 80, "rows": 24, "owner": client }).to_string()
    }

    /// Rex.app's answer to `client.perform` with Remote Control off.
    const REFUSED: &str = "!Error: stream response \"control-1\" failed: Rex.app is not \
         accepting actions. Turn on Remote Control in its Settings under Rex Server. \
         (status=403 code=refused)";
    const UNAVAILABLE: &str = "!Error: stream response \"control-1\" failed: client \
         client:app-1 is not connected (status=503 code=client_unavailable)";
    /// Rex's real answers when the app is slow or lacks the action.
    const TIMEOUT: &str = "!Error: stream response \"control-1\" failed: \"rexbook\" did \
         not answer within 1ms (status=504 code=timeout)";
    const NOT_SUPPORTED: &str = "!Error: stream response \"control-1\" failed: \"rexbook\" \
         does not offer \"wr.no_such_action\" (status=400 code=not_supported)";
    /// The refusal as `rex do` prints it, with no code: only the code counts.
    const REFUSED_WITHOUT_CODE: &str = "!Error: Rex.app is not accepting actions. Turn on \
         Remote Control in its Settings under Rex Server.";

    /// The caller's pane is in session B, window W, the pane `block:agent`
    /// is in the same window, and `clients` are connected.
    fn app_fake(owner_id: &str, clients: &[Value], select: &str) -> FakeRex {
        let resolved = json!({ "results": [{
            "status": "resolved",
            "target": { "session_id": "session:b", "window_id": "window:w" },
        }] })
        .to_string();
        let view = json!({
            "session_id": "session:b",
            "label": "b",
            "windows": [{ "window_id": "window:w", "label": "wm-feat", "layers": [] }],
        })
        .to_string();
        FakeRex::new(&[
            ("block call -b block:caller size", &owner(owner_id)),
            ("client.list", &json!({ "clients": clients }).to_string()),
            ("client.perform", select),
            ("target.resolve", &resolved),
            ("session.view", &view),
            ("session.focus_block", "{}"),
            ("session.focus_window", "{}"),
            ("session.list", &session_list(&[("session:b", "b")])),
        ])
        .in_pane(CALLER_SESSION, CALLER_BLOCK)
    }

    fn select_wm_feat(rex: &RexBackend) {
        rex.select_window_target(&WindowTarget::with_id(
            "wm-feat".into(),
            None,
            "window:w".into(),
        ))
        .unwrap();
    }

    /// Session B after `add`: the caller's pane in `main`, and the server
    /// focus on the new agent pane in `wm-feat`.
    fn after_add_fake() -> FakeRex {
        let window = |id: &str, label: &str, block: &str| {
            json!({
                "window_id": id,
                "label": label,
                "focused_block_id": block,
                "layers": [{ "kind": "tiled", "blocks": [{ "block_id": block }] }],
            })
        };
        let view = json!({
            "session_id": "session:b",
            "label": "b",
            "active_window_id": "window:feat",
            "windows": [
                window("window:main", "main", CALLER_BLOCK),
                window("window:feat", "wm-feat", "block:agent"),
            ],
        })
        .to_string();
        FakeRex::new(&[("session.view", &view)])
    }

    #[test]
    fn the_active_pane_is_the_callers_not_the_servers_focus() {
        let fake = after_add_fake().in_pane(CALLER_SESSION, CALLER_BLOCK);
        assert_eq!(fake.rex.active_pane_id().as_deref(), Some(CALLER_BLOCK));
    }

    #[test]
    fn the_active_pane_falls_back_to_the_servers_focus() {
        let fake = after_add_fake().in_pane(CALLER_SESSION, "block:closed");
        assert_eq!(fake.rex.active_pane_id().as_deref(), Some("block:agent"));
    }

    #[test]
    fn add_and_open_leave_the_app_alone() {
        // The app owns the caller's pane and is attached to its session, as
        // it is after showing the session once and moving on to another.
        let fake = app_fake(APP, &[app(APP, &[CALLER_SESSION])], "{}");
        fake.rex.select_pane("block:agent").unwrap();
        select_wm_feat(&fake.rex);
        fake.rex.select_window("", "wm-feat").unwrap();
        fake.rex.switch_to_session("", "b").unwrap();
        assert_eq!(fake.rex.shell_switch_session_cmd("b").unwrap(), "true");
        assert!(fake.calls("client.perform").is_empty());
        assert!(fake.calls("client.list").is_empty());
        assert_eq!(fake.calls("session.focus_block").len(), 1);
        assert_eq!(fake.calls("session.focus_window").len(), 2);
    }

    #[test]
    fn a_jump_shows_the_pane_on_the_app_that_owns_the_caller() {
        let fake = app_fake(APP, &[app(APP, &[CALLER_SESSION])], "{}");
        fake.rex.switch_to_pane("block:agent", None).unwrap();
        assert_eq!(
            fake.calls("client.perform"),
            [format!(
                "--autostart=false -S unix://{}/s.sock api call client.perform - \
                 {{\"action\":\"session.select\",\"args\":{{\"session_id\":\"session:b\",\
                 \"window_id\":\"window:w\"}},\"client_id\":\"client:app-1\",\"timeout_ms\":1000}}",
                fake.dir.path().display()
            )]
        );
        assert_eq!(fake.calls("session.focus_block").len(), 1);
    }

    #[test]
    fn a_refused_jump_fails_and_leaves_the_pane_unfocused() {
        let fake = app_fake(APP, &[app(APP, &[CALLER_SESSION])], REFUSED);
        let error = fake.rex.switch_to_pane("block:agent", None).unwrap_err();
        assert!(error.is::<AppRefused>());
        assert_eq!(error.to_string(), REMOTE_CONTROL_FIX);
        assert_eq!(fake.calls("client.perform").len(), 1);
        assert!(fake.calls("session.focus_block").is_empty());
    }

    #[test]
    fn a_jump_the_app_fails_for_another_reason_still_focuses_the_pane() {
        let fake = app_fake(APP, &[app(APP, &[CALLER_SESSION])], UNAVAILABLE);
        fake.rex.switch_to_pane("block:agent", None).unwrap();
        assert_eq!(fake.calls("client.perform").len(), 1);
        assert_eq!(fake.calls("session.focus_block").len(), 1);
    }

    #[test]
    fn an_app_that_never_answers_is_given_up_on() {
        let fake = app_fake(APP, &[app(APP, &[CALLER_SESSION])], "$exec sleep 30");
        let start = std::time::Instant::now();
        fake.rex.switch_to_pane("block:agent", None).unwrap();
        let waited = start.elapsed();
        assert!(
            waited >= APP_SELECT_TIMEOUT && waited < Duration::from_secs(8),
            "{waited:?}"
        );
        assert_eq!(fake.calls("session.focus_block").len(), 1);
    }

    #[test]
    fn only_a_refused_jump_prints_the_fix() {
        for (select, expected) in [
            ("{}", String::new()),
            (TIMEOUT, String::new()),
            (NOT_SUPPORTED, String::new()),
            (UNAVAILABLE, String::new()),
            (REFUSED_WITHOUT_CODE, String::new()),
            ("!Error: connection reset by peer", String::new()),
            (REFUSED, format!("{REMOTE_CONTROL_FIX}\n")),
        ] {
            let fake = app_fake(APP, &[app(APP, &[CALLER_SESSION])], select);
            let printed = AtomicBool::new(false);
            let mut out = Vec::new();
            let _ = fake.rex.jump_to_pane("block:agent", &printed, &mut out);
            assert_eq!(String::from_utf8(out).unwrap(), expected, "{select}");
        }
    }

    #[test]
    fn the_remote_control_fix_matches_the_docs() {
        assert_eq!(
            REMOTE_CONTROL_FIX,
            "Rex.app did not switch: Remote Control is off. Turn it on in Rex Settings."
        );
        let doc = include_str!("../../docs/src/content/docs/guide/rex.mdx");
        assert!(doc.contains(&format!("`{REMOTE_CONTROL_FIX}`")));
    }

    #[test]
    fn the_remote_control_fix_prints_once() {
        let printed = AtomicBool::new(false);
        let mut out = Vec::new();
        print_once(&printed, &mut out, REMOTE_CONTROL_FIX);
        print_once(&printed, &mut out, REMOTE_CONTROL_FIX);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("{REMOTE_CONTROL_FIX}\n")
        );
    }

    #[test]
    fn with_two_apps_only_the_one_owning_the_caller_is_asked() {
        let other = "client:app-2";
        let fake = app_fake(
            other,
            &[app(APP, &[CALLER_SESSION]), app(other, &[CALLER_SESSION])],
            "{}",
        );
        fake.rex.switch_to_pane("block:agent", None).unwrap();
        let selects = fake.calls("client.perform");
        assert_eq!(selects.len(), 1, "{selects:?}");
        assert!(
            selects[0].contains(r#""client_id":"client:app-2""#),
            "{selects:?}"
        );
    }

    #[test]
    fn a_jump_asks_no_owner_that_cannot_be_switched() {
        let owners = [
            // The app attaches to every session in its sidebar, the
            // caller's included, but another client owns the caller's pane.
            ("client:cli", vec![app(APP, &[CALLER_SESSION, "session:c"])]),
            (
                "client:cli",
                vec![client("client:cli", "cli", "connected", &[CALLER_SESSION])],
            ),
            (APP, vec![app(APP, &["session:c"])]),
            (APP, vec![client(APP, "app", "lost", &[CALLER_SESSION])]),
        ];
        for (owner, clients) in owners {
            let fake = app_fake(owner, &clients, "{}");
            fake.rex.switch_to_pane("block:agent", None).unwrap();
            assert!(
                fake.calls("client.perform").is_empty(),
                "{owner} {clients:?}"
            );
            assert_eq!(fake.calls("session.focus_block").len(), 1);
        }
    }

    #[test]
    fn outside_a_rex_pane_no_app_is_moved() {
        let mut fake = app_fake(APP, &[app(APP, &[CALLER_SESSION])], "{}");
        fake.rex.caller = Caller::default();
        fake.rex.switch_to_pane("block:agent", None).unwrap();
        assert!(fake.calls("client.perform").is_empty());
        assert!(fake.calls("client.list").is_empty());
    }

    #[test]
    fn client_list_reads_which_sessions_each_app_attached() {
        let list: ClientList = serde_json::from_str(CLIENT_LIST).unwrap();
        let app = ClientId("client:01a19e67-c952-7923-a102-902347c1c3c1".into());
        // A background session the app never showed: attachment alone does
        // not say which session the app shows.
        let background = SessionId("session:01a19ef9-36fc-7b33-82f2-f92a49dfc4a4".into());
        assert!(list.is_app_attached_to(&app, &background));
        assert!(!list.is_app_attached_to(&app, &SessionId("session:other".into())));
        let cli = ClientId("client:01a19ef9-3f38-7055-b9ee-b34985685609".into());
        assert!(!list.is_app_attached_to(&cli, &background));
    }

    fn view_json(session: &str, label: &str, windows: &[(&str, &str)]) -> String {
        let windows: Vec<Value> = windows
            .iter()
            .map(|(id, label)| json!({ "window_id": id, "label": label, "layers": [] }))
            .collect();
        json!({ "session_id": session, "label": label, "windows": windows }).to_string()
    }

    fn session_list(sessions: &[(&str, &str)]) -> String {
        let sessions: Vec<Value> = sessions
            .iter()
            .map(|(id, label)| json!({ "session_id": id, "label": label }))
            .collect();
        json!({ "sessions": sessions }).to_string()
    }

    #[test]
    fn killing_a_repeated_window_label_closes_one_window() {
        let fake = FakeRex::new(&[
            ("session.list", &session_list(&[("session:s", "proj")])),
            (
                "session.view",
                &view_json(
                    "session:s",
                    "proj",
                    &[("window:1", "wm-x"), ("window:2", "wm-x")],
                ),
            ),
            ("session.close_window", "{}"),
        ]);
        fake.rex.kill_window("wm-x").unwrap();
        let closes = fake.calls("session.close_window");
        assert_eq!(closes.len(), 1, "{closes:?}");
        assert!(closes[0].contains("window:1"), "{closes:?}");
    }

    #[test]
    fn killing_a_repeated_session_label_destroys_one_session() {
        let fake = FakeRex::new(&[
            (
                "session.list",
                &session_list(&[("session:1", "dup"), ("session:2", "dup")]),
            ),
            ("session.destroy", "{}"),
        ]);
        fake.rex.kill_session("dup").unwrap();
        let destroys = fake.calls("session.destroy");
        assert_eq!(destroys.len(), 1, "{destroys:?}");
        assert!(destroys[0].contains("session:1"), "{destroys:?}");
    }

    #[test]
    fn a_window_under_a_parent_session_is_found_in_that_session() {
        let fake = FakeRex::new(&[
            (
                "session.list",
                &session_list(&[("session:main", "main"), ("session:rp", "ReviewParents")]),
            ),
            (
                "session.view*session:main",
                &view_json("session:main", "main", &[("window:m", "wm-a")]),
            ),
            (
                "session.view*session:rp",
                &view_json("session:rp", "ReviewParents", &[("window:r", "wm-a")]),
            ),
        ]);
        let target = WindowTarget::new("wm-a".into(), Some("ReviewParents".into()));
        assert_eq!(
            fake.rex.target_window(&target).unwrap(),
            Some((SessionId("session:rp".into()), WindowId("window:r".into())))
        );
    }

    #[test]
    fn the_pane_scans_report_tiled_terminals_only() {
        let onboarding: SessionView = serde_json::from_str(VIEW_WITH_ONBOARDING).unwrap();
        let floating: SessionView = serde_json::from_str(VIEW_WITH_FLOATING).unwrap();
        let resolve = |view: &SessionView| {
            json!({ "results": [{
                "status": "resolved",
                "target": { "session_id": view.session_id.as_str() },
            }] })
            .to_string()
        };
        let onboarding_block = "block:01a19eaf-63fc-73f7-98de-5f7544025b81";
        let floating_block = "block:01a19e98-1751-7d75-a63c-2b457e2bb6ee";
        let fake = FakeRex::new(&[
            (
                "session.list",
                &session_list(&[
                    (onboarding.session_id.as_str(), "onb"),
                    (floating.session_id.as_str(), "wr5a"),
                ]),
            ),
            (
                &format!("session.view*{}", onboarding.session_id.as_str()),
                VIEW_WITH_ONBOARDING,
            ),
            (
                &format!("session.view*{}", floating.session_id.as_str()),
                VIEW_WITH_FLOATING,
            ),
            (
                &format!("target.resolve*{onboarding_block}"),
                &resolve(&onboarding),
            ),
            (
                &format!("target.resolve*{floating_block}"),
                &resolve(&floating),
            ),
            (" process ", PROCESS),
            (" title ", r#"{"title":""}"#),
        ]);
        let mut panes: Vec<_> = fake
            .rex
            .get_all_live_pane_info()
            .unwrap()
            .into_keys()
            .collect();
        panes.sort();
        assert_eq!(
            panes,
            [
                "block:01a19e95-dd18-7392-8f63-ecbea2aaae97",
                "block:01a19eb3-bba0-7877-a324-fd904ebf9873",
            ]
        );
        assert!(
            fake.rex
                .get_live_pane_info(onboarding_block)
                .unwrap()
                .is_none()
        );
        assert!(
            fake.rex
                .get_live_pane_info(floating_block)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn listing_on_a_server_whose_windows_cannot_be_read_still_lists_worktrees() {
        let repo = tempfile::tempdir().unwrap();
        crate::test_support::init_repo(repo.path());
        let fake = FakeRex::new(&[(
            "server.version",
            r#"{"rex_version":"0.0.1-old","api_revision":3}"#,
        )]);
        assert!(fake.rex.is_running().unwrap());
        assert!(fake.rex.get_all_window_names().is_err());
        let rows = crate::workflow::list_in(
            &crate::config::Config::default(),
            &fake.rex,
            false,
            &[],
            Some(repo.path()),
        )
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].has_mux_window);
    }

    #[test]
    fn every_spelling_of_one_socket_shares_one_version_check() {
        let fake = FakeRex::new(&[("session.list", &session_list(&[]))]);
        let socket = fake.dir.path().join("s.sock");
        std::fs::write(&socket, "").unwrap();
        let parent = fake
            .dir
            .path()
            .canonicalize()
            .unwrap()
            .display()
            .to_string();
        for spelling in [
            format!("unix://{parent}/s.sock"),
            format!("unix://{parent}//./s.sock"),
        ] {
            let rex = RexBackend {
                program: fake.rex.program.clone(),
                ..RexBackend::for_server(&spelling)
            };
            rex.sessions().unwrap();
        }
        assert_eq!(fake.calls("server.version").len(), 1);
    }

    const SESSION_VIEW: [&str; 4] = ["api", "call", "session.view", "-"];
    const CLOSE: [&str; 4] = ["api", "call", "session.close_window", "-"];

    impl FakeRex {
        /// Reuses reads for as long as a test runs, so only writes drop them.
        fn reusing(mut self) -> Self {
            self.rex.reuse = Duration::from_secs(3600);
            self
        }

        fn write(&self, name: &str, text: &str) {
            std::fs::write(self.dir.path().join(name), text).unwrap();
        }

        fn wait_for(&self, name: &str) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.dir.path().join(name).exists() {
                assert!(Instant::now() < deadline, "the fake rex never wrote {name}");
                thread::sleep(Duration::from_millis(5));
            }
        }

        fn view(&self) -> String {
            self.rex.run(&SESSION_VIEW, Some("{}")).unwrap()
        }
    }

    #[test]
    fn the_reads_a_command_reuses_are_these() {
        assert_eq!(
            READ_ONLY_API,
            [
                "session.list",
                "session.view",
                "target.resolve",
                "server.version",
                "client.list",
            ]
        );
        assert_eq!(READ_ONLY_BLOCK, ["process", "title", "size", "format"]);
    }

    #[test]
    fn every_write_drops_the_reads_before_it() {
        let block = |method| vec!["block", "call", "-b", "block:x", method, "-"];
        let api = |method| vec!["api", "call", method, "-"];
        let mut writes: Vec<Vec<&str>> = [
            "session.create",
            "session.destroy",
            "session.new_window",
            "session.move_window",
            "session.close_window",
            "session.focus_window",
            "session.set_window_label",
            "session.set_label",
            "session.new_split",
            "session.focus_block",
            "session.zoom_block",
            "session.set_block_label",
            "block.close",
            "block.event",
            "client.perform",
        ]
        .into_iter()
        .map(api)
        .collect();
        writes.push(block("write"));
        writes.push(vec!["send-key", "-b", "block:x", "Enter"]);
        let mut reads: Vec<Vec<&str>> = READ_ONLY_API.into_iter().map(api).collect();
        reads.extend(READ_ONLY_BLOCK.map(block));
        reads.push(vec!["whoami", "--json"]);

        for (call, drops) in writes
            .iter()
            .map(|call| (call, true))
            .chain(reads.iter().map(|call| (call, false)))
        {
            let fake = FakeRex::new(&[
                ("session.view", "{}"),
                ("server.version", r#"{"rex_version":"t","api_revision":8}"#),
                ("", "{}"),
            ])
            .reusing();
            fake.view();
            fake.rex.run(call, Some("{}")).unwrap();
            fake.view();
            let views = if drops { 2 } else { 1 };
            assert_eq!(fake.calls("session.view").len(), views, "{call:?}");
        }
    }

    #[test]
    fn a_read_that_spans_a_write_is_not_reused() {
        let fake = FakeRex::new(&[
            (
                "session.view",
                "$s=$(cat \"$dir/state\"); touch \"$dir/started\"; \
                 while [ ! -e \"$dir/release\" ]; do sleep 0.01; done; printf %s \"$s\"",
            ),
            ("session.close_window", "$echo new > \"$dir/state\""),
        ])
        .reusing();
        fake.write("state", "old");
        thread::scope(|scope| {
            let reader = scope.spawn(|| fake.view());
            fake.wait_for("started");
            fake.rex.run(&CLOSE, Some("{}")).unwrap();
            fake.write("release", "");
            assert_eq!(reader.join().unwrap(), "old");
        });
        assert_eq!(fake.view().trim(), "new");
    }

    #[test]
    fn a_read_made_during_a_write_is_not_reused_after_it() {
        let fake = FakeRex::new(&[
            ("session.view", "$cat \"$dir/state\""),
            (
                "session.close_window",
                "$touch \"$dir/writing\"; while [ ! -e \"$dir/read\" ]; do sleep 0.01; done; \
                 echo new > \"$dir/state\"",
            ),
        ])
        .reusing();
        fake.write("state", "old");
        thread::scope(|scope| {
            let writer = scope.spawn(|| fake.rex.run(&CLOSE, Some("{}")).unwrap());
            fake.wait_for("writing");
            assert_eq!(fake.view(), "old");
            fake.write("read", "");
            writer.join().unwrap();
        });
        assert_eq!(fake.view().trim(), "new");
    }

    #[test]
    fn a_failed_write_still_drops_the_reads_before_it() {
        let fake = FakeRex::new(&[
            ("session.view", "$cat \"$dir/state\""),
            (
                "session.close_window",
                "$echo new > \"$dir/state\"; echo 'Error: boom' >&2; exit 1",
            ),
        ])
        .reusing();
        fake.write("state", "old");
        assert_eq!(fake.view(), "old");
        assert!(fake.rex.run(&CLOSE, Some("{}")).is_err());
        assert_eq!(fake.view().trim(), "new");
    }

    #[test]
    fn a_timed_out_write_still_drops_the_reads_before_it() {
        let fake = FakeRex::new(&[
            ("session.view", "{}"),
            ("client.perform", "$while :; do sleep 0.01; done"),
        ])
        .reusing();
        fake.view();
        let perform = ["api", "call", "client.perform", "-"];
        let result = fake
            .rex
            .exec_within(&perform, Some("{}"), Some(Duration::from_millis(100)));
        assert!(result.is_err());
        fake.view();
        assert_eq!(fake.calls("session.view").len(), 2);
    }

    /// A read that takes most of the reuse window expires a full window
    /// after it started, not after it returned.
    #[test]
    fn a_read_is_reused_for_one_window_from_its_start() {
        let mut fake = FakeRex::new(&[(
            "session.view",
            "$touch \"$dir/started\"; \
             while [ ! -e \"$dir/release\" ]; do sleep 0.01; done; echo '{}'",
        )]);
        fake.rex.reuse = Duration::from_secs(1);
        thread::scope(|scope| {
            let reader = scope.spawn(|| fake.view());
            fake.wait_for("started");
            let started = Instant::now();
            thread::sleep(Duration::from_millis(800));
            fake.write("release", "");
            reader.join().unwrap();
            let expired = started + Duration::from_millis(1100);
            thread::sleep(expired.saturating_duration_since(Instant::now()));
        });
        fake.view();
        assert_eq!(fake.calls("session.view").len(), 2);
    }

    #[test]
    fn each_session_has_its_own_view() {
        let fake = FakeRex::new(&[
            ("session.view*session:a", &view_json("session:a", "a", &[])),
            ("session.view*session:b", &view_json("session:b", "b", &[])),
        ])
        .reusing();
        for label in ["a", "b"] {
            let view = fake
                .rex
                .view(&SessionId(format!("session:{label}")))
                .unwrap();
            assert_eq!(view.label, label);
        }
    }

    /// reconcile looks again at each agent whose pane its batch read missed,
    /// and prunes the agent when the second look also misses it. A pane
    /// made after the batch, or a window that moved between two sessions
    /// while the batch read them, must show on the second look.
    #[test]
    fn a_second_look_at_a_pane_the_batch_missed_reads_afresh() {
        let session = "session:01a19e60-6410-7e09-a3ee-f675512a3a06";
        let block = "block:01a19e60-6442-727c-b026-9bb98fffd538";
        let fake = FakeRex::new(&[
            ("session.list", &session_list(&[(session, "fixture")])),
            ("session.view", "$cat \"$dir/view\""),
            ("target.resolve", RESOLVE),
            (" process ", PROCESS),
            (" title ", r#"{"title":""}"#),
        ])
        .reusing();
        fake.write("view", &view_json(session, "fixture", &[]));
        assert!(fake.rex.get_all_live_pane_info().unwrap().is_empty());
        fake.write("view", VIEW);
        let agent: crate::state::AgentState = serde_json::from_value(json!({
            "pane_key": { "backend": "rex", "instance": "unix:///s.sock", "pane_id": block },
            "workdir": "/private/tmp",
            "pane_pid": 85152,
            "command": "sleep",
            "updated_ts": 0,
        }))
        .unwrap();
        assert!(fake.rex.validate_agent_alive(&agent).unwrap());
    }

    #[test]
    fn outside_changes_show_within_a_quarter_second() {
        assert_eq!(
            RexBackend::with_server(None).reuse,
            Duration::from_millis(250)
        );
    }
}
