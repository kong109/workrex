//! `node spawn`: a remote agent's `workrex add`, served on its node.
//!
//! The new workspace is a sibling of the agent's own: same project,
//! isolation, agent, config and environment snapshot, with a branch that
//! starts at the tip of the parent's branch. Everything but the branch
//! name, prompt and background flag comes from the parent's records, never
//! from the agent, and no hooks run. The laptop learns about it from `node
//! watch` and adopts it when it next looks (see `remote::siblings`).

use crate::bin_name::BIN;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use super::provision::{self, FileRecipe};
use super::{NodeRoot, Origin, base_ref, git, refuse, validate_id, validate_identity};
use crate::remote::cache;
use crate::remote::protocol::{
    ProvisionPayload, RemoveRequest, StartRequest, WorkspaceIdentity, code,
};

/// Set in the sessions of executions without a sandbox, where the agent's
/// `workrex add` reaches the node directly instead of through a supervisor.
pub const SESSION_ROOT_ENV: &str = "WORKREX_NODE_ROOT";
pub const SESSION_WORKSPACE_ENV: &str = "WORKREX_NODE_WORKSPACE";

pub fn session_parent() -> Option<(PathBuf, String)> {
    parent_from(
        std::env::var_os(SESSION_ROOT_ENV),
        std::env::var_os(SESSION_WORKSPACE_ENV),
        || std::env::current_dir().ok(),
    )
}

fn parent_from(
    root: Option<OsString>,
    ws_id: Option<OsString>,
    dir: impl FnOnce() -> Option<PathBuf>,
) -> Option<(PathBuf, String)> {
    match (root, ws_id) {
        (Some(root), Some(ws_id)) => Some((root.into(), ws_id.into_string().ok()?)),
        (None, None) => session_parent_from_dir(&dir()?),
        _ => None,
    }
}

fn session_parent_from_dir(dir: &Path) -> Option<(PathBuf, String)> {
    let dir = dir.canonicalize().ok()?;
    let root = dir
        .ancestors()
        .find(|ancestor| ancestor.join("state").join("workspaces").is_dir())?;
    let (_, ws_id) = NodeRoot::at(root.to_owned())
        .ok()?
        .workspaces()
        .into_iter()
        .filter_map(|ws| {
            let worktree = ws.worktree_path.canonicalize().ok()?;
            dir.starts_with(&worktree)
                .then(|| (worktree.components().count(), ws.identity.ws_id))
        })
        .max_by_key(|(depth, _)| *depth)?;
    Some((root.to_owned(), ws_id))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// Workspace of the agent asking; supplied by its supervisor.
    pub parent: String,
    pub branch: Option<String>,
    pub prompt: Option<String>,
    #[serde(default)]
    pub background: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResponse {
    pub ws_id: String,
    pub handle: String,
    pub branch: String,
}

pub fn spawn(root: &NodeRoot, request: SpawnRequest) -> Result<SpawnResponse> {
    validate_id(&request.parent, 'w', "workspace id")?;
    let parent = root.load(&request.parent)?.ok_or_else(|| {
        refuse(
            code::NOT_FOUND,
            format!("workspace {} does not exist on the node", request.parent),
        )
    })?;
    if !parent.progress.ready {
        return Err(refuse(
            code::FAILED,
            format!("workspace {} is not provisioned yet", request.parent),
        ));
    }
    let branch = request
        .branch
        .as_deref()
        .map(str::trim)
        .filter(|branch| !branch.is_empty())
        .ok_or_else(|| {
            refuse(
                code::BAD_REQUEST,
                "remote agents must name the branch: `workrex add <branch> -b -P <prompt-file>`",
            )
        })?
        .to_string();

    let (config, location) = crate::frozen_config::load(&root.config_path(&parent.identity.ws_id))?;
    let handle = crate::naming::derive_handle(&branch, None, &config)
        .map_err(|err| refuse(code::BAD_REQUEST, format!("{err:#}")))?;
    let repo = &parent.repo_path;
    let branch_ref = format!("refs/heads/{branch}");
    if git(repo, &["rev-parse", "--verify", "-q", &branch_ref]).is_ok() {
        return Err(refuse(
            code::CONFLICT,
            format!("branch '{branch}' already exists on the node; pick another name"),
        ));
    }
    let identity = WorkspaceIdentity {
        ws_id: cache::new_id('w'),
        project_id: parent.identity.project_id.clone(),
        handle,
        branch,
        base_sha: parent_tip(&parent)?,
    };
    validate_identity(&identity)?;

    let ws_id = identity.ws_id.clone();
    let lock = root.lock_workspace(&ws_id)?;
    let mut workspace = provision::claim(root, &identity, parent.isolation)?;
    let provisioned = (|| -> Result<()> {
        let location = location.map(|location| {
            crate::remote::portable::rebase_location(&location, &workspace.worktree_path)
        });
        let layout = super::execution::layout(&config, parent.agent.as_deref());
        workspace.origin = Some(Origin {
            parent: parent.identity.ws_id.clone(),
            base_label: parent.identity.branch.clone(),
            background: request.background,
            spawned_at: cache::now(),
            layout,
        });
        root.save(&workspace)?;
        git(repo, &["update-ref", &base_ref(&ws_id), &identity.base_sha])?;
        let recipe = provision::load_recipe(root, &parent.identity.ws_id)?;
        let run_file_ops = recipe.is_some();
        let (recipe, _stage) = recipe
            .map(|(recipe, stage)| (recipe, Some(stage)))
            .unwrap_or_default();
        let FileRecipe {
            copies,
            symlink_patterns,
            ignored,
        } = recipe;
        let payload = ProvisionPayload {
            cached_seeds: Vec::new(),
            config: crate::frozen_config::snapshot_document(&config, location.as_ref())?,
            agent: parent.agent.clone(),
            prompt: request.prompt.clone().filter(|prompt| !prompt.is_empty()),
            prompt_file_only: config.prompt_file_only.unwrap_or(false),
            copies,
            seeds: Vec::new(),
            symlink_patterns,
            ignored,
            run_file_ops,
            run_hooks: false,
            git_user_name: None,
            git_user_email: None,
            secrets: super::secrets::workspace(root, &parent.identity.ws_id)?,
        };
        provision::provision_worktree(root, &mut workspace, payload)
    })();
    drop(lock);
    let started = provisioned.and_then(|()| {
        super::execution::start(
            root,
            StartRequest {
                ws_id: ws_id.clone(),
                ex_id: cache::new_id('x'),
                use_prompt: true,
                run_commands: true,
                // A sibling's terminals live where its parent's do.
                terminal: match parent.execution.as_ref().and_then(|ex| ex.rex.as_ref()) {
                    Some(_) => crate::config::RemoteTerminal::Rex,
                    None => crate::config::RemoteTerminal::Rex,
                },
                session_group: parent.session_group.clone(),
            },
        )
    });
    if let Err(err) = started {
        let cleanup = super::remove::remove(
            root,
            RemoveRequest {
                ws_id: ws_id.clone(),
                expect: None,
                keep_branch: false,
                run_hooks: false,
            },
        );
        if let Err(cleanup) = cleanup {
            eprintln!("{BIN}: could not clean up workspace {ws_id}: {cleanup:#}");
        }
        return Err(err);
    }
    Ok(SpawnResponse {
        ws_id,
        handle: identity.handle,
        branch: identity.branch,
    })
}

/// Tip of the parent's branch in the node repository, read without the
/// agent's hooks or config: the agent can write refs and objects there.
/// Falls back to the parent's base.
fn parent_tip(parent: &super::Workspace) -> Result<String> {
    let spec = format!("refs/heads/{}^{{commit}}", parent.identity.branch);
    let mut cmd = crate::git::pinned_git(&parent.repo_path)
        .context("Refusing to read the parent's repository")?;
    let output = cmd
        .args(["rev-parse", "--verify", "-q", &spec])
        .output()
        .context("Failed to run git")?;
    let tip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if output.status.success() && !tip.is_empty() {
        return Ok(tip);
    }
    Ok(parent.identity.base_sha.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RemoteIsolation;

    fn node_with_workspace(handle: &str) -> (tempfile::TempDir, NodeRoot, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = NodeRoot::at(dir.path().canonicalize().unwrap()).unwrap();
        let identity = WorkspaceIdentity {
            ws_id: cache::new_id('w'),
            project_id: "project".into(),
            handle: handle.into(),
            branch: handle.into(),
            base_sha: "0".repeat(40),
        };
        let ws = super::super::Workspace::new(&root, identity, RemoteIsolation::None);
        std::fs::create_dir_all(ws.worktree_path.join("src/deep")).unwrap();
        root.save(&ws).unwrap();
        let ws_id = ws.identity.ws_id.clone();
        (dir, root, ws_id)
    }

    #[test]
    fn finds_the_workspace_from_its_worktree_or_any_subdirectory() {
        let (_dir, root, ws_id) = node_with_workspace("mine");
        let worktree = root.worktree_path("project", "mine");
        let expected = Some((root.path.clone(), ws_id));
        assert_eq!(session_parent_from_dir(&worktree), expected);
        assert_eq!(
            session_parent_from_dir(&worktree.join("src/deep")),
            expected
        );
    }

    #[test]
    fn finds_the_workspace_through_a_symlink() {
        let (_dir, root, ws_id) = node_with_workspace("mine");
        let link_dir = tempfile::tempdir().unwrap();
        let link = link_dir.path().join("link");
        std::os::unix::fs::symlink(root.worktree_path("project", "mine"), &link).unwrap();
        assert_eq!(
            session_parent_from_dir(&link.join("src")),
            Some((root.path.clone(), ws_id))
        );
    }

    #[test]
    fn a_partial_session_environment_never_falls_back_to_the_directory() {
        let (_dir, root, _) = node_with_workspace("mine");
        let worktree = root.worktree_path("project", "mine");
        let root_only = parent_from(Some(root.path.clone().into()), None, || {
            Some(worktree.clone())
        });
        let ws_only = parent_from(None, Some("w1".into()), || Some(worktree.clone()));
        assert_eq!(root_only, None);
        assert_eq!(ws_only, None);
    }

    #[test]
    fn ignores_directories_outside_node_workspaces() {
        let (_dir, root, _) = node_with_workspace("mine");
        let local = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.repo_path("project")).unwrap();
        assert_eq!(session_parent_from_dir(local.path()), None);
        assert_eq!(session_parent_from_dir(&root.repo_path("project")), None);
        assert_eq!(session_parent_from_dir(&root.path), None);
    }
}
