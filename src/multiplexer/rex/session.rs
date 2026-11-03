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
use std::path::Path;

use super::super::Multiplexer;
use super::super::types::CreateSessionParams;
use super::super::util;

use super::{RexBackend, SessionId, SessionSummary, SessionView};

#[derive(Debug, Default, Serialize, Deserialize)]
struct ServerRecords {
    #[serde(default)]
    names: HashMap<String, String>,
    #[serde(default)]
    origins: HashMap<String, String>,
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

/// Every read that feeds `change` must run inside `change`, under the lock:
/// a session list taken before it can miss a session that a concurrent `add`
/// records meanwhile, and pruning with it would drop that record.
fn update_registry(path: &Path, change: impl FnOnce(&mut Registry) -> Result<()>) -> Result<()> {
    let dir = path.parent().context("Rex registry path has no parent")?;
    fs::create_dir_all(dir)?;
    let lock = File::create(path.with_extension("lock"))?;
    lock.lock().context("Failed to lock the Rex registry")?;
    let mut registry = read_registry(path);
    change(&mut registry)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(serde_json::to_string(&registry)?.as_bytes())?;
    file.persist(path)?;
    Ok(())
}

/// Whether to move the app before closing `closing`. Rex does not report
/// which session an app shows. Run from inside the closing session, the app
/// most likely shows it. Run from anywhere else, moving the app could pull
/// the user away from what they are looking at, which tmux never does.
fn moves_app(caller: Option<&SessionId>, closing: &SessionId) -> bool {
    caller == Some(closing)
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

    fn update_records(&self, change: impl FnOnce(&mut ServerRecords) -> Result<()>) -> Result<()> {
        let path = self
            .registry
            .as_deref()
            .context("No state directory for the Rex registry")?;
        let key = self.server_key();
        update_registry(path, |registry| change(registry.entry(key).or_default()))
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

    /// Drops the records of sessions that are gone, listing them under the
    /// registry lock. Closing prunes rather than forgetting the closing
    /// session itself, so a close that never ran leaves its record intact.
    fn prune_records(&self) -> Result<()> {
        self.update_records(|records| {
            let live: Vec<SessionId> = self
                .raw_sessions()?
                .into_iter()
                .map(|session| session.session_id)
                .collect();
            records.retain_live(&live);
            Ok(())
        })
    }

    fn destination(
        &self,
        closing: &SessionId,
        preferred: Option<&str>,
    ) -> Result<Option<SessionId>> {
        let origin = self
            .records()
            .origins
            .remove(closing.as_str())
            .map(SessionId);
        Ok(pick_destination(
            &self.sessions()?,
            closing,
            preferred,
            origin,
        ))
    }

    /// The `rex` arguments that move the app showing the caller off
    /// `closing`, when it may be showing it.
    fn relocation(
        &self,
        closing: &SessionId,
        preferred: Option<&str>,
    ) -> Result<Option<Vec<String>>> {
        if !moves_app(self.caller.session.as_ref(), closing) {
            return Ok(None);
        }
        let Some(destination) = self.destination(closing, preferred)? else {
            return Ok(None);
        };
        Ok(self
            .app_showing_caller()?
            .map(|app| Self::select_args(&app, &destination, None)))
    }

    pub(super) fn close_session_to(&self, full_name: &str, preferred: Option<&str>) -> Result<()> {
        if let Some(session) = self.labeled_session(full_name)? {
            if let Some(select) = self.relocation(&session, preferred)? {
                let select: Vec<&str> = select.iter().map(String::as_str).collect();
                if let Err(error) = self.run(&select, None) {
                    tracing::debug!(error = %error, "rex:relocation select failed");
                }
            }
            self.destroy_session(&session)?;
            self.prune_records()?;
        }
        Ok(())
    }

    /// Rex.app shows the first session in its sidebar when the session it
    /// shows is destroyed, so the select runs to completion before the
    /// destroy starts.
    pub(super) fn shell_close_cmd(
        &self,
        session: &SessionId,
        preferred: Option<&str>,
    ) -> Result<String> {
        let destroy = self.shell_destroy_cmd(session);
        let script = match self.relocation(session, preferred)? {
            Some(select) => {
                let select: Vec<&str> = select.iter().map(String::as_str).collect();
                format!("{}; {destroy}", self.shell_rex_cmd(&select))
            }
            None => destroy,
        };
        self.prune_records()?;
        Ok(script)
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
}

#[cfg(test)]
mod tests {
    use super::super::tests::{
        APP, CALLER_BLOCK, CALLER_SESSION, FakeRex, clients, owner, session_list,
    };
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

    /// The caller's pane is in `session:b`, labeled `wm-a`, which the app
    /// showing it is about to lose; `home` is where it should go.
    fn closing_fake() -> FakeRex {
        FakeRex::new(&[
            ("block call -b block:caller size", &owner(APP)),
            ("client.list", &clients(&[(APP, &[CALLER_SESSION])])),
            ("session.select", "{}"),
            ("session.destroy", "{}"),
            (
                "session.list",
                &session_list(&[(CALLER_SESSION, "wm-a"), ("session:home", "home")]),
            ),
        ])
        .in_pane(CALLER_SESSION, CALLER_BLOCK)
    }

    fn assert_select_precedes_destroy(fake: &FakeRex) {
        let calls = fake.calls("");
        let position = |needle: &str| {
            calls
                .iter()
                .position(|call| call.contains(needle))
                .unwrap_or_else(|| panic!("no {needle} in {calls:#?}"))
        };
        assert!(
            calls[position("session.select")].contains("session_id=session:home"),
            "{calls:#?}"
        );
        assert!(
            position("session.select") < position("session.destroy"),
            "{calls:#?}"
        );
    }

    #[test]
    fn closing_moves_the_app_before_it_destroys_the_session() {
        let fake = closing_fake();
        fake.rex.close_session_to("wm-a", Some("home")).unwrap();
        assert_select_precedes_destroy(&fake);
    }

    #[test]
    fn a_deferred_close_moves_the_app_before_it_destroys_the_session() {
        let fake = closing_fake();
        let script = fake
            .rex
            .shell_close_session_named_cmd("wm-a", Some("home"))
            .unwrap();
        assert!(fake.calls("session.destroy").is_empty());
        let status = std::process::Command::new("sh")
            .args(["-c", &script])
            .stdin(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert_select_precedes_destroy(&fake);
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

    /// Each fake creates its own session, then lists the sessions both have
    /// created so far, taking `delay` to answer: a slow list taken before the
    /// registry lock is stale once a faster `add` has recorded its session.
    fn adding_fake(dir: &Path, session: &str, delay: &str) -> FakeRex {
        let live = dir.join("live");
        let create = dir.join(format!("create-{session}.sh"));
        let created = json!({
            "session_id": session,
            "initial_windows": [{ "block_ids": [format!("block:{session}")] }],
        });
        let entry = json!({ "session_id": session, "label": "wm" });
        std::fs::write(
            &create,
            format!("echo '{entry}' >> '{}'\necho '{created}'\n", live.display()),
        )
        .unwrap();
        let list = dir.join(format!("list-{session}.sh"));
        std::fs::write(
            &list,
            format!(
                "list=$(paste -sd, '{}')\nsleep {delay}\nprintf '{{\"sessions\":[%s]}}\\n' \"$list\"\n",
                live.display()
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
        let a = adding_fake(shared.path(), "session:a", "0.6");
        let b = adding_fake(shared.path(), "session:b", "0");
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
        };
        std::thread::scope(|scope| {
            scope.spawn(|| add(&a, "a"));
            scope.spawn(|| {
                std::thread::sleep(std::time::Duration::from_millis(150));
                add(&b, "b");
            });
        });
        let names = a.rex.records().names;
        assert_eq!(names.get("session:a").map(String::as_str), Some("wm-a"));
        assert_eq!(names.get("session:b").map(String::as_str), Some("wm-b"));
    }

    #[test]
    fn app_moves_only_when_it_may_show_the_closing_session() {
        let closing = SessionId("session:closing".into());
        let other = SessionId("session:other".into());
        assert!(!moves_app(None, &closing));
        assert!(moves_app(Some(&closing), &closing));
        assert!(!moves_app(Some(&other), &closing));
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
