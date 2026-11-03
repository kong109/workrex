//! The Rex control plane: places, sizes and removes sidebar blocks. The CLI
//! runs one pass over every session; the daemon runs a pass per session as
//! its events arrive. Both hold the block records lock for the pass.

use anyhow::{Context, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::config::{Config, SidebarWidth};
use crate::multiplexer::rex::RexBackend;
use crate::multiplexer::rex::events::Event;
use crate::multiplexer::rex::sidebar_api::{View, Window};

use super::observe::Observed;
use super::size::{self, SizeAction};
use super::store::{self, Files, Options, Records, WindowSidebar};

/// How long the daemon waits for more events before acting on a batch. A
/// layout change arrives as several events in a few milliseconds.
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

/// Which windows of a session a pass may add a sidebar to. Only new windows
/// and explicit requests get one, so a sidebar the user closed stays closed.
enum Create<'a> {
    All,
    Only(&'a HashSet<String>),
}

impl Create<'_> {
    fn allows(&self, window: &str) -> bool {
        match self {
            Create::All => true,
            Create::Only(windows) => windows.contains(window),
        }
    }
}

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
        })
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
    /// they are missing.
    pub fn reconcile_all(&mut self) -> Result<Records> {
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
        let result = self.sync(
            &mut guard.records,
            views.collect(),
            Create::All,
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
        let mut current = BTreeMap::new();
        let mut dragged = None;
        let mut first_error = None;
        for (session_id, view) in views {
            let Some(view) = view else {
                records.retain(|_, record| record.session_id != session_id);
                if let Some(observed) = observed {
                    observed.drop_session(&session_id);
                }
                continue;
            };
            let result = self.pass(records, &view, &scope, &create, sizes, configured, synced);
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
                records.remove(&window_id);
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
                if records.contains_key(&window.window_id) || !create.allows(&window.window_id) {
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

/// Run the control plane for the daemon until `observed.stop` is set.
pub(super) fn spawn(mut plane: Plane, observed: Arc<Observed>, socket: PathBuf) {
    thread::spawn(move || {
        // The daemon exits ten seconds after its last client. Under Rex no
        // hook restarts it, so the control plane stays a client itself.
        let (keepalive_tx, _keepalive_rx) = mpsc::sync_channel::<()>(1);
        let _keepalive = crate::command::sidebar::client::connect(&socket, keepalive_tx);
        while !observed.stop.load(Ordering::Relaxed) {
            if let Err(error) = run_stream(&mut plane, &observed) {
                tracing::warn!(%error, "sidebar event stream failed");
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn run_stream(plane: &mut Plane, observed: &Observed) -> Result<()> {
    let mut stream = plane.rex.events()?;
    observed.events_pid.store(stream.pid(), Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        while let Some(event) = stream.next_event() {
            if tx.send(event).is_err() {
                break;
            }
        }
    });

    // Windows created while no stream ran get their sidebar now.
    let records = plane.reconcile_all()?;
    observed.replace_all(
        &plane.rex,
        plane.rex.sidebar_views()?,
        &sidebar_blocks(&records),
    );
    wake_daemon();

    loop {
        if observed.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let first = match rx.recv_timeout(SAFETY_REBUILD) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                plane.reload_config();
                rebuild(plane, observed)?;
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        };
        let mut batch = vec![first];
        while let Ok(event) = rx.recv_timeout(SETTLE) {
            batch.push(event);
        }
        if let Err(error) = handle(plane, observed, batch) {
            tracing::warn!(%error, "sidebar control plane pass failed");
        }
    }
}

fn rebuild(plane: &mut Plane, observed: &Observed) -> Result<()> {
    let mut guard = plane.files.lock_records()?;
    let views = plane.rex.sidebar_views()?;
    let views = views
        .into_iter()
        .map(|view| (view.session_id.clone(), Some(view)))
        .collect();
    let empty = HashSet::new();
    let current = plane.sync(
        &mut guard.records,
        views,
        Create::Only(&empty),
        &HashMap::new(),
        None,
    );
    guard.save()?;
    let sidebars = sidebar_blocks(&guard.records);
    drop(guard);
    observed.replace_all(&plane.rex, current?.into_values().collect(), &sidebars);
    Ok(())
}

fn handle(plane: &mut Plane, observed: &Observed, batch: Vec<Event>) -> Result<()> {
    let sidebars = sidebar_blocks(&plane.files.records());
    let mut sessions: Vec<String> = Vec::new();
    let mut created: HashSet<String> = HashSet::new();
    let mut sizes = HashMap::new();
    let mut destroyed = HashSet::new();
    let mut wake = false;
    let touch = |sessions: &mut Vec<String>, session_id: &str| {
        if !sessions.iter().any(|id| id == session_id) {
            sessions.push(session_id.to_string());
        }
    };
    for event in &batch {
        observed.apply(event);
        match event {
            Event::WindowCreated {
                session_id,
                window_id,
            } => {
                created.insert(window_id.clone());
                touch(&mut sessions, session_id);
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
            Event::ProgramStatusChanged { .. } | Event::ProgramStatusRemoved { .. } => {
                wake = true;
            }
            _ => {}
        }
    }
    sessions.retain(|session_id| !destroyed.contains(session_id));

    if !sessions.is_empty() || !destroyed.is_empty() {
        let mut guard = plane.files.lock_records()?;
        let mut views: Vec<(String, Option<View>)> = destroyed
            .into_iter()
            .map(|session_id| (session_id, None))
            .collect();
        for session_id in sessions {
            let view = plane.rex.sidebar_view(&session_id)?;
            views.push((session_id, view));
        }
        let result = plane.sync(
            &mut guard.records,
            views,
            Create::Only(&created),
            &sizes,
            Some(observed),
        );
        guard.save()?;
        result?;
    }
    if wake {
        wake_daemon();
    }
    Ok(())
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
