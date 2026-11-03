use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use super::{BlockId, ClientEntry, ClientList, RexBackend, SessionView, WindowId};

const REMOTE_CONTROL_HINT: &str = "Rex.app did not switch tabs, so jumps stay in this tab. \
     Turn on Remote Control in Rex Settings to jump to any agent";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LayerKind {
    Tiled,
    Floating,
    #[default]
    #[serde(other)]
    Other,
}

/// Whether the user has been told that Rex.app refuses to switch tabs.
#[derive(Debug, Default)]
pub(super) enum RemoteControlHint {
    #[default]
    Unneeded,
    Pending,
    Shown,
}

impl ClientEntry {
    fn is_live_app(&self) -> bool {
        self.info.as_ref().is_some_and(|info| info.kind == "app") && self.connection_state != "lost"
    }
}

/// The app that opened the dashboard (`REX_CLIENT`) while it is still
/// connected, else the app that connected last. Only apps qualify: `-C`
/// aimed at any other client waits forever.
fn pick_app(clients: Vec<ClientEntry>, preferred: Option<&str>) -> Option<String> {
    let mut apps: Vec<String> = clients
        .into_iter()
        .filter(ClientEntry::is_live_app)
        .map(|client| client.client_id)
        .collect();
    if let Some(index) = preferred.and_then(|id| apps.iter().position(|app| app == id)) {
        return Some(apps.swap_remove(index));
    }
    apps.pop()
}

/// The block that was focused when the dashboard action opened its layer.
pub(super) fn origin_block() -> Option<BlockId> {
    std::env::var("WORKREX_REX_ORIGIN_BLOCK")
        .ok()
        .and_then(|id| BlockId::parse(&id).ok())
}

/// The pane the user is working in. While the dashboard has focus in its
/// floating layer, that is the block it was opened over, as a tmux popup
/// reports the pane beneath it.
pub(super) fn active_block(
    view: &SessionView,
    me: Option<&BlockId>,
    origin: Option<BlockId>,
) -> Option<BlockId> {
    let window = view.active_window()?;
    let focused = window.focused_block_id.as_ref()?;
    if me != Some(focused) {
        return Some(focused.clone());
    }
    if origin.is_some() {
        return origin;
    }
    let floating = window.layers.iter().any(|layer| {
        layer.kind == LayerKind::Floating && layer.blocks.iter().any(|b| &b.block_id == focused)
    });
    (!floating).then(|| focused.clone())
}

impl RexBackend {
    // replace with rex::session app-client selector once WR-3 lands
    pub(super) fn app_client(&self) -> Result<Option<String>> {
        let list: ClientList = self.api("client.list", json!({}))?;
        Ok(pick_app(
            list.clients,
            std::env::var("REX_CLIENT").ok().as_deref(),
        ))
    }

    /// Shows the block's tab on the app, then focuses the block. `here` is
    /// the block the user is in, whose tab the app shows. When the app
    /// refuses to switch tabs (Remote Control off), a block in that same tab
    /// is still focused, and the user is told once how to jump anywhere.
    pub(super) fn jump_to_block(&self, block: &BlockId, here: Option<&BlockId>) -> Result<()> {
        let target = self
            .resolve("block", block.as_str())?
            .ok_or_else(|| anyhow!("Rex block {} not found", block.as_str()))?;
        self.block_sessions
            .lock()
            .unwrap()
            .insert(block.clone(), target.session_id.clone());
        if let Err(refused) = self.show_on_app(&target.session_id, target.window_id.as_ref()) {
            if target.window_id.is_none() || target.window_id != self.window_of(here)? {
                return Err(refused);
            }
            let mut hint = self.remote_control_hint.lock().unwrap();
            if matches!(*hint, RemoteControlHint::Unneeded) {
                *hint = RemoteControlHint::Pending;
            }
        }
        self.block_session_call(
            "session.focus_block",
            block,
            json!({ "block_id": block.as_str() }),
        )?;
        Ok(())
    }

    fn window_of(&self, block: Option<&BlockId>) -> Result<Option<WindowId>> {
        let Some(block) = block else {
            return Ok(None);
        };
        Ok(self
            .resolve("block", block.as_str())?
            .and_then(|target| target.window_id))
    }

    pub(super) fn take_remote_control_hint(&self) -> Option<String> {
        let mut hint = self.remote_control_hint.lock().unwrap();
        if !matches!(*hint, RemoteControlHint::Pending) {
            return None;
        }
        *hint = RemoteControlHint::Shown;
        Some(REMOTE_CONTROL_HINT.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const LAYER_VIEW: &str = include_str!("../testdata/rex_session_view_dashboard_layer.json");
    const APP_CLIENTS: &str = include_str!("../testdata/rex_client_list.json");
    const NO_APP_CLIENTS: &str = include_str!("../testdata/rex_client_list_no_app.json");
    const TWO_APP_CLIENTS: &str = include_str!("../testdata/rex_client_list_two_apps.json");
    const SERVER_VERSION: &str = include_str!("../testdata/rex_server_version_mac.json");
    const DASHBOARD: &str = "block:01a19e98-1751-7d75-a63c-2b457e2bb6ee";
    const SHELL: &str = "block:01a19e95-dd18-7392-8f63-ecbea2aaae97";
    const AGENT: &str = "block:agent";
    const AGENT_SESSION: &str = "session:agent";
    const AGENT_WINDOW: &str = "window:agent";
    const APP: &str = "client:01a19e67-c952-7923-a102-902347c1c3c1";
    const LATER_APP: &str = "client:01a1a0c2-0000-7000-8000-000000000002";
    const HERE_IN_AGENT_TAB: &str = "block:here-in-agent-tab";
    const HERE_ELSEWHERE: &str = "block:here-elsewhere";

    fn block(id: &str) -> BlockId {
        BlockId(id.to_string())
    }

    fn layer_view() -> SessionView {
        serde_json::from_str(LAYER_VIEW).unwrap()
    }

    /// A `rex` that logs each call's arguments and input, one line per call,
    /// and answers the calls a jump makes. `HERE_IN_AGENT_TAB` resolves to
    /// the agent's window, any other block to another window.
    struct FakeRex {
        dir: tempfile::TempDir,
    }

    impl FakeRex {
        fn new(clients: &str) -> Self {
            Self::with_select(clients, "echo '{}'")
        }

        /// The app refuses `session.select`, as it does with Remote Control off.
        fn refusing_select(clients: &str) -> Self {
            Self::with_select(
                clients,
                "echo 'Error: client refused session.select: not_supported' >&2; exit 1",
            )
        }

        fn with_select(clients: &str, select: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("clients.json"), clients).unwrap();
            fs::write(dir.path().join("version.json"), SERVER_VERSION).unwrap();
            let script = format!(
                r#"#!/bin/sh
d='{dir}'
input=''
[ "$(eval echo \${{$#}})" = - ] && input=$(cat)
echo "$* $input" >> "$d/log"
case "$* $input" in
  *target.resolve*{AGENT}\"*) echo '{{"results":[{{"status":"resolved","target":{{"session_id":"{AGENT_SESSION}","window_id":"{AGENT_WINDOW}"}}}}]}}' ;;
  *target.resolve*{HERE_IN_AGENT_TAB}*) echo '{{"results":[{{"status":"resolved","target":{{"session_id":"{AGENT_SESSION}","window_id":"{AGENT_WINDOW}"}}}}]}}' ;;
  *target.resolve*) echo '{{"results":[{{"status":"resolved","target":{{"session_id":"session:other","window_id":"window:other"}}}}]}}' ;;
  *client.list*) cat "$d/clients.json" ;;
  *server.version*) cat "$d/version.json" ;;
  *session.select*) {select} ;;
  *session.focus_block*) echo '{{"revision":1}}' ;;
  *) echo '{{}}' ;;
esac
"#,
                dir = dir.path().display(),
            );
            let program = dir.path().join("rex");
            fs::write(&program, script).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir }
        }

        fn backend(&self) -> RexBackend {
            RexBackend {
                program: self.dir.path().join("rex").display().to_string(),
                ..RexBackend::for_server(&format!("unix://{}/s.sock", self.dir.path().display()))
            }
        }

        fn calls(&self) -> Vec<String> {
            let log = fs::read_to_string(self.dir.path().join("log")).unwrap_or_default();
            log.lines()
                .filter(|line| {
                    !["target.resolve", "client.list", "server.version"]
                        .iter()
                        .any(|method| line.contains(method))
                })
                .map(str::to_string)
                .collect()
        }
    }

    fn assert_focuses_agent(call: &str) {
        assert!(call.contains("api call session.focus_block -"), "{call}");
        assert!(
            call.contains(&format!(r#""session_id":"{AGENT_SESSION}""#)),
            "{call}"
        );
        assert!(call.contains(&format!(r#""block_id":"{AGENT}""#)), "{call}");
    }

    fn select_call(app: &str) -> String {
        format!("-C {app} do session.select session_id={AGENT_SESSION} window_id={AGENT_WINDOW} ")
    }

    #[test]
    fn jump_shows_the_agents_session_and_window_on_the_app_before_focusing_it() {
        let rex = FakeRex::new(APP_CLIENTS);
        rex.backend().jump_to_block(&block(AGENT), None).unwrap();

        let calls = rex.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].ends_with(&select_call(APP)), "{calls:?}");
        assert_focuses_agent(&calls[1]);
    }

    #[test]
    fn jump_without_an_app_only_focuses() {
        let rex = FakeRex::new(NO_APP_CLIENTS);
        rex.backend().jump_to_block(&block(AGENT), None).unwrap();

        let calls = rex.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_focuses_agent(&calls[0]);
    }

    #[test]
    fn jump_with_two_apps_targets_the_live_app_that_connected_last() {
        let rex = FakeRex::new(TWO_APP_CLIENTS);
        rex.backend().jump_to_block(&block(AGENT), None).unwrap();

        let calls = rex.calls();
        assert!(calls[0].ends_with(&select_call(LATER_APP)), "{calls:?}");
    }

    #[test]
    fn refused_select_still_focuses_a_block_in_the_users_tab_and_hints_once() {
        let rex = FakeRex::refusing_select(APP_CLIENTS);
        let backend = rex.backend();
        let here = block(HERE_IN_AGENT_TAB);

        backend.jump_to_block(&block(AGENT), Some(&here)).unwrap();
        let calls = rex.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_focuses_agent(&calls[1]);
        let hint = backend.take_remote_control_hint().expect("a hint");
        assert!(hint.contains("Turn on Remote Control"), "{hint}");

        backend.jump_to_block(&block(AGENT), Some(&here)).unwrap();
        assert_eq!(backend.take_remote_control_hint(), None);
    }

    #[test]
    fn refused_select_for_another_tab_fails_with_the_hint_and_keeps_focus() {
        let rex = FakeRex::refusing_select(APP_CLIENTS);
        let backend = rex.backend();

        let err = backend
            .jump_to_block(&block(AGENT), Some(&block(HERE_ELSEWHERE)))
            .unwrap_err();
        assert!(err.to_string().contains("Turn on Remote Control"), "{err}");
        let calls = rex.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].contains("session.select"), "{calls:?}");
    }

    #[test]
    fn app_choice_prefers_the_app_that_opened_the_dashboard() {
        let clients = || {
            serde_json::from_str::<ClientList>(TWO_APP_CLIENTS)
                .unwrap()
                .clients
        };
        assert_eq!(pick_app(clients(), Some(APP)), Some(APP.to_string()));
        assert_eq!(pick_app(clients(), None), Some(LATER_APP.to_string()));
        let lost = "client:01a1a0c2-0000-7000-8000-000000000003";
        let cli = "client:01a19e99-4682-783d-8b8d-c76ae0c3fa13";
        assert_eq!(pick_app(clients(), Some(lost)), Some(LATER_APP.to_string()));
        assert_eq!(pick_app(clients(), Some(cli)), Some(LATER_APP.to_string()));
    }

    #[test]
    fn active_pane_in_the_dashboard_layer_is_the_origin() {
        let view = layer_view();
        let me = block(DASHBOARD);
        assert_eq!(
            active_block(&view, Some(&me), Some(block(SHELL))),
            Some(block(SHELL))
        );
    }

    #[test]
    fn dashboard_layer_without_an_origin_reports_no_active_pane() {
        assert_eq!(
            active_block(&layer_view(), Some(&block(DASHBOARD)), None),
            None
        );
    }

    #[test]
    fn another_program_sees_the_focused_block() {
        let view = layer_view();
        assert_eq!(
            active_block(&view, Some(&block(SHELL)), Some(block(SHELL))),
            Some(block(DASHBOARD))
        );
        assert_eq!(active_block(&view, None, None), Some(block(DASHBOARD)));
    }
}
