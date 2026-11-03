use super::{BlockId, SessionView};

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
        layer.kind == "floating" && layer.blocks.iter().any(|b| &b.block_id == focused)
    });
    (!floating).then(|| focused.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiplexer::Multiplexer;
    use crate::multiplexer::rex::RexBackend;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    const LAYER_VIEW: &str = include_str!("../testdata/rex_session_view_dashboard_layer.json");
    const APP_CLIENTS: &str = include_str!("../testdata/rex_client_list.json");
    const NO_APP_CLIENTS: &str = include_str!("../testdata/rex_client_list_no_app.json");
    const DASHBOARD: &str = "block:01a19e98-1751-7d75-a63c-2b457e2bb6ee";
    const SHELL: &str = "block:01a19e95-dd18-7392-8f63-ecbea2aaae97";
    const AGENT: &str = "block:agent";
    const AGENT_SESSION: &str = "session:agent";
    const AGENT_WINDOW: &str = "window:agent";

    fn block(id: &str) -> BlockId {
        BlockId(id.to_string())
    }

    fn layer_view() -> SessionView {
        serde_json::from_str(LAYER_VIEW).unwrap()
    }

    /// A `rex` that logs each call's arguments and input, one line per call,
    /// and answers the calls a jump makes.
    struct FakeRex {
        dir: tempfile::TempDir,
    }

    impl FakeRex {
        fn new(clients: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("clients.json"), clients).unwrap();
            let script = format!(
                r#"#!/bin/sh
d='{dir}'
input=''
[ "$(eval echo \${{$#}})" = - ] && input=$(cat)
echo "$* $input" >> "$d/log"
case "$*" in
  *target.resolve*) echo '{{"results":[{{"status":"resolved","target":{{"session_id":"{AGENT_SESSION}","window_id":"{AGENT_WINDOW}"}}}}]}}' ;;
  *client.list*) cat "$d/clients.json" ;;
  *server.version*) echo '{{"rex_version":"t","api_revision":8}}' ;;
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
            RexBackend::with_program(self.dir.path().join("rex"))
        }

        fn calls(&self) -> Vec<String> {
            let log = fs::read_to_string(self.dir.path().join("log")).unwrap_or_default();
            log.lines()
                .filter(|line| {
                    !line.contains("target.resolve")
                        && !line.contains("client.list")
                        && !line.contains("server.version")
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

    #[test]
    fn jump_shows_the_agents_session_and_window_before_focusing_it() {
        let rex = FakeRex::new(APP_CLIENTS);
        rex.backend().select_pane(AGENT).unwrap();

        let calls = rex.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(
            calls[0].ends_with(&format!(
                "do session.select session_id={AGENT_SESSION} window_id={AGENT_WINDOW} "
            )),
            "{calls:?}"
        );
        assert_focuses_agent(&calls[1]);
    }

    #[test]
    fn jump_without_an_app_only_focuses() {
        let rex = FakeRex::new(NO_APP_CLIENTS);
        rex.backend().select_pane(AGENT).unwrap();

        let calls = rex.calls();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert_focuses_agent(&calls[0]);
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
