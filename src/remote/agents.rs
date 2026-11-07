//! Native remote agents in local views and commands: observations supply rows;
//! recorded server and block identities route terminal actions.

use crate::bin_name::BIN;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::RemoteSpec;
use super::cache::{self, SentTurn, WorkspaceRecord};
use super::client::Node;
use super::node::observe::{
    CaptureRequest, CaptureResponse, ObservedAgent, SendRequest, SendResponse,
};
use super::observe::{self, Observation};
use super::protocol::{self, DiffMode, DiffRequest, DiffResponse};
use crate::config::Config;
use crate::multiplexer::{AgentPane, EndReason, RemoteAgent, RemoteRef};
use crate::multiplexer::{Multiplexer, rex::RexBackend};

/// The remote workspace `target` names: a handle or branch of this
/// repository (local worktrees win), or `project:handle`.
pub fn find_target(target: &str) -> Option<WorkspaceRecord> {
    if let Some((project, handle)) = target.split_once(':')
        && !project.is_empty()
        && !handle.is_empty()
    {
        let mut matching = cache::all().into_iter().filter(|record| {
            record.handle == handle
                && record
                    .project_root
                    .file_name()
                    .is_some_and(|name| name == project)
        });
        let record = matching.next()?;
        return matching.next().is_none().then_some(record);
    }
    cache::find_here(target)
}

/// Stable view key; terminal actions resolve the scoped [`RemoteRef`] instead.
pub fn detached_pane_id(ws_id: &str, pane: usize) -> String {
    format!("%remote-{ws_id}-{pane}")
}

/// Native rows come from the owned records and node observations. A workspace
/// that has not been observed yet shows one row without a status.
pub fn rows(
    records: &[WorkspaceRecord],
    observations: &HashMap<String, Observation>,
    hosts: &HashMap<String, String>,
    now: u64,
) -> Vec<AgentPane> {
    let mut rows = Vec::new();
    for record in records {
        let Some(execution) = &record.execution else {
            continue;
        };
        let observation = observations.get(&record.remote);
        let observed = observation.and_then(|o| o.execution(&record.ws_id, &execution.ex_id));
        let git = observed.and_then(|o| o.git.clone()).map(|mut status| {
            let base = record.base_branch();
            status.behind = behind(record, &base);
            status.base_branch = base;
            status
        });
        let connected = observation.is_some_and(|o| o.is_connected(now));
        let remote = |pane: usize| RemoteAgent {
            at: RemoteRef {
                remote: record.remote.clone(),
                ws_id: record.ws_id.clone(),
                ex_id: execution.ex_id.clone(),
                pane,
            },
            handle: record.handle.clone(),
            project_root: record.project_root.clone(),
            host: hosts.get(&record.remote).cloned().unwrap_or_default(),
            attached: false,
            connected,
            connecting: !connected && observe::connecting(&record.remote, observation, now),
            last_seen: observation.and_then(|o| o.last_seen),
            git: git.clone(),
            ended: observed.and_then(|o| o.ended),
        };
        let Some(observed) = observed else {
            rows.push(detached_row(
                record,
                None,
                None,
                remote(execution.panes.first().copied().unwrap_or(0)),
            ));
            continue;
        };
        // An execution that ended has no agents left; one row says so.
        if observed.ended.is_some() {
            let pane = execution.panes.first().copied().unwrap_or(0);
            rows.push(detached_row(record, None, observation, remote(pane)));
            continue;
        }
        for pane in &observed.panes {
            let Some(agent) = &pane.agent else {
                continue;
            };
            rows.push(detached_row(
                record,
                Some(agent),
                observation,
                remote(pane.index),
            ));
        }
    }
    rows
}

/// How long a [`behind`] count is reused.
const BEHIND_TTL: Duration = Duration::from_secs(10);

/// Commits `base` gained since `record`'s workspace was created from it.
/// Views ask on every refresh, so counts are reused for a while.
fn behind(record: &WorkspaceRecord, base: &str) -> usize {
    type Key = (PathBuf, String, String);
    static COUNTS: OnceLock<Mutex<HashMap<Key, (Instant, usize)>>> = OnceLock::new();
    let key = (
        record.project_root.clone(),
        record.base_sha.clone(),
        base.to_string(),
    );
    let mut counts = COUNTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    if let Some((at, count)) = counts.get(&key)
        && at.elapsed() < BEHIND_TTL
    {
        return *count;
    }
    let range = format!("{}..{base}", record.base_sha);
    let count = super::git::run(&record.project_root, &["rev-list", "--count", &range])
        .ok()
        .and_then(|count| count.parse().ok())
        .unwrap_or(0);
    counts.insert(key, (Instant::now(), count));
    count
}

fn detached_row(
    record: &WorkspaceRecord,
    agent: Option<&ObservedAgent>,
    observation: Option<&Observation>,
    remote: RemoteAgent,
) -> AgentPane {
    let local_ts = |ts: u64| observation.map_or(ts, |o| o.local_ts(ts));
    AgentPane {
        session: String::new(),
        window_name: record.window_name.clone(),
        pane_id: detached_pane_id(&record.ws_id, remote.at.pane),
        window_id: String::new(),
        window_index: None,
        path: record.local_path.clone(),
        pane_title: agent.and_then(|agent| agent.title.clone()),
        status: agent.and_then(|agent| agent.status),
        status_ts: agent.and_then(|agent| agent.status_ts).map(local_ts),
        activity_ts: None,
        updated_ts: None,
        window_cmd: None,
        agent_command: None,
        agent_kind: agent.and_then(|agent| agent.agent_kind.clone()),
        prompt: agent.and_then(|agent| agent.prompt.clone()),
        remote: Some(remote),
    }
}

/// Local rows followed by native remote rows, with observations kept current.
pub fn for_views(config: &Config, agents: Vec<AgentPane>, now: u64) -> Vec<AgentPane> {
    let mut local: Vec<_> = agents
        .into_iter()
        .filter(|agent| agent.remote.is_none())
        .collect();
    let mut records = cache::all();
    records.retain(|record| !record.cleanup_pending());
    if records.is_empty() {
        return local;
    }
    observe::spawn(config, &remotes_in_use(&records));
    let observations = observations(config, &records, false);
    local.extend(rows(&records, &observations, &hosts(config), now));
    local
}

/// The worktree label of a remote agent: `handle@remote`.
pub fn label(worktree: &str, remote: &str) -> String {
    format!("{worktree}@{remote}")
}

/// What views say about a remote they cannot reach or an execution that
/// ended under its agent; `None` while it runs and the remote answers.
pub fn connectivity(remote: &RemoteAgent, now: u64) -> Option<String> {
    if remote.connected {
        return remote.ended.map(|reason| match reason {
            EndReason::HostReboot => "exited · host rebooted · `workrex open` resumes".to_string(),
        });
    }
    if remote.connecting {
        return Some("connecting".to_string());
    }
    Some(match remote.last_seen {
        Some(seen) => format!(
            "disconnected · seen {} ago",
            crate::util::format_elapsed_secs(now.saturating_sub(seen))
        ),
        None => "disconnected".to_string(),
    })
}

/// The short form of [`connectivity`] that rows show ahead of the agent's
/// last title, which they keep.
pub fn row_title(remote: &RemoteAgent, now: u64, title: Option<&str>) -> Option<String> {
    let state = if remote.connected {
        remote.ended?.label().to_string()
    } else if remote.connecting {
        "connecting".to_string()
    } else {
        match remote.last_seen {
            Some(seen) => format!(
                "disconnected {}",
                crate::util::format_elapsed_secs(now.saturating_sub(seen))
            ),
            None => "disconnected".to_string(),
        }
    };
    Some(match title.filter(|title| !title.is_empty()) {
        Some(title) => format!("{state} · {title}"),
        None => state,
    })
}

/// Show this exact configured block on its recorded Rex server.
pub fn open_window(remote: &RemoteAgent) -> Result<()> {
    let record = current_record(&remote.at)?;
    let (config, _) = record.creator_config()?;
    let spec = RemoteSpec::resolve(&config, &record.remote)?;
    observe::ensure_background(&spec, &record.project_root)?;
    super::rex_jump::select_pane(&config, &record, remote.at.pane)
}

fn current_record(at: &RemoteRef) -> Result<WorkspaceRecord> {
    cache::all()
        .into_iter()
        .find(|record| {
            record.remote == at.remote
                && record.ws_id == at.ws_id
                && record
                    .execution
                    .as_ref()
                    .is_some_and(|execution| execution.ex_id == at.ex_id)
        })
        .ok_or_else(|| anyhow!("The selected remote execution is no longer current"))
}

/// Resolve the row's immutable block on its owning server. Never substitute
/// another configured pane, a user split, or a later workspace execution.
fn native_target(at: &RemoteRef) -> Result<(RexBackend, String)> {
    let record = current_record(at)?;
    if !super::rex_jump::is_open(&record)? {
        anyhow::bail!("The selected remote execution has closed");
    }
    let placement = record.execution.unwrap().rex.unwrap();
    let block = placement
        .blocks
        .get(&at.pane)
        .ok_or_else(|| anyhow!("Configured pane {} has no recorded Rex block", at.pane))?
        .clone();
    Ok((RexBackend::for_server(&placement.host_label), block))
}

pub fn send_key(at: &RemoteRef, key: &str) -> Result<()> {
    let (server, block) = native_target(at)?;
    server.send_key(&block, key)
}

pub fn paste_text(at: &RemoteRef, text: &str) -> Result<()> {
    let (server, block) = native_target(at)?;
    server.paste_text(&block, text)
}

pub fn send_keys_to_agent(at: &RemoteRef, keys: &str, agent_kind: Option<&str>) -> Result<()> {
    let (server, block) = native_target(at)?;
    server.send_keys_to_agent(&block, keys, agent_kind)
}

pub fn kill_pane(at: &RemoteRef) -> Result<()> {
    let (server, block) = native_target(at)?;
    server.kill_pane(&block)
}

pub fn capture_native(at: &RemoteRef, lines: u16) -> Result<String> {
    let (server, block) = native_target(at)?;
    server
        .capture_pane(&block, lines)
        .ok_or_else(|| anyhow!("Failed to capture the selected remote Rex pane"))
}

pub fn fetch_diff_for(at: &RemoteRef, mode: DiffMode) -> Result<CachedDiff> {
    let record = current_record(at)?;
    let (config, _) = record.creator_config()?;
    fetch_diff(&config, &at.remote, &at.ws_id, mode)
}

/// ssh destinations of the configured remotes, for display.
pub fn hosts(config: &Config) -> HashMap<String, String> {
    config
        .remotes
        .iter()
        .map(|(name, remote)| (name.clone(), remote.host.clone()))
        .collect()
}

/// The remotes of `records` that run an execution, which are the ones
/// worth observing.
fn remotes_in_use(records: &[WorkspaceRecord]) -> Vec<String> {
    let mut names: Vec<String> = records
        .iter()
        .filter(|record| record.execution.is_some())
        .map(|record| record.remote.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The observations of the remotes of `records`: as last written, or with
/// `fetch` made current by asking nodes whose observation is stale.
pub fn observations(
    config: &Config,
    records: &[WorkspaceRecord],
    fetch: bool,
) -> HashMap<String, Observation> {
    remotes_in_use(records)
        .into_iter()
        .filter_map(|name| {
            let observation = if fetch {
                Some(observe::current(config, &name))
            } else {
                observe::load(&name)
            };
            observation.map(|o| (name, o))
        })
        .collect()
}

/// Type `text` into the workspace's agent through the node, and remember
/// the agent's state before it so `wait` holds out for the turn it starts.
pub fn send(config: &Config, record: &WorkspaceRecord, text: &str) -> Result<()> {
    let Some(execution) = &record.execution else {
        return Err(no_execution(record));
    };
    let spec = RemoteSpec::resolve(config, &record.remote)?;
    let node = Node::connect_reader(&spec, protocol::SEND)?;
    let response: SendResponse = node.call(
        protocol::SEND,
        &SendRequest {
            ws_id: record.ws_id.clone(),
            pane: None,
            text: text.to_string(),
        },
        false,
    )?;
    // Nodes from before the baseline report nothing to tell the turn by.
    let turn = response.pane.map(|pane| SentTurn {
        ex_id: execution.ex_id.clone(),
        pane,
        status: response.status,
        status_ts: response.status_ts,
        sent_at: cache::now(),
    });
    if let Err(err) = cache::set_sent_turn(&record.remote, &record.ws_id, turn.as_ref()) {
        tracing::warn!(handle = %record.handle, "failed to record the sent turn: {err:#}");
    }
    Ok(())
}

/// The last `lines` of pane `pane` of the workspace, or of its agent pane,
/// from the node.
pub fn capture(
    config: &Config,
    record: &WorkspaceRecord,
    pane: Option<usize>,
    lines: u16,
) -> Result<String> {
    if record.execution.is_none() {
        return Err(no_execution(record));
    }
    capture_from(config, &record.remote, &record.ws_id, pane, lines)
}

/// [`capture`] of workspace `ws_id` on `remote`.
pub fn capture_from(
    config: &Config,
    remote: &str,
    ws_id: &str,
    pane: Option<usize>,
    lines: u16,
) -> Result<String> {
    let spec = RemoteSpec::resolve(config, remote)?;
    let node = Node::connect_reader(&spec, protocol::CAPTURE)?;
    let response: CaptureResponse = node.call(
        protocol::CAPTURE,
        &CaptureRequest {
            ws_id: ws_id.to_string(),
            pane,
            lines,
        },
        false,
    )?;
    Ok(response.text)
}

/// A workspace's diff as its node last returned it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedDiff {
    pub text: String,
    pub truncated: bool,
    /// When it was fetched (laptop clock).
    pub fetched_at: u64,
}

/// The diff of workspace `ws_id` last fetched from `remote`.
pub fn cached_diff(remote: &str, ws_id: &str, mode: DiffMode) -> Option<CachedDiff> {
    let bytes = std::fs::read(cache::diff_path(remote, ws_id, mode).ok()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Fetch the diff of workspace `ws_id` from `remote` and keep it for
/// [`cached_diff`].
pub fn fetch_diff(
    config: &Config,
    remote: &str,
    ws_id: &str,
    mode: DiffMode,
) -> Result<CachedDiff> {
    let spec = RemoteSpec::resolve(config, remote)?;
    let node = Node::connect_reader(&spec, protocol::DIFF)?;
    let response: DiffResponse = node.call(
        protocol::DIFF,
        &DiffRequest {
            ws_id: ws_id.to_string(),
            mode,
        },
        false,
    )?;
    let diff = CachedDiff {
        text: response.text,
        truncated: response.truncated,
        fetched_at: cache::now(),
    };
    let path = cache::diff_path(remote, ws_id, mode)?;
    let saved = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .map_err(anyhow::Error::from)
        .and_then(|()| Ok(serde_json::to_vec(&diff)?))
        .and_then(|bytes| crate::util::write_atomic(&path, &bytes));
    if let Err(err) = saved {
        tracing::debug!(path = %path.display(), "failed to cache a remote diff: {err:#}");
    }
    Ok(diff)
}

fn no_execution(record: &WorkspaceRecord) -> anyhow::Error {
    anyhow!(
        "remote workspace '{}' has no running agent; `{BIN} open {}` starts it",
        record.handle,
        record.handle
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multiplexer::AgentStatus;
    use crate::remote::cache::ExecutionRecord;
    use crate::remote::node::observe::{ObservedPane, ObservedWorkspace};
    use std::path::PathBuf;

    #[test]
    fn native_input_uses_the_selected_block_and_rejects_stale_identity() {
        use crate::remote::protocol::RexPlacement;
        use crate::test_support::{ISOLATED_TEST_CANARY, is_isolated_child, run_isolated_test};
        use std::os::unix::fs::PermissionsExt;

        const NAME: &str = "remote::agents::tests::native_input_uses_the_selected_block_and_rejects_stale_identity";
        if !is_isolated_child(NAME) {
            let temp = tempfile::tempdir().unwrap();
            run_isolated_test(
                NAME,
                temp.path(),
                &[
                    ("REX_BIN_DIR", temp.path()),
                    ("XDG_STATE_HOME", temp.path()),
                    ("REX_SESSION", std::path::Path::new("session:local")),
                    ("REX_BLOCK", std::path::Path::new("block:local")),
                ],
            );
            return;
        }
        let dir = std::env::current_dir().unwrap();
        let version: serde_json::Value = serde_json::from_str(include_str!(
            "../multiplexer/testdata/rex_server_version_mac.json"
        ))
        .unwrap();
        let process: serde_json::Value =
            serde_json::from_str(include_str!("../multiplexer/testdata/rex_process.json")).unwrap();
        let mut answer = serde_json::json!({
            "version": version, "status": {"instance_id": "instance"},
            "reads": {"session:owned": {
                "session": {"session_id": "session:owned", "label": "agent"},
                "blocks": {"block:selected": {
                    "window": {"window_id": "window:owned", "label": "agent"},
                    "process": process
                }}
            }}
        });
        let read_path = dir.join("read.json");
        std::fs::write(&read_path, answer.to_string()).unwrap();
        let program = dir.join("rex");
        std::fs::write(&program, r#"#!/bin/sh
test -z "$REX_SESSION$REX_BLOCK" || exit 1
case "$*" in
  *" do -e "*) cat "$REX_BIN_DIR/read.json" ;;
  *"api call target.resolve"*) echo '{"results":[{"status":"resolved","target":{"session_id":"session:owned"}}]}' ;;
  *"block call -b block:selected format"*) echo '{"content":"selected pane output"}' ;;
  *"send-key -b block:selected "*|*"block call -b block:selected write"*|*"api call block.event"*|*"api call block.close"*)
    printf '%s\n' "$*" >> "$REX_BIN_DIR/mutations"
    cat >> "$REX_BIN_DIR/mutations"
    printf '\n' >> "$REX_BIN_DIR/mutations"
    echo '{}' ;;
  *) echo "Unexpected target: $*" >&2; exit 1 ;;
esac
"#).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut record = record();
        record.project_root = dir.clone();
        record.execution.as_mut().unwrap().rex = Some(RexPlacement {
            host_label: "node.example".into(),
            instance_id: "instance".into(),
            sessions: [(0, "session:owned".into()), (1, "session:owned".into())].into(),
            blocks: [(0, "block:closed".into()), (1, "block:selected".into())].into(),
            shells: Default::default(),
        });
        record.save().unwrap();
        let at = RemoteRef {
            remote: "box".into(),
            ws_id: "w1".into(),
            ex_id: "x1".into(),
            pane: 1,
        };
        send_key(&at, "Enter").unwrap();
        paste_text(&at, "pasted").unwrap();
        send_keys_to_agent(&at, "commit", None).unwrap();
        assert_eq!(capture_native(&at, 20).unwrap(), "selected pane output");
        kill_pane(&at).unwrap();
        let mutations = std::fs::read_to_string(dir.join("mutations")).unwrap();
        assert!(mutations.contains("-S node.example"), "{mutations}");
        assert!(mutations.contains("block:selected"), "{mutations}");
        assert!(
            mutations.contains("pasted") && mutations.contains("commit"),
            "{mutations}"
        );
        assert!(
            !mutations.contains("block:closed") && !mutations.contains("block:local"),
            "{mutations}"
        );

        let mut invalid = at.clone();
        invalid.pane = 2;
        assert!(send_key(&invalid, "Enter").is_err());
        answer["status"]["instance_id"] = serde_json::json!("replacement");
        std::fs::write(&read_path, answer.to_string()).unwrap();
        assert!(send_key(&at, "Enter").is_err());
        answer["status"]["instance_id"] = serde_json::json!("instance");
        std::fs::write(&read_path, answer.to_string()).unwrap();
        record.execution.as_mut().unwrap().ex_id = "x2".into();
        record.save().unwrap();
        assert!(send_key(&at, "Enter").is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("mutations")).unwrap(),
            mutations
        );
        println!("{ISOLATED_TEST_CANARY}");
    }

    fn record() -> WorkspaceRecord {
        let mut record = WorkspaceRecord::new(
            "w1".into(),
            "box",
            "p".into(),
            PathBuf::from("/src/app"),
            "feat",
            "feat",
            "0".repeat(40),
            None,
            "wr-feat".into(),
            PathBuf::from("/src/app__worktrees/feat"),
        );
        record.execution = Some(ExecutionRecord {
            ex_id: "x1".into(),
            panes: vec![0, 1],
            node_bin: "workrex-1".into(),
            rex: None,
        });
        record
    }

    fn observation(connected_at: u64, panes: Vec<ObservedPane>) -> Observation {
        Observation {
            connected: true,
            last_seen: Some(connected_at),
            skew: 100,
            workspaces: vec![ObservedWorkspace {
                ws_id: "w1".into(),
                ex_id: "x1".into(),
                panes,
                git: None,
                sibling: None,
                operation: None,
                ended: None,
                rex: None,
            }],
        }
    }

    fn agent_pane(index: usize, status: AgentStatus) -> ObservedPane {
        ObservedPane {
            index,
            alive: true,
            agent: Some(ObservedAgent {
                status: Some(status),
                status_ts: Some(10),
                title: Some("Fixing".into()),
                agent_kind: Some("claude".into()),
                prompt: None,
            }),
        }
    }

    fn attach_row(pane: usize) -> AgentPane {
        let mut remote = RemoteAgent::new(RemoteRef {
            remote: "box".into(),
            ws_id: "w1".into(),
            ex_id: "x1".into(),
            pane,
        });
        remote.handle = String::new();
        AgentPane {
            session: "main".into(),
            window_name: "wr-feat".into(),
            pane_id: "%7".into(),
            window_id: "@3".into(),
            window_index: Some(2),
            path: "/src/app__worktrees/feat".into(),
            pane_title: Some("Gi=31;AAAA".into()),
            status: Some(AgentStatus::Done),
            status_ts: Some(110),
            activity_ts: None,
            updated_ts: None,
            window_cmd: None,
            agent_command: None,
            agent_kind: None,
            prompt: None,
            remote: Some(remote),
        }
    }

    fn hosts() -> HashMap<String, String> {
        HashMap::from([("box".to_string(), "me@box".to_string())])
    }

    #[test]
    fn native_observations_discard_stale_local_remote_rows() {
        use crate::test_support::{ISOLATED_TEST_CANARY, is_isolated_child, run_isolated_test};
        const NAME: &str =
            "remote::agents::tests::native_observations_discard_stale_local_remote_rows";
        if !is_isolated_child(NAME) {
            let temp = tempfile::tempdir().unwrap();
            run_isolated_test(NAME, temp.path(), &[("XDG_STATE_HOME", temp.path())]);
            return;
        }
        let rec = record();
        rec.save().unwrap();
        let owner = cache::lock_file("box", "observer-owner.v4.lock").unwrap();
        owner.lock().unwrap();
        let stream = cache::lock_file("box", "observer.v4.lock").unwrap();
        stream.lock().unwrap();
        let observation = observation(1000, vec![agent_pane(1, AgentStatus::Working)]);
        std::fs::write(
            cache::remote_dir("box").unwrap().join("observed.v4.json"),
            serde_json::to_vec(&observation).unwrap(),
        )
        .unwrap();
        let config = Config {
            remotes: [(
                "box".into(),
                crate::config::RemoteConfig {
                    host: "example".into(),
                    path: None,
                    isolation: None,
                    binary: None,
                    terminal: None,
                    rex_client: None,
                },
            )]
            .into(),
            ..Default::default()
        };
        let stale = attach_row(0);
        let mut local = stale.clone();
        local.remote = None;
        local.pane_id = "block:local".into();
        let result = for_views(&config, vec![stale, local], 1001);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].pane_id, "block:local");
        assert_eq!(result[1].pane_id, detached_pane_id("w1", 1));
        assert_eq!(result[1].status, Some(AgentStatus::Working));
        assert_eq!(result[1].pane_title.as_deref(), Some("Fixing"));
        assert_eq!(result[1].remote.as_ref().unwrap().at.pane, 1);
        assert!(!result[1].remote.as_ref().unwrap().attached);
        println!("{ISOLATED_TEST_CANARY}");
    }

    #[test]
    fn detached_agents_get_rows_from_the_observation() {
        let observations = HashMap::from([(
            "box".to_string(),
            observation(
                1000,
                vec![
                    agent_pane(0, AgentStatus::Working),
                    ObservedPane {
                        index: 1,
                        alive: true,
                        agent: None,
                    },
                ],
            ),
        )]);
        let rows = rows(&[record()], &observations, &hosts(), 1005);
        let [row] = rows.as_slice() else {
            panic!("{rows:?}")
        };
        assert_eq!(row.pane_id, "%remote-w1-0");
        assert_eq!(row.path, PathBuf::from("/src/app__worktrees/feat"));
        assert_eq!(row.status, Some(AgentStatus::Working));
        assert_eq!(row.status_ts, Some(110));
        let remote = row.remote.as_ref().unwrap();
        assert!(!remote.attached && remote.connected);
        assert_eq!(remote.host, "me@box");
        assert_eq!(remote.handle, "feat");

        // Stale: still listed, disconnected since the last answer.
        let rows = super::rows(&[record()], &observations, &hosts(), 1100);
        let remote = rows[0].remote.as_ref().unwrap();
        assert!(!remote.connected);
        assert_eq!(remote.last_seen, Some(1000));
    }

    #[test]
    fn native_rows_preserve_the_observed_pane_identity() {
        let observations = HashMap::from([(
            "box".to_string(),
            observation(1000, vec![agent_pane(0, AgentStatus::Done)]),
        )]);
        let rows = rows(&[record()], &observations, &hosts(), 1001);
        let [row] = rows.as_slice() else {
            panic!("{rows:?}")
        };
        assert_eq!(row.pane_id, detached_pane_id("w1", 0));
        assert_eq!(row.pane_title.as_deref(), Some("Fixing"));
        assert_eq!(row.agent_kind.as_deref(), Some("claude"));
        assert!(!row.remote.as_ref().unwrap().attached);

        // A missing observed title stays missing.
        let mut untitled = agent_pane(0, AgentStatus::Done);
        untitled.agent.as_mut().unwrap().title = None;
        let observations = HashMap::from([("box".to_string(), observation(1000, vec![untitled]))]);
        let rows = super::rows(&[record()], &observations, &hosts(), 1001);
        assert_eq!(rows[0].pane_title, None);
    }

    #[test]
    fn unobserved_workspaces_still_show() {
        let rows_without = rows(&[record()], &HashMap::new(), &hosts(), 5);
        let [row] = rows_without.as_slice() else {
            panic!("{rows_without:?}")
        };
        assert_eq!(row.status, None);
        assert!(!row.remote.as_ref().unwrap().connected);

        let mut ended = record();
        ended.execution = None;
        assert!(rows(&[ended], &HashMap::new(), &hosts(), 5).is_empty());
    }

    #[test]
    fn a_remote_reads_connecting_until_it_answers_the_view() {
        let mut watched = record();
        watched.remote = "box-connecting".into();
        observe::observing_since("box-connecting", 1000);
        // The last observation is two minutes older than the view.
        let observations = HashMap::from([(
            "box-connecting".to_string(),
            Observation {
                last_seen: Some(880),
                ..Default::default()
            },
        )]);
        let rows_now = rows(&[watched], &observations, &hosts(), 1002);
        let remote = rows_now[0].remote.as_ref().unwrap();
        assert!(remote.connecting && !remote.connected);
        assert_eq!(connectivity(remote, 1002).as_deref(), Some("connecting"));

        // A remote no view of this process observes is just disconnected.
        let unwatched = rows(&[record()], &observations, &hosts(), 1002);
        assert!(!unwatched[0].remote.as_ref().unwrap().connecting);
    }

    #[test]
    fn a_connected_row_is_never_connecting() {
        let mut watched = record();
        watched.remote = "box-kept-current".into();
        observe::observing_since("box-kept-current", 1000);
        // Another view's observer kept the file current before this view
        // started, so the remote is connected but has not answered this view.
        let observations = HashMap::from([(
            "box-kept-current".to_string(),
            Observation {
                connected: true,
                last_seen: Some(999),
                ..Default::default()
            },
        )]);
        let rows_now = rows(&[watched], &observations, &hosts(), 1002);
        let remote = rows_now[0].remote.as_ref().unwrap();
        assert!(remote.connected && !remote.connecting);
    }

    #[test]
    fn rows_keep_the_title_beside_connectivity() {
        let rows = rows(&[record()], &HashMap::new(), &hosts(), 5);
        let mut remote = rows[0].remote.clone().unwrap();
        remote.last_seen = Some(1000);
        assert_eq!(
            row_title(&remote, 1120, Some("Fixing")).as_deref(),
            Some("disconnected 2m · Fixing")
        );
        remote.last_seen = None;
        assert_eq!(
            row_title(&remote, 5, Some("")).as_deref(),
            Some("disconnected")
        );
        // Until a view's new observer hears from the remote.
        remote.last_seen = Some(1000);
        remote.connecting = true;
        assert_eq!(
            row_title(&remote, 1120, Some("Fixing")).as_deref(),
            Some("connecting · Fixing")
        );
        assert_eq!(connectivity(&remote, 1120).as_deref(), Some("connecting"));
        remote.connecting = false;
        remote.connected = true;
        assert_eq!(row_title(&remote, 5, Some("Fixing")), None);
        remote.ended = Some(EndReason::HostReboot);
        assert_eq!(
            row_title(&remote, 5, None).as_deref(),
            Some("exited (host rebooted)")
        );
    }
}
