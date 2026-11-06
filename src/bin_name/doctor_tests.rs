//! Doctor fixes keep user data verbatim; only the command name in the template is [`BIN`].

use super::*;

#[test]
fn fixes_keep_names_that_spell_workrex() {
    let spec = RemoteSpec {
        name: "workrex".into(),
        host: "workrex".into(),
        path: "~/.workrex-node".into(),
        isolation: RemoteIsolation::Container,
        isolation_explicit: true,
        binary: None,
        terminal: Default::default(),
    };
    let facts = parse_facts(
        "os=Linux\narch=x86_64\nuser=workrex\nhostname=box\nrex=rex 1\nrex_server={}\n\
         git=git version 2.43.0\nlinger=no\ndocker_status=1\ndocker_out=permission denied\n",
    );
    let cx = Context {
        spec: &spec,
        node: &NodeStatus::Missing,
        node_path: "~/.workrex-node/bin/workrex-1",
        mismatch_hint: "rebuild",
        disconnect_hint: Some("ssh -O exit -o ControlPath=/s workrex"),
    };
    let (rows, _) = evaluate(&facts, &cx);
    let fix = |check: &str| {
        let row = rows.iter().find(|row| row.check == check).unwrap();
        row.fix.clone().unwrap()
    };
    assert_eq!(fix("node"), format!("{BIN} remote setup workrex"));
    assert_eq!(fix("linger"), "sudo loginctl enable-linger workrex");
    assert_eq!(
        fix("container"),
        "sudo usermod -aG docker workrex, then reconnect: ssh -O exit -o ControlPath=/s workrex"
    );
}
