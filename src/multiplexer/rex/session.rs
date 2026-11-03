//! Rex session labels are not stable. Rex's Claude Code hook tells the agent
//! to rename its session on the first request, so a managed session is
//! tracked by the ID it was created with, in a small registry under the
//! workmux state directory. The registry also keeps the session each managed
//! session was opened from, which Rex does not remember for us.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use super::super::Multiplexer;
use super::super::types::CreateSessionParams;
use super::super::util;

use super::super::agent::shell_quote;
use super::{RexBackend, SessionId, SessionSummary, SessionView, found};

#[derive(Debug, Default, Serialize, Deserialize)]
struct ServerRecords {
    #[serde(default)]
    names: HashMap<String, String>,
    #[serde(default)]
    origins: HashMap<String, String>,
    /// The order a close moved out of place, until that close puts it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    restore: Option<Restore>,
}

/// Where a closing session's destination goes back to once the close is
/// done: next to its neighbors in the order taken before the first move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Restore {
    destination: String,
    /// The session order without the closing session.
    order: Vec<String>,
}

impl Restore {
    /// `rex session move` places that put the destination back, nearest
    /// neighbor first, so a neighbor closed meanwhile yields to the next.
    fn places(&self) -> Vec<[&str; 2]> {
        let Some(index) = self.order.iter().position(|id| *id == self.destination) else {
            return Vec::new();
        };
        let successors = self.order[index + 1..]
            .iter()
            .map(|id| ["--before", id.as_str()]);
        let predecessors = self.order[..index]
            .iter()
            .rev()
            .map(|id| ["--after", id.as_str()]);
        successors.chain(predecessors).collect()
    }
}

impl ServerRecords {
    fn retain_live(&mut self, live: &[SessionId]) {
        let live = |id: &String| live.iter().any(|session| session.as_str() == id);
        self.names.retain(|id, _| live(id));
        self.origins.retain(|id, origin| live(id) && live(origin));
    }
}

type ServerAddress = String;
type Registry = HashMap<ServerAddress, ServerRecords>;

fn read_registry(path: &Path) -> Registry {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The registry under its cross-process lock, held until this is dropped.
/// Every read that feeds a change must happen while it is held: a session
/// list taken before it can miss a session that a concurrent `add` records
/// meanwhile, and pruning with it would drop that record.
struct LockedRegistry {
    path: PathBuf,
    _lock: File,
    registry: Registry,
}

impl LockedRegistry {
    fn open(path: &Path) -> Result<Self> {
        let dir = path.parent().context("Rex registry path has no parent")?;
        fs::create_dir_all(dir)?;
        let lock = File::create(path.with_extension("lock"))?;
        lock.lock().context("Failed to lock the Rex registry")?;
        Ok(Self {
            path: path.to_path_buf(),
            _lock: lock,
            registry: read_registry(path),
        })
    }

    fn save(&self) -> Result<()> {
        let dir = self
            .path
            .parent()
            .context("Rex registry path has no parent")?;
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        file.write_all(serde_json::to_string(&self.registry)?.as_bytes())?;
        file.persist(&self.path)?;
        Ok(())
    }
}

struct LockedRecords {
    locked: LockedRegistry,
    key: ServerAddress,
}

impl LockedRecords {
    fn records(&mut self) -> &mut ServerRecords {
        self.locked.registry.entry(self.key.clone()).or_default()
    }

    fn save(&self) -> Result<()> {
        self.locked.save()
    }
}

/// Where the app goes when the session it shows closes: the preferred
/// session, then the session the closing one was opened from.
fn pick_destination(
    sessions: &[SessionSummary],
    closing: &SessionId,
    preferred: Option<&str>,
    origin: Option<SessionId>,
) -> Option<SessionId> {
    let other = |id: &SessionId| id != closing;
    if let Some(name) = preferred
        && let Some(session) = sessions
            .iter()
            .find(|session| session.label == name && other(&session.session_id))
    {
        return Some(session.session_id.clone());
    }
    origin.filter(|origin| {
        other(origin) && sessions.iter().any(|session| &session.session_id == origin)
    })
}

/// Names the worktree, not the Rex session: an agent renames its session.
pub(super) fn new_session_hint(worktree: &str) -> String {
    format!("Rex.app stays put: to see {worktree}, press Enter on its row in workmux dashboard.")
}

fn session_ids(sessions: &[SessionSummary]) -> Vec<SessionId> {
    sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect()
}

fn matches_id_or_label(session: &SessionSummary, name: &str) -> bool {
    session.session_id.as_str() == name || session.label == name
}

impl RexBackend {
    pub(super) fn session_named(&self, name: &str) -> Result<bool> {
        Ok(self
            .sessions()?
            .iter()
            .any(|session| matches_id_or_label(session, name)))
    }

    fn server_key(&self) -> String {
        self.resolve_instance_id()
            .unwrap_or_else(|_| "default".to_string())
    }

    fn records(&self) -> ServerRecords {
        self.registry
            .as_deref()
            .map(read_registry)
            .and_then(|mut registry| registry.remove(&self.server_key()))
            .unwrap_or_default()
    }

    fn lock_records(&self) -> Result<LockedRecords> {
        let path = self
            .registry
            .as_deref()
            .context("No state directory for the Rex registry")?;
        Ok(LockedRecords {
            locked: LockedRegistry::open(path)?,
            key: self.server_key(),
        })
    }

    fn update_records(&self, change: impl FnOnce(&mut ServerRecords) -> Result<()>) -> Result<()> {
        let mut locked = self.lock_records()?;
        change(locked.records())?;
        locked.save()
    }

    pub(super) fn apply_managed_names(&self, sessions: &mut [SessionSummary]) {
        let names = self.records().names;
        if names.is_empty() {
            return;
        }
        for session in sessions.iter_mut() {
            if let Some(name) = names.get(session.session_id.as_str()) {
                session.label = name.clone();
            }
        }
        sessions.sort_by_key(|session| !names.contains_key(session.session_id.as_str()));
    }

    pub(super) fn name_view(&self, view: &mut SessionView) {
        if let Some(name) = self.records().names.remove(view.session_id.as_str()) {
            view.label = name;
        }
    }

    pub(super) fn create_managed_session(&self, params: CreateSessionParams) -> Result<String> {
        #[derive(Deserialize)]
        struct CreatedWindow {
            block_ids: Vec<String>,
        }
        #[derive(Deserialize)]
        struct Created {
            session_id: SessionId,
            #[serde(default)]
            initial_windows: Vec<CreatedWindow>,
        }

        let full_name = util::prefixed(params.prefix, params.name);
        let mut window = json!({ "layout": self.block_layout(params.cwd, None)? });
        if let Some(label) = params.initial_window_name {
            window["window_label"] = json!(label);
        }
        let created: Created = self
            .api(
                "session.create",
                json!({ "label": full_name, "initial_windows": [window] }),
            )
            .context("Failed to create Rex session")?;
        let origin = self.caller.session.clone();
        self.update_records(|records| {
            records
                .names
                .insert(created.session_id.as_str().to_string(), full_name);
            if let Some(origin) = origin.filter(|origin| origin != &created.session_id) {
                records.origins.insert(
                    created.session_id.as_str().to_string(),
                    origin.as_str().to_string(),
                );
            }
            Ok(())
        })?;
        created
            .initial_windows
            .into_iter()
            .next()
            .and_then(|window| window.block_ids.into_iter().next())
            .ok_or_else(|| anyhow!("Rex created a session without a block"))
    }

    pub(super) fn rename_managed_session(
        &self,
        old_full_name: &str,
        new_full_name: &str,
    ) -> Result<()> {
        let session = self.first_session_labeled(old_full_name)?;
        self.api::<Value>(
            "session.set_label",
            json!({ "session_id": session.as_str(), "label": new_full_name }),
        )?;
        self.update_records(|records| {
            if let Some(name) = records.names.get_mut(session.as_str()) {
                *name = new_full_name.to_string();
            }
            Ok(())
        })
    }

    /// Rex.app shows the first session in the order when the session it
    /// shows is destroyed, and an app showing another session stays put.
    /// Rex does not say which app shows `closing`, so no app is asked to
    /// move: the destination goes first for the destroy and then back next
    /// to its neighbors, and only an app that showed `closing` lands on it.
    fn plan_restore(
        &self,
        closing: &SessionId,
        preferred: Option<&str>,
        origins: &HashMap<String, String>,
    ) -> Result<Option<Restore>> {
        let mut sessions = self.raw_sessions()?;
        let order = session_ids(&sessions)
            .into_iter()
            .filter(|id| id != closing)
            .map(|id| id.as_str().to_string())
            .collect();
        self.apply_managed_names(&mut sessions);
        let origin = origins.get(closing.as_str()).cloned().map(SessionId);
        Ok(
            pick_destination(&sessions, closing, preferred, origin).map(|destination| Restore {
                destination: destination.as_str().to_string(),
                order,
            }),
        )
    }

    /// `rex session move` attaches to the session it moves, which
    /// `session.move` requires.
    fn move_session(&self, session: &str, place: [&str; 2]) -> bool {
        let moved = self.run(&["session", "move", session, place[0], place[1]], None);
        if let Err(error) = &moved {
            tracing::debug!(error = %error, "rex: session move failed");
        }
        moved.is_ok()
    }

    fn put_back(&self, restore: &Restore) {
        restore
            .places()
            .into_iter()
            .any(|place| self.move_session(&restore.destination, place));
    }

    /// The whole close runs under the registry lock, so two closes, or a
    /// close and a deferred one, never interleave their moves. The order is
    /// recorded before the first move and cleared after the move back, so a
    /// close killed in between leaves the next close the order to put back
    /// before it reads the order itself.
    fn close_locked(
        &self,
        find: impl FnOnce() -> Result<Option<SessionId>>,
        preferred: Option<&str>,
    ) -> Result<()> {
        let mut locked = self.lock_records()?;
        if let Some(left) = locked.records().restore.take() {
            self.put_back(&left);
            locked.save()?;
        }
        let Some(session) = find()? else {
            return Ok(());
        };
        let restore = self.plan_restore(&session, preferred, &locked.records().origins)?;
        if let Some(restore) = &restore {
            locked.records().restore = Some(restore.clone());
            locked.save()?;
            self.move_session(&restore.destination, ["--index", "1"]);
        }
        let destroyed = self.destroy_session(&session);
        if let Some(restore) = &restore {
            self.put_back(restore);
        }
        // Closing prunes rather than forgetting the closing session itself,
        // so a close that never ran leaves its record intact.
        let live = self.raw_sessions();
        let records = locked.records();
        records.restore = None;
        if let Ok(live) = &live {
            records.retain_live(&session_ids(live));
        }
        locked.save()?;
        destroyed?;
        live.map(drop)
    }

    pub(super) fn close_session_to(&self, full_name: &str, preferred: Option<&str>) -> Result<()> {
        self.close_locked(|| self.labeled_session(full_name), preferred)
    }

    /// What `_session-close` runs: the deferred half of a close, for a
    /// session that may be gone by the time it runs.
    pub fn close_session_with_id(&self, id: &str, preferred: Option<&str>) -> Result<()> {
        let session = SessionId(id.to_string());
        self.close_locked(
            || {
                let live = session_ids(&self.raw_sessions()?);
                Ok(live.contains(&session).then_some(session))
            },
            preferred,
        )
    }

    /// A shell has no portable flock, so a deferred close runs the hidden
    /// `_session-close` command, which takes the registry lock like any
    /// other close.
    pub(super) fn shell_close_cmd(
        &self,
        session: &SessionId,
        preferred: Option<&str>,
    ) -> Result<String> {
        let exe = std::env::current_exe().context("Failed to locate the workmux executable")?;
        let mut words = vec!["env".to_string()];
        for var in Self::INHERITED_SCOPE_VARS {
            words.extend(["-u".to_string(), var.to_string()]);
        }
        if let Some(server) = &self.server {
            words.push(shell_quote(&format!("REX_SERVER={server}")));
        }
        words.push(shell_quote(&exe.to_string_lossy()));
        words.extend(["_session-close".to_string(), shell_quote(session.as_str())]);
        if let Some(preferred) = preferred {
            words.extend(["--to".to_string(), shell_quote(preferred)]);
        }
        Ok(format!("{} >/dev/null 2>&1 || true", words.join(" ")))
    }

    pub(super) fn shell_close_session_named_cmd(
        &self,
        full_name: &str,
        preferred: Option<&str>,
    ) -> Result<String> {
        Ok(match self.labeled_session(full_name)? {
            Some(session) => self.shell_close_cmd(&session, preferred)?,
            None => String::new(),
        })
    }

    /// The pane a dashboard jump to the session `full_name` lands on: the
    /// focused block of its active window.
    pub(super) fn session_pane(&self, full_name: &str) -> Result<Option<String>> {
        let Some(session) = self.labeled_session(full_name)? else {
            return Ok(None);
        };
        Ok(found(self.view(&session))?
            .and_then(|view| view.active_window()?.focused_block_id.clone())
            .map(|block| block.as_str().to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{CALLER_BLOCK, CALLER_SESSION, FakeRex, session_list};
    use super::*;

    #[test]
    fn a_session_is_found_by_id_after_its_label_changed() {
        let session: SessionSummary =
            serde_json::from_value(json!({"session_id": "session:1", "label": "write-date-file"}))
                .unwrap();
        assert!(matches_id_or_label(&session, "session:1"));
        assert!(matches_id_or_label(&session, "write-date-file"));
        assert!(!matches_id_or_label(&session, "session:2"));
    }

    fn summaries(sessions: &[(&str, &str)]) -> Vec<SessionSummary> {
        serde_json::from_str::<Value>(&session_list(sessions)).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|session| serde_json::from_value(session.clone()).unwrap())
            .collect()
    }

    #[test]
    fn the_destination_is_the_preferred_session_then_the_origin() {
        let sessions = summaries(&[
            ("session:closing", "wm-a"),
            ("session:home", "home"),
            ("session:origin", "work"),
        ]);
        let closing = SessionId("session:closing".into());
        let origin = || Some(SessionId("session:origin".into()));
        let pick = |preferred, origin| pick_destination(&sessions, &closing, preferred, origin);
        assert_eq!(
            pick(Some("home"), origin()),
            Some(SessionId("session:home".into()))
        );
        // A preferred session that is gone, or is the closing one, yields to the origin.
        assert_eq!(pick(Some("gone"), origin()), origin());
        assert_eq!(pick(Some("wm-a"), origin()), origin());
        assert_eq!(pick(None, origin()), origin());
        // An origin that is gone, or is the closing session, is no destination.
        assert_eq!(pick(None, Some(SessionId("session:gone".into()))), None);
        assert_eq!(pick(None, Some(closing.clone())), None);
        assert_eq!(pick(None, None), None);
    }

    /// The caller's pane is in `session:b`, labeled `wm-a`, which is about
    /// to close; `home`, second once `wm-a` is gone, is where an app showing
    /// it should land.
    fn closing_fake() -> FakeRex {
        FakeRex::new(&[
            ("session move", "{}"),
            ("session.destroy", "{}"),
            (
                "session.list",
                &session_list(&[
                    ("session:first", "first"),
                    (CALLER_SESSION, "wm-a"),
                    ("session:home", "home"),
                ]),
            ),
        ])
        .in_pane(CALLER_SESSION, CALLER_BLOCK)
    }

    /// The `session move` and `session.destroy` calls, in order, without
    /// the global arguments.
    fn steps(fake: &FakeRex) -> Vec<String> {
        fake.calls("")
            .into_iter()
            .filter(|call| call.contains("session move") || call.contains("session.destroy"))
            .map(|call| {
                let call = call.split(" -S ").nth(1).unwrap_or(&call);
                call.split_once(' ')
                    .map_or(call, |(_, rest)| rest)
                    .trim()
                    .to_string()
            })
            .collect()
    }

    fn assert_destination_leads_the_destroy(fake: &FakeRex) {
        let steps = steps(fake);
        assert_eq!(steps.len(), 3, "{steps:#?}");
        assert_eq!(steps[0], "session move session:home --index 1");
        assert!(steps[1].contains("session.destroy"), "{steps:#?}");
        // Back next to its neighbor, not to an index a concurrent change can shift.
        assert_eq!(steps[2], "session move session:home --after session:first");
        assert!(fake.calls(" do ").is_empty(), "no app is asked to move");
        assert!(fake.calls("client.list").is_empty());
        assert_eq!(fake.rex.records().restore, None);
    }

    #[test]
    fn closing_puts_the_destination_first_for_the_destroy_and_then_back() {
        let fake = closing_fake();
        fake.rex.close_session_to("wm-a", Some("home")).unwrap();
        assert_destination_leads_the_destroy(&fake);

        let mut outside = closing_fake();
        outside.rex.caller = Default::default();
        outside.rex.close_session_to("wm-a", Some("home")).unwrap();
        assert_destination_leads_the_destroy(&outside);
    }

    #[test]
    fn a_deferred_close_runs_the_locked_close_later() {
        let fake = closing_fake();
        let script = fake
            .rex
            .shell_close_session_named_cmd("wm-a", Some("home"))
            .unwrap();
        assert!(steps(&fake).is_empty(), "scheduling moves nothing");
        let exe = std::env::current_exe().unwrap();
        assert_eq!(
            script,
            format!(
                "env -u REX_SESSION -u REX_BLOCK 'REX_SERVER=unix://{}/s.sock' {} \
                 _session-close {CALLER_SESSION} --to home >/dev/null 2>&1 || true",
                fake.dir.path().display(),
                shell_quote(&exe.to_string_lossy())
            )
        );

        fake.rex
            .close_session_with_id(CALLER_SESSION, Some("home"))
            .unwrap();
        assert_destination_leads_the_destroy(&fake);
    }

    #[test]
    fn a_deferred_close_of_a_gone_session_does_nothing() {
        let fake = closing_fake();
        fake.rex
            .close_session_with_id("session:gone", Some("home"))
            .unwrap();
        assert!(steps(&fake).is_empty());
    }

    #[test]
    fn closing_without_a_destination_only_destroys() {
        let fake = closing_fake();
        fake.rex.close_session_to("wm-a", None).unwrap();
        assert!(fake.calls("session move").is_empty());
        assert_eq!(fake.calls("session.destroy").len(), 1);
    }

    #[test]
    fn a_failed_destroy_still_moves_the_destination_back() {
        let fake = FakeRex::new(&[
            ("session move", "{}"),
            ("session.destroy", "!Error: destroy failed"),
            (
                "session.list",
                &session_list(&[
                    ("session:first", "first"),
                    (CALLER_SESSION, "wm-a"),
                    ("session:home", "home"),
                ]),
            ),
        ]);
        let error = fake.rex.close_session_to("wm-a", Some("home")).unwrap_err();
        assert!(format!("{error:#}").contains("destroy failed"), "{error:#}");
        let steps = steps(&fake);
        assert_eq!(steps.len(), 3, "{steps:#?}");
        assert_eq!(steps[2], "session move session:home --after session:first");
        assert_eq!(fake.rex.records().restore, None);
    }

    #[test]
    fn the_order_is_recorded_before_the_first_move() {
        let state = tempfile::tempdir().unwrap();
        let registry = state.path().join("rex-sessions.json");
        let seen = state.path().join("seen");
        // The first move snapshots the registry as a close killed right
        // after it would leave it.
        let mut fake = FakeRex::new(&[
            (
                "session move",
                &format!(
                    "$[ -e '{seen}' ] || cp '{}' '{seen}'; echo '{{}}'",
                    registry.display(),
                    seen = seen.display()
                ),
            ),
            ("session.destroy", "{}"),
            (
                "session.list",
                &session_list(&[
                    ("session:first", "first"),
                    (CALLER_SESSION, "wm-a"),
                    ("session:home", "home"),
                ]),
            ),
        ]);
        fake.rex.registry = Some(registry);
        fake.rex.close_session_to("wm-a", Some("home")).unwrap();
        let left: Registry =
            serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
        assert_eq!(
            left.into_values().next().unwrap().restore,
            Some(Restore {
                destination: "session:home".into(),
                order: vec!["session:first".into(), "session:home".into()],
            })
        );
        assert_eq!(fake.rex.records().restore, None);
    }

    #[test]
    fn a_close_first_puts_back_an_order_a_killed_close_left() {
        let fake = closing_fake();
        fake.rex
            .update_records(|records| {
                records.restore = Some(Restore {
                    destination: "session:home".into(),
                    order: vec![
                        "session:gone".into(),
                        "session:home".into(),
                        "session:first".into(),
                    ],
                });
                Ok(())
            })
            .unwrap();
        fake.rex.close_session_to("wm-a", None).unwrap();
        let steps = steps(&fake);
        assert_eq!(steps[0], "session move session:home --before session:first");
        assert!(steps[1].contains("session.destroy"), "{steps:#?}");
        assert_eq!(steps.len(), 2, "{steps:#?}");
        assert_eq!(fake.rex.records().restore, None);
    }

    #[test]
    fn the_destination_goes_back_next_to_its_nearest_surviving_neighbor() {
        let restore = Restore {
            destination: "d".into(),
            order: ["a", "b", "d", "e", "f"].map(String::from).to_vec(),
        };
        assert_eq!(
            restore.places(),
            [
                ["--before", "e"],
                ["--before", "f"],
                ["--after", "b"],
                ["--after", "a"]
            ]
        );
        let fake = FakeRex::new(&[
            ("session move d --before e", "!Error: session e not found"),
            ("session move d --before f", "{}"),
        ]);
        fake.rex.put_back(&restore);
        assert_eq!(fake.calls("session move").len(), 2);
    }

    #[test]
    fn concurrent_closes_do_not_interleave() {
        let fake = FakeRex::new(&[
            ("session move", "$sleep 0.2; echo '{}'"),
            ("session.destroy", "{}"),
            (
                "session.list",
                &session_list(&[
                    ("session:first", "first"),
                    ("session:a", "wm-a"),
                    ("session:b", "wm-b"),
                    ("session:home", "home"),
                ]),
            ),
        ]);
        std::thread::scope(|scope| {
            for name in ["wm-a", "wm-b"] {
                let rex = &fake.rex;
                scope.spawn(move || rex.close_session_to(name, Some("home")).unwrap());
            }
        });
        let kinds: Vec<&str> = steps(&fake)
            .iter()
            .map(|step| {
                if step.contains("destroy") {
                    "destroy"
                } else {
                    "move"
                }
            })
            .collect();
        assert_eq!(
            kinds,
            ["move", "destroy", "move", "move", "destroy", "move"],
            "{:#?}",
            steps(&fake)
        );
    }

    #[test]
    fn a_deferred_close_keeps_the_record_until_the_session_is_gone() {
        let fake = closing_fake();
        fake.rex
            .update_records(|records| {
                records.names.insert(CALLER_SESSION.into(), "wm-a".into());
                Ok(())
            })
            .unwrap();
        fake.rex
            .shell_close_session_named_cmd("wm-a", None)
            .unwrap();
        assert_eq!(
            fake.rex.records().names.get(CALLER_SESSION).unwrap(),
            "wm-a"
        );
    }

    /// Each fake creates its own session and, if `create_managed_session`
    /// lists sessions, lists the ones created so far. File gates fix the
    /// interleaving: `b` creates only after `a` listed (or finished), and a
    /// list by `a` answers only after `b` recorded, so a list taken before
    /// the registry lock is always stale.
    fn adding_fake(dir: &Path, session: &str, create_gate: &str, list_gate: &str) -> FakeRex {
        let live = dir.join("live");
        let wait = |gate: &str| {
            if gate.is_empty() {
                String::new()
            } else {
                let gates = gate
                    .split('|')
                    .map(|gate| format!("[ -e '{}' ]", dir.join(gate).display()))
                    .collect::<Vec<_>>()
                    .join(" || ");
                format!("i=0; until {gates} || [ $i -ge 200 ]; do sleep 0.05; i=$((i+1)); done\n")
            }
        };
        let create = dir.join(format!("create-{session}.sh"));
        let created = json!({
            "session_id": session,
            "initial_windows": [{ "block_ids": [format!("block:{session}")] }],
        });
        let entry = json!({ "session_id": session, "label": "wm" });
        std::fs::write(
            &create,
            format!(
                "{}echo '{entry}' >> '{}'\necho '{created}'\n",
                wait(create_gate),
                live.display()
            ),
        )
        .unwrap();
        let list = dir.join(format!("list-{session}.sh"));
        std::fs::write(
            &list,
            format!(
                "list=$(paste -sd, '{}')\ntouch '{}'\n{}printf '{{\"sessions\":[%s]}}\\n' \"$list\"\n",
                live.display(),
                dir.join(format!("listed-{session}")).display(),
                wait(list_gate),
            ),
        )
        .unwrap();
        let mut fake = FakeRex::new(&[
            ("session.create", &format!("$sh '{}'", create.display())),
            ("session.list", &format!("$sh '{}'", list.display())),
        ]);
        fake.rex.registry = Some(dir.join("rex-sessions.json"));
        fake.rex.server = Some(format!("unix://{}/s.sock", dir.display()));
        fake
    }

    #[test]
    fn concurrent_adds_keep_both_sessions_recorded() {
        let shared = tempfile::tempdir().unwrap();
        let a = adding_fake(shared.path(), "session:a", "", "recorded-b");
        let b = adding_fake(shared.path(), "session:b", "listed-session:a|added-a", "");
        let cwd = shared.path().to_path_buf();
        let add = |fake: &FakeRex, name: &str| {
            fake.rex
                .create_managed_session(CreateSessionParams {
                    prefix: "wm-",
                    name,
                    cwd: &cwd,
                    initial_window_name: None,
                })
                .unwrap();
            std::fs::write(shared.path().join(format!("added-{name}")), "").unwrap();
            if name == "b" {
                std::fs::write(shared.path().join("recorded-b"), "").unwrap();
            }
        };
        std::thread::scope(|scope| {
            scope.spawn(|| add(&a, "a"));
            scope.spawn(|| add(&b, "b"));
        });
        let names = a.rex.records().names;
        assert_eq!(names.get("session:a").map(String::as_str), Some("wm-a"));
        assert_eq!(names.get("session:b").map(String::as_str), Some("wm-b"));
    }

    #[test]
    fn retain_live_drops_dead_sessions_and_dead_origins() {
        let mut records = ServerRecords::default();
        for (id, name) in [("session:a", "wm-a"), ("session:b", "wm-b")] {
            records.names.insert(id.into(), name.into());
        }
        records
            .origins
            .insert("session:a".into(), "session:home".into());
        records
            .origins
            .insert("session:b".into(), "session:gone".into());
        let live = ["session:a", "session:b", "session:home"].map(|id| SessionId(id.into()));
        records.retain_live(&live[..2]);
        assert_eq!(records.names.len(), 2);
        assert!(records.origins.is_empty());

        records
            .origins
            .insert("session:a".into(), "session:home".into());
        records.retain_live(&live);
        assert_eq!(records.origins.get("session:a").unwrap(), "session:home");
    }
}
