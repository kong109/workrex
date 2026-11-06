use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use tracing::info;

use crate::config::{self, MuxMode};
use crate::git;
use crate::multiplexer::Multiplexer;
#[cfg(test)]
use crate::state::PaneKey;
use crate::state::store::RecoveryAgentChoice;
use crate::state::{AgentState, AgentStateSource, ResurrectionAgentState, StateStore};
use crate::util::canon_or_self;

#[derive(Debug)]
pub enum ResurrectAction {
    Restore,
    SkipAlreadyOpen,
    SkipMain,
    SkipUnavailable(String),
}

#[derive(Debug)]
pub struct ResurrectCandidate {
    pub handle: String,
    pub action: ResurrectAction,
    pub stale_sources: Vec<AgentStateSource>,
    pub mode: MuxMode,
    pub agent: Option<String>,
}

/// A cached remote workspace whose native Rex target can be checked.
#[derive(Debug)]
pub struct RemoteResurrectCandidate {
    pub record: crate::remote::cache::WorkspaceRecord,
    pub action: ResurrectAction,
    pub stale_sources: Vec<AgentStateSource>,
}

pub struct ResurrectPlan {
    pub candidates: Vec<ResurrectCandidate>,
    pub remote: Vec<RemoteResurrectCandidate>,
    /// Stale local sources of remote workspaces which no longer
    /// exist; nothing restores them.
    pub forgotten_remote_sources: Vec<AgentStateSource>,
    pub unmatched_states: usize,
}

type ResurrectHandleState = (MuxMode, Option<RecoveryAgentChoice>, Vec<AgentStateSource>);

fn update_selected_agent(selected: &mut Option<RecoveryAgentChoice>, agent: &AgentState) {
    let Some(candidate) = RecoveryAgentChoice::from_state(agent) else {
        return;
    };
    if selected
        .as_ref()
        .is_none_or(|current| candidate.preferred_over(current))
    {
        *selected = Some(candidate);
    }
}

/// Build a plan of what to restore based on stale agent state files.
///
/// Loads raw (non-reconciled) agent states and cross-references them against
/// existing git worktrees and live multiplexer state to determine which
/// worktrees need restoration.
pub fn plan(store: &StateStore, mux: &dyn Multiplexer) -> Result<ResurrectPlan> {
    let backend = mux.name();
    let instance = mux.instance_id();
    let relevant = store.resurrection_snapshot(backend, &instance)?;

    info!(
        relevant_count = relevant.len(),
        backend, instance, "resurrect:plan loading agent state"
    );

    // Get worktrees for current repo
    let worktrees = git::list_worktrees()?;
    let main_root = git::get_main_worktree_root()?;
    let canon_main = canon_or_self(&main_root);

    // Build canonical worktree map: (canon_path, handle)
    let wt_map: Vec<(PathBuf, String)> = worktrees
        .iter()
        .map(|(path, _branch)| {
            let handle = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            (canon_or_self(path), handle)
        })
        .collect();

    // Get live mux state for skip detection
    let mux_windows = mux.get_all_window_names()?;
    let mux_sessions = mux.get_all_session_names()?;
    let config = config::Config::load(None)?;
    let prefix = config.window_prefix();

    // Use config default mode as fallback for worktrees with no stored mode,
    // matching the resolution logic in workflow::open
    let default_mode = config.mode();

    // Group agent states by matched worktree handle
    let mut by_handle: HashMap<String, ResurrectHandleState> = HashMap::new();
    let mut by_workspace: HashMap<(String, String), Vec<AgentStateSource>> = HashMap::new();
    let mut unmatched_states = 0usize;

    for stored in relevant {
        let ResurrectionAgentState {
            state: agent,
            source,
            represented_count,
        } = stored;
        // Remote state belongs to the cached native execution, not a local worktree.
        if let Some(at) = &agent.remote {
            by_workspace
                .entry((at.remote.clone(), at.ws_id.clone()))
                .or_default()
                .push(source);
            continue;
        }
        let canon_agent = canon_or_self(&agent.workdir);

        // Find matching worktree using descendant path matching
        // (agent workdir may be a subdirectory of the worktree root)
        let matched = wt_map
            .iter()
            .find(|(canon_wt, _)| canon_agent == *canon_wt || canon_agent.starts_with(canon_wt));

        match matched {
            Some((_canon_wt, handle)) if !git::get_worktree_attachment(handle).manages_mux() => {
                unmatched_states += 1;
            }
            Some((_canon_wt, handle)) => {
                info!(
                    pane_id = %agent.pane_key.pane_id,
                    workdir = %agent.workdir.display(),
                    handle,
                    boot_id = ?agent.boot_id,
                    status = ?agent.status,
                    "resurrect:plan matched agent to worktree"
                );
                let mode = git::get_worktree_mode_opt(handle).unwrap_or(default_mode);
                let entry = by_handle
                    .entry(handle.clone())
                    .or_insert_with(|| (mode, None, Vec::new()));
                update_selected_agent(&mut entry.1, &agent);
                entry.2.push(source);
            }
            None => {
                info!(
                    pane_id = %agent.pane_key.pane_id,
                    workdir = %agent.workdir.display(),
                    "resurrect:plan no matching worktree (other project or removed)"
                );
                unmatched_states = unmatched_states.saturating_add(represented_count);
            }
        }
    }

    // Determine action per handle
    let mut candidates = Vec::new();
    for (handle, (mode, agent, pane_keys)) in by_handle {
        let canon_wt = wt_map
            .iter()
            .find(|(_, h)| *h == handle)
            .map(|(p, _)| p.clone())
            .unwrap_or_default();

        let action = if canon_wt == canon_main {
            ResurrectAction::SkipMain
        } else {
            let target_name = if mode == MuxMode::Session {
                git::get_worktree_target_session(&handle).unwrap_or_else(|| handle.clone())
            } else {
                git::get_worktree_target_window(&handle).unwrap_or_else(|| handle.clone())
            };
            let prefixed = crate::multiplexer::util::prefixed(prefix, &target_name);
            let is_open = if mode == MuxMode::Session {
                mux_sessions.contains(&prefixed)
            } else if mux.supports_window_ownership() {
                match git::get_worktree_window_token(&handle) {
                    Some(token) => !mux
                        .resolve_owned_window_targets(
                            &token,
                            &prefixed,
                            git::get_worktree_window_session(&handle).as_deref(),
                            &canon_wt,
                        )?
                        .is_empty(),
                    None => mux_windows.contains(&prefixed),
                }
            } else {
                mux_windows.contains(&prefixed)
            };
            if is_open {
                ResurrectAction::SkipAlreadyOpen
            } else {
                ResurrectAction::Restore
            }
        };

        let agent = agent.map(|choice| choice.command);

        info!(
            handle,
            action = ?action,
            mode = ?mode,
            agent = agent.as_deref(),
            pane_count = pane_keys.len(),
            "resurrect:plan determined action for handle"
        );

        candidates.push(ResurrectCandidate {
            handle,
            action,
            stale_sources: pane_keys,
            mode,
            agent,
        });
    }

    // Sort by handle for deterministic output
    candidates.sort_by(|a, b| a.handle.cmp(&b.handle));

    let (remote, forgotten_remote_sources) = plan_remote(by_workspace, &canon_main)?;

    Ok(ResurrectPlan {
        candidates,
        remote,
        forgotten_remote_sources,
        unmatched_states,
    })
}

/// Native executions in this repository, joined to any stale local state.
/// Native sessions need no local agent-state file to remain eligible.
fn plan_remote(
    mut by_workspace: HashMap<(String, String), Vec<AgentStateSource>>,
    canon_main: &std::path::Path,
) -> Result<(Vec<RemoteResurrectCandidate>, Vec<AgentStateSource>)> {
    let mut candidates = Vec::new();
    let mut forgotten = Vec::new();
    let mut unavailable = HashMap::<(String, String), String>::new();
    for record in crate::remote::cache::all() {
        let key = (record.remote.clone(), record.ws_id.clone());
        let sources = by_workspace.remove(&key).unwrap_or_default();
        if canon_or_self(&record.project_root) != canon_main {
            continue;
        }
        if record.execution.is_none() || record.cleanup_pending() {
            forgotten.extend(sources);
            continue;
        }
        let server = record.execution.as_ref().and_then(|execution| {
            execution
                .rex
                .as_ref()
                .map(|placement| (placement.host_label.clone(), placement.instance_id.clone()))
        });
        let known_error = server.as_ref().and_then(|server| unavailable.get(server));
        let action = match known_error {
            Some(error) => ResurrectAction::SkipUnavailable(error.clone()),
            None => match crate::remote::rex_jump::is_open(&record) {
                Ok(true) => ResurrectAction::SkipAlreadyOpen,
                Ok(false) => ResurrectAction::Restore,
                Err(error) => {
                    let server_unavailable =
                        error.is::<crate::remote::rex_jump::ServerUnavailable>();
                    let message = format!("{error:#}");
                    if server_unavailable && let Some(server) = server {
                        unavailable.insert(server, message.clone());
                    }
                    ResurrectAction::SkipUnavailable(message)
                }
            },
        };
        candidates.push(RemoteResurrectCandidate {
            record,
            action,
            stale_sources: sources,
        });
    }
    forgotten.extend(by_workspace.into_values().flatten());
    candidates.sort_by(|a, b| a.record.handle.cmp(&b.record.handle));
    Ok((candidates, forgotten))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use crate::remote::cache::{ExecutionRecord, WorkspaceRecord};
    use crate::remote::protocol::RexPlacement;
    use crate::test_support::{ISOLATED_TEST_CANARY, is_isolated_child, run_isolated_test};

    #[test]
    fn native_remote_plan_needs_no_local_state_and_preserves_unavailable_targets() {
        const NAME: &str = "workflow::resurrect::tests::native_remote_plan_needs_no_local_state_and_preserves_unavailable_targets";
        if !is_isolated_child(NAME) {
            let temp = tempfile::tempdir().unwrap();
            let bin = temp.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            run_isolated_test(
                NAME,
                temp.path(),
                &[("XDG_STATE_HOME", temp.path()), ("REX_BIN_DIR", &bin)],
            );
            return;
        }
        let root = std::env::current_dir().unwrap();
        let program = PathBuf::from(std::env::var_os("REX_BIN_DIR").unwrap()).join("rex");
        let mut record = WorkspaceRecord::new(
            "w-native".into(),
            "node",
            "project".into(),
            root.clone(),
            "feat",
            "feat",
            "base".into(),
            None,
            "feat".into(),
            root.join("feat"),
        );
        record.execution = Some(ExecutionRecord {
            ex_id: "e-native".into(),
            panes: vec![0],
            node_bin: "node".into(),
            rex: Some(RexPlacement {
                host_label: "native-server".into(),
                instance_id: "native-instance".into(),
                sessions: [(0, "session:owned".into())].into(),
                blocks: [(0, "block:owned".into())].into(),
                shells: Default::default(),
            }),
        });
        record.save().unwrap();
        let version: serde_json::Value = serde_json::from_str(include_str!(
            "../multiplexer/testdata/rex_server_version_mac.json"
        ))
        .unwrap();
        let process: serde_json::Value =
            serde_json::from_str(include_str!("../multiplexer/testdata/rex_process.json")).unwrap();
        let mut answer = serde_json::json!({
            "version": version,
            "status": {"instance_id": "native-instance"},
            "reads": {"session:owned": {
                "session": {"session_id": "session:owned", "label": "renamed"},
                "blocks": {"block:owned": {
                    "window": {"window_id": "window:owned", "label": "renamed"},
                    "process": process
                }}
            }}
        });
        let respond = |answer: &serde_json::Value| {
            std::fs::write(
                &program,
                format!("#!/bin/sh\ncat <<'JSON'\n{answer}\nJSON\n"),
            )
            .unwrap();
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        respond(&answer);
        let (live, forgotten) = plan_remote(HashMap::new(), &root).unwrap();
        assert!(forgotten.is_empty());
        assert_eq!(live.len(), 1);
        assert!(live[0].stale_sources.is_empty());
        assert!(matches!(live[0].action, ResurrectAction::SkipAlreadyOpen));

        answer["reads"]["session:owned"]["blocks"] = serde_json::json!({});
        respond(&answer);
        let (closed, _) = plan_remote(HashMap::new(), &root).unwrap();
        assert!(matches!(closed[0].action, ResurrectAction::Restore));

        std::fs::write(&program, "#!/bin/sh\necho offline >&2\nexit 1\n").unwrap();
        let sources = HashMap::from([(
            ("node".into(), "w-native".into()),
            vec![AgentStateSource::Recovery {
                backend: "rex".into(),
                instance: "local".into(),
                workdir: root.join("feat"),
                revision: 1,
            }],
        )]);
        let (unavailable, forgotten) = plan_remote(sources, &root).unwrap();
        assert!(forgotten.is_empty());
        assert_eq!(unavailable[0].stale_sources.len(), 1);
        assert!(matches!(&unavailable[0].action,
            ResurrectAction::SkipUnavailable(error) if error.contains("offline")));
        assert!(crate::remote::cache::all()[0].execution.is_some());
        println!("{ISOLATED_TEST_CANARY}");
    }

    fn test_agent_state(command: &str, agent_kind: Option<&str>, updated_ts: u64) -> AgentState {
        AgentState {
            pane_key: PaneKey {
                backend: "rex".to_string(),
                instance: "default".to_string(),
                pane_id: format!("%{updated_ts}"),
            },
            workdir: PathBuf::from("/repo/worktree"),
            status: None,
            status_ts: None,
            activity_ts: Some(updated_ts),
            pane_title: None,
            pane_pid: 12345,
            command: command.to_string(),
            updated_ts,
            window_name: None,
            session_name: None,
            boot_id: None,
            agent_kind: agent_kind.map(|kind| kind.to_string()),
            agent_session_id: None,
            prompt: None,
            remote: None,
        }
    }

    #[test]
    fn resurrect_agent_prefers_valid_agent_kind() {
        let state = test_agent_state("node", Some("codex"), 1);

        assert_eq!(
            RecoveryAgentChoice::from_state(&state).map(|choice| choice.command),
            Some("codex".to_string())
        );
    }

    #[test]
    fn resurrect_agent_uses_known_foreground_command() {
        let state = test_agent_state("codex --yolo", None, 1);

        assert_eq!(
            RecoveryAgentChoice::from_state(&state).map(|choice| choice.command),
            Some("codex".to_string())
        );
    }

    #[test]
    fn resurrect_agent_ignores_unknown_foreground_command() {
        let state = test_agent_state("node", None, 1);

        assert_eq!(RecoveryAgentChoice::from_state(&state), None);
    }

    #[test]
    fn update_selected_agent_keeps_newest_valid_agent() {
        let older = test_agent_state("claude", None, 10);
        let newer_invalid = test_agent_state("node", None, 30);
        let newer = test_agent_state("codex", None, 20);
        let mut selected = None;

        update_selected_agent(&mut selected, &older);
        update_selected_agent(&mut selected, &newer_invalid);
        update_selected_agent(&mut selected, &newer);

        assert_eq!(
            selected.map(|choice| choice.command),
            Some("codex".to_string())
        );
    }

    #[test]
    fn update_selected_agent_breaks_ties_deterministically() {
        let command_state = test_agent_state("codex", None, 10);
        let kind_state = test_agent_state("node", Some("pi"), 10);
        let mut selected = None;

        update_selected_agent(&mut selected, &command_state);
        update_selected_agent(&mut selected, &kind_state);

        assert_eq!(
            selected.map(|choice| choice.command),
            Some("pi".to_string())
        );
    }
}
