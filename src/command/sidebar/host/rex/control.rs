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
            if let Some(stray) = &record.stray {
                let open = window.block_ids().any(|block| block == stray)
                    && self.spare.as_deref() != Some(stray.as_str());
                if open {
                    self.rex.close_sidebar_block(session_id, stray)?;
                }
                records.get_mut(&window_id).expect("record exists").stray = None;
                if open && window.block_ids().any(|block| block == record.block_id) {
                    result.placed.insert(window_id.clone());
                    if in_scope {
                        continue;
                    }
                }
            }
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
                // A split that lands between a pass's read and its placement
                // nests the sidebar, and Rex has no anchor for the window's
                // root. Placing again anchors the root split, which a block
                // split cannot displace. A sidebar the user moved elsewhere
                // is not this shape and stays where it is.
                let nested = window
                    .root_split()
                    .and_then(|root| root.before.split.as_deref())
                    .is_some_and(|inner| {
                        inner.before.block_id.as_deref() == Some(&record.block_id)
                    });
                if nested && let Some(mut placed) = self.place(session_id, window, synced)? {
                    // A close can fail after Rex carried it out, when the
                    // rex CLI times out.
                    if let Err(error) = self.rex.close_sidebar_block(session_id, &record.block_id)
                        && self.block_remains(session_id, &record.block_id)
                    {
                        if self
                            .rex
                            .close_sidebar_block(session_id, &placed.block_id)
                            .is_err()
                            && self.block_remains(session_id, &placed.block_id)
                        {
                            placed.stray = Some(record.block_id.clone());
                            records.insert(window_id.clone(), placed);
                        }
                        return Err(error);
                    }
                    records.insert(window_id.clone(), placed);
                    result.placed.insert(window_id.clone());
                }
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
            stray: None,
            applied_ratio: ratio,
            last_cols: u32::from(target),
            window_cols,
        }))
    }

    /// Whether `block_id` is still in the session, by a fresh read. A read
    /// that fails counts as yes.
    fn block_remains(&self, session_id: &str, block_id: &str) -> bool {
        self.rex
            .sidebar_view(session_id)
            .ok()
            .flatten()
            .is_none_or(|view| {
                view.windows
                    .iter()
                    .any(|window| window.block_ids().any(|block| block == block_id))
            })
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
    let exe =
        std::env::current_exe().context(crate::rebrand!("Failed to locate the workmux binary"))?;
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
        self.observed.program_status.follow(false);
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
    /// Every kept program status is read again too, for a change Rex never
    /// announced.
    fn sweep(&mut self) -> Result<()> {
        self.observed.program_status.expire();
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
        let mut new_sessions = HashSet::new();
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
                Event::SessionCreated { session_id } => {
                    new_sessions.insert(session_id.clone());
                    touch(&mut sessions, session_id);
                    wake = true;
                }
                Event::WindowClosed { session_id, .. }
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
            let pass = self.pass_over(sessions, &new_sessions, destroyed, sizes);
            self.work.settle(pass);
        }
        wake
    }

    /// Rex reports a new session with `session_created` alone, and no
    /// `window_created` follows for the windows it starts with, so the read
    /// of a session in `new_sessions` names the windows owed a sidebar.
    fn pass_over(
        &mut self,
        sessions: Vec<String>,
        new_sessions: &HashSet<String>,
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
            if let Some(view) = view.as_ref().filter(|_| new_sessions.contains(&session_id)) {
                for window in &view.windows {
                    self.work
                        .created
                        .insert(window.window_id.clone(), session_id.clone());
                }
            }
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
        self.observed.program_status.follow(true);
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
    use crate::multiplexer::{AgentStatus, Multiplexer, ReportedStatus};

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
    fn an_exited_program_wakes_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let mut follower = unreachable_follower(&dir);
        assert!(follower.handle(vec![Event::ChildExited {
            session_id: "session:1".into(),
            block_id: "block:1".into(),
        }]));
    }

    /// The daemon's tick reads program status through its own backend,
    /// `mux`, and the follower's events keep what that backend read current.
    #[test]
    fn a_program_status_event_wakes_the_daemon_to_read_the_new_status() {
        const PROCESS: &str = include_str!("../../../../multiplexer/testdata/rex_process.json");
        let dir = tempfile::tempdir().unwrap();
        let status = dir.path().join("status");
        let process = dir.path().join("process");
        std::fs::write(&process, PROCESS).unwrap();
        let mux = RexBackend::scripted(
            dir.path(),
            format!(
                "cat > /dev/null\ncase \"$*\" in\n\
                 \x20 *server.version*) echo '{{\"rex_version\":\"t\",\"api_revision\":8}}';;\n\
                 \x20 *whoami*) echo '{{\"endpoint\":\"unix:///s.sock\",\"instance_id\":\"boot\"}}';;\n\
                 \x20 *events*) exec sleep 60;;\n\
                 \x20 *program_status*) cat '{}';;\n\
                 \x20 *process*) cat '{}';;\n\
                 \x20 *) exit 1;;\nesac\n",
                status.display(),
                process.display(),
            ),
        );
        let files = Files::at(dir.path().join("state"), "test");
        let mut follower = Follower {
            plane: Plane::new(mux.detached_for_events(), files, Config::default()).unwrap(),
            observed: Arc::new(Observed::new(mux.program_statuses())),
            stream: None,
            work: Work::default(),
        };
        let block = "block:01a19e59-43d8-72fd-860c-3a1d6f183cc3";
        let report = |state: &str| {
            let record = format!(r#"{{"state":"{state}","updated_at":"2026-11-03T08:00:00Z"}}"#);
            std::fs::write(&status, format!(r#"{{"records":[{record}]}}"#)).unwrap();
        };
        let shown = || match mux.reported_status(block) {
            Some(ReportedStatus::Agent { status, .. }) => Some(status),
            _ => None,
        };

        assert!(follower.connect().is_ok());
        report("working");
        assert_eq!(shown(), Some(AgentStatus::Working));
        report("blocked");
        assert_eq!(shown(), Some(AgentStatus::Working), "kept until an event");
        assert!(follower.handle(vec![Event::ProgramStatusChanged {
            session_id: "session:1".into(),
            block_id: block.into(),
            state: "blocked".into(),
            kind: None,
            app: None,
        }]));
        assert_eq!(shown(), Some(AgentStatus::Waiting));
        report("done");
        follower.disconnect();
        assert_eq!(
            shown(),
            Some(AgentStatus::Done),
            "kept past the stream's end"
        );
    }

    #[test]
    fn the_safety_rebuild_reads_every_kept_status_again() {
        // verify11 probe C: a program that reported working and then exec'd
        // another keeps its pid and loses its name, and Rex sends no event
        // until the new program exits.
        let dir = tempfile::tempdir().unwrap();
        let status = dir.path().join("status");
        let process = dir.path().join("process");
        let mux = RexBackend::scripted(
            dir.path(),
            format!(
                "cat > /dev/null\ncase \"$*\" in\n\
                 \x20 *server.version*) echo '{{\"rex_version\":\"t\",\"api_revision\":8}}';;\n\
                 \x20 *session.list*) echo '{{\"sessions\":[]}}';;\n\
                 \x20 *program_status*) cat '{}';;\n\
                 \x20 *process*) cat '{}';;\n\
                 \x20 *) exit 1;;\nesac\n",
                status.display(),
                process.display(),
            ),
        );
        let files = Files::at(dir.path().join("state"), "test");
        let mut follower = Follower {
            plane: Plane::new(mux.detached_for_events(), files, Config::default()).unwrap(),
            observed: Arc::new(Observed::new(mux.program_statuses())),
            stream: None,
            work: Work::default(),
        };
        let block = "block:01a19e59-43d8-72fd-860c-3a1d6f183cc3";
        let foreground = |name: &str| {
            let foreground = format!(r#"{{"foreground":{{"pid":82590,"name":"{name}"}}}}"#);
            std::fs::write(&process, foreground).unwrap();
        };
        let record = r#"{"state":"working","updated_at":"2026-11-03T08:00:00Z","owner":{"pid":82590,"name":"Python"}}"#;
        std::fs::write(&status, format!(r#"{{"records":[{record}]}}"#)).unwrap();
        foreground("Python");
        let shown = || match mux.reported_status(block) {
            Some(ReportedStatus::Agent { status, .. }) => Some(status),
            _ => None,
        };

        follower.observed.program_status.follow(true);
        assert_eq!(shown(), Some(AgentStatus::Working));
        foreground("sleep");
        assert_eq!(shown(), Some(AgentStatus::Working), "no event came");
        follower.sweep().unwrap();
        assert_eq!(shown(), None, "the record's owner left the foreground");
    }

    #[test]
    fn a_removed_program_status_wakes_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let mux = RexBackend::scripted(dir.path(), "cat > /dev/null\nexit 1\n".to_string());
        let files = Files::at(dir.path().join("state"), "test");
        let mut follower = Follower {
            plane: Plane::new(mux.detached_for_events(), files, Config::default()).unwrap(),
            observed: Arc::new(Observed::new(mux.program_statuses())),
            stream: None,
            work: Work::default(),
        };
        assert!(follower.handle(vec![Event::ProgramStatusRemoved {
            session_id: "session:1".into(),
            block_id: "block:01a19e59-43d8-72fd-860c-3a1d6f183cc3".into(),
        }]));
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

    /// Rex reports a new session with `session_created` alone: no
    /// `window_created` follows for the window it starts with.
    #[test]
    fn a_new_session_gets_a_sidebar_from_its_session_created_event() {
        const VIEW: &str =
            include_str!("../../../../multiplexer/testdata/rex_sidebar_view_before.json");
        const SESSION: &str = "session:01a19e92-e5aa-7c77-a9b2-9042bbdbf620";
        const WINDOW: &str = "window:01a19e92-e5ba-7c40-bd4e-37d60b3ba9bb";
        let dir = tempfile::tempdir().unwrap();
        let view = dir.path().join("view");
        std::fs::write(&view, VIEW).unwrap();
        let rex = RexBackend::scripted(
            dir.path(),
            format!(
                "cat > /dev/null\ncase \"$*\" in\n\
                 \x20 *server.version*) echo '{{\"rex_version\":\"t\",\"api_revision\":8}}';;\n\
                 \x20 *session.view*) cat '{}';;\n\
                 \x20 *' size '*) echo '{{\"columns\":120,\"rows\":40}}';;\n\
                 \x20 *session.new_split*) \
                 echo '{{\"block_ids\":[\"block:sidebar\"],\"split_ids\":[\"split:sidebar\"]}}';;\n\
                 \x20 *) exit 1;;\nesac\n",
                view.display()
            ),
        );
        let files = Files::at(dir.path().join("state"), "test");
        files
            .update_prefs(|prefs| {
                Scope::Global {
                    optout: HashSet::new(),
                }
                .write(prefs)
            })
            .unwrap();
        let plane = Plane::new(rex.detached_for_events(), files, Config::default()).unwrap();
        let mut follower = Follower {
            plane: plane.for_daemon(),
            observed: Arc::default(),
            stream: None,
            work: Work::default(),
        };

        assert!(
            follower.handle(vec![Event::SessionCreated {
                session_id: SESSION.into(),
            }]),
            "a new session wakes the daemon, so every sidebar lists it"
        );
        let records = follower.plane.files.records();
        assert_eq!(
            records.get(WINDOW).map(|record| record.block_id.as_str()),
            Some("block:sidebar"),
            "{records:?}"
        );
        assert!(follower.work.created.is_empty());
    }

    const NEW_VIEW: &str =
        include_str!("../../../../multiplexer/testdata/rex_sidebar_view_before.json");
    const NEW_SESSION: &str = "session:01a19e92-e5aa-7c77-a9b2-9042bbdbf620";
    const NEW_WINDOW: &str = "window:01a19e92-e5ba-7c40-bd4e-37d60b3ba9bb";

    /// A follower over a scripted rex that shows `view` and gives each
    /// sidebar split its own block, with a global sidebar on.
    fn follower_showing(dir: &std::path::Path, view: &str) -> Follower {
        let path = dir.join("view");
        std::fs::write(&path, view).unwrap();
        let rex = RexBackend::scripted(
            dir,
            format!(
                "cat > /dev/null\ncase \"$*\" in\n\
                 \x20 *server.version*) echo '{{\"rex_version\":\"t\",\"api_revision\":8}}';;\n\
                 \x20 *session.view*) cat '{}';;\n\
                 \x20 *' size '*) echo '{{\"columns\":120,\"rows\":40}}';;\n\
                 \x20 *session.new_split*) n=$(($(cat '{dir}/splits' 2>/dev/null || echo 0) + 1)); \
                 echo $n > '{dir}/splits'; \
                 echo \"{{\\\"block_ids\\\":[\\\"block:sidebar$n\\\"],\\\"split_ids\\\":[\\\"split:sidebar$n\\\"]}}\";;\n\
                 \x20 *) exit 1;;\nesac\n",
                path.display(),
                dir = dir.display(),
            ),
        );
        let files = Files::at(dir.join("state"), "test");
        files
            .update_prefs(|prefs| {
                Scope::Global {
                    optout: HashSet::new(),
                }
                .write(prefs)
            })
            .unwrap();
        let plane = Plane::new(rex.detached_for_events(), files, Config::default()).unwrap();
        Follower {
            plane: plane.for_daemon(),
            observed: Arc::default(),
            stream: None,
            work: Work::default(),
        }
    }

    /// `session.create` can start a session with several windows, and
    /// session_created stands for all of them.
    #[test]
    fn every_window_a_new_session_starts_with_gets_a_sidebar() {
        let mut view: serde_json::Value = serde_json::from_str(NEW_VIEW).unwrap();
        let first = view["windows"][0].to_string();
        let second: serde_json::Value =
            serde_json::from_str(&first.replace("01a19e9", "02b29f9")).unwrap();
        let second_id = second["window_id"].as_str().unwrap().to_string();
        view["windows"].as_array_mut().unwrap().push(second);
        let dir = tempfile::tempdir().unwrap();
        let mut follower = follower_showing(dir.path(), &view.to_string());

        follower.handle(vec![Event::SessionCreated {
            session_id: NEW_SESSION.into(),
        }]);
        let records = follower.plane.files.records();
        assert!(records.contains_key(NEW_WINDOW), "{records:?}");
        assert!(records.contains_key(&second_id), "{records:?}");
    }

    /// Only a creation event owes a window a sidebar: a change in a session
    /// that was already there places none.
    #[test]
    fn a_view_change_in_an_older_session_places_no_sidebar() {
        let dir = tempfile::tempdir().unwrap();
        let mut follower = follower_showing(dir.path(), NEW_VIEW);

        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        let records = follower.plane.files.records();
        assert!(!records.contains_key(NEW_WINDOW), "{records:?}");
    }

    fn leaf(block: &str, rect: [f64; 4]) -> serde_json::Value {
        serde_json::json!({ "rect": rect, "block_id": block })
    }

    fn split(
        id: &str,
        ratio: f64,
        rect: [f64; 4],
        before: serde_json::Value,
        after: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "rect": rect,
            "split": { "split_id": id, "direction": "horizontal", "ratio": ratio,
                       "before": before, "after": after },
        })
    }

    /// A view of `NEW_SESSION` whose one window, `NEW_WINDOW`, has `layout`
    /// and the blocks `blocks`.
    fn view_of(layout: serde_json::Value, blocks: &[&str]) -> String {
        let blocks: Vec<_> = blocks
            .iter()
            .map(|block| serde_json::json!({ "block_id": block }))
            .collect();
        serde_json::json!({
            "session_id": NEW_SESSION,
            "windows": [{
                "window_id": NEW_WINDOW,
                "layers": [{ "kind": "tiled", "layout": layout, "blocks": blocks }],
            }],
        })
        .to_string()
    }

    /// A follower over a scripted rex that logs each call to `dir/calls`
    /// and shows `views[n]` after its n-th sidebar split, with a global
    /// sidebar on. While `dir/split-fails` or `dir/close-fails` exists,
    /// every split or close fails; while `dir/close1-fails` exists, closing
    /// `block:sidebar1` fails. With `dir/arm`, the next close makes the view
    /// read after it fail once.
    fn follower_replaying(path: &std::path::Path, views: &[String]) -> Follower {
        // A sweep that succeeds wakes the daemon with SIGUSR1, which would
        // otherwise end the test process.
        signal_hook::flag::register(signal_hook::consts::SIGUSR1, Arc::default()).unwrap();
        for (n, view) in views.iter().enumerate() {
            std::fs::write(path.join(format!("view-{n}")), view).unwrap();
        }
        let calls = path.join("calls");
        let splits = path.join("splits");
        let rex = RexBackend::scripted(
            path,
            format!(
                "params=$(cat)\necho \"$* $params\" >> '{calls}'\n\
                 n=$(cat '{splits}' 2>/dev/null || echo 0)\ncase \"$*\" in\n\
                 \x20 *server.version*) echo '{{\"rex_version\":\"t\",\"api_revision\":8}}';;\n\
                 \x20 *session.view*) [ -f '{dir}/view-fails-next' ] && \
                 {{ rm '{dir}/view-fails-next'; exit 1; }}; cat \"{dir}/view-$n\";;\n\
                 \x20 *session.list*) \
                 echo '{{\"sessions\":[{{\"session_id\":\"{session}\",\"label\":\"x\"}}]}}';;\n\
                 \x20 *' size '*) echo '{{\"columns\":120,\"rows\":40}}';;\n\
                 \x20 *session.new_split*) [ -f '{dir}/split-fails' ] && exit 1; \
                 n=$((n + 1)); echo $n > '{splits}'; \
                 echo \"{{\\\"block_ids\\\":[\\\"block:sidebar$n\\\"],\\\"split_ids\\\":[\\\"split:sidebar$n\\\"]}}\";;\n\
                 \x20 *block.close*) [ -f '{dir}/arm' ] && \
                 {{ rm '{dir}/arm'; touch '{dir}/view-fails-next'; }}; \
                 case \"$params\" in *sidebar1*) \
                 [ -f '{dir}/close1-fails' ] && exit 1;; esac; \
                 [ -f '{dir}/close-fails' ] && exit 1; echo '{{}}';;\n\
                 \x20 *session.resize_pane*) echo '{{}}';;\n\
                 \x20 *) exit 1;;\nesac\n",
                calls = calls.display(),
                splits = splits.display(),
                dir = path.display(),
                session = NEW_SESSION,
            ),
        );
        let files = Files::at(path.join("state"), "test");
        files
            .update_prefs(|prefs| {
                Scope::Global {
                    optout: HashSet::new(),
                }
                .write(prefs)
            })
            .unwrap();
        let plane = Plane::new(rex.detached_for_events(), files, Config::default()).unwrap();
        Follower {
            plane: plane.for_daemon(),
            observed: Arc::default(),
            stream: None,
            work: Work::default(),
        }
    }

    /// The race's result: sidebar1 nested beside `a` inside the add's split.
    fn nested_view() -> String {
        view_of(
            split(
                "split:add",
                0.5,
                [0.0, 0.0, 1.0, 1.0],
                split(
                    "split:sidebar1",
                    0.3125,
                    [0.0, 0.0, 0.5, 1.0],
                    leaf("block:sidebar1", [0.0, 0.0, 0.156, 1.0]),
                    leaf("block:a", [0.156, 0.0, 0.344, 1.0]),
                ),
                leaf("block:b", [0.5, 0.0, 0.5, 1.0]),
            ),
            &["block:sidebar1", "block:a", "block:b"],
        )
    }

    /// Replays the race WR-3 fix8 found. The daemon reads the add's new
    /// window while it has one block, the add splits that block, and the
    /// daemon's split, anchored on the block, lands inside the add's split.
    #[test]
    fn a_split_that_lands_before_the_placement_still_leaves_the_sidebar_at_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let whole = [0.0, 0.0, 1.0, 1.0];
        let views = [
            view_of(leaf("block:a", whole), &["block:a"]),
            nested_view(),
            view_of(
                split(
                    "split:sidebar2",
                    0.18,
                    whole,
                    leaf("block:sidebar2", [0.0, 0.0, 0.18, 1.0]),
                    split(
                        "split:add",
                        0.5,
                        [0.18, 0.0, 0.82, 1.0],
                        leaf("block:a", [0.18, 0.0, 0.41, 1.0]),
                        leaf("block:b", [0.59, 0.0, 0.41, 1.0]),
                    ),
                ),
                &["block:sidebar2", "block:a", "block:b"],
            ),
        ];
        let mut follower = follower_replaying(path, &views);
        let calls = path.join("calls");
        let sidebar = |follower: &Follower| {
            let records = follower.plane.files.records();
            records
                .get(NEW_WINDOW)
                .map(|record| record.block_id.clone())
        };
        let changed = || {
            vec![Event::ViewChanged {
                session_id: NEW_SESSION.into(),
            }]
        };

        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        assert_eq!(sidebar(&follower).as_deref(), Some("block:sidebar1"));

        follower.handle(changed());
        let log = std::fs::read_to_string(&calls).unwrap();
        let placements: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("session.new_split"))
            .collect();
        assert_eq!(placements.len(), 2, "{log}");
        assert!(
            placements[1].contains(r#""anchor_id":"split:add""#)
                && placements[1].contains(r#""side":"before""#),
            "the sidebar moves to the root the add's split made: {log}"
        );
        assert!(
            log.lines().any(|line| line.contains("block.close")
                && line.contains(r#""block_id":"block:sidebar1""#)),
            "the nested sidebar is closed: {log}"
        );
        assert_eq!(sidebar(&follower).as_deref(), Some("block:sidebar2"));
        let applied = follower.plane.files.records()[NEW_WINDOW].applied_ratio;
        assert!(
            (applied - 0.18).abs() < 1e-9,
            "the moved sidebar's ratio is read back from the root: {applied}"
        );

        follower.handle(changed());
        let log = std::fs::read_to_string(&calls).unwrap();
        assert_eq!(
            log.matches("session.new_split").count(),
            2,
            "a sidebar at the root stays: {log}"
        );
        assert_eq!(sidebar(&follower).as_deref(), Some("block:sidebar2"));
    }

    /// A sidebar the user floated out of the layout is theirs to place.
    #[test]
    fn a_floating_sidebar_stays_where_the_user_put_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let whole = [0.0, 0.0, 1.0, 1.0];
        let mut floated: serde_json::Value = serde_json::from_str(&view_of(
            split(
                "split:add",
                0.5,
                whole,
                leaf("block:a", [0.0, 0.0, 0.5, 1.0]),
                leaf("block:b", [0.5, 0.0, 0.5, 1.0]),
            ),
            &["block:a", "block:b"],
        ))
        .unwrap();
        floated["windows"][0]["layers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "kind": "floating",
                "layout": leaf("block:sidebar1", [0.1, 0.1, 0.3, 0.8]),
                "blocks": [{ "block_id": "block:sidebar1" }],
            }));
        let views = [
            view_of(leaf("block:a", whole), &["block:a"]),
            floated.to_string(),
        ];
        let mut follower = follower_replaying(path, &views);

        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        let log = std::fs::read_to_string(path.join("calls")).unwrap();
        assert_eq!(log.matches("session.new_split").count(), 1, "{log}");
        assert!(!log.contains("block.close"), "{log}");
        let records = follower.plane.files.records();
        assert_eq!(
            records
                .get(NEW_WINDOW)
                .map(|record| record.block_id.as_str()),
            Some("block:sidebar1")
        );
    }

    fn sidebar_of(follower: &Follower) -> Option<String> {
        let records = follower.plane.files.records();
        records
            .get(NEW_WINDOW)
            .map(|record| record.block_id.clone())
    }

    fn place_then_change(follower: &mut Follower) {
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
    }

    /// A sidebar left as the only tile, its window's other block floated,
    /// has no root split to move to; placing it again would never settle.
    #[test]
    fn a_sidebar_that_is_the_only_tile_stays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let whole = [0.0, 0.0, 1.0, 1.0];
        let mut alone: serde_json::Value =
            serde_json::from_str(&view_of(leaf("block:sidebar1", whole), &["block:sidebar1"]))
                .unwrap();
        alone["windows"][0]["layers"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "kind": "floating",
                "layout": leaf("block:a", [0.3, 0.1, 0.6, 0.8]),
                "blocks": [{ "block_id": "block:a" }],
            }));
        let views = [
            view_of(leaf("block:a", whole), &["block:a"]),
            alone.to_string(),
            alone.to_string().replace("sidebar1", "sidebar2"),
        ];
        let mut follower = follower_replaying(path, &views);
        place_then_change(&mut follower);
        let log = std::fs::read_to_string(path.join("calls")).unwrap();
        assert_eq!(log.matches("session.new_split").count(), 1, "{log}");
        assert!(!log.contains("block.close"), "{log}");
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar1"));
    }

    /// A tiled sidebar the user moved to the right of `b` is not the race's
    /// nesting, so it stays where they put it.
    #[test]
    fn a_tiled_sidebar_the_user_moved_stays_where_they_put_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let whole = [0.0, 0.0, 1.0, 1.0];
        let views = [
            view_of(leaf("block:a", whole), &["block:a"]),
            view_of(
                split(
                    "split:add",
                    0.5,
                    whole,
                    leaf("block:a", [0.0, 0.0, 0.5, 1.0]),
                    split(
                        "split:moved",
                        0.7,
                        [0.5, 0.0, 0.5, 1.0],
                        leaf("block:b", [0.5, 0.0, 0.35, 1.0]),
                        leaf("block:sidebar1", [0.85, 0.0, 0.15, 1.0]),
                    ),
                ),
                &["block:a", "block:b", "block:sidebar1"],
            ),
        ];
        let mut follower = follower_replaying(path, &views);
        place_then_change(&mut follower);
        let log = std::fs::read_to_string(path.join("calls")).unwrap();
        assert_eq!(log.matches("session.new_split").count(), 1, "{log}");
        assert!(!log.contains("block.close"), "{log}");
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar1"));
    }

    /// The new sidebar goes in before the nested one is closed, so a failed
    /// placement leaves the window as it was for the next pass.
    #[test]
    fn a_failed_move_to_the_root_keeps_the_nested_sidebar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let views = [
            view_of(leaf("block:a", [0.0, 0.0, 1.0, 1.0]), &["block:a"]),
            nested_view(),
        ];
        let mut follower = follower_replaying(path, &views);
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        std::fs::write(path.join("split-fails"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        let log = std::fs::read_to_string(path.join("calls")).unwrap();
        assert_eq!(log.matches("session.new_split").count(), 2, "{log}");
        assert!(!log.contains("block.close"), "{log}");
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar1"));
    }

    /// When the nested sidebar will not close, the new one is closed and the
    /// record keeps the nested one, so the next pass repairs it again. The
    /// view after the failed pass is the nested one, as Rex shows it once
    /// the new sidebar is closed.
    #[test]
    fn a_failed_close_of_the_nested_sidebar_is_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let whole = [0.0, 0.0, 1.0, 1.0];
        let views = [
            view_of(leaf("block:a", whole), &["block:a"]),
            nested_view(),
            nested_view(),
            view_of(
                split(
                    "split:sidebar3",
                    0.18,
                    whole,
                    leaf("block:sidebar3", [0.0, 0.0, 0.18, 1.0]),
                    split(
                        "split:add",
                        0.5,
                        [0.18, 0.0, 0.82, 1.0],
                        leaf("block:a", [0.18, 0.0, 0.41, 1.0]),
                        leaf("block:b", [0.59, 0.0, 0.41, 1.0]),
                    ),
                ),
                &["block:sidebar3", "block:a", "block:b"],
            ),
        ];
        let mut follower = follower_replaying(path, &views);
        let closes = || {
            let log = std::fs::read_to_string(path.join("calls")).unwrap();
            log.lines()
                .filter(|line| line.contains("block.close"))
                .map(|line| {
                    let at = line.find("block:sidebar").unwrap();
                    line[at..at + "block:sidebarN".len()].to_string()
                })
                .collect::<Vec<_>>()
        };
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        std::fs::write(path.join("close1-fails"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        assert_eq!(closes(), ["block:sidebar1", "block:sidebar2"]);
        assert!(
            follower.work.retry_at.is_some(),
            "a failed close schedules a retry"
        );
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar1"));

        std::fs::remove_file(path.join("close1-fails")).unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        assert_eq!(
            closes(),
            ["block:sidebar1", "block:sidebar2", "block:sidebar1"]
        );
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar3"));
    }

    /// A sidebar the user moved beside `a`, inside the add's split, is not
    /// the race's nesting either.
    #[test]
    fn a_sidebar_moved_beside_the_first_pane_stays() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let whole = [0.0, 0.0, 1.0, 1.0];
        let views = [
            view_of(leaf("block:a", whole), &["block:a"]),
            view_of(
                split(
                    "split:add",
                    0.5,
                    whole,
                    split(
                        "split:moved",
                        0.7,
                        [0.0, 0.0, 0.5, 1.0],
                        leaf("block:a", [0.0, 0.0, 0.35, 1.0]),
                        leaf("block:sidebar1", [0.35, 0.0, 0.15, 1.0]),
                    ),
                    leaf("block:b", [0.5, 0.0, 0.5, 1.0]),
                ),
                &["block:a", "block:sidebar1", "block:b"],
            ),
        ];
        let mut follower = follower_replaying(path, &views);
        place_then_change(&mut follower);
        let log = std::fs::read_to_string(path.join("calls")).unwrap();
        assert_eq!(log.matches("session.new_split").count(), 1, "{log}");
        assert!(!log.contains("block.close"), "{log}");
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar1"));
    }

    /// `sidebar2` at the root, over `inner`.
    fn root_sidebar2_over(inner: serde_json::Value, blocks: &[&str]) -> String {
        view_of(
            split(
                "split:sidebar2",
                0.18,
                [0.0, 0.0, 1.0, 1.0],
                leaf("block:sidebar2", [0.0, 0.0, 0.18, 1.0]),
                inner,
            ),
            blocks,
        )
    }

    /// A close Rex carries out but reports as failed, as when the rex CLI
    /// times out, leaves the new sidebar in place and recorded.
    #[test]
    fn a_close_rex_carried_out_but_reported_as_failed_keeps_the_new_sidebar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let views = [
            view_of(leaf("block:a", [0.0, 0.0, 1.0, 1.0]), &["block:a"]),
            nested_view(),
            root_sidebar2_view(),
        ];
        let mut follower = follower_replaying(path, &views);
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        std::fs::write(path.join("close1-fails"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        std::fs::remove_file(path.join("close1-fails")).unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        follower.work.sweep = true;
        follower.run_owed();
        assert!(!follower.work.owed(), "the sweep failed");
        let log = std::fs::read_to_string(path.join("calls")).unwrap();
        assert_eq!(log.matches("block.close").count(), 1, "{log}");
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar2"));
        let closed = follower.plane.files.daemon().windows_with(store::CLOSED);
        assert!(closed.is_empty(), "{closed:?}");
    }

    /// When the new sidebar will not close either, the record moves to it
    /// and a later pass closes the nested one, once.
    #[test]
    fn a_sidebar_left_by_two_failed_closes_is_closed_later() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let views = [
            view_of(leaf("block:a", [0.0, 0.0, 1.0, 1.0]), &["block:a"]),
            nested_view(),
            both_sidebars_view(),
        ];
        let mut follower = follower_replaying(path, &views);
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        std::fs::write(path.join("close-fails"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        assert!(follower.work.retry_at.is_some(), "no retry scheduled");
        std::fs::remove_file(path.join("close-fails")).unwrap();
        for _ in 0..3 {
            follower.handle(vec![Event::ViewChanged {
                session_id: NEW_SESSION.into(),
            }]);
        }
        follower.work.sweep = true;
        follower.run_owed();
        assert!(!follower.work.owed(), "the sweep failed");
        let log = std::fs::read_to_string(path.join("calls")).unwrap();
        let closes: Vec<&str> = log
            .lines()
            .filter(|line| line.contains("block.close"))
            .collect();
        assert_eq!(closes.len(), 3, "{log}");
        assert!(closes[2].contains("block:sidebar1"), "{log}");
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar2"));
    }

    /// The view after both closes of a repair failed: `sidebar2` at the root
    /// over the nested `sidebar1`.
    fn both_sidebars_view() -> String {
        root_sidebar2_over(
            split(
                "split:add",
                0.5,
                [0.18, 0.0, 0.82, 1.0],
                split(
                    "split:sidebar1",
                    0.3125,
                    [0.18, 0.0, 0.41, 1.0],
                    leaf("block:sidebar1", [0.18, 0.0, 0.128, 1.0]),
                    leaf("block:a", [0.308, 0.0, 0.282, 1.0]),
                ),
                leaf("block:b", [0.59, 0.0, 0.41, 1.0]),
            ),
            &["block:sidebar2", "block:sidebar1", "block:a", "block:b"],
        )
    }

    /// `sidebar2` at the root over `[a, b]`.
    fn root_sidebar2_view() -> String {
        root_sidebar2_over(
            split(
                "split:add",
                0.5,
                [0.18, 0.0, 0.82, 1.0],
                leaf("block:a", [0.18, 0.0, 0.41, 1.0]),
                leaf("block:b", [0.59, 0.0, 0.41, 1.0]),
            ),
            &["block:sidebar2", "block:a", "block:b"],
        )
    }

    /// A follower whose repair failed both closes, so its record names
    /// `sidebar2` with `sidebar1` as the stray. `views[2]` shows both.
    fn follower_with_a_stray(path: &std::path::Path) -> Follower {
        let views = [
            view_of(leaf("block:a", [0.0, 0.0, 1.0, 1.0]), &["block:a"]),
            nested_view(),
            both_sidebars_view(),
        ];
        let mut follower = follower_replaying(path, &views);
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        std::fs::write(path.join("close-fails"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        std::fs::remove_file(path.join("close-fails")).unwrap();
        let record = follower.plane.files.records()[NEW_WINDOW].clone();
        assert_eq!(
            (record.block_id.as_str(), record.stray.as_deref()),
            ("block:sidebar2", Some("block:sidebar1"))
        );
        follower
    }

    fn closes_in(path: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(path.join("calls"))
            .unwrap()
            .lines()
            .filter(|line| line.contains("block.close"))
            .map(String::from)
            .collect()
    }

    /// `sidebar off` while a stray is pending closes the stray and the
    /// root sidebar.
    #[test]
    fn sidebar_off_with_a_pending_stray_closes_both_sidebars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let follower = follower_with_a_stray(path);
        follower
            .plane
            .files
            .update_prefs(|prefs| Scope::Off.write(prefs))
            .unwrap();
        let mut cli = Plane::new(
            follower.plane.rex.detached(),
            follower.plane.files.clone(),
            Config::default(),
        )
        .unwrap();
        let before = closes_in(path).len();
        cli.reconcile_all(false).unwrap();
        let closes = closes_in(path);
        let closed = &closes[before..];
        assert!(
            closed.iter().any(|line| line.contains("block:sidebar1"))
                && closed.iter().any(|line| line.contains("block:sidebar2")),
            "{closes:?}"
        );
        assert!(follower.plane.files.records().is_empty());
    }

    /// When the rollback close is carried out but reported as failed, the
    /// record keeps the nested sidebar, and the next pass repairs it.
    #[test]
    fn a_rollback_rex_carried_out_but_reported_as_failed_keeps_the_nested_sidebar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let views = [
            view_of(leaf("block:a", [0.0, 0.0, 1.0, 1.0]), &["block:a"]),
            nested_view(),
            nested_view(),
            root_sidebar2_view().replace("sidebar2", "sidebar3"),
        ];
        let mut follower = follower_replaying(path, &views);
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        std::fs::write(path.join("close-fails"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        std::fs::remove_file(path.join("close-fails")).unwrap();
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar1"));
        for _ in 0..3 {
            follower.handle(vec![Event::ViewChanged {
                session_id: NEW_SESSION.into(),
            }]);
        }
        follower.work.sweep = true;
        follower.run_owed();
        assert!(!follower.work.owed(), "the sweep failed");
        let closed = follower.plane.files.daemon().windows_with(store::CLOSED);
        assert!(closed.is_empty(), "{closed:?}");
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar3"));
    }

    /// A user who closes the root sidebar while a stray is pending closes
    /// the window's sidebar: the stray is closed too.
    #[test]
    fn closing_the_root_sidebar_while_a_stray_is_pending_closes_the_stray() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let mut follower = follower_with_a_stray(path);
        std::fs::write(path.join("view-2"), nested_view()).unwrap();
        let before = closes_in(path).len();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        let closes = closes_in(path);
        assert_eq!(closes.len(), before + 1, "{closes:?}");
        assert!(closes[before].contains("block:sidebar1"), "{closes:?}");
        assert_eq!(sidebar_of(&follower), None);
        let closed = follower.plane.files.daemon().windows_with(store::CLOSED);
        assert!(closed.contains_key(NEW_WINDOW), "{closed:?}");
    }

    /// A stray that is the block a quitting sidebar runs in is left for it
    /// to close.
    #[test]
    fn a_stray_that_is_the_spare_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let mut follower = follower_with_a_stray(path);
        follower.plane.spare = Some("block:sidebar1".into());
        let before = closes_in(path).len();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        assert_eq!(closes_in(path).len(), before);
    }

    /// A stray the user moved to another window is not closed.
    #[test]
    fn a_stray_moved_to_another_window_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let mut follower = follower_with_a_stray(path);
        let mut view: serde_json::Value = serde_json::from_str(&root_sidebar2_view()).unwrap();
        view["windows"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "window_id": "window:other",
                "layers": [{
                    "kind": "tiled",
                    "layout": leaf("block:sidebar1", [0.0, 0.0, 1.0, 1.0]),
                    "blocks": [{ "block_id": "block:sidebar1" }],
                }],
            }));
        std::fs::write(path.join("view-2"), view.to_string()).unwrap();
        let before = closes_in(path).len();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        assert_eq!(closes_in(path).len(), before);
        assert_eq!(follower.plane.files.records()[NEW_WINDOW].stray, None);
    }

    /// A stray whose close fails stays recorded, and a retry is scheduled.
    #[test]
    fn a_stray_that_will_not_close_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let mut follower = follower_with_a_stray(path);
        follower.work.retry_at = None;
        std::fs::write(path.join("close-fails"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        assert_eq!(
            follower.plane.files.records()[NEW_WINDOW].stray.as_deref(),
            Some("block:sidebar1")
        );
        assert!(follower.work.retry_at.is_some());
    }

    /// When the nested close fails and the view cannot be read again, the
    /// nested block counts as still there: the new sidebar is closed.
    #[test]
    fn a_failed_read_after_a_failed_close_counts_as_still_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let views = [
            view_of(leaf("block:a", [0.0, 0.0, 1.0, 1.0]), &["block:a"]),
            nested_view(),
            both_sidebars_view(),
        ];
        let mut follower = follower_replaying(path, &views);
        follower.handle(vec![Event::WindowCreated {
            session_id: NEW_SESSION.into(),
            window_id: NEW_WINDOW.into(),
        }]);
        std::fs::write(path.join("close1-fails"), "").unwrap();
        std::fs::write(path.join("arm"), "").unwrap();
        follower.handle(vec![Event::ViewChanged {
            session_id: NEW_SESSION.into(),
        }]);
        let closes = closes_in(path);
        assert!(
            closes.iter().any(|line| line.contains("block:sidebar2")),
            "{closes:?}"
        );
        assert_eq!(sidebar_of(&follower).as_deref(), Some("block:sidebar1"));
    }
}
