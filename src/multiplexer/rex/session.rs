//! Session mode under Rex: which Rex.app client to switch, where to send it
//! before a session closes, and which Rex session belongs to a workmux name.
//!
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
use std::os::fd::AsRawFd;
use std::path::PathBuf;

use super::super::Multiplexer;
use super::super::types::CreateSessionParams;
use super::super::util;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{ClientInfo, RexBackend, SessionId, SessionSummary, SessionView, WindowId};

/// A client connected to the Rex server, such as Rex.app or the rex CLI.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub(super) struct ClientId(String);

impl ClientId {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

const SELECT_TIMEOUT_MS: u64 = 2000;

static FIX_PRINTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Deserialize)]
struct ClientList {
    #[serde(default)]
    clients: Vec<Client>,
}

#[derive(Debug, Deserialize)]
struct Client {
    #[serde(default)]
    client_id: Option<ClientId>,
    #[serde(default)]
    info: Option<ClientInfo>,
    #[serde(default)]
    connection_state: String,
}

/// Connected app clients, most recently connected first. `client.list`
/// returns clients in the order they connected.
fn app_clients(list: ClientList) -> Vec<ClientId> {
    list.clients
        .into_iter()
        .rev()
        .filter(|client| client.connection_state != "lost")
        .filter(|client| client.info.as_ref().is_some_and(|info| info.kind == "app"))
        .filter_map(|client| client.client_id)
        .collect()
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ServerRecords {
    /// Managed session ID to its workmux name, such as `wm-feat-a`.
    #[serde(default)]
    names: HashMap<String, String>,
    /// Managed session ID to the session the app showed when it was opened.
    #[serde(default)]
    origins: HashMap<String, String>,
}

impl ServerRecords {
    fn forget(&mut self, session: &SessionId) {
        self.names.remove(session.as_str());
        self.origins.remove(session.as_str());
    }

    fn retain_live(&mut self, live: &[SessionId]) {
        let live = |id: &String| live.iter().any(|session| session.as_str() == id);
        self.names.retain(|id, _| live(id));
        self.origins.retain(|id, origin| live(id) && live(origin));
    }
}

/// Registry records per Rex server, keyed by its address.
type Registry = HashMap<String, ServerRecords>;

fn registry_path() -> Result<PathBuf> {
    Ok(crate::xdg::state_dir()?.join("rex-sessions.json"))
}

fn read_registry() -> Registry {
    registry_path()
        .ok()
        .and_then(|path| fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Read, change and write the registry under an exclusive lock, so
/// concurrent `add` and `rm` runs do not drop each other's records.
fn update_registry(change: impl FnOnce(&mut Registry)) -> Result<()> {
    let path = registry_path()?;
    let dir = path.parent().context("Rex registry path has no parent")?;
    fs::create_dir_all(dir)?;
    let lock = File::create(path.with_extension("lock"))?;
    // SAFETY: the descriptor stays open for the whole call, and the lock is
    // released when `lock` is dropped.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error()).context("Failed to lock the Rex registry");
    }
    let mut registry = read_registry();
    change(&mut registry);
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(serde_json::to_string(&registry)?.as_bytes())?;
    file.persist(&path)?;
    Ok(())
}

/// Whether to move the app before closing `closing`. Rex does not report
/// which session an app shows. Run from inside the closing session, the app
/// most likely shows it. Run from anywhere else, moving the app could pull
/// the user away from what they are looking at, which tmux never does.
fn moves_app(caller: Option<&SessionId>, closing: &SessionId) -> bool {
    caller == Some(closing)
}

/// Rex.app shows the first session in its sidebar when the session it shows
/// is destroyed, so the select runs to completion before the destroy starts.
fn select_then_destroy(select: Option<String>, destroy: String) -> String {
    match select {
        Some(select) => format!("{select}; {destroy}"),
        None => destroy,
    }
}

fn select_args(app: &ClientId, session: &SessionId, window: Option<&WindowId>) -> Value {
    let mut target = json!({ "session_id": session.as_str() });
    if let Some(window) = window {
        target["window_id"] = json!(window.as_str());
    }
    json!({
        "client_id": app.as_str(),
        "action": "session.select",
        "args": target,
        "origin": "api",
        "timeout_ms": SELECT_TIMEOUT_MS,
    })
}

/// Whether `name` is the session's Rex ID or its workmux name. Deferred
/// cleanup waits on the ID, and the label may have been renamed since.
fn names_session(session: &SessionSummary, name: &str) -> bool {
    session.session_id.as_str() == name || session.label == name
}

impl RexBackend {
    pub(super) fn session_named(&self, name: &str) -> Result<bool> {
        Ok(self
            .sessions()?
            .iter()
            .any(|session| names_session(session, name)))
    }

    fn server_key(&self) -> String {
        self.resolve_instance_id()
            .unwrap_or_else(|_| "default".to_string())
    }

    fn managed_names(&self) -> HashMap<String, String> {
        read_registry()
            .remove(&self.server_key())
            .map(|records| records.names)
            .unwrap_or_default()
    }

    fn update_records(&self, change: impl FnOnce(&mut ServerRecords)) -> Result<()> {
        let key = self.server_key();
        update_registry(|registry| change(registry.entry(key).or_default()))
    }

    /// Report managed sessions under their workmux names, whatever their
    /// current label, and list them before unmanaged sessions that happen to
    /// carry the same label.
    pub(super) fn name_sessions(&self, sessions: &mut [SessionSummary]) {
        let names = self.managed_names();
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
        if let Some(name) = self.managed_names().remove(view.session_id.as_str()) {
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
        let origin = Self::env_session();
        let live: Vec<SessionId> = self
            .raw_sessions()?
            .into_iter()
            .map(|s| s.session_id)
            .collect();
        self.update_records(|records| {
            records.retain_live(&live);
            records
                .names
                .insert(created.session_id.as_str().to_string(), full_name);
            if let Some(origin) = origin.filter(|origin| origin != &created.session_id) {
                records.origins.insert(
                    created.session_id.as_str().to_string(),
                    origin.as_str().to_string(),
                );
            }
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
        })
    }

    /// Connected Rex.app clients attached to `session` (every app when
    /// `session` is `None`), most recently connected first. Only these may be
    /// asked to perform actions: a request aimed at a CLI client waits out the
    /// whole timeout. Other Rex modules switch the app through `select_on_app`.
    pub(super) fn app_clients_for(&self, session: Option<&SessionId>) -> Result<Vec<ClientId>> {
        let args = match session {
            Some(session) => json!({ "session_id": session.as_str() }),
            None => json!({}),
        };
        Ok(app_clients(self.api("client.list", args)?))
    }

    fn perform_select(
        &self,
        app: &ClientId,
        session: &SessionId,
        window: Option<&WindowId>,
    ) -> Result<()> {
        self.api::<Value>("client.perform", select_args(app, session, window))?;
        Ok(())
    }

    /// Ask the app attached to the caller's session to show `session`, on
    /// `window` when given, trying the most recently connected app first. Rex.app
    /// follows only this request: `session.focus_window` moves the session's
    /// active window but not the tab the app shows. With no app connected there is
    /// nothing to switch. When every app declines, print the fix once and go on.
    pub(super) fn select_on_app(
        &self,
        session: &SessionId,
        window: Option<&WindowId>,
    ) -> Result<()> {
        let apps = self.app_clients_for(Self::env_session().as_ref())?;
        let mut declined = None;
        for app in &apps {
            match self.perform_select(app, session, window) {
                Ok(()) => return Ok(()),
                Err(error) => declined = Some(error),
            }
        }
        if let Some(error) = declined
            && !FIX_PRINTED.swap(true, Ordering::Relaxed)
        {
            eprintln!(
                "Rex.app did not switch sessions ({error:#}). To let workmux switch sessions, \
                 turn on Remote Control in Rex.app Settings under Rex Server."
            );
        }
        Ok(())
    }

    /// A shell command that asks the caller's app to show `session`, or `true`
    /// when no app is connected.
    pub(super) fn shell_select_on_app_cmd(&self, session: &SessionId) -> Result<String> {
        Ok(
            match self.app_clients_for(Self::env_session().as_ref())?.first() {
                Some(app) => self.shell_api_cmd("client.perform", select_args(app, session, None)),
                None => "true".to_string(),
            },
        )
    }

    /// The session the app goes to before `closing` closes: the preferred
    /// session by name, else the session `closing` was opened from.
    fn destination(
        &self,
        closing: &SessionId,
        preferred: Option<&str>,
    ) -> Result<Option<SessionId>> {
        let sessions = self.sessions()?;
        let other = |id: &SessionId| id != closing;
        if let Some(name) = preferred
            && let Some(session) = sessions
                .iter()
                .find(|session| session.label == name && other(&session.session_id))
        {
            return Ok(Some(session.session_id.clone()));
        }
        let origin = read_registry()
            .remove(&self.server_key())
            .and_then(|mut records| records.origins.remove(closing.as_str()))
            .map(SessionId);
        Ok(origin.filter(|origin| {
            other(origin) && sessions.iter().any(|session| &session.session_id == origin)
        }))
    }

    /// The app and session to select before `closing` closes, if it should move.
    fn relocation(
        &self,
        closing: &SessionId,
        preferred: Option<&str>,
    ) -> Result<Option<(ClientId, SessionId)>> {
        if !moves_app(Self::env_session().as_ref(), closing) {
            return Ok(None);
        }
        let Some(destination) = self.destination(closing, preferred)? else {
            return Ok(None);
        };
        Ok(self
            .app_clients_for(Some(closing))?
            .into_iter()
            .next()
            .map(|app| (app, destination)))
    }

    /// Close every session named `full_name`, first sending the app to the
    /// destination. Rex.app shows the first session in its sidebar when the
    /// session it shows is destroyed, so the order matters.
    pub(super) fn close_sessions_to(&self, full_name: &str, preferred: Option<&str>) -> Result<()> {
        for session in self.sessions_labeled(full_name)? {
            if let Some((app, destination)) = self.relocation(&session, preferred)?
                && let Err(error) = self.perform_select(&app, &destination, None)
            {
                tracing::debug!(error = %error, "rex:relocation select failed");
            }
            self.destroy_session(&session)?;
            self.forget_session(&session)?;
        }
        Ok(())
    }

    fn forget_session(&self, session: &SessionId) -> Result<()> {
        self.update_records(|records| records.forget(session))
    }

    /// The deferred form of `close_sessions_to` for one session.
    pub(super) fn shell_close_session_cmd(
        &self,
        session: &SessionId,
        preferred: Option<&str>,
    ) -> Result<String> {
        let select = self
            .relocation(session, preferred)?
            .map(|(app, destination)| {
                self.shell_api_cmd("client.perform", select_args(&app, &destination, None))
            });
        // The script outlives this process, so forget the session now.
        self.forget_session(session)?;
        Ok(select_then_destroy(select, self.shell_destroy_cmd(session)))
    }

    pub(super) fn shell_close_sessions_to_cmd(
        &self,
        full_name: &str,
        preferred: Option<&str>,
    ) -> Result<String> {
        Ok(self
            .sessions_labeled(full_name)?
            .iter()
            .map(|session| self.shell_close_session_cmd(session, preferred))
            .collect::<Result<Vec<_>>>()?
            .join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_a_window_names_it_so_the_app_shows_that_tab() {
        let app = ClientId("client:app".into());
        let session = SessionId("session:1".into());
        let window = WindowId("window:2".into());
        assert_eq!(
            select_args(&app, &session, Some(&window))["args"],
            json!({"session_id": "session:1", "window_id": "window:2"})
        );
        assert_eq!(
            select_args(&app, &session, None)["args"],
            json!({"session_id": "session:1"})
        );
    }

    #[test]
    fn a_session_is_found_by_id_after_its_label_changed() {
        let session: SessionSummary =
            serde_json::from_value(json!({"session_id": "session:1", "label": "write-date-file"}))
                .unwrap();
        assert!(names_session(&session, "session:1"));
        assert!(names_session(&session, "write-date-file"));
        assert!(!names_session(&session, "session:2"));
    }

    fn apps(fixture: &str) -> Vec<String> {
        app_clients(serde_json::from_str(fixture).unwrap())
            .into_iter()
            .map(|id| id.0)
            .collect()
    }

    #[test]
    fn zero_app_clients_select_nothing() {
        assert!(apps(include_str!("../testdata/rex_client_ls_zero_app.json")).is_empty());
    }

    #[test]
    fn one_app_client_is_chosen_over_cli_and_unnamed_clients() {
        assert_eq!(
            apps(include_str!("../testdata/rex_client_ls_one_app.json")),
            ["client:01a19e67-c952-7923-a102-902347c1c3c1"]
        );
    }

    #[test]
    fn cli_clients_attached_to_the_session_are_never_chosen() {
        assert_eq!(
            apps(include_str!("../testdata/rex_client_ls_app_plus_cli.json")),
            ["client:01a19e67-c952-7923-a102-902347c1c3c1"]
        );
    }

    #[test]
    fn two_app_clients_put_the_most_recently_connected_first() {
        assert_eq!(
            apps(include_str!("../testdata/rex_client_ls_two_apps.json")),
            [
                "client:01a19e9a-1111-7000-8000-000000000002",
                "client:01a19e67-c952-7923-a102-902347c1c3c1",
            ]
        );
    }

    #[test]
    fn lost_app_clients_are_skipped() {
        let list = r#"{"clients":[
            {"client_id":"client:a","info":{"kind":"app"},"connection_state":"connected"},
            {"client_id":"client:b","info":{"kind":"app"},"connection_state":"lost"}]}"#;
        assert_eq!(apps(list), ["client:a"]);
    }

    #[test]
    fn deferred_close_selects_before_it_destroys() {
        let script = select_then_destroy(Some("select".into()), "destroy".into());
        assert!(script.find("select").unwrap() < script.find("destroy").unwrap());
        assert_eq!(select_then_destroy(None, "destroy".into()), "destroy");
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
