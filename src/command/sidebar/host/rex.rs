//! The sidebar on Rex. Each window in scope gets a full-height block on its
//! left running the sidebar TUI; the daemon follows `rex events` to place
//! blocks in new windows and keep their width.

mod control;
mod link;
mod observe;
mod size;
mod store;

use anyhow::{Context, Result, anyhow, bail};
use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::{Config, SidebarGroupBy, SidebarPosition};
use crate::multiplexer::rex::RexBackend;
use crate::multiplexer::{
    Appearance, Change, DaemonIo, HostPane, Multiplexer, Nav, Reach, SidebarHost, SidebarRequest,
};

use super::super::{NavAction, app, daemon, view};
use control::{Plane, Scope};
use observe::{Observed, RexDaemonIo};
use store::Files;

impl SidebarHost for RexBackend {
    fn handle(&self, request: SidebarRequest) -> Result<()> {
        Requests::new(self)?.handle(request)
    }

    fn refresh(&self) {
        if let Ok(requests) = Requests::new(self) {
            requests.signal(libc::SIGUSR1);
        }
    }

    fn pane_identity(&self) -> Option<HostPane> {
        let own = RexBackend::own_block()?;
        let view = self.sidebar_view(&own.session_id).ok()??;
        let window = view
            .windows
            .iter()
            .find(|window| window.block_ids().any(|block| block == own.block_id))?;
        Some(HostPane {
            session_name: view.label.clone(),
            session_id: view.session_id.clone(),
            window_id: window.window_id.clone(),
            pane_id: own.block_id,
        })
    }

    fn attach_daemon(&self) -> Result<Box<dyn DaemonIo>> {
        let instance = self.resolve_instance_id()?;
        let files = Files::for_instance(&instance)?;
        let plane = Plane::new(self.detached(), files.clone(), Config::load(None)?)?;
        let observed = Arc::new(Observed::default());
        // The first tick must see the server, or the daemon would treat
        // every agent's pane as gone.
        let sidebars = control::sidebar_blocks(&files.records());
        observed.replace_all(&plane.rex, plane.rex.sidebar_views()?, &sidebars);
        control::spawn(plane, observed.clone(), daemon::socket_path(&instance));
        Ok(Box::new(RexDaemonIo { observed, files }))
    }
}

struct Requests<'a> {
    rex: &'a RexBackend,
    files: Files,
    instance: String,
}

impl<'a> Requests<'a> {
    fn new(rex: &'a RexBackend) -> Result<Self> {
        let instance = rex.resolve_instance_id()?;
        Ok(Self {
            files: Files::for_instance(&instance)?,
            rex,
            instance,
        })
    }

    fn handle(&self, request: SidebarRequest) -> Result<()> {
        match request {
            SidebarRequest::Switch {
                change,
                reach,
                appearance,
            } => self.switch(change, reach, appearance),
            SidebarRequest::Navigate(nav) => self.navigate(nav),
            SidebarRequest::Filter(mode) => self.filter(mode.as_deref()),
            SidebarRequest::Group { mode, clear } => self.group(mode.as_deref(), clear),
            SidebarRequest::Views(mode) => self.views(mode.as_deref()),
            SidebarRequest::ExpandedGroups(mut labels) => {
                labels.sort_unstable();
                self.files.update_prefs(|prefs| {
                    prefs.set_global(store::EXPANDED, Some(labels.join("\t")))
                })?;
                self.signal(libc::SIGUSR1);
                Ok(())
            }
            SidebarRequest::ToggleSleeping { pane_id } => {
                self.files.update_prefs(|prefs| {
                    let mut panes = prefs.global_set(store::SLEEPING);
                    if !panes.insert(pane_id.clone()) {
                        panes.remove(&pane_id);
                    }
                    prefs.set_global_set(store::SLEEPING, &panes);
                })?;
                self.signal(libc::SIGUSR1);
                Ok(())
            }
            SidebarRequest::Quit { pane } => self.quit(pane),
        }
    }

    fn current_session(&self) -> Result<String> {
        self.rex
            .current_session_id()?
            .ok_or_else(|| anyhow!("--session needs a Rex session: run it inside a Rex block"))
    }

    fn switch(&self, change: Change, reach: Reach, appearance: Appearance) -> Result<()> {
        let mut config = Config::load(None)?;
        let scope = Scope::from_prefs(&self.files.prefs());
        let change = match change {
            Change::Toggle => self.toggle_target(reach, &scope)?,
            change => change,
        };
        let next = match (change, reach) {
            (Change::On, Reach::Global) => Scope::Global {
                optout: match &scope {
                    Scope::Global { optout } => optout.clone(),
                    _ => Default::default(),
                },
            },
            (Change::On, Reach::CurrentSession) => {
                let session = self.current_session()?;
                match scope.clone() {
                    Scope::Global { mut optout } => {
                        optout.remove(&session);
                        Scope::Global { optout }
                    }
                    Scope::Sessions(mut ids) => {
                        ids.insert(session);
                        Scope::Sessions(ids)
                    }
                    Scope::Off => Scope::Sessions([session].into()),
                }
            }
            (Change::Off, Reach::CurrentSession) => {
                let session = self.current_session()?;
                match scope.clone() {
                    Scope::Global { mut optout } => {
                        optout.insert(session);
                        Scope::Global { optout }
                    }
                    Scope::Sessions(mut ids) => {
                        ids.remove(&session);
                        if ids.is_empty() {
                            Scope::Off
                        } else {
                            Scope::Sessions(ids)
                        }
                    }
                    Scope::Off => Scope::Off,
                }
            }
            (Change::Off, Reach::Global) | (Change::Toggle, _) => Scope::Off,
        };

        if change == Change::On {
            if appearance.height.is_some() {
                bail!("--height sizes a top sidebar, and under Rex the sidebar opens on the left");
            }
            match (appearance.position, config.sidebar.position) {
                (Some(SidebarPosition::Top), _) => {
                    bail!("--position top needs tmux; under Rex the sidebar opens on the left")
                }
                (None, Some(SidebarPosition::Top)) => bail!(
                    "your config sets sidebar.position to top, which needs tmux; under Rex \
                     pass --position left or set sidebar.position: left"
                ),
                _ => {}
            }
            let _ = std::thread::spawn(crate::tips::mark_sidebar_used);
        }
        self.files.update_prefs(|prefs| {
            next.write(prefs);
            if next != Scope::Off && change == Change::On {
                prefs.set_global(store::POSITION, Some("left".into()));
                if let Some(width) = &appearance.width {
                    prefs.set_global(store::WIDTH, Some(width_text(width)));
                }
            }
        })?;

        if next != Scope::Off {
            self.ensure_daemon()?;
        }
        if appearance.width.is_some() {
            config.sidebar.width = appearance.width;
        }
        Plane::new(self.rex.detached(), self.files.clone(), config)?
            .reconcile_all(change == Change::On)?;
        if next == Scope::Off {
            self.signal(libc::SIGTERM);
        }
        Ok(())
    }

    fn toggle_target(&self, reach: Reach, scope: &Scope) -> Result<Change> {
        if reach == Reach::CurrentSession
            && let Scope::Global { optout } = scope
        {
            let session = self.current_session()?;
            return Ok(if optout.contains(&session) {
                Change::On
            } else {
                Change::Off
            });
        }
        let current = self.rex.current_window_id()?;
        let has_sidebar = current.is_some_and(|window| self.files.records().contains_key(&window));
        Ok(if has_sidebar { Change::Off } else { Change::On })
    }

    /// Leave the sidebar from inside it, the way `q` does on tmux: this
    /// session's sidebars go away, or every sidebar when the scope is global.
    fn quit(&self, pane: HostPane) -> Result<()> {
        let scope = Scope::from_prefs(&self.files.prefs());
        let next = match scope {
            Scope::Sessions(mut ids) => {
                ids.remove(&pane.session_id);
                if ids.is_empty() {
                    Scope::Off
                } else {
                    Scope::Sessions(ids)
                }
            }
            _ => Scope::Off,
        };
        self.files.update_prefs(|prefs| next.write(prefs))?;
        // Closing this block ends this process, so it goes last.
        let config = Config::load(None).unwrap_or_default();
        Plane::new(self.rex.detached(), self.files.clone(), config)?
            .sparing(&pane.pane_id)
            .reconcile_all(false)?;
        if next == Scope::Off {
            self.signal(libc::SIGTERM);
        }
        let mut guard = self.files.lock_records()?;
        guard
            .records
            .retain(|_, record| record.block_id != pane.pane_id);
        guard.save()?;
        drop(guard);
        self.rex
            .close_sidebar_block(&pane.session_id, &pane.pane_id)
    }

    fn navigate(&self, nav: Nav) -> Result<()> {
        let daemon = self.files.daemon();
        let agents = daemon.global(store::AGENTS).unwrap_or_default();
        let mut panes: Vec<&str> = agents.split_whitespace().collect();
        if panes.is_empty() {
            bail!("no sidebar agents found (is the sidebar running?)");
        }

        let views = self.rex.sidebar_views()?;
        let mut pane_window_ids = HashMap::new();
        let mut pane_sessions = HashMap::new();
        for view in &views {
            for window in &view.windows {
                for block in window.block_ids() {
                    pane_window_ids.insert(block.to_string(), window.window_id.clone());
                    pane_sessions.insert(block.to_string(), view.label.as_str());
                }
            }
        }
        let current_pane = self
            .rex
            .current_pane_id()
            .or_else(|| self.rex.active_pane_id())
            .unwrap_or_default();
        let current_window = pane_window_ids
            .get(&current_pane)
            .cloned()
            .unwrap_or_default();
        let current_session = pane_sessions
            .get(&current_pane)
            .copied()
            .unwrap_or_default();

        if let Some(scope) = daemon
            .window(&current_window, view::SCOPE_OPTION)
            .and_then(|value| serde_json::from_str::<view::WindowView>(value).ok())
        {
            panes.retain(|pane| scope.pane_ids.contains(*pane));
        }
        if self.filter_mode() == app::SidebarFilterMode::Session {
            panes.retain(|pane| pane_sessions.get(*pane) == Some(&current_session));
        }
        if panes.is_empty() {
            bail!("no matching agents found");
        }

        let anchor = super::super::navigation_anchor_pane(
            &panes,
            &current_pane,
            &current_window,
            &pane_window_ids,
        );
        let current_idx = anchor.and_then(|pane| panes.iter().position(|&id| id == pane));
        let action = match nav {
            Nav::Next => NavAction::Next,
            Nav::Prev => NavAction::Prev,
            Nav::Jump(n) => NavAction::Jump(n),
        };
        let target = super::super::compute_nav_target(&action, current_idx, panes.len())
            .ok_or_else(|| match nav {
                Nav::Jump(n) => anyhow!("agent {} out of range (1-{})", n, panes.len()),
                _ => anyhow!("no matching agents found"),
            })?;
        self.rex.switch_to_pane(panes[target], None)?;
        self.signal(libc::SIGUSR1);
        Ok(())
    }

    fn filter_mode(&self) -> app::SidebarFilterMode {
        crate::state::StateStore::new()
            .and_then(|store| store.load_settings())
            .ok()
            .and_then(|settings| settings.sidebar_filter)
            .and_then(|mode| super::super::parse_sidebar_filter_mode(&mode).ok())
            .unwrap_or_default()
    }

    fn filter(&self, mode: Option<&str>) -> Result<()> {
        let mode = match mode {
            Some(mode) => super::super::parse_sidebar_filter_mode(mode)?,
            None => self.filter_mode().toggle(),
        };
        let store = crate::state::StateStore::new()?;
        let mut settings = store.load_settings()?;
        settings.sidebar_filter = Some(mode.as_str().to_string());
        store.save_settings(&settings)?;
        self.signal(libc::SIGUSR1);
        Ok(())
    }

    fn group(&self, mode: Option<&str>, clear: bool) -> Result<()> {
        let configured = Config::load(None)
            .map(|config| config.sidebar.group_by())
            .unwrap_or_default();
        let store = crate::state::StateStore::new()?;
        let mut settings = store.load_settings()?;
        let current = settings
            .sidebar_group_by
            .as_deref()
            .map(|mode| super::super::parse_sidebar_group_by(mode).unwrap_or(configured))
            .unwrap_or(configured);
        let next = if clear {
            None
        } else {
            Some(match mode {
                Some(mode) => super::super::parse_sidebar_group_by(mode)?,
                None => match current {
                    Some(_) => None,
                    None => configured.or(Some(SidebarGroupBy::Project)),
                },
            })
        };
        settings.sidebar_group_by =
            next.map(|mode| super::super::group_by_option_value(mode).to_string());
        store.save_settings(&settings)?;
        self.signal(libc::SIGUSR1);
        Ok(())
    }

    fn views(&self, mode: Option<&str>) -> Result<()> {
        let enabled = match mode {
            Some("on") => true,
            Some("off") => false,
            Some(mode) => bail!("invalid sidebar view mode {mode:?}; expected on or off"),
            None => !view::is_enabled(self.files.prefs().global(view::OPTION)),
        };
        self.files.update_prefs(|prefs| {
            prefs.set_global(
                view::OPTION,
                Some(if enabled { "on" } else { "off" }.into()),
            )
        })?;
        self.signal(libc::SIGUSR1);
        Ok(())
    }

    fn daemon_running(&self) -> bool {
        UnixStream::connect(daemon::socket_path(&self.instance)).is_ok()
    }

    /// Signal the daemon, but only one that answers on its socket: the pid
    /// file outlives a crash, and the pid may since belong to another
    /// process.
    fn signal(&self, signal: libc::c_int) {
        if !self.daemon_running() {
            return;
        }
        let pid = self
            .files
            .daemon()
            .global(store::DAEMON_PID)
            .and_then(|pid| pid.parse::<libc::pid_t>().ok());
        if let Some(pid) = pid.filter(|pid| *pid > 0) {
            // SAFETY: kill(2) takes no pointers.
            unsafe {
                libc::kill(pid, signal);
            }
        }
    }

    /// Start the daemon with tmux out of reach. The CLI may run in a tmux
    /// pane next to Rex, and upstream daemon code still calls tmux in
    /// places.
    fn ensure_daemon(&self) -> Result<()> {
        if self.daemon_running() {
            return Ok(());
        }
        let socket = daemon::socket_path(&self.instance);
        let _ = std::fs::remove_file(&socket);
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("_sidebar-daemon")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        control::contain(&mut command, self.rex, &self.files)?;
        command
            .spawn()
            .context("Failed to start the sidebar daemon")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if self.daemon_running() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Err(anyhow!("Sidebar daemon failed to start"))
    }
}

fn width_text(width: &crate::config::SidebarWidth) -> String {
    match width {
        crate::config::SidebarWidth::Absolute(cols) => cols.to_string(),
        crate::config::SidebarWidth::Percent(percent) => format!("{percent}%"),
    }
}
