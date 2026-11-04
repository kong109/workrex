//! The daemon's view of the Rex server. The control plane keeps it current
//! from events, so a daemon tick reads memory and forks nothing: each `rex`
//! call costs about 24 ms of CPU, against one `list-panes` per tick on tmux.

use anyhow::Result;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::multiplexer::rex::events::Event;
use crate::multiplexer::rex::sidebar_api::View;
use crate::multiplexer::rex::{ProgramStatuses, RexBackend};
use crate::multiplexer::tmux::TmuxSidebarSnapshot;
use crate::multiplexer::{DaemonIo, LivePaneInfo};

use super::store::{self, Files, Options};

/// The sidebar choices upstream persists in settings.json.
#[derive(Debug, Default)]
pub(super) struct Persisted {
    pub layout: Option<String>,
    pub filter: Option<String>,
    pub group_by: Option<String>,
}

impl Persisted {
    pub fn load() -> Self {
        crate::state::StateStore::new()
            .and_then(|store| store.load_settings())
            .map(|settings| Self {
                layout: settings.sidebar_layout,
                filter: settings.sidebar_filter,
                group_by: settings.sidebar_group_by,
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Default)]
struct Cache {
    views: BTreeMap<String, View>,
    panes: HashMap<String, LivePaneInfo>,
    boot_id: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct Observed {
    cache: Mutex<Cache>,
    pub events_pid: AtomicU32,
    /// The program statuses the daemon's tick reads through its own backend.
    pub program_status: Arc<ProgramStatuses>,
}

impl Observed {
    pub fn new(program_status: Arc<ProgramStatuses>) -> Self {
        Self {
            program_status,
            ..Self::default()
        }
    }

    /// Replace every session's view, as at start and on the safety rebuild.
    pub fn replace_all(&self, rex: &RexBackend, views: Vec<View>, sidebars: &HashSet<String>) {
        let boot_id = rex.boot_id().ok();
        {
            let mut cache = self.cache.lock().unwrap();
            cache.views = views
                .into_iter()
                .map(|view| (view.session_id.clone(), view))
                .collect();
            if boot_id.is_some() {
                cache.boot_id = boot_id;
            }
        }
        self.fill(rex, sidebars);
    }

    pub fn put_view(&self, rex: &RexBackend, view: View, sidebars: &HashSet<String>) {
        self.cache
            .lock()
            .unwrap()
            .views
            .insert(view.session_id.clone(), view);
        self.fill(rex, sidebars);
    }

    /// Every window in the cached views.
    pub fn window_ids(&self) -> HashSet<String> {
        let cache = self.cache.lock().unwrap();
        cache
            .views
            .values()
            .flat_map(|view| &view.windows)
            .map(|window| window.window_id.clone())
            .collect()
    }

    pub fn drop_session(&self, session_id: &str) {
        let mut cache = self.cache.lock().unwrap();
        cache.views.remove(session_id);
        prune(&mut cache, &HashSet::new());
    }

    /// Fold a terminal event into the pane it describes.
    pub fn apply(&self, event: &Event) {
        self.program_status.apply(event);
        let mut cache = self.cache.lock().unwrap();
        match event {
            Event::ProcessChanged {
                block_id,
                foreground: Some(foreground),
                ..
            } => {
                if let Some(pane) = cache.panes.get_mut(block_id) {
                    pane.current_command = Some(foreground.command());
                    if let Some(cwd) = &foreground.cwd {
                        pane.working_dir = cwd.clone();
                    }
                }
            }
            Event::TitleChanged {
                block_id, title, ..
            } => {
                if let Some(pane) = cache.panes.get_mut(block_id) {
                    pane.title = (!title.is_empty()).then(|| title.clone());
                }
            }
            // The next fill asks Rex again, which skips an exited program.
            Event::ChildExited { block_id, .. } => {
                cache.panes.remove(block_id);
            }
            _ => {}
        }
    }

    /// Ask Rex about blocks the cache has not seen, and forget blocks that
    /// are gone or are sidebars.
    fn fill(&self, rex: &RexBackend, sidebars: &HashSet<String>) {
        let (views, mut known) = {
            let mut cache = self.cache.lock().unwrap();
            prune(&mut cache, sidebars);
            let views: Vec<View> = cache.views.values().cloned().collect();
            let known: HashSet<String> = cache.panes.keys().cloned().collect();
            (views, known)
        };
        known.extend(sidebars.iter().cloned());
        let fresh = rex.sidebar_live_panes(&views, &known);
        if !fresh.is_empty() {
            self.cache.lock().unwrap().panes.extend(fresh);
        }
    }

    pub fn snapshot(&self, prefs: &Options, settings: &Persisted) -> TmuxSidebarSnapshot {
        let cache = self.cache.lock().unwrap();
        build(
            &cache.views,
            &cache.panes,
            cache.boot_id.clone(),
            prefs,
            settings,
        )
    }
}

fn prune(cache: &mut Cache, sidebars: &HashSet<String>) {
    let live: HashSet<&str> = cache
        .views
        .values()
        .flat_map(|view| view.windows.iter().flat_map(|window| window.block_ids()))
        .collect();
    cache
        .panes
        .retain(|block, _| live.contains(block.as_str()) && !sidebars.contains(block));
}

fn build(
    views: &BTreeMap<String, View>,
    panes: &HashMap<String, LivePaneInfo>,
    boot_id: Option<String>,
    prefs: &Options,
    settings: &Persisted,
) -> TmuxSidebarSnapshot {
    let mut snapshot = TmuxSidebarSnapshot {
        live_panes: HashMap::new(),
        window_statuses: HashMap::new(),
        active_windows: HashSet::new(),
        pane_window_ids: HashMap::new(),
        pane_window_indexes: HashMap::new(),
        active_pane_ids: HashSet::new(),
        window_pane_counts: HashMap::new(),
        server_boot_id: boot_id,
        position: prefs.global(store::POSITION).map(String::from),
        layout: settings.layout.clone(),
        filter: settings.filter.clone(),
        views_enabled: prefs
            .global(crate::command::sidebar::view::OPTION)
            .map(String::from),
        sleeping_panes: prefs.global(store::SLEEPING).map(String::from),
        group_by: settings.group_by.clone(),
        expanded_groups: prefs.global(store::EXPANDED).map(String::from),
    };
    for view in views.values() {
        for (index, window) in view.windows.iter().enumerate() {
            let index = u32::try_from(index).unwrap_or(u32::MAX);
            if view.active_window_id.as_deref() == Some(window.window_id.as_str()) {
                snapshot
                    .active_windows
                    .insert((view.label.clone(), window.window_id.clone()));
            }
            if let Some(focused) = &window.focused_block_id {
                snapshot.active_pane_ids.insert(focused.clone());
            }
            let mut count = 0;
            for block in window.block_ids() {
                count += 1;
                snapshot
                    .pane_window_ids
                    .insert(block.to_string(), window.window_id.clone());
                snapshot
                    .pane_window_indexes
                    .insert(block.to_string(), index);
                if let Some(pane) = panes.get(block) {
                    let mut pane = pane.clone();
                    pane.window_index = Some(index);
                    snapshot.live_panes.insert(block.to_string(), pane);
                }
            }
            snapshot
                .window_pane_counts
                .insert(window.window_id.clone(), count);
        }
    }
    snapshot
}

/// The daemon's reads and writes under Rex. Dropping it, as the daemon
/// exits, stops the control plane's event stream.
pub(super) struct RexDaemonIo {
    pub observed: Arc<Observed>,
    pub files: Files,
}

impl DaemonIo for RexDaemonIo {
    fn sidebar_snapshot(&self) -> Result<TmuxSidebarSnapshot> {
        Ok(self
            .observed
            .snapshot(&self.files.prefs(), &Persisted::load()))
    }

    fn set_global_option(&self, name: &str, value: &str) -> Result<()> {
        self.files
            .update_daemon(|daemon| daemon.set_global(name, Some(value.to_string())))
    }

    fn unset_global_option(&self, name: &str) -> Result<()> {
        self.files
            .update_daemon(|daemon| daemon.set_global(name, None))
    }

    fn set_window_option(&self, window: &str, name: &str, value: &str) -> Result<()> {
        self.files
            .update_daemon(|daemon| daemon.set_window(window, name, Some(value.to_string())))
    }

    fn unset_window_option(&self, window: &str, name: &str) -> Result<()> {
        self.files
            .update_daemon(|daemon| daemon.set_window(window, name, None))
    }
}

impl Drop for RexDaemonIo {
    fn drop(&mut self) {
        let pid = self.observed.events_pid.load(Ordering::Relaxed);
        if pid != 0 {
            // SAFETY: kill(2) on a pid this process spawned and still owns.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const MULTI_WINDOW: &str =
        include_str!("../../../../multiplexer/testdata/rex_sidebar_view_multi.json");

    fn pane(cwd: &str) -> LivePaneInfo {
        LivePaneInfo {
            pid: Some(1),
            current_command: Some("zsh".into()),
            working_dir: PathBuf::from(cwd),
            title: None,
            session: Some("wm-proj".into()),
            window: Some("wm-feat-a".into()),
            session_id: None,
            window_id: None,
            window_index: None,
        }
    }

    #[test]
    fn the_snapshot_maps_every_block_and_only_scanned_panes() {
        let view: View = serde_json::from_str(MULTI_WINDOW).unwrap();
        let first = &view.windows[0];
        let block = first.block_ids().nth(1).unwrap().to_string();
        let mut views = BTreeMap::new();
        views.insert(view.session_id.clone(), view.clone());
        let panes = HashMap::from([(block.clone(), pane("/repo"))]);
        let mut prefs = Options::default();
        prefs.set_global(store::POSITION, Some("left".into()));
        prefs.set_global(store::SLEEPING, Some(block.clone()));
        let settings = Persisted {
            filter: Some("session".into()),
            ..Persisted::default()
        };

        let snapshot = build(&views, &panes, Some("boot".into()), &prefs, &settings);

        assert_eq!(snapshot.live_panes.len(), 1);
        assert_eq!(snapshot.live_panes[&block].window_index, Some(0));
        let blocks: usize = view.windows.iter().map(|w| w.block_ids().count()).sum();
        assert_eq!(snapshot.pane_window_ids.len(), blocks);
        assert_eq!(snapshot.window_pane_counts.len(), view.windows.len());
        assert_eq!(
            snapshot.window_pane_counts[&first.window_id],
            first.block_ids().count()
        );
        assert!(
            snapshot
                .active_windows
                .contains(&(view.label.clone(), view.active_window_id.clone().unwrap()))
        );
        assert_eq!(snapshot.position.as_deref(), Some("left"));
        assert_eq!(snapshot.sleeping_panes.as_deref(), Some(block.as_str()));
        assert_eq!(snapshot.filter.as_deref(), Some("session"));
        assert_eq!(snapshot.server_boot_id.as_deref(), Some("boot"));
        assert!(snapshot.window_statuses.is_empty());
    }

    #[test]
    fn terminal_events_update_cached_panes_without_a_scan() {
        let observed = Observed::default();
        observed
            .cache
            .lock()
            .unwrap()
            .panes
            .insert("block:1".into(), pane("/repo"));
        observed.apply(
            &Event::parse(r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:1","name":"process_changed","payload":{"generation":3,"foreground":{"pid":9,"name":"claude","cwd":"/repo/wt"}}}}}}"#)
                .unwrap(),
        );
        observed.apply(&Event::TitleChanged {
            session_id: "session:1".into(),
            block_id: "block:1".into(),
            title: "agent".into(),
        });
        let cache = observed.cache.lock().unwrap();
        let pane = &cache.panes["block:1"];
        assert_eq!(pane.current_command.as_deref(), Some("claude"));
        assert_eq!(pane.working_dir, PathBuf::from("/repo/wt"));
        assert_eq!(pane.title.as_deref(), Some("agent"));
    }

    #[test]
    fn a_process_event_names_the_command_the_agent_started_as() {
        let observed = Observed::default();
        observed
            .cache
            .lock()
            .unwrap()
            .panes
            .insert("block:1".into(), pane("/repo"));
        observed.apply(
            &Event::parse(r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:1","name":"process_changed","payload":{"generation":4,"foreground":{"pid":9,"name":"agy.1793778097211245000.old","argv0":"agy","cwd":"/repo/wt"}}}}}}"#)
                .unwrap(),
        );
        let cache = observed.cache.lock().unwrap();
        assert_eq!(
            cache.panes["block:1"].current_command.as_deref(),
            Some("agy")
        );
    }

    #[test]
    fn an_exited_program_leaves_the_snapshot() {
        let view: View = serde_json::from_str(MULTI_WINDOW).unwrap();
        let block = view.windows[0].block_ids().next().unwrap().to_string();
        let observed = Observed::default();
        {
            let mut cache = observed.cache.lock().unwrap();
            cache.views.insert(view.session_id.clone(), view.clone());
            cache.panes.insert(block.clone(), pane("/repo"));
        }
        let exited = format!(
            r#"{{"type":"session_event","payload":{{"session_id":"{}","event":{{"type":"block_event","payload":{{"block_id":"{block}","creator_name":"com.superlogical.terminal","name":"child_exited","payload":{{"exit_code":3}}}}}}}}}}"#,
            view.session_id
        );

        observed.apply(&Event::parse(&exited).unwrap());

        let snapshot = observed.snapshot(&Options::default(), &Persisted::default());
        assert!(!snapshot.live_panes.contains_key(&block));
    }

    #[test]
    fn sidebars_and_closed_blocks_leave_the_scan() {
        let view: View = serde_json::from_str(MULTI_WINDOW).unwrap();
        let block = view.windows[0].block_ids().next().unwrap().to_string();
        let mut cache = Cache::default();
        cache.views.insert(view.session_id.clone(), view);
        cache.panes.insert(block.clone(), pane("/repo"));
        cache.panes.insert("block:gone".into(), pane("/gone"));
        prune(&mut cache, &HashSet::new());
        assert_eq!(cache.panes.len(), 1);
        prune(&mut cache, &HashSet::from([block]));
        assert!(cache.panes.is_empty());
    }
}
