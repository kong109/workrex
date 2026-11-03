use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use super::{AppSwitch, BlockId, Multiplexer, RexBackend, SessionView, WindowId};

const REMOTE_CONTROL_HINT: &str = "Turn on Remote Control in Rex Settings to jump across tabs";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LayerKind {
    Tiled,
    Floating,
    #[default]
    #[serde(other)]
    Other,
}

/// The block that was focused when the dashboard action opened its layer.
pub(super) fn origin_block() -> Option<BlockId> {
    std::env::var("WORKREX_REX_ORIGIN_BLOCK")
        .ok()
        .and_then(|id| BlockId::parse(&id).ok())
}

/// The pane the user is working in: the caller's pane while it is alive,
/// else the block the server has focused. The dashboard in its floating
/// layer reports the block it was opened over, as a tmux popup reports the
/// pane beneath it, and no pane when it does not know that block.
pub(super) fn active_block(
    view: &SessionView,
    me: Option<&BlockId>,
    origin: Option<BlockId>,
) -> Option<BlockId> {
    let Some((me, window)) = me.and_then(|me| Some((me, view.window_of(me)?))) else {
        return view.active_window()?.focused_block_id.clone();
    };
    let floating = window.layers.iter().any(|layer| {
        layer.kind == LayerKind::Floating && layer.blocks.iter().any(|b| &b.block_id == me)
    });
    if floating { origin } else { Some(me.clone()) }
}

impl RexBackend {
    /// The dashboard layer's jump: the app shows the block's tab, then the
    /// block is focused and the layer closes. When the app refuses (Remote
    /// Control off), a block in the tab the layer is open over is still
    /// focused; any other jump fails with a hint short enough for the
    /// footer and leaves focus alone. Nothing is printed, since the layer's
    /// display would swallow it. An app that does not answer may still
    /// switch late, so the block is focused as `switch_to_pane` does.
    pub(super) fn jump_to_block(&self, block: &BlockId) -> Result<()> {
        let target = self
            .resolve("block", block.as_str())?
            .ok_or_else(|| anyhow!("Rex block {} not found", block.as_str()))?;
        self.block_sessions
            .lock()
            .unwrap()
            .insert(block.clone(), target.session_id.clone());
        if self.show_on_app(&target.session_id, target.window_id.as_ref()) == AppSwitch::Refused {
            let here = self.caller.origin.as_ref();
            if target.window_id.is_none() || target.window_id != self.window_of(here)? {
                return Err(anyhow!(REMOTE_CONTROL_HINT));
            }
        }
        self.select_pane(block.as_str())
    }

    fn window_of(&self, block: Option<&BlockId>) -> Result<Option<WindowId>> {
        let Some(block) = block else {
            return Ok(None);
        };
        Ok(self
            .resolve("block", block.as_str())?
            .and_then(|target| target.window_id))
    }

    /// Focuses the block the dashboard was opened over. A layer whose
    /// program exits hands focus to the last block of the tab instead.
    pub(super) fn return_to_origin(&self) {
        if let Some(origin) = &self.caller.origin {
            let _ = self.block_session_call(
                "session.focus_block",
                origin,
                json!({ "block_id": origin.as_str() }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Caller, SessionId};
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const LAYER_VIEW: &str = include_str!("../testdata/rex_session_view_dashboard_layer.json");
    const APP_CLIENTS: &str = include_str!("../testdata/rex_client_list.json");
    const NO_APP_CLIENTS: &str = include_str!("../testdata/rex_client_list_no_app.json");
    const SERVER_VERSION: &str = include_str!("../testdata/rex_server_version_mac.json");
    const DASHBOARD: &str = "block:01a19e98-1751-7d75-a63c-2b457e2bb6ee";
    const SHELL: &str = "block:01a19e95-dd18-7392-8f63-ecbea2aaae97";
    const AGENT: &str = "block:agent";
    const AGENT_SESSION: &str = "session:agent";
    const AGENT_WINDOW: &str = "window:agent";
    const APP: &str = "client:01a19e67-c952-7923-a102-902347c1c3c1";
    /// A session `APP` is attached to in `APP_CLIENTS`.
    const HERE_SESSION: &str = "session:01a19a0d-248b-7036-a995-09504a4b77e2";
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
    /// the agent's window, any other block to another window. `APP` owns
    /// every block, as Rex.app owns the dashboard layer it opened.
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
                "echo 'Error: Remote Control is off for this client (code=refused)' >&2; exit 1",
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
  *"block call"*size*) echo '{{"columns":80,"rows":24,"owner":"{APP}"}}' ;;
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

        fn backend(&self, caller: Caller) -> RexBackend {
            RexBackend {
                program: self.dir.path().join("rex").display().to_string(),
                caller,
                ..RexBackend::for_server(&format!("unix://{}/s.sock", self.dir.path().display()))
            }
        }

        fn calls(&self) -> Vec<String> {
            let log = fs::read_to_string(self.dir.path().join("log")).unwrap_or_default();
            log.lines()
                .filter(|line| {
                    ["target.resolve", "client.list", "server.version", " size "]
                        .iter()
                        .all(|method| !line.contains(method))
                })
                .map(str::to_string)
                .collect()
        }
    }

    /// The dashboard in its layer, opened over `origin`.
    fn in_dashboard(origin: &str) -> Caller {
        Caller {
            session: Some(SessionId(HERE_SESSION.into())),
            block: Some(block(DASHBOARD)),
            origin: Some(block(origin)),
        }
    }

    /// A program in the `here` pane itself, with no dashboard layer.
    fn in_pane(here: &str) -> Caller {
        Caller {
            session: Some(SessionId(HERE_SESSION.into())),
            block: Some(block(here)),
            origin: None,
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

    fn select_call() -> String {
        format!("-C {APP} do session.select session_id={AGENT_SESSION} window_id={AGENT_WINDOW} ")
    }

    #[test]
    fn jump_shows_the_agents_tab_on_the_app_then_focuses_it() {
        let rex = FakeRex::new(APP_CLIENTS);
        rex.backend(in_dashboard(HERE_ELSEWHERE))
            .jump_to_block(&block(AGENT))
            .unwrap();

        let calls = rex.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].ends_with(&select_call()), "{calls:?}");
        assert_focuses_agent(&calls[1]);
    }

    #[test]
    fn jump_without_an_app_only_focuses() {
        let rex = FakeRex::new(NO_APP_CLIENTS);
        rex.backend(in_dashboard(HERE_ELSEWHERE))
            .jump_to_block(&block(AGENT))
            .unwrap();

        let calls = rex.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_focuses_agent(&calls[0]);
    }

    #[test]
    fn refused_select_still_focuses_a_block_in_the_dashboards_tab() {
        let rex = FakeRex::refusing_select(APP_CLIENTS);
        rex.backend(in_dashboard(HERE_IN_AGENT_TAB))
            .jump_to_block(&block(AGENT))
            .unwrap();

        let calls = rex.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].ends_with(&select_call()), "{calls:?}");
        assert_focuses_agent(&calls[1]);
    }

    #[test]
    fn refused_select_for_another_tab_fails_with_the_hint_and_keeps_focus() {
        let rex = FakeRex::refusing_select(APP_CLIENTS);
        let err = rex
            .backend(in_dashboard(HERE_ELSEWHERE))
            .jump_to_block(&block(AGENT))
            .unwrap_err();

        assert_eq!(err.to_string(), REMOTE_CONTROL_HINT);
        let calls = rex.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].ends_with(&select_call()), "{calls:?}");
    }

    #[test]
    fn refused_select_outside_the_dashboard_fails_with_the_fix() {
        let rex = FakeRex::refusing_select(APP_CLIENTS);
        let err = rex
            .backend(in_pane(HERE_ELSEWHERE))
            .switch_to_pane(AGENT, None)
            .unwrap_err();

        assert_eq!(err.to_string(), super::super::REMOTE_CONTROL_FIX);
        let calls = rex.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].ends_with(&select_call()), "{calls:?}");
    }

    #[test]
    fn the_dashboard_jumps_through_its_layer_rule() {
        let rex = FakeRex::refusing_select(APP_CLIENTS);
        let err = rex
            .backend(in_dashboard(HERE_ELSEWHERE))
            .switch_to_pane(AGENT, None)
            .unwrap_err();

        assert_eq!(err.to_string(), REMOTE_CONTROL_HINT);
    }

    #[test]
    fn an_app_that_never_answers_still_lands_the_jump_from_the_dashboard() {
        let rex = FakeRex::with_select(APP_CLIENTS, "exec sleep 30");
        let start = std::time::Instant::now();
        rex.backend(in_dashboard(HERE_ELSEWHERE))
            .jump_to_block(&block(AGENT))
            .unwrap();

        assert!(start.elapsed() < std::time::Duration::from_secs(8));
        let calls = rex.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[0].ends_with(&select_call()), "{calls:?}");
        assert_focuses_agent(&calls[1]);
    }

    /// Runs refused dashboard jumps in a child test process, whose stderr
    /// is its own and whose print-once flag is fresh.
    #[test]
    fn refused_jumps_from_the_dashboard_print_nothing() {
        if std::env::var_os("WORKREX_LAYER_TEST_CHILD").is_some() {
            let rex = FakeRex::refusing_select(APP_CLIENTS);
            rex.backend(in_dashboard(HERE_ELSEWHERE))
                .jump_to_block(&block(AGENT))
                .unwrap_err();
            let rex = FakeRex::refusing_select(APP_CLIENTS);
            rex.backend(in_dashboard(HERE_IN_AGENT_TAB))
                .jump_to_block(&block(AGENT))
                .unwrap();
            return;
        }
        let (_, path) = module_path!().split_once("::").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{path}::refused_jumps_from_the_dashboard_print_nothing"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env("WORKREX_LAYER_TEST_CHILD", "1")
            .output()
            .unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
        assert_eq!(String::from_utf8_lossy(&output.stderr), "");
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
    fn a_program_in_a_pane_sees_its_own_pane() {
        let view = layer_view();
        assert_eq!(
            active_block(&view, Some(&block(SHELL)), None),
            Some(block(SHELL))
        );
        assert_eq!(active_block(&view, None, None), Some(block(DASHBOARD)));
    }
}
