//! The Rex control plane: places, sizes and removes sidebar blocks. The CLI
//! runs one pass over every session; the daemon runs a pass per session as
//! its events arrive. Both hold the block records lock for the pass.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{ErrorKind, Read};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{Config, SidebarWidth};
use crate::multiplexer::rex::RexBackend;
use crate::multiplexer::rex::events::{Event, EventStream};
use crate::multiplexer::rex::sidebar_api::{View, Window};

use super::link::{self, Failure, Link, Server};
use super::observe::Observed;
use super::size::{self, SizeAction};
use super::store::{self, Files, Options, Records, WindowSidebar};

/// How long the daemon gathers events before acting on a batch. A layout
/// change arrives as several events in a few milliseconds. The window is
/// fixed rather than reset by each event: agents retitle their terminals
/// many times a second, so a busy server never goes quiet for 50 ms.
const SETTLE: Duration = Duration::from_millis(50);
/// Re-read every session this often in case an event was lost.
const SAFETY_REBUILD: Duration = Duration::from_secs(30);

/// Which sessions get a sidebar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Scope {
    Off,
    Global { optout: HashSet<String> },
    Sessions(HashSet<String>),
}

impl Scope {
    pub fn from_prefs(prefs: &Options) -> Self {
        match prefs.global(store::SCOPE).map(str::trim) {
            None | Some("") => Scope::Off,
            Some("global") => Scope::Global {
                optout: prefs.global_set(store::OPTOUT),
            },
            Some(_) => Scope::Sessions(prefs.global_set(store::SCOPE)),
        }
    }

    /// Store the scope. Turning the sidebar off also drops the settings that
    /// last only while it is on.
    pub fn write(&self, prefs: &mut Options) {
        match self {
            Scope::Off => {
                for name in [
                    store::SCOPE,
                    store::OPTOUT,
                    store::POSITION,
                    store::WIDTH,
                    store::SLEEPING,
                ] {
                    prefs.set_global(name, None);
                }
            }
            Scope::Global { optout } => {
                prefs.set_global(store::SCOPE, Some("global".into()));
                prefs.set_global_set(store::OPTOUT, optout);
            }
            Scope::Sessions(ids) => {
                prefs.set_global_set(store::SCOPE, ids);
                prefs.set_global(store::OPTOUT, None);
            }
        }
    }

    pub fn includes(&self, session_id: &str) -> bool {
        match self {
            Scope::Off => false,
            Scope::Global { optout } => !optout.contains(session_id),
            Scope::Sessions(ids) => ids.contains(session_id),
        }
    }
}

/// Which windows of a session a pass may add a sidebar to.
enum Create<'a> {
    /// `sidebar on`: every window, the ones whose sidebar the user closed
    /// included.
    Reopen,
    /// Every window but those whose sidebar the user closed.
    All,
    Only(&'a HashSet<String>),
}

impl Create<'_> {
    fn allows(&self, window: &str, closed: &Closed) -> bool {
        match self {
            Create::Reopen => true,
            Create::All => !closed.contains_key(window),
            Create::Only(windows) => windows.contains(window) && !closed.contains_key(window),
        }
    }
}

/// Windows whose sidebar the user closed, with their session ids.
type Closed = BTreeMap<String, String>;

#[derive(Default)]
struct PassResult {
    /// Windows whose sidebar ratio needs reading back from Rex.
    placed: HashSet<String>,
    dragged: Option<u16>,
}

/// One pass's inputs that do not change during it.
pub(super) struct Plane {
    pub rex: RexBackend,
    pub files: Files,
    config: Config,
    configured_width: Option<SidebarWidth>,
    command: Vec<String>,
    cwd: PathBuf,
    /// A sidebar block the pass must leave in place: the one a quitting
    /// sidebar runs in, which it closes last.
    spare: Option<String>,
    /// The daemon records which sidebars the user closed; other passes
    /// leave a closed sidebar's record for it to find.
    daemon: bool,
}

impl Plane {
    pub fn new(rex: RexBackend, files: Files, config: Config) -> Result<Self> {
        let command = sidebar_command(&rex, &files)?;
        let configured_width = config.sidebar.width.clone();
        Ok(Self {
            rex,
            files,
            config,
            configured_width,
            command,
            cwd: dirs_home(),
            spare: None,
            daemon: false,
        })
    }

    pub fn for_daemon(mut self) -> Self {
        self.daemon = true;
        self
    }

    pub fn sparing(mut self, block_id: &str) -> Self {
        self.spare = Some(block_id.to_string());
        self
    }

    fn reload_config(&mut self) {
        if let Ok(config) = Config::load(None) {
            self.configured_width = config.sidebar.width.clone();
            self.config = config;
        }
    }

    /// A `--width` given to `sidebar on` stays in effect like a configured
    /// width until the sidebar is turned off.
    fn width_policy(&mut self, prefs: &Options) -> (bool, Option<u16>) {
        let cli_width = prefs
            .global(store::WIDTH)
            .and_then(|value| value.parse::<SidebarWidth>().ok());
        self.config.sidebar.width = cli_width.or_else(|| self.configured_width.clone());
        let synced = crate::state::StateStore::new()
            .and_then(|store| store.load_settings())
            .ok()
            .and_then(|settings| settings.sidebar_width);
        (self.config.sidebar.width.is_some(), synced)
    }

    fn target(&self, window_cols: u32, synced: Option<u16>) -> u16 {
        crate::command::sidebar::resolve_width_for(&self.config, size::extent(window_cols), synced)
    }

    /// Bring every session in line with the scope, adding sidebars where
    /// they are missing. `reopen` also brings back the ones the user closed.
    pub fn reconcile_all(&mut self, reopen: bool) -> Result<Records> {
        // Views are read under the lock: a view from before another pass
        // placed a sidebar would make that sidebar look lost.
        let mut guard = self.files.lock_records()?;
        let views = self.rex.sidebar_views()?;
        let sessions: HashSet<&str> = views.iter().map(|view| view.session_id.as_str()).collect();
        guard
            .records
            .retain(|_, record| sessions.contains(record.session_id.as_str()));
        let views = views
            .into_iter()
            .map(|view| (view.session_id.clone(), Some(view)));
        let create = if reopen { Create::Reopen } else { Create::All };
        let result = self.sync(
            &mut guard.records,
            views.collect(),
            create,
            &HashMap::new(),
            None,
        );
        guard.save()?;
        result.map(|_| guard.records.clone())
    }

    /// Run a pass over each session in `views`, then spread a dragged width
    /// to the other windows. Returns the views as they stand afterwards.
    fn sync(
        &mut self,
        records: &mut Records,
        views: Vec<(String, Option<View>)>,
        create: Create,
        sizes: &HashMap<String, u32>,
        observed: Option<&Observed>,
    ) -> Result<BTreeMap<String, View>> {
        let prefs = self.files.prefs();
        let scope = Scope::from_prefs(&prefs);
        let (configured, mut synced) = self.width_policy(&prefs);
        let closed_before = self.files.daemon().windows_with(store::CLOSED);
        let mut closed = closed_before.clone();
        let mut current = BTreeMap::new();
        let mut dragged = None;
        let mut first_error = None;
        for (session_id, view) in views {
            let Some(view) = view else {
                records.retain(|_, record| record.session_id != session_id);
                closed.retain(|_, session| *session != session_id);
                if let Some(observed) = observed {
                    observed.drop_session(&session_id);
                }
                continue;
            };
            closed.retain(|window, session| {
                *session != session_id || view.windows.iter().any(|w| w.window_id == *window)
            });
            let result = self.pass(
                records,
                &view,
                &scope,
                &create,
                &mut closed,
                sizes,
                configured,
                synced,
            );
            let view = match result {
                Ok(result) => {
                    dragged = dragged.or(result.dragged);
                    if result.placed.is_empty() {
                        view
                    } else {
                        self.read_back(records, &session_id, &result.placed)
                            .unwrap_or(view)
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, session_id, "sidebar pass failed");
                    first_error.get_or_insert(error);
                    view
                }
            };
            current.insert(session_id, view);
        }

        if let Some(cols) = dragged {
            if let Err(error) = persist_width(cols) {
                tracing::warn!(%error, "failed to persist the dragged sidebar width");
            }
            synced = Some(cols);
            self.spread(records, &mut current, &scope, configured, synced)?;
        }

        if self.daemon {
            closed.retain(|window, _| !records.contains_key(window));
            if closed != closed_before
                && let Err(error) = self.files.update_daemon(|daemon| {
                    for window in closed_before.keys() {
                        daemon.set_window(window, store::CLOSED, None);
                    }
                    for (window, session) in &closed {
                        daemon.set_window(window, store::CLOSED, Some(session.clone()));
                    }
                })
            {
                tracing::warn!(%error, "failed to record closed sidebars");
            }
        }
        if let Some(observed) = observed {
            let sidebars = sidebar_blocks(records);
            for view in current.values() {
                observed.put_view(&self.rex, view.clone(), &sidebars);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(current),
        }
    }

    /// Apply a new synced width to every sidebar not already at it.
    fn spread(
        &mut self,
        records: &mut Records,
        current: &mut BTreeMap<String, View>,
        scope: &Scope,
        configured: bool,
        synced: Option<u16>,
    ) -> Result<()> {
        let sessions: HashSet<String> = records
            .values()
            .map(|record| record.session_id.clone())
            .collect();
        for session_id in sessions {
            let view = match current.get(&session_id) {
                Some(view) => view.clone(),
                None => match self.rex.sidebar_view(&session_id)? {
                    Some(view) => view,
                    None => continue,
                },
            };
            let result = self.pass(
                records,
                &view,
                scope,
                &Create::Only(&HashSet::new()),
                &mut Closed::new(),
                &HashMap::new(),
                configured,
                synced,
            )?;
            if !result.placed.is_empty()
                && let Some(view) = self.read_back(records, &session_id, &result.placed)
            {
                current.insert(session_id, view);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn pass(
        &self,
        records: &mut Records,
        view: &View,
        scope: &Scope,
        create: &Create,
        closed: &mut Closed,
        sizes: &HashMap<String, u32>,
        configured: bool,
        synced: Option<u16>,
    ) -> Result<PassResult> {
        let mut result = PassResult::default();
        let session_id = view.session_id.as_str();
        let in_scope = scope.includes(session_id);
        let windows: HashMap<&str, &Window> = view
            .windows
            .iter()
            .map(|window| (window.window_id.as_str(), window))
            .collect();

        let ours: Vec<String> = records
            .values()
            .filter(|record| record.session_id == session_id)
            .map(|record| record.window_id.clone())
            .collect();
        for window_id in ours {
            let record = records[&window_id].clone();
            let Some(window) = windows.get(window_id.as_str()) else {
                records.remove(&window_id);
                continue;
            };
            if !window.block_ids().any(|block| block == record.block_id) {
                if self.daemon {
                    closed.insert(window_id.clone(), session_id.to_string());
                    records.remove(&window_id);
                } else if matches!(create, Create::Reopen) || !in_scope {
                    records.remove(&window_id);
                }
                continue;
            }
            if self.spare.as_deref() == Some(record.block_id.as_str()) {
                continue;
            }
            if window.block_ids().all(|block| block == record.block_id) {
                self.rex.close_window_by_id(session_id, &window_id)?;
                records.remove(&window_id);
                continue;
            }
            if !in_scope {
                self.rex.close_sidebar_block(session_id, &record.block_id)?;
                records.remove(&window_id);
                continue;
            }
            let Some(root) = window
                .root_split()
                .filter(|root| root.before.block_id.as_deref() == Some(&record.block_id))
            else {
                continue;
            };
            let observed_cols = sizes.get(&record.block_id).copied();
            let window_cols = observed_cols
                .and_then(|cols| size::window_cols(cols, root.ratio))
                .unwrap_or(record.window_cols);
            let cols = observed_cols.unwrap_or(record.last_cols);
            let target = self.target(window_cols, synced);
            let action = size::decide(&record, root.ratio, cols, window_cols, target, configured);
            let record = records.get_mut(&window_id).expect("record exists");
            record.window_cols = window_cols;
            match action {
                SizeAction::Keep => record.last_cols = cols,
                SizeAction::Enforce { ratio } | SizeAction::SnapBack { ratio } => {
                    let Some((neighbor, _)) = root.after.first_block() else {
                        continue;
                    };
                    self.rex
                        .resize_sidebar(session_id, &record.block_id, neighbor, ratio)?;
                    record.last_cols = u32::from(target);
                    result.placed.insert(window_id.clone());
                }
                SizeAction::PersistDrag { cols } => {
                    record.applied_ratio = root.ratio;
                    record.last_cols = u32::from(cols);
                    result.dragged = Some(cols);
                }
            }
        }

        if in_scope {
            for window in &view.windows {
                if records.contains_key(&window.window_id)
                    || !create.allows(&window.window_id, closed)
                {
                    continue;
                }
                if let Some(record) = self.place(session_id, window, synced)? {
                    result.placed.insert(window.window_id.clone());
                    records.insert(window.window_id.clone(), record);
                }
            }
        }
        Ok(result)
    }

    fn place(
        &self,
        session_id: &str,
        window: &Window,
        synced: Option<u16>,
    ) -> Result<Option<WindowSidebar>> {
        let (Some(anchor), Some(layout)) = (
            window.anchor(),
            window.tiled().and_then(|layer| layer.layout.as_ref()),
        ) else {
            return Ok(None);
        };
        let Some((measured, rect_w)) = layout.first_block() else {
            return Ok(None);
        };
        let window_cols = self
            .rex
            .block_size(measured)?
            .and_then(|size| size::window_cols(size.columns, rect_w))
            .unwrap_or(80);
        let target = self.target(window_cols, synced);
        let ratio = size::sidebar_ratio(target, window_cols);
        let created =
            self.rex
                .new_sidebar_split(session_id, anchor, ratio, &self.cwd, &self.command)?;
        Ok(Some(WindowSidebar {
            session_id: session_id.to_string(),
            window_id: window.window_id.clone(),
            block_id: created.block_id,
            split_id: created.split_id,
            applied_ratio: ratio,
            last_cols: u32::from(target),
            window_cols,
        }))
    }

    /// Record the ratios Rex settled on for sidebars just placed or resized,
    /// so the view change our own call causes is not read as a drag.
    fn read_back(
        &self,
        records: &mut Records,
        session_id: &str,
        placed: &HashSet<String>,
    ) -> Option<View> {
        let view = self.rex.sidebar_view(session_id).ok().flatten()?;
        for window in &view.windows {
            if !placed.contains(&window.window_id) {
                continue;
            }
            if let (Some(record), Some(root)) =
                (records.get_mut(&window.window_id), window.root_split())
                && root.before.block_id.as_deref() == Some(&record.block_id)
            {
                record.applied_ratio = root.ratio;
                record.split_id = root.split_id.clone();
            }
        }
        Some(view)
    }
}

fn persist_width(cols: u16) -> Result<()> {
    let store = crate::state::StateStore::new()?;
    let mut settings = store.load_settings()?;
    settings.sidebar_width = Some(cols);
    store.save_settings(&settings)
}

pub(super) fn sidebar_blocks(records: &Records) -> HashSet<String> {
    records
        .values()
        .map(|record| record.block_id.clone())
        .collect()
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Variables that place the daemon socket, state and config. Blocks inherit
/// the server's environment, not ours, so a sidebar started from a shell with
/// other values would look for another daemon.
const PASSED_ON: [&str; 4] = ["HOME", "TMPDIR", "XDG_STATE_HOME", "XDG_CONFIG_HOME"];

/// The command a sidebar block runs: our environment, the server address
/// and the `rex` location, with tmux pointed at an empty socket directory.
fn sidebar_command(rex: &RexBackend, files: &Files) -> Result<Vec<String>> {
    let exe = std::env::current_exe().context("Failed to locate the workmux binary")?;
    let mut command: Vec<String> = ["/usr/bin/env", "-u", "TMUX", "-u", "TMUX_PANE"]
        .map(String::from)
        .to_vec();
    for name in PASSED_ON {
        if let Some(value) = std::env::var_os(name) {
            command.push(format!("{name}={}", value.to_string_lossy()));
        }
    }
    command.push(format!("TMUX_TMPDIR={}", files.no_tmux_dir()?.display()));
    command.push("WORKMUX_BACKEND=rex".to_string());
    if let Some(server) = rex.server_address() {
        command.push(format!("REX_SERVER={server}"));
    }
    if let Some(dir) = rex_bin_dir() {
        command.push(format!("REX_BIN_DIR={}", dir.display()));
    }
    command.push(exe.to_string_lossy().into_owned());
    command.push("_sidebar-run".to_string());
    Ok(command)
}

pub(super) fn rex_bin_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("REX_BIN_DIR") {
        return Some(PathBuf::from(dir));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find(|dir| dir.join("rex").is_file())
}

/// The environment for a process that must not reach a tmux server.
pub(super) fn contain(
    command: &mut std::process::Command,
    rex: &RexBackend,
    files: &Files,
) -> Result<()> {
    command
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("TMUX_TMPDIR", files.no_tmux_dir()?)
        .env("WORKMUX_BACKEND", "rex");
    if let Some(server) = rex.server_address() {
        command.env("REX_SERVER", server);
    }
    Ok(())
}

/// Run the control plane for the daemon. When the server is gone it ends
/// the daemon, whatever clients are still connected.
pub(super) fn spawn(plane: Plane, observed: Arc<Observed>, socket: PathBuf) {
    keep_alive(socket);
    thread::spawn(move || {
        let mut follower = Follower {
            plane: plane.for_daemon(),
            observed,
            stream: None,
            work: Work::default(),
        };
        let mut link = Link::start();
        while link != Link::Gone {
            link = link.step(&mut follower);
        }
        follower.disconnect();
        tracing::info!("rex server gone; stopping the sidebar daemon");
        let _ = signal_hook::low_level::raise(signal_hook::consts::SIGTERM);
    });
}

/// The daemon exits ten seconds after its last client, and under Rex no hook
/// restarts it. So the control plane holds a client connection of its own,
/// reading and dropping the snapshots it gets, and the daemon lives as long
/// as its server.
fn keep_alive(socket: PathBuf) {
    thread::spawn(move || {
        let mut buf = [0u8; 64 * 1024];
        loop {
            let Ok(mut stream) = UnixStream::connect(&socket) else {
                thread::sleep(Duration::from_millis(50));
                continue;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                        ) => {}
                    Err(_) => break,
                }
            }
        }
    });
}

/// Work a failed pass leaves for the next one.
#[derive(Default)]
struct Work {
    /// Windows owed a sidebar, with their session ids.
    created: BTreeMap<String, String>,
    /// A pass over every session is owed.
    sweep: bool,
    /// The owed sweep may add a sidebar to any window: the server is not
    /// the one last seen.
    full: bool,
    failures: u32,
    retry_at: Option<Instant>,
}

impl Work {
    fn owed(&self) -> bool {
        self.sweep || !self.created.is_empty()
    }

    fn done(&mut self) {
        *self = Work::default();
    }

    fn failed(&mut self, error: anyhow::Error) {
        tracing::warn!(%error, "sidebar control plane pass failed");
        self.sweep = true;
        self.failures += 1;
        self.retry_at = Some(Instant::now() + link::backoff(self.failures));
    }

    /// Settle a pass over the sessions of every owed window. A failed pass
    /// keeps them owed.
    fn settle(&mut self, pass: Result<()>) {
        match pass {
            Ok(()) => self.created.clear(),
            Err(error) => self.failed(error),
        }
    }
}

/// The daemon's side of the server: the event stream and the passes it
/// drives.
struct Follower {
    plane: Plane,
    observed: Arc<Observed>,
    stream: Option<EventStream>,
    work: Work,
}

impl Follower {
    fn disconnect(&mut self) {
        self.observed.events_pid.store(0, Ordering::Relaxed);
        self.stream = None;
    }

    /// Run owed work if there is any, keeping it on failure.
    fn run_owed(&mut self) {
        if !self.work.owed() {
            return;
        }
        match self.sweep() {
            Ok(()) => {
                self.work.done();
                wake_daemon();
            }
            Err(error) => self.work.failed(error),
        }
    }

    /// Read every session and pass over it. Windows the cache has not seen
    /// were created while no event reached us, so they are owed a sidebar.
    fn sweep(&mut self) -> Result<()> {
        let plane = &mut self.plane;
        let mut guard = plane.files.lock_records()?;
        let views = plane.rex.sidebar_views()?;
        let known = self.observed.window_ids();
        for view in &views {
            for window in &view.windows {
                if !known.contains(&window.window_id) {
                    self.work
                        .created
                        .insert(window.window_id.clone(), view.session_id.clone());
                }
            }
        }
        let live: HashSet<&str> = views.iter().map(|view| view.session_id.as_str()).collect();
        let gone: HashSet<String> = guard
            .records
            .values()
            .map(|record| record.session_id.clone())
            .chain(
                plane
                    .files
                    .daemon()
                    .windows_with(store::CLOSED)
                    .into_values(),
            )
            .filter(|session_id| !live.contains(session_id.as_str()))
            .collect();
        let mut sessions: Vec<(String, Option<View>)> = gone
            .into_iter()
            .map(|session_id| (session_id, None))
            .collect();
        sessions.extend(
            views
                .into_iter()
                .map(|view| (view.session_id.clone(), Some(view))),
        );
        let windows: HashSet<String> = self.work.created.keys().cloned().collect();
        let create = if self.work.full {
            Create::All
        } else {
            Create::Only(&windows)
        };
        let current = plane.sync(&mut guard.records, sessions, create, &HashMap::new(), None);
        guard.save()?;
        let sidebars = sidebar_blocks(&guard.records);
        drop(guard);
        self.observed
            .replace_all(&plane.rex, current?.into_values().collect(), &sidebars);
        Ok(())
    }

    /// Pass over the sessions `batch` touched. Returns whether the daemon
    /// should observe at once.
    fn handle(&mut self, batch: Vec<Event>) -> bool {
        let sidebars = sidebar_blocks(&self.plane.files.records());
        let mut sessions: Vec<String> = Vec::new();
        let mut sizes = HashMap::new();
        let mut destroyed = HashSet::new();
        let mut wake = false;
        let touch = |sessions: &mut Vec<String>, session_id: &str| {
            if !sessions.iter().any(|id| id == session_id) {
                sessions.push(session_id.to_string());
            }
        };
        for event in &batch {
            self.observed.apply(event);
            match event {
                Event::WindowCreated {
                    session_id,
                    window_id,
                } => {
                    self.work
                        .created
                        .insert(window_id.clone(), session_id.clone());
                    wake = true;
                }
                Event::SessionCreated { session_id }
                | Event::WindowClosed { session_id, .. }
                | Event::BlockClosed { session_id, .. }
                | Event::ViewChanged { session_id } => {
                    touch(&mut sessions, session_id);
                    wake = true;
                }
                Event::SessionDestroyed { session_id } => {
                    destroyed.insert(session_id.clone());
                    wake = true;
                }
                Event::SizeChanged {
                    session_id,
                    block_id,
                    columns,
                    ..
                } if sidebars.contains(block_id) => {
                    sizes.insert(block_id.clone(), *columns);
                    touch(&mut sessions, session_id);
                }
                Event::ProgramStatusChanged { .. }
                | Event::ProgramStatusRemoved { .. }
                | Event::ChildExited { .. } => {
                    wake = true;
                }
                _ => {}
            }
        }
        for session_id in self.work.created.values() {
            touch(&mut sessions, session_id);
        }
        sessions.retain(|session_id| !destroyed.contains(session_id));

        if !sessions.is_empty() || !destroyed.is_empty() {
            let pass = self.pass_over(sessions, destroyed, sizes);
            self.work.settle(pass);
        }
        wake
    }

    fn pass_over(
        &mut self,
        sessions: Vec<String>,
        destroyed: HashSet<String>,
        sizes: HashMap<String, u32>,
    ) -> Result<()> {
        let plane = &mut self.plane;
        let mut guard = plane.files.lock_records()?;
        let mut views: Vec<(String, Option<View>)> = destroyed
            .into_iter()
            .map(|session_id| (session_id, None))
            .collect();
        for session_id in sessions {
            let view = plane.rex.sidebar_view(&session_id)?;
            views.push((session_id, view));
        }
        let windows: HashSet<String> = self.work.created.keys().cloned().collect();
        let result = plane.sync(
            &mut guard.records,
            views,
            Create::Only(&windows),
            &sizes,
            Some(&self.observed),
        );
        guard.save()?;
        result.map(drop)
    }
}

impl Server for Follower {
    fn connect(&mut self) -> Result<String, Failure> {
        self.disconnect();
        let stream = self.plane.rex.events()?;
        let boot_id = self.plane.rex.boot_id()?;
        self.observed
            .events_pid
            .store(stream.pid(), Ordering::Relaxed);
        self.stream = Some(stream);
        Ok(boot_id)
    }

    fn resync(&mut self, full: bool) {
        self.work.sweep = true;
        self.work.full |= full;
        self.work.retry_at = None;
        self.run_owed();
    }

    fn follow(&mut self) {
        let mut next_sweep = Instant::now() + SAFETY_REBUILD;
        loop {
            let Some(stream) = &self.stream else {
                return;
            };
            let due = self
                .work
                .retry_at
                .filter(|_| self.work.owed())
                .map_or(next_sweep, |at| at.min(next_sweep));
            let batch = match stream
                .receiver()
                .recv_timeout(due.saturating_duration_since(Instant::now()))
            {
                Ok(first) => Some(gather(first, stream.receiver(), SETTLE)),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.disconnect();
                    return;
                }
            };
            if let Some(batch) = batch
                && self.handle(batch)
            {
                wake_daemon();
            }
            let now = Instant::now();
            if now >= next_sweep {
                next_sweep = now + SAFETY_REBUILD;
                self.plane.reload_config();
                self.work.sweep = true;
                self.run_owed();
            } else if self.work.retry_at.is_some_and(|at| now >= at) {
                self.run_owed();
            }
        }
    }

    fn wait(&mut self, delay: Duration) {
        thread::sleep(delay);
    }
}

/// `first` and every event that arrives within `settle` of it.
fn gather<T>(first: T, rx: &mpsc::Receiver<T>, settle: Duration) -> Vec<T> {
    let mut batch = vec![first];
    let deadline = Instant::now() + settle;
    while let Some(left) = deadline.checked_duration_since(Instant::now())
        && let Ok(event) = rx.recv_timeout(left)
    {
        batch.push(event);
    }
    batch
}

/// Ask this daemon's tick loop for an observation now, through the same
/// signal `workmux sidebar` sends it.
fn wake_daemon() {
    let _ = signal_hook::low_level::raise(signal_hook::consts::SIGUSR1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_pass_keeps_its_windows_owed_and_retries_on_backoff() {
        let mut work = Work::default();
        work.created.insert("window:1".into(), "session:1".into());
        let before = Instant::now();
        work.settle(Err(anyhow::anyhow!("session.view timed out")));
        assert!(work.created.contains_key("window:1"));
        assert!(work.sweep);
        let retry_at = work.retry_at.expect("a retry");
        assert!(retry_at >= before + link::backoff(1));
        assert!(retry_at <= Instant::now() + link::backoff(1));

        work.settle(Ok(()));
        assert!(work.created.is_empty());
    }

    /// A follower whose every Rex call fails: its server's socket does not
    /// exist.
    fn unreachable_follower(dir: &tempfile::TempDir) -> Follower {
        let rex = RexBackend::for_server("unix:///nonexistent/wr6-test.sock");
        let files = Files::at(dir.path().to_path_buf(), "test");
        let plane = Plane::new(rex, files, Config::default()).unwrap();
        Follower {
            plane,
            observed: Arc::default(),
            stream: None,
            work: Work::default(),
        }
    }

    #[test]
    fn a_failed_event_pass_schedules_a_retry() {
        let dir = tempfile::tempdir().unwrap();
        let mut follower = unreachable_follower(&dir);
        let before = Instant::now();
        follower.handle(vec![Event::WindowCreated {
            session_id: "session:1".into(),
            window_id: "window:1".into(),
        }]);
        assert!(follower.work.created.contains_key("window:1"));
        let retry_at = follower.work.retry_at.expect("a retry");
        assert!(retry_at >= before + link::backoff(1));
    }

    #[test]
    fn a_steady_event_stream_still_closes_its_batch() {
        // Terminals retitled every 5 ms never leave the stream quiet. A
        // window reset by each event would hold the batch for the whole 2 s.
        let (tx, rx) = mpsc::channel();
        let sender = thread::spawn(move || {
            let until = Instant::now() + Duration::from_secs(2);
            let mut n = 0u32;
            while Instant::now() < until && tx.send(n).is_ok() {
                n += 1;
                thread::sleep(Duration::from_millis(5));
            }
        });
        let started = Instant::now();
        let batch = gather(rx.recv().unwrap(), &rx, Duration::from_millis(50));
        let took = started.elapsed();
        assert!(took < Duration::from_millis(500), "batch took {took:?}");
        assert!(batch.len() > 1, "{batch:?}");
        assert!(batch.windows(2).all(|pair| pair[0] < pair[1]));
        // The events after the deadline wait for the next batch.
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_ok());
        drop(rx);
        sender.join().unwrap();
    }

    #[test]
    fn a_quiet_stream_closes_its_batch_at_the_deadline() {
        let (tx, rx) = mpsc::channel();
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        let started = Instant::now();
        let batch = gather(rx.recv().unwrap(), &rx, Duration::from_millis(50));
        assert_eq!(batch, vec![1, 2]);
        assert!(started.elapsed() >= Duration::from_millis(50));
        drop(tx);
    }

    #[test]
    fn scope_round_trips_through_preferences() {
        for scope in [
            Scope::Off,
            Scope::Global {
                optout: HashSet::from(["session:b".to_string()]),
            },
            Scope::Sessions(HashSet::from([
                "session:a".to_string(),
                "session:c".to_string(),
            ])),
        ] {
            let mut prefs = Options::default();
            scope.write(&mut prefs);
            assert_eq!(Scope::from_prefs(&prefs), scope);
        }
    }

    #[test]
    fn scope_decides_which_sessions_get_a_sidebar() {
        let global = Scope::Global {
            optout: HashSet::from(["session:b".to_string()]),
        };
        assert!(global.includes("session:a"));
        assert!(!global.includes("session:b"));
        let sessions = Scope::Sessions(HashSet::from(["session:a".to_string()]));
        assert!(sessions.includes("session:a"));
        assert!(!sessions.includes("session:z"));
        assert!(!Scope::Off.includes("session:a"));
    }
}
