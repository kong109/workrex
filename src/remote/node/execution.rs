//! Start a remote workspace in one Rex session with the configured block layout.
//! The node owns the session; Rex.app displays it directly over the tailnet.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::host::{NewSession, SessionHost};
use super::{
    Execution, ExecutionState, NODE_ROOT_ENV, NodeRoot, PaneProgress, refuse, secrets,
    stop_containers_mounting, validate_id,
};
use crate::config::{Config, PaneConfig, RemoteIsolation};
use crate::multiplexer::PaneSetupOptions;
use crate::multiplexer::types::ResumeMode;
use crate::remote::protocol::{StartRequest, StartResponse, code, session_name};
use crate::shell::shell_quote;

/// The panes of a workspace that get a session, with their configured
/// index: the agent is placed as `setup_panes` would, a pane without a split
/// direction after the first is skipped, and no panes means one shell.
pub fn pane_plan(config: &Config, agent: Option<&str>) -> Vec<(usize, PaneConfig)> {
    let configured = config.panes.clone().unwrap_or_default();
    let resolved = crate::workflow::resolve_pane_configuration(&configured, config, agent);
    if resolved.is_empty() {
        return vec![(0, PaneConfig::default())];
    }
    resolved
        .into_iter()
        .enumerate()
        .filter(|(index, pane)| *index == 0 || pane.split.is_some())
        .collect()
}

/// `run/agents/<pane>` under a node root: present, holding the time it was
/// written, while a supervisor's container runtime runs in that pane of the
/// node Rex server and reads its input. Removed when the runtime exits.
pub struct AgentMarker(Option<PathBuf>);

impl AgentMarker {
    fn path(root: &NodeRoot, pane_id: &str) -> Option<PathBuf> {
        let number = pane_id.strip_prefix("block:")?;
        (!number.is_empty()
            && number
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        .then(|| root.path.join("run").join("agents").join(number))
    }

    /// Write the marker of `pane_id`; failures only lose the marker.
    pub fn write(root: &NodeRoot, pane_id: &str) -> Self {
        let Some(path) = Self::path(root, pane_id) else {
            return Self(None);
        };
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .map_err(anyhow::Error::from)
            .and_then(|()| {
                crate::util::write_atomic(&path, crate::remote::cache::now().to_string().as_bytes())
            });
        match written {
            Ok(()) => Self(Some(path)),
            Err(err) => {
                tracing::warn!("failed to write {}: {err:#}", path.display());
                Self(None)
            }
        }
    }

    /// When the marker of `pane_id` was written (node clock), if it exists.
    pub fn read(root: &NodeRoot, pane_id: &str) -> Option<u64> {
        fs::read_to_string(Self::path(root, pane_id)?)
            .ok()?
            .trim()
            .parse()
            .ok()
    }

    /// Remove the markers of panes gone from the node Rex server: a
    /// supervisor killed with its session leaves its marker behind.
    pub fn sweep(root: &NodeRoot) {
        use crate::multiplexer::Multiplexer;
        Self::sweep_with(root, || {
            crate::multiplexer::rex::RexBackend::new()
                .get_all_live_pane_info()
                .ok()
                .map(|live| live.into_keys().collect())
        });
    }

    fn sweep_with(root: &NodeRoot, read: impl FnOnce() -> Option<HashSet<String>>) {
        let dir = root.path.join("run").join("agents");
        let Ok(entries) = fs::read_dir(&dir) else {
            return;
        };
        let markers: Vec<_> = entries.flatten().collect();
        let Some(live) = read() else {
            return;
        };
        for entry in markers {
            let block = format!("block:{}", entry.file_name().to_string_lossy());
            if !live.contains(&block) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

impl Drop for AgentMarker {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

/// Index of the first pane of [`pane_plan`] that runs the agent.
pub fn agent_pane(config: &Config, agent: Option<&str>, working_dir: &Path) -> Option<usize> {
    pane_plan(config, agent)
        .into_iter()
        .find_map(|(index, pane)| {
            crate::multiplexer::util::resolve_pane_command_with_config(
                pane.command.as_deref(),
                true,
                None,
                working_dir,
                config,
                agent,
                "sh",
            )
            .is_some_and(|resolved| resolved.selected_agent.is_some())
            .then_some(index)
        })
}

/// [`pane_plan`] as the laptop records it to lay out the window.
pub fn layout(config: &Config, agent: Option<&str>) -> Vec<crate::remote::cache::LayoutPane> {
    pane_plan(config, agent)
        .into_iter()
        .map(|(index, pane)| crate::remote::cache::LayoutPane { index, pane })
        .collect()
}

pub fn start(root: &NodeRoot, request: StartRequest) -> Result<StartResponse> {
    validate_id(&request.ws_id, 'w', "workspace id")?;
    validate_id(&request.ex_id, 'x', "execution id")?;
    let _lock = root.lock_workspace(&request.ws_id)?;
    let mut workspace = root.load(&request.ws_id)?.ok_or_else(|| {
        refuse(
            code::NOT_FOUND,
            format!("workspace {} does not exist on the node", request.ws_id),
        )
    })?;
    if !workspace.progress.ready {
        return Err(refuse(
            code::FAILED,
            format!("workspace {} is not provisioned yet", request.ws_id),
        ));
    }
    let boot_id = super::boot_id();
    let resume = match &workspace.execution {
        Some(execution) if execution.state == ExecutionState::Running => {
            // A live execution keeps the host it started on.
            let live = match SessionHost::for_execution(root, execution) {
                Ok(host) => execution
                    .panes
                    .iter()
                    .any(|pane| host.pane_alive(execution, pane.index)),
                Err(_) if execution.rex.is_some() => false,
                Err(err) => return Err(err),
            };
            if live {
                return Ok(StartResponse {
                    ex_id: execution.ex_id.clone(),
                    panes: execution.panes.iter().map(|pane| pane.index).collect(),
                    reused: true,
                    after_reboot: false,
                    continued: false,
                    rex: execution.rex.clone(),
                });
            }
            false
        }
        Some(execution) => execution.ex_id == request.ex_id,
        None => false,
    };
    let host = match &workspace.execution {
        Some(execution) if resume => SessionHost::for_execution(root, execution)?,
        _ => SessionHost::open_for_start(root, request.terminal)?,
    };
    let pin = std::env::current_exe().context("Failed to locate the node binary")?;
    if !resume {
        let mut continues = false;
        if let Some(previous) = workspace.execution.take() {
            // A replaced Rex server took the sessions with it, as a reboot does.
            let rex_replaced = previous
                .rex
                .as_ref()
                .zip(host.placement())
                .is_some_and(|(then, now)| then.instance_id != now.instance_id);
            continues = previous.ended_by_reboot(boot_id.as_deref()) || rex_replaced;
            super::host::stop_execution(root, &previous)?;
        }
        if workspace.isolation == RemoteIsolation::Container {
            stop_containers_mounting(&workspace.worktree_path)?;
        }
        workspace.execution = Some(Execution {
            ex_id: request.ex_id.clone(),
            state: ExecutionState::Starting,
            panes: Vec::new(),
            boot_id: boot_id.clone(),
            pin: pin
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
            continues,
            agent_kind: None,
            closed_shells: Default::default(),
            rex: host.placement(),
        });
        root.save(&workspace)?;
    }
    // An agent the host rebooted under picks up its conversation again.
    let resume_mode = match &workspace.execution {
        Some(execution) if execution.continues => ResumeMode::Continue,
        _ => ResumeMode::None,
    };

    let config_path = root.config_path(&request.ws_id);
    let (config, _) = crate::frozen_config::load(&config_path)?;
    let agent_kind = workspace.agent_kind(&config);
    workspace
        .execution
        .as_mut()
        .expect("execution recorded")
        .agent_kind = agent_kind.map(str::to_string);
    let plan = pane_plan(&config, workspace.agent.as_deref());

    super::trust::ensure_for_execution(root, &workspace, &config);

    let supervisor = format!(
        "{} sandbox run --node-root {} --node-workspace {}",
        shell_quote(&pin.to_string_lossy()),
        shell_quote(&root.path.to_string_lossy()),
        request.ws_id
    );
    // Without a container, agent hooks and commands typed in any pane run
    // the node's `workrex`, which must use the node store.
    let status_bin = match workspace.isolation {
        RemoteIsolation::None => Some(write_status_wrapper(root, &pin)?),
        RemoteIsolation::Container => None,
    };
    let prompt = workspace
        .prompt_path
        .clone()
        .filter(|_| request.use_prompt && !workspace.prompt_file_only);
    let working_dir = workspace.working_dir().to_path_buf();
    let worktree = workspace.worktree_path.clone();
    // Containers get the workspace's secrets from their supervisor; panes
    // without one get them in their session's environment.
    let session_secrets = match workspace.isolation {
        RemoteIsolation::None => secrets::for_execution(root, &request.ws_id, agent_kind)?,
        RemoteIsolation::Container => Default::default(),
    };

    let execution = workspace.execution.as_ref().expect("execution recorded");
    host.kill_execution(root, execution);
    workspace
        .execution
        .as_mut()
        .expect("execution recorded")
        .rex = host.placement();
    let execution = workspace.execution.as_ref().expect("execution recorded");
    let name = session_name(&execution.ex_id, plan[0].0);
    let spawn_env = (workspace.isolation == RemoteIsolation::None)
        .then_some((root.path.as_path(), request.ws_id.as_str()));
    let spec = NewSession {
        name: &name,
        window_label: &workspace.identity.handle,
        cwd: &working_dir,
        config_path: &config_path,
        spawn_env,
        secrets: &session_secrets,
    };
    let created = host.create(root, &spec)?;
    let session = created.rex_session.clone().expect("native session created");
    let environment = super::host::block_env(&spec, std::env::vars());
    let shell = host.mux().get_default_shell()?;
    let SessionHost::Rex(rex) = &host;
    let mut environment_files = Vec::new();
    for (ordinal, (index, pane)) in plan.iter().enumerate() {
        let respawns = ordinal > 0
            || crate::multiplexer::util::resolve_pane_command_with_config(
                pane.command.as_deref(),
                request.run_commands,
                prompt.as_deref(),
                &working_dir,
                &config,
                workspace.agent.as_deref(),
                &shell,
            )
            .is_some();
        if respawns {
            let file = secrets::export_file(root, &format!("{name}.{index}.agent"), &environment)?;
            rex.mux.source_on_respawn(file.to_path_buf());
            environment_files.push(file);
        }
    }
    let execution = workspace.execution.as_mut().expect("execution recorded");
    let placement = execution.rex.as_mut().expect("Rex placement recorded");
    placement.sessions = plan
        .iter()
        .map(|(index, _)| (*index, session.clone()))
        .collect();
    root.save(&workspace)?;
    created.wait_ready()?;
    let panes: Vec<PaneConfig> = plan.iter().map(|(_, pane)| pane.clone()).collect();
    let setup = host
        .mux()
        .setup_panes(
            &created.pane_id,
            &panes,
            &working_dir,
            PaneSetupOptions {
                run_commands: request.run_commands,
                prompt_file_path: prompt.as_deref(),
                worktree_root: Some(&worktree),
                lima_vm_name: None,
                resume_mode: resume_mode.clone(),
                sandbox_supervisor: Some(&supervisor),
                path_prefix: status_bin.as_deref(),
            },
            &config,
            workspace.agent.as_deref(),
        )
        .context("Failed to start remote Rex layout")?;
    let execution = workspace.execution.as_mut().expect("execution recorded");
    execution.rex.as_mut().expect("Rex placement").blocks = plan
        .iter()
        .zip(&setup.pane_ids)
        .map(|((index, _), id)| (*index, id.clone()))
        .collect();
    execution.panes = plan
        .iter()
        .map(|(index, _)| PaneProgress { index: *index })
        .collect();
    root.save(&workspace)?;
    host.mux().select_pane(&setup.focus_pane_id)?;
    if let Some(zoom) = setup.zoom_pane_id {
        host.mux().zoom_pane(&zoom)?;
    }
    for file in &environment_files {
        secrets::wait_consumed(file)?;
    }
    drop(environment_files);
    drop(created);

    let continue_flag = crate::multiplexer::agent::resolve_profile(agent_kind)
        .continue_flag()
        .is_some();
    let execution = workspace.execution.as_mut().expect("execution recorded");
    execution.state = ExecutionState::Running;
    execution.panes.sort_by_key(|pane| pane.index);
    let response = StartResponse {
        ex_id: execution.ex_id.clone(),
        panes: execution.panes.iter().map(|pane| pane.index).collect(),
        reused: false,
        after_reboot: execution.continues,
        continued: execution.continues && request.run_commands && continue_flag,
        rex: execution.rex.clone(),
    };
    root.save(&workspace)?;
    Ok(response)
}

/// Write `run/bin/workrex`, which runs `pin` against the node store, and
/// return its directory.
pub(super) fn write_status_wrapper(root: &NodeRoot, pin: &Path) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let dir = root.path.join("run").join("bin");
    fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let script = format!(
        "#!/bin/sh\n# Written by workrex.\nexport XDG_STATE_HOME={}\nexec {} \"$@\"\n",
        shell_quote(&root.path.join("state").to_string_lossy()),
        shell_quote(&pin.to_string_lossy())
    );
    let path = dir.join(crate::bin_name::BIN);
    if fs::read_to_string(&path).ok().as_deref() != Some(script.as_str()) {
        crate::util::write_atomic(&path, script.as_bytes())?;
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("Failed to make {} executable", path.display()))?;
    Ok(dir)
}

/// Node variables a session does not inherit: those that steer node verbs,
/// and the node's own Rex client.
pub(super) const SESSION_EXCLUDED_ENV: [&str; 5] = [
    crate::frozen_config::FROZEN_CONFIG_ENV,
    super::spawn::SESSION_ROOT_ENV,
    super::spawn::SESSION_WORKSPACE_ENV,
    NODE_ROOT_ENV,
    "XDG_STATE_HOME",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SplitDirection;

    #[test]
    fn plan_skips_unsplit_panes_and_defaults_to_one_shell() {
        let config = Config::default();
        assert_eq!(pane_plan(&config, None).len(), 1);

        let config = Config {
            panes: Some(vec![
                PaneConfig {
                    command: Some("<agent>".into()),
                    ..Default::default()
                },
                PaneConfig::default(),
                PaneConfig {
                    split: Some(SplitDirection::Horizontal),
                    ..Default::default()
                },
            ]),
            ..Config::default()
        };
        let indices: Vec<usize> = pane_plan(&config, None).iter().map(|(i, _)| *i).collect();
        assert_eq!(indices, [0, 2]);
    }

    #[test]
    fn marker_sweep_keeps_markers_created_after_its_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let root = NodeRoot {
            path: dir.path().to_path_buf(),
        };
        let old = AgentMarker::write(&root, "block:old");
        let mut new = None;
        AgentMarker::sweep_with(&root, || {
            new = Some(AgentMarker::write(&root, "block:new"));
            Some(HashSet::new())
        });
        assert_eq!(AgentMarker::read(&root, "block:old"), None);
        assert!(AgentMarker::read(&root, "block:new").is_some());
        drop((old, new));
    }

    #[test]
    fn marker_sweep_keeps_every_marker_when_rex_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let root = NodeRoot {
            path: dir.path().to_path_buf(),
        };
        let marker = AgentMarker::write(&root, "block:live");
        AgentMarker::sweep_with(&root, || None);
        assert!(AgentMarker::read(&root, "block:live").is_some());
        drop(marker);
    }

    #[test]
    fn an_agent_marker_lasts_while_it_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let root = NodeRoot {
            path: dir.path().to_path_buf(),
        };
        let marker = AgentMarker::write(&root, "block:test-12");
        assert!(AgentMarker::read(&root, "block:test-12").is_some_and(|at| at > 0));
        assert_eq!(AgentMarker::read(&root, "block:test-13"), None);
        drop(marker);
        assert_eq!(AgentMarker::read(&root, "block:test-12"), None);
        // Only Rex pane ids name a file.
        drop(AgentMarker::write(&root, "../x"));
        assert!(!dir.path().join("run/x").exists());
    }

    #[test]
    fn the_agent_pane_is_the_first_that_runs_the_agent() {
        let dir = std::env::temp_dir();
        let pane = |command: &str, split| PaneConfig {
            command: Some(command.into()),
            split,
            ..Default::default()
        };
        let config = Config {
            agent: Some("codex".into()),
            panes: Some(vec![
                pane("bash", None),
                pane("<agent>", Some(SplitDirection::Horizontal)),
                pane("<agent>", Some(SplitDirection::Vertical)),
            ]),
            ..Config::default()
        };
        assert_eq!(agent_pane(&config, None, &dir), Some(1));
        let config = Config {
            panes: Some(vec![pane("htop", None)]),
            ..config
        };
        assert_eq!(agent_pane(&config, None, &dir), None);
    }
}
