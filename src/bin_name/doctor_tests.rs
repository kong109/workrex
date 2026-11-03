//! Doctor fixes keep user data verbatim; only the command name in the template is [`BIN`].

use super::*;

#[test]
fn fixes_keep_names_that_spell_workmux() {
    let spec = RemoteSpec {
        name: "workmux".into(),
        host: "workmux".into(),
        path: "~/.workmux-node".into(),
        isolation: RemoteIsolation::Container,
        isolation_explicit: true,
        binary: None,
    };
    let facts = parse_facts(
        "os=Linux\narch=x86_64\nuser=workmux\nhostname=box\ntmux=tmux 3.4\n\
         git=git version 2.43.0\nlinger=no\ndocker_status=1\ndocker_out=permission denied\n",
    );
    let cx = Context {
        spec: &spec,
        node: &NodeStatus::Missing,
        node_path: "~/.workmux-node/bin/workrex-1",
        mismatch_hint: "rebuild",
        disconnect_hint: Some("ssh -O exit -o ControlPath=/s workmux"),
    };
    let (rows, _) = evaluate(&facts, &cx);
    let fix = |check: &str| {
        let row = rows.iter().find(|row| row.check == check).unwrap();
        row.fix.clone().unwrap()
    };
    assert_eq!(fix("node"), format!("{BIN} remote setup workmux"));
    assert_eq!(fix("linger"), "sudo loginctl enable-linger workmux");
    assert_eq!(
        fix("container"),
        "sudo usermod -aG docker workmux, then reconnect: ssh -O exit -o ControlPath=/s workmux"
    );
}
