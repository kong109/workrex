//! Folder-trust records for node worktrees, in the stores of the agent a
//! workspace runs (see `crate::agent_trust`). Claude also gets an entry for
//! `<root>/projects`, for older versions that honor a parent's entry.

use crate::agent_trust::{self, Store, with_canonical};
use crate::bin_name::BIN;

use anyhow::Result;

use super::{NodeRoot, Workspace};
use crate::config::{Config, RemoteIsolation};

/// Make sure the config the workspace's agent reads trusts the node's
/// worktrees, for agents that ask. A failure is reported and leaves the
/// file alone; the agent then shows the dialog.
pub fn ensure_for_execution(root: &NodeRoot, workspace: &Workspace, config: &Config) {
    let Some(store) = workspace.agent_kind(config).and_then(agent_trust::store) else {
        return;
    };
    if let Err(err) = ensure(root, workspace, config, store) {
        eprintln!("{BIN}: could not mark the node's worktrees as trusted: {err:#}");
    }
}

fn ensure(root: &NodeRoot, workspace: &Workspace, config: &Config, store: &Store) -> Result<()> {
    let mut dirs = store.dirs(
        &workspace.repo_path,
        &workspace.worktree_path,
        workspace.working_dir(),
    );
    if store.kind == "claude" {
        dirs.extend(with_canonical(&root.path.join("projects")));
    }
    match workspace.isolation {
        // A container sees Claude's sandbox config and the codex home
        // mounted from the host; the other agents' stores are not mounted.
        RemoteIsolation::Container => match store.kind {
            "claude" => match crate::sandbox::SandboxPaths::new() {
                Some(paths) => agent_trust::ensure_trusted(
                    &paths.config_file,
                    &dirs,
                    crate::sandbox::CLAUDE_ONBOARDING_JSON,
                ),
                None => Ok(()),
            },
            "codex" => match config.sandbox.resolved_agent_config_dir("codex") {
                Some(home) => store.trust(&home.join(store.file), &dirs),
                None => Ok(()),
            },
            _ => Ok(()),
        },
        RemoteIsolation::None => match host_path(root, workspace, config, store) {
            Some(path) => store.trust(&path, &dirs),
            None => Ok(()),
        },
    }
}

/// Before the node deletes the workspace's worktree, drop its entry from
/// the store of the workspace's agent, if that store is keyed by worktree.
/// A failure is reported and the removal goes on.
pub fn forget_for_removal(root: &NodeRoot, workspace: &Workspace, config: &Config) {
    let Some(store) = workspace.agent_kind(config).and_then(agent_trust::store) else {
        return;
    };
    if workspace.isolation == RemoteIsolation::Container {
        return;
    }
    let Some(path) = host_path(root, workspace, config, store) else {
        return;
    };
    if let Err(err) = store.forget(&path, &workspace.worktree_path) {
        eprintln!("{BIN}: could not drop the worktree's trust entry: {err:#}");
    }
}

/// Drop the entries for an orphaned node worktree from every store keyed by
/// worktree, at the paths the node's login environment names.
pub fn forget_orphan(worktree: &std::path::Path) {
    let login = super::login_env();
    agent_trust::forget_everywhere(worktree, |name| {
        login
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .or_else(|| std::env::var(name).ok())
    });
}

/// The store's file on the node host, as the workspace's agent sees it.
fn host_path(
    root: &NodeRoot,
    workspace: &Workspace,
    config: &Config,
    store: &Store,
) -> Option<std::path::PathBuf> {
    store.path(
        |name| host_env(root, workspace, config, store.kind, name),
        workspace.working_dir(),
    )
}

/// `name` in the environment of the workspace's agent on the node host:
/// from the agent's `env` (which the command line sets last), its inline
/// assignments, the workspace's secrets, the login environment or this
/// process. An `env` entry taken `from_env` reads that variable instead.
fn host_env(
    root: &NodeRoot,
    workspace: &Workspace,
    config: &Config,
    kind: &str,
    name: &str,
) -> Option<String> {
    let command =
        crate::multiplexer::agent::resolve_selected_agent(config, workspace.agent.as_deref())
            .map(|agent| agent.command);
    agent_trust::agent_env(command.as_ref(), name, |name| {
        super::secrets::for_execution(root, &workspace.identity.ws_id, Some(kind))
            .ok()
            .and_then(|secrets| secrets.0.get(name).cloned())
            .or_else(|| {
                super::login_env()
                    .into_iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value)
            })
            .or_else(|| std::env::var(name).ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::Path;

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn claude_trusts_the_workspace_repository() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("claude-home");
        let root = NodeRoot {
            path: dir.path().join("node"),
        };
        let mut workspace = Workspace::new(
            &root,
            crate::remote::protocol::WorkspaceIdentity {
                ws_id: "w0123456789ab".into(),
                project_id: "app-0123456789ab".into(),
                handle: "feature".into(),
                branch: "feature".into(),
                base_sha: "a".repeat(40),
            },
            RemoteIsolation::None,
        );
        let path = home.join(".claude.json");
        let config: Config = serde_yaml::from_str(&format!(
            "agents:\n  cl:\n    command: claude\n    type: claude\n    env:\n      CLAUDE_CONFIG_DIR: {}\n",
            home.display()
        ))
        .unwrap();
        workspace.agent = Some("cl".into());
        ensure(
            &root,
            &workspace,
            &config,
            agent_trust::store("claude").unwrap(),
        )
        .unwrap();
        let value = read(&path);
        let repo = workspace.repo_path.to_string_lossy().into_owned();
        assert_eq!(
            value["projects"][repo.as_str()]["hasTrustDialogAccepted"],
            Value::Bool(true)
        );
    }

    #[test]
    fn host_env_prefers_the_agent_env_over_inline_assignments() {
        let dir = tempfile::tempdir().unwrap();
        let root = NodeRoot {
            path: dir.path().to_path_buf(),
        };
        let mut workspace = Workspace::new(
            &root,
            crate::remote::protocol::WorkspaceIdentity {
                ws_id: "w0123456789ab".into(),
                project_id: "app-0123456789ab".into(),
                handle: "feature".into(),
                branch: "feature".into(),
                base_sha: "a".repeat(40),
            },
            RemoteIsolation::None,
        );
        workspace.agent = Some("cx".into());
        let config: Config = serde_yaml::from_str(
            "agents:\n  cx:\n    command: CODEX_HOME=/srv/codex-a codex\n    type: codex\n    env:\n      CODEX_HOME: /srv/codex-b\n",
        )
        .unwrap();
        assert_eq!(
            host_env(&root, &workspace, &config, "codex", "CODEX_HOME").as_deref(),
            Some("/srv/codex-b")
        );
        let config: Config = serde_yaml::from_str(
            "agents:\n  cx:\n    command: CODEX_HOME=/srv/codex-a codex\n    type: codex\n",
        )
        .unwrap();
        assert_eq!(
            host_env(&root, &workspace, &config, "codex", "CODEX_HOME").as_deref(),
            Some("/srv/codex-a")
        );
        let config: Config = serde_yaml::from_str(
            "agents:\n  cx:\n    command: CODEX_HOME=/srv/codex-a codex\n    type: codex\n    env:\n      CODEX_HOME:\n        from_env: HOME\n",
        )
        .unwrap();
        assert_eq!(
            host_env(&root, &workspace, &config, "codex", "CODEX_HOME"),
            std::env::var("HOME").ok()
        );
        let config: Config = serde_yaml::from_str(
            "agents:\n  cx:\n    command: CODEX_HOME=/srv/codex-a codex\n    type: codex\n    env:\n      CODEX_HOME:\n        from_env: WORKREX_TEST_UNSET_CODEX_HOME\n",
        )
        .unwrap();
        assert_eq!(
            host_env(&root, &workspace, &config, "codex", "CODEX_HOME"),
            None
        );
        let config: Config = serde_yaml::from_str(
            "agents:\n  cx:\n    command: CODEX_HOME=.codex-state codex\n    type: codex\n",
        )
        .unwrap();
        assert_eq!(
            host_path(
                &root,
                &workspace,
                &config,
                agent_trust::store("codex").unwrap()
            ),
            Some(workspace.working_dir().join(".codex-state/config.toml"))
        );
    }
}
