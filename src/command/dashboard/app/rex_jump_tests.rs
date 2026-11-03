//! Dashboard jumps against a fake `rex`, in a child process whose
//! environment points the backend, config and state at a temp dir.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use super::App;
use crate::config::MuxMode;
use crate::multiplexer::rex::{AppRefused, RexBackend};
use crate::test_support;
use crate::workflow::types::WorktreeInfo;

/// The caller's pane is `block:caller` in `session:b`, labeled `wm-feat`,
/// whose active window focuses `block:agent`; `client:app-1` owns the
/// caller's pane. `session.select` is refused while `refuse` exists.
const FAKE_REX: &str = r#"#!/bin/sh
dir=$(dirname "$0")
call="$* $(cat)"
echo "$call" >> "$dir/calls"
case "$call" in
  *server.version*) echo '{"rex_version":"t","api_revision":8}';;
  *block?call?-b?block:caller?size*) echo '{"columns":80,"rows":24,"owner":"client:app-1"}';;
  *client.list*) echo '{"clients":[{"client_id":"client:app-1","info":{"kind":"app"},"connection_state":"connected","session_ids":["session:b"]}]}';;
  *session.select*)
    if [ -e "$dir/refuse" ]; then
      echo 'Error: Remote Control is off for this client (code=refused)' >&2; exit 1
    fi
    echo '{}';;
  *target.resolve*) echo '{"results":[{"status":"resolved","target":{"session_id":"session:b","window_id":"window:w"}}]}';;
  *session.view*) echo '{"session_id":"session:b","label":"wm-feat","active_window_id":"window:w","windows":[{"window_id":"window:w","label":"wm-feat","focused_block_id":"block:agent","layers":[]}]}';;
  *session.list*) echo '{"sessions":[{"session_id":"session:b","label":"wm-feat"}]}';;
  *) echo '{}';;
esac
"#;

/// Runs `body` in a child process against the fake `rex`; `refuse` makes
/// the app refuse every jump.
fn with_rex_app(test_name: &str, refuse: bool, body: impl FnOnce(&mut App, &Path)) {
    if !test_support::is_isolated_child(test_name) {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let rex = bin.join("rex");
        std::fs::write(&rex, FAKE_REX).unwrap();
        std::fs::set_permissions(&rex, std::fs::Permissions::from_mode(0o755)).unwrap();
        if refuse {
            std::fs::write(bin.join("refuse"), "").unwrap();
        }
        let home = temp.path().join("home");
        let dirs: Vec<(&str, PathBuf)> = vec![
            ("HOME", home.clone()),
            ("XDG_CONFIG_HOME", home.join("config")),
            ("XDG_STATE_HOME", home.join("state")),
            ("XDG_CACHE_HOME", home.join("cache")),
            ("XDG_DATA_HOME", home.join("data")),
        ];
        for (_, dir) in &dirs {
            std::fs::create_dir_all(dir).unwrap();
        }
        let server = format!("unix://{}/s.sock", temp.path().display());
        let mut envs: Vec<(&str, &Path)> = dirs.iter().map(|(k, v)| (*k, v.as_path())).collect();
        envs.extend([
            ("REX_BIN_DIR", bin.as_path()),
            ("REX_SERVER", Path::new(&server)),
            ("REX_SESSION", Path::new("session:b")),
            ("REX_BLOCK", Path::new("block:caller")),
        ]);
        test_support::run_isolated_test(test_name, temp.path(), &envs);
        return;
    }
    let bin = PathBuf::from(std::env::var_os("REX_BIN_DIR").unwrap());
    let (tx, _rx) = mpsc::channel();
    let mut app = App::new(Arc::new(RexBackend::new()), false, tx).unwrap();
    body(&mut app, &bin.join("calls"));
    println!("{}", test_support::ISOLATED_TEST_CANARY);
}

fn selects(calls: &Path) -> Vec<String> {
    std::fs::read_to_string(calls)
        .unwrap_or_default()
        .lines()
        .filter(|call| call.contains(" do session.select "))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_refused_jump_shows_the_fix_in_the_status_line() {
    with_rex_app(
        "command::dashboard::app::rex_jump_tests::a_refused_jump_shows_the_fix_in_the_status_line",
        true,
        |app, calls| {
            app.switch_to_pane_and_track("block:agent");
            assert_eq!(selects(calls).len(), 1, "the app was asked");
            assert_eq!(
                app.status_message.as_ref().map(|(text, _)| text.clone()),
                Some(AppRefused.to_string())
            );
            assert!(!app.should_jump, "the dashboard stays open");
        },
    );
}

/// Makes the shown status, and its hold, `age` old.
fn age_status(app: &mut App, age: Duration) {
    let then = Instant::now() - age;
    if let Some((_, created)) = &mut app.status_message {
        *created = then;
    }
    if let Some(held) = &mut app.held_status {
        held.set_at = then;
    }
}

#[test]
fn a_refused_jump_fix_stays_until_a_key_and_five_seconds() {
    with_rex_app(
        "command::dashboard::app::rex_jump_tests::a_refused_jump_fix_stays_until_a_key_and_five_seconds",
        true,
        |app, _| {
            let shown = |app: &App| app.status_message.as_ref().map(|(text, _)| text.clone());
            let fix = Some(AppRefused.to_string());

            app.switch_to_pane_and_track("block:agent");
            age_status(app, Duration::from_secs(60));
            app.expire_status();
            assert_eq!(shown(app), fix, "no key was pressed");

            app.switch_to_pane_and_track("block:agent");
            age_status(app, Duration::from_secs(2));
            app.note_keypress();
            app.expire_status();
            assert_eq!(shown(app), fix, "a key came before five seconds");
            age_status(app, Duration::from_secs(5));
            app.expire_status();
            assert_eq!(shown(app), None);

            app.switch_to_pane_and_track("block:agent");
            age_status(app, Duration::from_secs(5));
            app.note_keypress();
            assert_eq!(shown(app), None, "a key after five seconds clears it");

            app.switch_to_pane_and_track("block:agent");
            let earlier = Instant::now() - Duration::from_secs(2);
            app.status_message = Some(("Sweep complete".into(), earlier));
            app.expire_status();
            assert_eq!(shown(app), None, "other statuses keep the short timeout");
        },
    );
}

#[test]
fn enter_on_a_session_worktree_without_an_agent_moves_the_app_there() {
    with_rex_app(
        "command::dashboard::app::rex_jump_tests::enter_on_a_session_worktree_without_an_agent_moves_the_app_there",
        false,
        |app, calls| {
            app.all_agents.clear();
            app.worktrees = vec![WorktreeInfo {
                repo_key: PathBuf::from("/nonexistent/repo/.git"),
                handle: "feat".into(),
                branch: "feat".into(),
                path: PathBuf::from("/nonexistent/repo__worktrees/feat"),
                is_main: false,
                mode: MuxMode::Session,
                has_mux_window: true,
                has_unmerged: false,
                pr_info: None,
                agent_status: None,
                created_at: None,
                base_branch: None,
            }];
            app.worktree_table_state.select(Some(0));
            app.jump_to_selected_worktree();
            let selects = selects(calls);
            assert_eq!(selects.len(), 1, "{selects:#?}");
            assert!(
                selects[0].contains("-C client:app-1 do session.select session_id=session:b"),
                "{selects:#?}"
            );
            assert!(app.status_message.is_none(), "{:?}", app.status_message);
        },
    );
}
