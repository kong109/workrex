//! Agents workrex launched, kept until their first status hook so readers
//! can find a pane whose agent has not reported, such as one held at a
//! folder-trust prompt.
//!
//! The launcher writes one file per pane under `launched/`. Once a hook
//! writes the pane's state file, that file speaks for the pane, and pruning
//! readers drop the launch record, as they do once the pane is gone.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::multiplexer::{AgentPane, Launch, LivePaneInfo, Multiplexer};

use super::{PaneKey, StateStore};

/// How old a record must be before a reader may prune it, so a reader
/// holding a pane snapshot taken before the launch keeps a fresh record.
const PRUNE_AFTER_SECS: u64 = 10;

/// Age past which a record is pruned whatever its instance, so records of
/// retired Rex servers do not pile up.
const RECORD_TTL_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct LaunchRecord {
    pub pane_key: PaneKey,
    pub workdir: PathBuf,
    /// When the agent was launched (Unix seconds).
    pub at: u64,
    /// PID of the pane's shell, which tells a recycled pane ID.
    pub pane_pid: Option<u32>,
    /// The pane's foreground program when the agent was typed into it.
    pub shell: Option<String>,
    pub agent_kind: String,
    /// The launch carried a prompt.
    pub prompted: bool,
}

/// Record that workrex is about to type a `agent_kind` agent into the shell
/// of `pane_id`, with a prompt when `prompted`. Failures only lose the
/// record.
pub fn record_launch<M: Multiplexer + ?Sized>(
    mux: &M,
    pane_id: &str,
    workdir: &Path,
    agent_kind: &str,
    prompted: bool,
) {
    if std::env::var_os("WORKREX_DISABLE_SET_WINDOW_STATUS").is_some() {
        return;
    }
    let record = LaunchRecord::of(mux, pane_id, workdir, agent_kind, prompted);
    if let Err(error) = StateStore::new().and_then(|store| store.write_launch(&record)) {
        warn!(%error, pane_id, "failed to record the agent launch");
    }
}

impl LaunchRecord {
    /// The launch of an agent into `pane_id` now, while its shell has the
    /// foreground.
    fn of<M: Multiplexer + ?Sized>(
        mux: &M,
        pane_id: &str,
        workdir: &Path,
        agent_kind: &str,
        prompted: bool,
    ) -> Self {
        let live = mux.get_live_pane_info(pane_id).ok().flatten();
        Self {
            pane_key: PaneKey {
                backend: mux.name().to_string(),
                instance: mux.instance_id(),
                pane_id: pane_id.to_string(),
            },
            workdir: workdir.to_path_buf(),
            at: crate::remote::cache::now(),
            pane_pid: live.as_ref().and_then(|live| live.pid),
            shell: live.and_then(|live| live.current_command),
            agent_kind: agent_kind.to_string(),
            prompted,
        }
    }
}

impl StateStore {
    pub(crate) fn write_launch(&self, record: &LaunchRecord) -> Result<()> {
        let dir = self.launched_dir();
        fs::create_dir_all(&dir)?;
        crate::util::write_atomic(
            &dir.join(record.pane_key.to_filename()),
            serde_json::to_string_pretty(record)?.as_bytes(),
        )
    }

    /// Agent panes of `backend` `instance` launched by workrex whose agents
    /// fired no hook: none of `hooked` names them. With `prune`, records
    /// older than [`PRUNE_AFTER_SECS`] of hooked panes, of panes gone from
    /// `live_panes` and of recycled pane IDs are removed, as are records of
    /// any instance older than [`RECORD_TTL_SECS`]. Unreadable records and
    /// failed removals are skipped, never failing the load.
    pub(crate) fn unhooked_launches(
        &self,
        backend: &str,
        instance: &str,
        hooked: &HashSet<String>,
        live_panes: &HashMap<String, LivePaneInfo>,
        prune: bool,
    ) -> Vec<AgentPane> {
        let entries = match fs::read_dir(self.launched_dir()) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                warn!(%error, "failed to read agent launch records");
                return Vec::new();
            }
        };
        let now = crate::remote::cache::now();
        let mut panes = Vec::new();
        for entry in entries {
            let path = match entry {
                Ok(entry) => entry.path(),
                Err(error) => {
                    warn!(%error, "failed to read an agent launch record");
                    continue;
                }
            };
            let Some(record) = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<LaunchRecord>(&bytes).ok())
            else {
                continue;
            };
            let age = now.saturating_sub(record.at);
            let key = &record.pane_key;
            let ours = key.backend == backend && key.instance == instance;
            let live = live_panes
                .get(&key.pane_id)
                .filter(|live| ours && !recycled(&record, live));
            match live {
                _ if prune && age >= RECORD_TTL_SECS => remove(&path),
                Some(live) if !hooked.contains(&key.pane_id) => panes.push(pane(&record, live)),
                _ if prune && ours && age >= PRUNE_AFTER_SECS => remove(&path),
                _ => {}
            }
        }
        panes.sort_by(|a, b| a.pane_id.cmp(&b.pane_id));
        panes
    }
}

fn remove(path: &Path) {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            warn!(%error, path = %path.display(), "failed to remove an agent launch record");
        }
        _ => {}
    }
}

fn recycled(record: &LaunchRecord, live: &LivePaneInfo) -> bool {
    record
        .pane_pid
        .zip(live.pid)
        .is_some_and(|(recorded, live)| recorded != live)
}

fn pane(record: &LaunchRecord, live: &LivePaneInfo) -> AgentPane {
    AgentPane {
        session: live.session.clone().unwrap_or_default(),
        window_name: live.window.clone().unwrap_or_default(),
        pane_id: record.pane_key.pane_id.clone(),
        window_id: live.window_id.clone().unwrap_or_default(),
        window_index: live.window_index,
        path: record.workdir.clone(),
        pane_title: live.title.clone(),
        status: None,
        status_ts: None,
        activity_ts: None,
        updated_ts: None,
        window_cmd: live.current_command.clone(),
        agent_command: None,
        agent_kind: Some(record.agent_kind.clone()),
        prompt: None,
        remote: None,
        launched: Some(Launch {
            at: record.at,
            prompted: record.prompted,
            at_shell: live.current_command.is_some() && live.current_command == record.shell,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::super::reported::fake_mux::{FakeMux, INSTANCE};
    use super::super::reported::tests::hooked;
    use super::*;
    use crate::multiplexer::{AgentStatus, FIRST_HOOK_GRACE_SECS, PaneStatus};

    fn pane(pid: u32, command: &str) -> LivePaneInfo {
        LivePaneInfo {
            pid: Some(pid),
            current_command: Some(command.to_string()),
            working_dir: PathBuf::from("/repo/wt"),
            title: None,
            session: Some("main".to_string()),
            window: Some("wr-feat".to_string()),
            session_id: None,
            window_id: None,
            window_index: None,
        }
    }

    /// A store holding the launch, `age` seconds ago, of a `gemini` agent
    /// into `block:a`, whose shell had the foreground at launch.
    fn launched(prompted: bool, age: u64) -> (tempfile::TempDir, StateStore, u64) {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::with_path(dir.path().to_path_buf()).unwrap();
        let record = record(prompted, age);
        store.write_launch(&record).unwrap();
        (dir, store, record.at)
    }

    fn record(prompted: bool, age: u64) -> LaunchRecord {
        let at_launch = FakeMux {
            fresh: HashMap::from([("block:a".to_string(), pane(7, "zsh"))]),
            ..FakeMux::default()
        };
        let mut record = LaunchRecord::of(
            &at_launch,
            "block:a",
            Path::new("/repo/wt"),
            "gemini",
            prompted,
        );
        assert_eq!(record.pane_key.instance, INSTANCE);
        record.at -= age;
        record
    }

    fn running(command: &str) -> FakeMux {
        FakeMux {
            live: HashMap::from([("block:a".to_string(), pane(7, command))]),
            ..FakeMux::default()
        }
    }

    fn agents(store: &StateStore, mux: &FakeMux) -> Vec<AgentPane> {
        store.load_all_reconciled_agents(mux).unwrap()
    }

    fn records(store: &StateStore) -> usize {
        fs::read_dir(store.launched_dir()).unwrap().count()
    }

    #[test]
    fn a_launched_agent_is_an_agent_before_its_first_hook() {
        let (_dir, store, at) = launched(true, 0);

        let found = agents(&store, &running("gemini"));

        assert_eq!(found.len(), 1);
        let agent = &found[0];
        assert_eq!(agent.pane_id, "block:a");
        assert_eq!(agent.path, PathBuf::from("/repo/wt"));
        assert_eq!(agent.agent_kind.as_deref(), Some("gemini"));
        assert_eq!(agent.status, None);
        assert!(!agent.launched.unwrap().at_shell);
        assert_eq!(agent.pane_status(at + FIRST_HOOK_GRACE_SECS - 1), None);
        assert_eq!(
            agent.pane_status(at + FIRST_HOOK_GRACE_SECS),
            Some(PaneStatus::Stalled)
        );
    }

    #[test]
    fn an_unprompted_agent_never_stalls() {
        let (_dir, store, at) = launched(false, 0);

        let found = agents(&store, &running("gemini"));

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pane_status(at + 10 * FIRST_HOOK_GRACE_SECS), None);
    }

    #[test]
    fn a_pane_back_at_its_shell_is_marked() {
        let (_dir, store, _) = launched(true, 0);

        let found = agents(&store, &running("zsh"));

        assert!(found[0].launched.unwrap().at_shell);
    }

    #[test]
    fn the_first_hook_replaces_the_launch() {
        let (_dir, store, at) = launched(true, PRUNE_AFTER_SECS);
        let state = hooked("block:a", AgentStatus::Working, at);
        store.upsert_agent(&state).unwrap();
        let mux = running("claude");

        let found = agents(&store, &mux);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].status, Some(AgentStatus::Working));
        assert_eq!(found[0].launched, None);

        store.delete_agent(&state.pane_key).unwrap();
        assert!(agents(&store, &mux).is_empty());
    }

    #[test]
    fn a_closed_or_recycled_pane_drops_the_launch() {
        for mux in [
            FakeMux::default(),
            FakeMux {
                live: HashMap::from([("block:a".to_string(), pane(8, "gemini"))]),
                ..FakeMux::default()
            },
        ] {
            let (_dir, store, _) = launched(true, PRUNE_AFTER_SECS);
            assert!(agents(&store, &mux).is_empty());
            assert!(agents(&store, &running("gemini")).is_empty());
        }
    }

    #[test]
    fn a_reader_whose_snapshot_misses_a_fresh_launch_keeps_it() {
        let (_dir, store, _) = launched(true, PRUNE_AFTER_SECS - 1);

        assert!(agents(&store, &FakeMux::default()).is_empty());

        assert_eq!(agents(&store, &running("gemini")).len(), 1);
    }

    #[test]
    fn records_of_any_instance_expire() {
        let (_dir, store, _) = launched(true, RECORD_TTL_SECS - 1);
        let mut retired = record(true, RECORD_TTL_SECS);
        retired.pane_key.instance = "unix:///tmp/retired.sock".to_string();
        store.write_launch(&retired).unwrap();
        let mut current = record(true, RECORD_TTL_SECS);
        current.pane_key.pane_id = "block:b".to_string();
        store.write_launch(&current).unwrap();
        let mux = FakeMux {
            live: HashMap::from([
                ("block:a".to_string(), pane(7, "gemini")),
                ("block:b".to_string(), pane(7, "gemini")),
            ]),
            ..FakeMux::default()
        };

        let found = agents(&store, &mux);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pane_id, "block:a");
        assert_eq!(records(&store), 1);
    }

    #[test]
    fn an_unreadable_launch_directory_does_not_fail_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = StateStore::with_path(dir.path().to_path_buf()).unwrap();
        fs::write(store.launched_dir(), "not a directory").unwrap();

        assert!(agents(&store, &running("gemini")).is_empty());
    }
}
