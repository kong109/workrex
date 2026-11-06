//! Show a Rex-hosted remote agent in Rex.app. The app is a client of the
//! node's Rex server over the tailnet, so the jump selects the agent's
//! session there; nothing runs locally.

use anyhow::{Context, Result, anyhow, bail};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use super::cache::WorkspaceRecord;
use super::protocol::RexPlacement;
use crate::config::Config;
use crate::multiplexer::Multiplexer;
use crate::multiplexer::rex::{HostedRead, RexBackend};

/// Show a live configured session, preserving its current block focus.
pub fn select(config: &Config, record: &WorkspaceRecord) -> Result<()> {
    let session = live_session(record)?.ok_or_else(|| {
        anyhow!(
            "The recorded Rex execution for '{}' has closed",
            record.handle
        )
    })?;
    let placement = record
        .execution
        .as_ref()
        .and_then(|execution| execution.rex.as_ref())
        .expect("a live session has a native placement");
    let server = RexBackend::for_server(&placement.host_label);
    let client = super::rex_client(config, &record.remote);
    server.select_on_app(client.as_deref(), &session)
}

/// Show the configured pane, even when several blocks share its session.
pub fn select_pane(config: &Config, record: &WorkspaceRecord, pane: usize) -> Result<()> {
    if !is_open(record)? {
        bail!(
            "The recorded Rex execution for '{}' has closed",
            record.handle
        );
    }
    let placement = record.execution.as_ref().unwrap().rex.as_ref().unwrap();
    let block = placement
        .blocks
        .get(&pane)
        .ok_or_else(|| anyhow!("Configured pane {pane} has no recorded Rex block"))?;
    let session = &placement.sessions[&pane];
    let server = RexBackend::for_server(&placement.host_label);
    let client = super::rex_client(config, &record.remote);
    server.select_on_app(client.as_deref(), session)?;
    server.select_pane(block)
}

const TARGET_READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub(crate) struct ServerUnavailable(anyhow::Error);

impl std::fmt::Display for ServerUnavailable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.0)
    }
}

impl std::error::Error for ServerUnavailable {}

/// Whether any configured terminal block still lives on its recorded server.
/// A failed read is an error, never evidence that the target closed.
pub fn is_open(record: &WorkspaceRecord) -> Result<bool> {
    Ok(live_session(record)?.is_some())
}

fn live_session(record: &WorkspaceRecord) -> Result<Option<String>> {
    let Some(execution) = &record.execution else {
        return Ok(None);
    };
    let placement = execution.rex.as_ref().ok_or_else(|| {
        anyhow!(
            "Remote workspace '{}' has no native Rex placement",
            record.handle
        )
    })?;
    validate_targets(placement)?;
    if execution.panes.iter().copied().collect::<BTreeSet<_>>()
        != placement.blocks.keys().copied().collect()
    {
        bail!("The remote execution's configured panes do not match its recorded Rex targets");
    }
    let sessions: Vec<&str> = placement
        .sessions
        .values()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let blocks: Vec<&str> = placement.blocks.values().map(String::as_str).collect();
    let server = RexBackend::for_server(&placement.host_label);
    let (instance, reads) = server
        .read_hosted_with_instance(&sessions, &blocks, TARGET_READ_TIMEOUT)
        .map_err(ServerUnavailable)
        .with_context(|| {
            format!(
                "Could not check Rex target for '{}' on '{}'",
                record.handle, record.remote
            )
        })?;
    target_session(placement, &instance, &reads).map(|session| session.map(str::to_string))
}

fn validate_targets(placement: &RexPlacement) -> Result<()> {
    if placement.host_label.is_empty() || placement.instance_id.is_empty() {
        bail!("The remote execution has no recorded Rex server identity");
    }
    if placement.blocks.is_empty() || placement.sessions.is_empty() {
        bail!(
            "The remote execution has no recorded Rex block identity; reopen it with a matching node build"
        );
    }
    for index in placement.sessions.keys() {
        if !placement.blocks.contains_key(index) {
            bail!("Configured pane {index} has no recorded Rex block");
        }
    }
    for (index, block) in &placement.blocks {
        let session = placement
            .sessions
            .get(index)
            .ok_or_else(|| anyhow!("Configured pane {index} has no recorded Rex session"))?;
        if !session.starts_with("session:") || !block.starts_with("block:") {
            bail!("Configured pane {index} has an invalid Rex identity");
        }
    }
    Ok(())
}

fn target_session<'a>(
    placement: &'a RexPlacement,
    instance: &str,
    reads: &HashMap<String, HostedRead>,
) -> Result<Option<&'a str>> {
    if instance != placement.instance_id {
        return Ok(None);
    }
    let mut unavailable = None;
    for (index, block) in &placement.blocks {
        let session = &placement.sessions[index];
        match reads.get(session) {
            Some(HostedRead::Live(panes)) => {
                if panes
                    .get(block)
                    .is_some_and(|pane| pane.session_id.as_deref() == Some(session.as_str()))
                {
                    return Ok(Some(session));
                }
            }
            Some(HostedRead::Gone) => {}
            Some(HostedRead::Unreadable(error)) => {
                unavailable = Some(format!("{session}: {error:#}"))
            }
            None => unavailable = Some(format!("Rex did not return recorded session {session}")),
        }
    }
    match unavailable {
        Some(error) => Err(anyhow!(error)),
        None => Ok(None),
    }
}

/// Drop only targets that the owning Rex server confirms have closed.
pub fn retain_open(records: &mut Vec<WorkspaceRecord>) -> Result<()> {
    let keep: Vec<bool> = records.iter().map(is_open).collect::<Result<_>>()?;
    let mut index = 0;
    records.retain(|_| {
        let live = keep[index];
        index += 1;
        live
    });
    Ok(())
}

/// Wait for configured native blocks to close; disconnects remain errors.
pub fn wait_until_closed(record: &WorkspaceRecord) -> Result<()> {
    while is_open(record)? {
        std::thread::sleep(Duration::from_millis(500));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiplexer::LivePaneInfo;

    fn target_is_open(
        placement: &RexPlacement,
        instance: &str,
        reads: &HashMap<String, HostedRead>,
    ) -> Result<bool> {
        Ok(target_session(placement, instance, reads)?.is_some())
    }

    fn placement() -> RexPlacement {
        RexPlacement {
            host_label: "node".into(),
            instance_id: "instance".into(),
            sessions: [(0, "session:owned".into())].into(),
            blocks: [(0, "block:owned".into())].into(),
            shells: Default::default(),
        }
    }

    fn pane(session: &str) -> LivePaneInfo {
        LivePaneInfo {
            pid: Some(1),
            current_command: Some("agent".into()),
            working_dir: "/repo".into(),
            title: None,
            session: Some("renamed session".into()),
            window: Some("renamed window".into()),
            session_id: Some(session.into()),
            window_id: Some("window:owned".into()),
            window_index: None,
        }
    }

    #[test]
    fn only_recorded_native_blocks_count_as_open() {
        let placement = placement();
        let mut panes = HashMap::from([("block:user-split".into(), pane("session:owned"))]);
        let reads = HashMap::from([("session:owned".into(), HostedRead::Live(panes.clone()))]);
        assert!(!target_is_open(&placement, "instance", &reads).unwrap());
        panes.insert("block:owned".into(), pane("session:owned"));
        let reads = HashMap::from([("session:owned".into(), HostedRead::Live(panes))]);
        assert!(target_is_open(&placement, "instance", &reads).unwrap());
        assert!(!target_is_open(&placement, "replaced-instance", &reads).unwrap());
    }

    #[test]
    fn an_unreadable_target_is_not_closed() {
        let reads = HashMap::from([(
            "session:owned".into(),
            HostedRead::Unreadable(anyhow!("offline")),
        )]);
        assert!(target_is_open(&placement(), "instance", &reads).is_err());
        assert!(target_is_open(&placement(), "instance", &HashMap::new()).is_err());
        let reads = HashMap::from([("session:owned".into(), HostedRead::Gone)]);
        assert!(!target_is_open(&placement(), "instance", &reads).unwrap());
    }

    #[test]
    fn a_block_in_another_session_is_not_the_owned_target() {
        let reads = HashMap::from([(
            "session:owned".into(),
            HostedRead::Live(HashMap::from([(
                "block:owned".into(),
                pane("session:other"),
            )])),
        )]);
        assert!(!target_is_open(&placement(), "instance", &reads).unwrap());
    }

    #[test]
    fn incomplete_native_placement_is_not_a_closed_target() {
        let mut placement = placement();
        placement.sessions.insert(1, "session:second".into());
        assert!(validate_targets(&placement).is_err());
        placement.blocks.insert(1, "block:second".into());
        assert!(validate_targets(&placement).is_ok());
        placement.sessions.remove(&0);
        assert!(validate_targets(&placement).is_err());
        let mut placement = self::placement();
        placement.instance_id.clear();
        assert!(validate_targets(&placement).is_err());
    }

    #[test]
    fn a_shared_session_remains_available_when_its_first_block_exits() {
        let mut placement = placement();
        placement.sessions.insert(1, "session:owned".into());
        placement.blocks.insert(1, "block:second".into());
        let reads = HashMap::from([(
            "session:owned".into(),
            HostedRead::Live(HashMap::from([(
                "block:second".into(),
                pane("session:owned"),
            )])),
        )]);
        assert_eq!(
            target_session(&placement, "instance", &reads).unwrap(),
            Some("session:owned")
        );
        placement.sessions.insert(1, "session:second".into());
        let reads = HashMap::from([
            ("session:owned".into(), HostedRead::Gone),
            (
                "session:second".into(),
                HostedRead::Live(HashMap::from([(
                    "block:second".into(),
                    pane("session:second"),
                )])),
            ),
        ]);
        assert_eq!(
            target_session(&placement, "instance", &reads).unwrap(),
            Some("session:second")
        );
    }

    #[test]
    fn native_jumps_preserve_focus_until_the_app_accepts_the_session() {
        use crate::remote::cache::ExecutionRecord;
        use crate::test_support::{ISOLATED_TEST_CANARY, is_isolated_child, run_isolated_test};
        use std::os::unix::fs::PermissionsExt;

        const NAME: &str = "remote::rex_jump::tests::native_jumps_preserve_focus_until_the_app_accepts_the_session";
        if !is_isolated_child(NAME) {
            let temp = tempfile::tempdir().unwrap();
            run_isolated_test(
                NAME,
                temp.path(),
                &[
                    ("REX_BIN_DIR", temp.path()),
                    ("XDG_STATE_HOME", temp.path()),
                ],
            );
            return;
        }
        let dir = std::env::current_dir().unwrap();
        let mut placement = placement();
        placement.sessions.insert(1, "session:owned".into());
        placement.blocks.insert(1, "block:second".into());
        let version: serde_json::Value = serde_json::from_str(include_str!(
            "../multiplexer/testdata/rex_server_version_mac.json"
        ))
        .unwrap();
        let process: serde_json::Value =
            serde_json::from_str(include_str!("../multiplexer/testdata/rex_process.json")).unwrap();
        let answer = serde_json::json!({
            "version": version, "status": {"instance_id": "instance"},
            "reads": {"session:owned": {
                "session": {"session_id": "session:owned", "label": "renamed"},
                "blocks": {"block:second": {
                    "window": {"window_id": "window:owned", "label": "renamed"},
                    "process": process
                }}
            }}
        });
        std::fs::write(dir.join("read.json"), answer.to_string()).unwrap();
        let program = dir.join("rex");
        std::fs::write(&program, r#"#!/bin/sh
case "$*" in
  *client.list*) echo '{"clients":[{"client_id":"client:app","connection_state":"connected","info":{"kind":"app"}}]}' ;;
  *client.perform*)
    cat > "$REX_BIN_DIR/selected.json"
    echo select >> "$REX_BIN_DIR/calls"
    if [ -f "$REX_BIN_DIR/refuse" ]; then echo 'Error: code=refused' >&2; exit 1; fi
    echo '{}' ;;
  *target.resolve*) echo '{"results":[{"status":"resolved","target":{"session_id":"session:owned","window_id":"window:owned"}}]}' ;;
  *session.focus_block*)
    cat > "$REX_BIN_DIR/focused.json"
    echo focus >> "$REX_BIN_DIR/calls"
    echo '{}' ;;
  *" do -e "*) cat "$REX_BIN_DIR/read.json" ;;
  *) echo "Unexpected focus mutation: $*" >&2; exit 1 ;;
esac
"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut record = WorkspaceRecord::new(
            "w-jump".into(),
            "node",
            "project".into(),
            dir.clone(),
            "feat",
            "feat",
            "base".into(),
            None,
            "feat".into(),
            dir.join("feat"),
        );
        record.execution = Some(ExecutionRecord {
            ex_id: "e-jump".into(),
            panes: vec![0, 1],
            node_bin: "node".into(),
            rex: Some(placement),
        });
        select(&Config::default(), &record).unwrap();
        let selected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("selected.json")).unwrap()).unwrap();
        assert_eq!(selected["action"], "session.select");
        assert_eq!(
            selected["args"],
            serde_json::json!({"session_id": "session:owned"})
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("calls")).unwrap(),
            "select\n"
        );
        assert!(!dir.join("focused.json").exists());

        std::fs::write(dir.join("calls"), "").unwrap();
        std::fs::write(dir.join("refuse"), "").unwrap();
        assert!(select_pane(&Config::default(), &record, 1).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("calls")).unwrap(),
            "select\n"
        );
        assert!(!dir.join("focused.json").exists());

        std::fs::remove_file(dir.join("refuse")).unwrap();
        std::fs::write(dir.join("calls"), "").unwrap();
        select_pane(&Config::default(), &record, 1).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("calls")).unwrap(),
            "select\nfocus\n"
        );
        let focused: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("focused.json")).unwrap()).unwrap();
        assert_eq!(focused["session_id"], "session:owned");
        assert_eq!(focused["block_id"], "block:second");
        println!("{ISOLATED_TEST_CANARY}");
    }
}
