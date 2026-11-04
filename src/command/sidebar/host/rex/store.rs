//! Sidebar state for one Rex server. Rex has no user options, so the values
//! tmux keeps in `@workmux_*` options live in three files: preferences,
//! written by the CLI and the sidebar TUI; the daemon's outputs, written only
//! by the daemon; and the sidebar blocks placed, written by the CLI, the
//! daemon's control plane and a quitting sidebar. Every write holds the
//! file's lock for its read-modify-write.

use anyhow::{Context, Result};
use nix::fcntl::{Flock, FlockArg};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

pub(super) const SCOPE: &str = "@workmux_sidebar_scope";
pub(super) const OPTOUT: &str = "@workmux_sidebar_optout_sessions";
pub(super) const POSITION: &str = "@workmux_sidebar_position";
pub(super) const WIDTH: &str = "@workmux_sidebar_width";
pub(super) const SLEEPING: &str = "@workmux_sleeping_panes";
pub(super) const EXPANDED: &str = "@workmux_sidebar_expanded";
pub(super) const DAEMON_PID: &str = "@workmux_sidebar_daemon_pid";
pub(super) const AGENTS: &str = "@workmux_sidebar_agents";
/// Per window, in the daemon's file: the window's sidebar was closed by the
/// user, so no reconcile brings it back. The value is the session id.
pub(super) const CLOSED: &str = "@workmux_sidebar_closed";

/// Option values by name, server-wide and per window, the way tmux scopes
/// them.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Options {
    #[serde(default)]
    pub globals: BTreeMap<String, String>,
    #[serde(default)]
    pub windows: BTreeMap<String, BTreeMap<String, String>>,
}

impl Options {
    pub fn global(&self, name: &str) -> Option<&str> {
        self.globals
            .get(name)
            .map(String::as_str)
            .filter(|value| !value.is_empty())
    }

    pub fn window(&self, window: &str, name: &str) -> Option<&str> {
        self.windows.get(window)?.get(name).map(String::as_str)
    }

    pub fn set_global(&mut self, name: &str, value: Option<String>) {
        match value.filter(|value| !value.is_empty()) {
            Some(value) => self.globals.insert(name.to_string(), value),
            None => self.globals.remove(name),
        };
    }

    pub fn set_window(&mut self, window: &str, name: &str, value: Option<String>) {
        match value {
            Some(value) => {
                self.windows
                    .entry(window.to_string())
                    .or_default()
                    .insert(name.to_string(), value);
            }
            None => {
                if let Some(options) = self.windows.get_mut(window) {
                    options.remove(name);
                    if options.is_empty() {
                        self.windows.remove(window);
                    }
                }
            }
        }
    }

    pub fn global_set(&self, name: &str) -> HashSet<String> {
        self.global(name)
            .map(|value| value.split_whitespace().map(String::from).collect())
            .unwrap_or_default()
    }

    /// Every window that has option `name`, with its value.
    pub fn windows_with(&self, name: &str) -> BTreeMap<String, String> {
        self.windows
            .iter()
            .filter_map(|(window, options)| Some((window.clone(), options.get(name)?.clone())))
            .collect()
    }

    pub fn set_global_set(&mut self, name: &str, ids: &HashSet<String>) {
        let mut ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        ids.sort_unstable();
        self.set_global(name, Some(ids.join(" ")));
    }
}

/// A sidebar block the control plane placed, keyed by its window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct WindowSidebar {
    pub session_id: String,
    pub window_id: String,
    pub block_id: String,
    pub split_id: String,
    /// An older sidebar block in the window that failed to close. The next
    /// pass closes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stray: Option<String>,
    /// The root split ratio as Rex reported it after the last placement.
    /// A different ratio in a later view means the user dragged the border.
    pub applied_ratio: f64,
    pub last_cols: u32,
    pub window_cols: u32,
    /// The window's tiled blocks as the last pass saw them, and when a pass
    /// first saw them change, in Unix milliseconds.
    #[serde(default)]
    pub panes: String,
    #[serde(default)]
    pub panes_changed_ms: u64,
}

pub(super) type Records = BTreeMap<String, WindowSidebar>;

#[derive(Debug, Clone)]
pub(super) struct Files {
    dir: PathBuf,
    key: String,
}

impl Files {
    pub fn for_instance(instance_id: &str) -> Result<Self> {
        Ok(Self::at(
            crate::xdg::state_dir()?.join("sidebar"),
            instance_id,
        ))
    }

    pub fn at(dir: PathBuf, instance_id: &str) -> Self {
        Self {
            dir,
            key: instance_key(instance_id),
        }
    }

    fn path(&self, kind: &str) -> PathBuf {
        self.dir.join(format!("rex-{}-{kind}.json", self.key))
    }

    /// An empty directory the sidebar's tmux calls resolve their socket in,
    /// so an upstream tmux call under Rex finds no server instead of an
    /// unrelated one.
    pub fn no_tmux_dir(&self) -> Result<PathBuf> {
        let dir = self.dir.join("no-tmux");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create {}", dir.display()))?;
        Ok(dir)
    }

    pub fn prefs(&self) -> Options {
        read(&self.path("prefs"))
    }

    pub fn update_prefs<R>(&self, change: impl FnOnce(&mut Options) -> R) -> Result<R> {
        let _lock = self.lock("prefs")?;
        update(&self.path("prefs"), change)
    }

    pub fn daemon(&self) -> Options {
        read(&self.path("daemon"))
    }

    /// Only the daemon writes this file, but from two threads, its tick and
    /// its control plane, so writes take the lock.
    pub fn update_daemon<R>(&self, change: impl FnOnce(&mut Options) -> R) -> Result<R> {
        let _lock = self.lock("daemon")?;
        update(&self.path("daemon"), change)
    }

    pub fn records(&self) -> Records {
        read(&self.path("blocks"))
    }

    /// Hold the block records for a read-modify-write. The CLI and the
    /// daemon both place and close sidebars, one at a time.
    pub fn lock_records(&self) -> Result<RecordsGuard> {
        let lock = self.lock("blocks")?;
        let path = self.path("blocks");
        Ok(RecordsGuard {
            records: read(&path),
            path,
            _lock: lock,
        })
    }

    fn lock(&self, kind: &str) -> Result<Flock<File>> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("Failed to create {}", self.dir.display()))?;
        let path = self.dir.join(format!("rex-{}-{kind}.lock", self.key));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("Failed to open {}", path.display()))?;
        Flock::lock(file, FlockArg::LockExclusive)
            .map_err(|(_file, errno)| errno)
            .with_context(|| format!("Failed to lock {}", path.display()))
    }
}

pub(super) struct RecordsGuard {
    pub records: Records,
    path: PathBuf,
    _lock: Flock<File>,
}

impl RecordsGuard {
    pub fn save(&self) -> Result<()> {
        write(&self.path, &self.records)
    }
}

/// The daemon socket's key scheme, so every file for one server sorts
/// together and the name stays short.
fn instance_key(instance_id: &str) -> String {
    let mut key = 0xcbf29ce484222325u64;
    for byte in instance_id.as_bytes() {
        key ^= u64::from(*byte);
        key = key.wrapping_mul(0x100000001b3);
    }
    format!("{key:016x}")
}

/// A missing or unreadable file is the default value: a corrupt preference
/// must not stop the sidebar.
fn read<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
        .unwrap_or_default()
}

fn update<T, R>(path: &Path, change: impl FnOnce(&mut T) -> R) -> Result<R>
where
    T: DeserializeOwned + Serialize + Default + PartialEq,
{
    let before: T = read(path);
    let mut value: T = read(path);
    let result = change(&mut value);
    if value != before {
        write(path, &value)?;
    }
    Ok(result)
}

fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    crate::util::write_atomic(path, serde_json::to_string_pretty(value)?.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(window: &str) -> WindowSidebar {
        WindowSidebar {
            session_id: "session:1".into(),
            window_id: window.into(),
            block_id: format!("block:{window}"),
            split_id: format!("split:{window}"),
            stray: None,
            applied_ratio: 0.18,
            last_cols: 30,
            window_cols: 166,
            panes: String::new(),
            panes_changed_ms: 0,
        }
    }

    /// A records file from before `stray` existed loads, and a record
    /// without a stray writes no `stray` key.
    #[test]
    fn an_old_records_file_loads_and_writes_no_stray() {
        let old = r#"{"window:w":{"session_id":"session:s","window_id":"window:w","block_id":"block:b","split_id":"split:x","applied_ratio":0.18,"last_cols":30,"window_cols":166}}"#;
        let records: Records = serde_json::from_str(old).unwrap();
        assert_eq!(records["window:w"].stray, None);
        let written = serde_json::to_string(&records).unwrap();
        assert!(!written.contains("stray"), "{written}");
    }

    #[test]
    fn missing_files_are_defaults_and_writes_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::at(dir.path().join("sidebar"), "unix:///tmp/wr.sock");
        assert_eq!(files.prefs(), Options::default());
        assert!(files.records().is_empty());

        files
            .update_prefs(|prefs| prefs.set_global(SCOPE, Some("global".into())))
            .unwrap();
        files
            .update_daemon(|daemon| {
                daemon.set_window("window:1", "@workmux_sidebar_scope", Some("{}".into()))
            })
            .unwrap();
        let mut guard = files.lock_records().unwrap();
        guard.records.insert("window:1".into(), record("window:1"));
        guard.save().unwrap();
        drop(guard);

        assert_eq!(files.prefs().global(SCOPE), Some("global"));
        assert_eq!(
            files.daemon().window("window:1", "@workmux_sidebar_scope"),
            Some("{}")
        );
        assert_eq!(files.records()["window:1"], record("window:1"));
    }

    #[test]
    fn unknown_keys_and_corrupt_files_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::at(dir.path().to_path_buf(), "default");
        std::fs::write(
            files.path("prefs"),
            r#"{"globals":{"@workmux_sidebar_scope":"global"},"future":1}"#,
        )
        .unwrap();
        assert_eq!(files.prefs().global(SCOPE), Some("global"));
        std::fs::write(files.path("blocks"), "{not json").unwrap();
        assert!(files.records().is_empty());
    }

    #[test]
    fn concurrent_preference_writers_both_land() {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::at(dir.path().to_path_buf(), "default");
        let writers: Vec<_> = (0..8)
            .map(|n| {
                let files = files.clone();
                std::thread::spawn(move || {
                    files
                        .update_prefs(|prefs| {
                            let mut ids = prefs.global_set(OPTOUT);
                            ids.insert(format!("session:{n}"));
                            std::thread::sleep(std::time::Duration::from_millis(5));
                            prefs.set_global_set(OPTOUT, &ids);
                        })
                        .unwrap();
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(files.prefs().global_set(OPTOUT).len(), 8);
    }

    #[test]
    fn instances_get_distinct_short_keys() {
        let a = Files::at(PathBuf::from("/s"), "unix:///tmp/a.sock");
        let b = Files::at(PathBuf::from("/s"), "unix:///tmp/b.sock");
        assert_ne!(a.path("prefs"), b.path("prefs"));
        assert_eq!(a.key.len(), 16);
    }

    #[test]
    fn empty_values_clear_options() {
        let mut options = Options::default();
        options.set_global(SLEEPING, Some("block:1".into()));
        options.set_global(SLEEPING, Some(String::new()));
        assert_eq!(options.global(SLEEPING), None);
        options.set_window("window:1", "x", Some("1".into()));
        options.set_window("window:1", "x", None);
        assert!(options.windows.is_empty());
    }
}
