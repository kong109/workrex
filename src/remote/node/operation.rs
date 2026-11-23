//! Requests from a workspace for laptop-owned lifecycle operations.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{NodeRoot, validate_id, validate_identity};
use crate::remote::cache;

pub const TIMEOUT: u64 = 15;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    Remove {
        force: bool,
    },
    Merge {
        into: Option<String>,
        rebase: bool,
        squash: bool,
        ignore_uncommitted: bool,
        keep: bool,
        cleanup: bool,
        no_verify: bool,
        no_hooks: bool,
        #[serde(default)]
        notification: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub operation: Operation,
    pub requested_at: u64,
    pub claimed: bool,
    /// Made by a sandboxed agent through its supervisor. The container
    /// cannot write the node store, so it cannot clear this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sandboxed: bool,
    /// The laptop's stages so far, shown while the requester waits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<String>,
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub success: bool,
    pub message: String,
}

/// What a laptop worker learns when it claims a request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claim {
    pub operation: Operation,
    #[serde(default)]
    pub sandboxed: bool,
}

impl Request {
    pub fn pending(&self, now: u64) -> bool {
        !self.claimed && self.outcome.is_none() && now.saturating_sub(self.requested_at) < TIMEOUT
    }
}

pub fn in_session() -> bool {
    if crate::sandbox::guest::is_sandbox_guest() {
        return false;
    }
    std::env::var_os(super::spawn::SESSION_ROOT_ENV).is_some()
        || std::env::var_os(super::spawn::SESSION_WORKSPACE_ENV).is_some()
        || session_target().is_some()
}

fn session_target() -> Option<(PathBuf, String)> {
    session_target_from(
        std::env::var_os(super::spawn::SESSION_ROOT_ENV),
        std::env::var_os(super::spawn::SESSION_WORKSPACE_ENV),
        crate::sandbox::guest::is_sandbox_guest(),
        || std::env::current_dir().ok(),
        trusted_node_roots,
    )
}

fn session_target_from(
    root: Option<OsString>,
    ws_id: Option<OsString>,
    sandbox_guest: bool,
    dir: impl FnOnce() -> Option<PathBuf>,
    roots: impl FnOnce() -> Vec<PathBuf>,
) -> Option<(PathBuf, String)> {
    if sandbox_guest {
        return None;
    }
    match (root, ws_id) {
        (Some(root), Some(ws_id)) => Some((root.into(), ws_id.into_string().ok()?)),
        (None, None) => workspace_from_dir(&dir()?, &roots()),
        _ => None,
    }
}

fn trusted_node_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(pin) = super::path_shim::shim_path()
        .ok()
        .and_then(|shim| super::path_shim::installed_target(&shim))
    {
        let pin = PathBuf::from(pin);
        if let Some(bin_dir) = pin.parent()
            && bin_dir.file_name() == Some(std::ffi::OsStr::new("bin"))
            && let Some(root) = bin_dir.parent()
        {
            roots.push(root.to_path_buf());
        }
    }
    if let Some(home) = home::home_dir() {
        let default_root = home.join(crate::bin_name::NODE_ROOT_MARKER);
        if !roots.contains(&default_root) {
            roots.push(default_root);
        }
    }
    roots
}

fn workspace_from_dir(dir: &Path, roots: &[PathBuf]) -> Option<(PathBuf, String)> {
    let canonical_dir = dir.canonicalize().ok()?;
    let mut best: Option<(usize, PathBuf, String)> = None;
    for root in roots {
        let Ok(canonical_root) = root.canonicalize() else {
            continue;
        };
        if !canonical_dir.starts_with(&canonical_root) {
            continue;
        }
        let Ok(node_root) = NodeRoot::at(root.clone()) else {
            continue;
        };
        for ws in node_root.workspaces() {
            if validate_identity(&ws.identity).is_err() {
                continue;
            }
            let expected = node_root.worktree_path(&ws.identity.project_id, &ws.identity.handle);
            let Ok(canonical_expected) = expected.canonicalize() else {
                continue;
            };
            let Ok(canonical_recorded) = ws.worktree_path.canonicalize() else {
                continue;
            };
            if canonical_recorded != canonical_expected
                || !canonical_expected.starts_with(&canonical_root)
            {
                continue;
            }
            if canonical_dir.starts_with(&canonical_expected) {
                let depth = canonical_expected.components().count();
                if best.as_ref().is_none_or(|(d, _, _)| depth > *d) {
                    best = Some((depth, root.clone(), ws.identity.ws_id));
                }
            }
        }
    }
    best.map(|(_, root, ws_id)| (root, ws_id))
}

pub fn validate_names(names: &[String], handle: &str) -> Result<()> {
    if names.len() > 1
        || names
            .first()
            .is_some_and(|name| name != "." && name != handle)
    {
        bail!("a remote workspace may only request an operation on its own handle");
    }
    Ok(())
}

/// Request `operation` from a workspace pane and wait for the laptop,
/// showing its progress.
pub fn submit(names: &[String], operation: Operation) -> Result<()> {
    let (path, ws_id) = session_target().ok_or_else(|| {
        anyhow::anyhow!("incomplete remote workspace session; run this on the local machine")
    })?;
    let root = NodeRoot::at(path)?;
    let mut status = Status::new();
    let result = request(&root, &ws_id, names, operation, false, &mut |lines| {
        status.show(lines)
    });
    status.finish();
    if let Some(message) = result? {
        println!("{message}");
    }
    Ok(())
}

/// Ask the laptop to perform `operation` on workspace `ws_id`, and wait for
/// its outcome. `show` gets every line so far: the request, then the
/// laptop's stages. Returns the success message, or None when the
/// workspace went away with the operation.
pub fn request(
    root: &NodeRoot,
    ws_id: &str,
    names: &[String],
    operation: Operation,
    sandboxed: bool,
    show: &mut dyn FnMut(&[String]),
) -> Result<Option<String>> {
    validate_id(ws_id, 'w', "workspace id")?;
    let id = cache::new_id('r');
    let announce = {
        let _lock = root.lock_workspace(ws_id)?;
        let mut ws = root
            .load(ws_id)?
            .ok_or_else(|| anyhow::anyhow!("workspace no longer exists"))?;
        validate_names(names, &ws.identity.handle)?;
        if ws
            .operation
            .as_ref()
            .is_some_and(|r| r.outcome.is_none() && (r.claimed || r.pending(cache::now())))
        {
            bail!("a request to the local machine is already pending for this workspace");
        }
        let verb = match operation {
            Operation::Remove { .. } => "Removing",
            Operation::Merge { .. } => "Merging",
        };
        let announce = format!("{verb} '{}' on the local machine…", ws.identity.handle);
        ws.operation = Some(Request {
            id: id.clone(),
            operation,
            requested_at: cache::now(),
            claimed: false,
            sandboxed,
            progress: Vec::new(),
            outcome: None,
        });
        root.save(&ws)?;
        announce
    };
    let mut lines = vec![announce];
    show(&lines);
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let Some(ws) = root.load(ws_id)? else {
            return Ok(None);
        };
        let Some(request) = ws.operation.as_ref().filter(|r| r.id == id) else {
            bail!("the request to the local machine was withdrawn");
        };
        lines.truncate(1);
        lines.extend(request.progress.iter().cloned());
        show(&lines);
        if let Some(outcome) = &request.outcome {
            if outcome.success {
                return Ok(Some(outcome.message.clone()));
            }
            bail!("{}", outcome.message);
        }
        if !request.claimed && !request.pending(cache::now()) {
            // Claim and withdrawal serialize on the same lock and node clock.
            if let Ok(_lock) = root.lock_workspace(ws_id) {
                let Some(mut ws) = root.load(ws_id)? else {
                    return Ok(None);
                };
                if ws
                    .operation
                    .as_ref()
                    .is_some_and(|r| r.id == id && !r.claimed)
                {
                    ws.operation = None;
                    root.save(&ws)?;
                    bail!(
                        "the local machine isn't connected or runs an older workrex; request withdrawn"
                    );
                }
            }
        }
    }
}

/// The waiting requester's view of the laptop's progress. A terminal gets one
/// line per stage with a live elapsed time on the latest; other output gets
/// plain lines.
struct Status {
    seen: usize,
    current: Option<String>,
    since: Instant,
    shown: u64,
    live: bool,
}

impl Status {
    fn new() -> Self {
        use std::io::IsTerminal;
        Self {
            seen: 0,
            current: None,
            since: Instant::now(),
            shown: 0,
            live: std::io::stdout().is_terminal(),
        }
    }

    fn show(&mut self, stages: &[String]) {
        use std::io::Write;
        for stage in stages.iter().skip(self.seen) {
            self.finish();
            // The laptop is trusted, but a terminal must not interpret its text.
            let stage: String = stage.chars().filter(|c| !c.is_control()).collect();
            if self.live {
                print!("{stage}");
                self.current = Some(stage);
            } else {
                println!("{stage}");
            }
            self.since = Instant::now();
            self.shown = 0;
        }
        self.seen = self.seen.max(stages.len());
        let elapsed = self.since.elapsed().as_secs();
        if let Some(stage) = &self.current
            && elapsed > self.shown
        {
            self.shown = elapsed;
            print!("\r\x1b[K{stage} ({elapsed}s)");
        }
        let _ = std::io::stdout().flush();
    }

    fn finish(&mut self) {
        if self.current.take().is_some() {
            println!();
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Update {
    pub ws_id: String,
    pub id: String,
    /// Reports a stage of a claimed request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    /// Neither field claims the request; Some reports its outcome.
    pub outcome: Option<Outcome>,
}

pub fn update(root: &NodeRoot, update: Update) -> Result<Claim> {
    validate_id(&update.ws_id, 'w', "workspace id")?;
    validate_id(&update.id, 'r', "request id")?;
    // Claims and replies must survive brief contention with workspace verbs.
    let deadline = Instant::now() + Duration::from_secs(5);
    let _lock = loop {
        match root.lock_workspace(&update.ws_id) {
            Ok(lock) => break lock,
            Err(err)
                if crate::remote::client::error_code(&err)
                    == Some(crate::remote::protocol::code::BUSY)
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => return Err(err),
        }
    };
    let mut ws = root
        .load(&update.ws_id)?
        .ok_or_else(|| anyhow::anyhow!("workspace no longer exists"))?;
    let request = ws
        .operation
        .as_mut()
        .filter(|r| r.id == update.id)
        .ok_or_else(|| anyhow::anyhow!("request no longer exists"))?;
    if update.outcome.is_some() || update.progress.is_some() {
        if !request.claimed || request.outcome.is_some() {
            bail!("request is not running")
        }
        // A few stages per operation; the bound keeps the record small.
        if let Some(progress) = update.progress
            && request.progress.len() < 32
        {
            request.progress.push(progress);
        }
        if let Some(outcome) = update.outcome {
            request.outcome = Some(outcome);
        }
    } else {
        if !request.pending(cache::now()) {
            bail!("request expired or already claimed")
        }
        request.claimed = true;
    }
    let claim = Claim {
        operation: request.operation.clone(),
        sandboxed: request.sandboxed,
    };
    root.save(&ws)?;
    Ok(claim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_are_atomic_and_expired_requests_cannot_run() {
        let dir = tempfile::tempdir().unwrap();
        let root = NodeRoot::at(dir.path().to_owned()).unwrap();
        let identity = crate::remote::protocol::WorkspaceIdentity {
            ws_id: cache::new_id('w'),
            project_id: "project".into(),
            handle: "mine".into(),
            branch: "mine".into(),
            base_sha: "0".repeat(40),
        };
        let mut ws =
            super::super::Workspace::new(&root, identity, crate::config::RemoteIsolation::None);
        let id = cache::new_id('r');
        ws.operation = Some(Request {
            id: id.clone(),
            operation: Operation::Remove { force: false },
            requested_at: cache::now(),
            claimed: false,
            sandboxed: false,
            progress: Vec::new(),
            outcome: None,
        });
        root.save(&ws).unwrap();
        let claim = || Update {
            ws_id: ws.identity.ws_id.clone(),
            id: id.clone(),
            progress: None,
            outcome: None,
        };
        let mut progress = claim();
        progress.progress = Some("Running pre_merge hooks…".into());
        assert!(update(&root, progress).is_err());
        assert!(update(&root, claim()).is_ok());
        assert!(update(&root, claim()).is_err());
        let mut progress = claim();
        progress.progress = Some("Running pre_merge hooks…".into());
        update(&root, progress).unwrap();
        assert_eq!(
            root.load(&ws.identity.ws_id)
                .unwrap()
                .unwrap()
                .operation
                .unwrap()
                .progress,
            ["Running pre_merge hooks…"]
        );
        let mut complete = claim();
        complete.outcome = Some(Outcome {
            success: false,
            message: "refused".into(),
        });
        update(&root, complete).unwrap();
        assert!(update(&root, claim()).is_err());
        ws.operation.as_mut().unwrap().requested_at = cache::now() - TIMEOUT;
        root.save(&ws).unwrap();
        assert!(update(&root, claim()).is_err());
        ws.operation = None;
        root.save(&ws).unwrap();
        assert!(update(&root, claim()).is_err());
    }

    #[test]
    fn own_handle_only() {
        for names in [vec![], vec![".".into()], vec!["mine".into()]] {
            assert!(validate_names(&names, "mine").is_ok());
        }
        for names in [vec!["other".into()], vec!["mine".into(), "mine".into()]] {
            assert!(validate_names(&names, "mine").is_err());
        }
    }

    #[test]
    fn expiry_and_claim() {
        let mut request = Request {
            id: cache::new_id('r'),
            operation: Operation::Remove { force: false },
            requested_at: 100,
            claimed: false,
            sandboxed: false,
            progress: Vec::new(),
            outcome: None,
        };
        assert!(request.pending(114));
        assert!(!request.pending(115));
        request.claimed = true;
        assert!(!request.pending(101));
    }

    fn node_with_workspace(handle: &str) -> (tempfile::TempDir, NodeRoot, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = NodeRoot::at(dir.path().to_path_buf()).unwrap();
        let identity = crate::remote::protocol::WorkspaceIdentity {
            ws_id: cache::new_id('w'),
            project_id: "project".into(),
            handle: handle.into(),
            branch: handle.into(),
            base_sha: "0".repeat(40),
        };
        let ws =
            super::super::Workspace::new(&root, identity, crate::config::RemoteIsolation::None);
        std::fs::create_dir_all(ws.worktree_path.join("src/deep")).unwrap();
        root.save(&ws).unwrap();
        let ws_id = ws.identity.ws_id.clone();
        (dir, root, ws_id)
    }

    #[test]
    fn finds_workspace_from_trusted_root_and_subdirectories() {
        let (_dir, root, ws_id) = node_with_workspace("mine");
        let worktree = root.worktree_path("project", "mine");
        let roots = vec![root.path.clone()];
        let expected = Some((root.path.clone(), ws_id));
        assert_eq!(workspace_from_dir(&worktree, &roots), expected);
        assert_eq!(
            workspace_from_dir(&worktree.join("src/deep"), &roots),
            expected
        );
    }

    #[test]
    fn preserves_uncanonicalized_symlinked_node_root() {
        let (_dir, real_root, _) = node_with_workspace("mine");
        let link_dir = tempfile::tempdir().unwrap();
        let symlink_root = link_dir.path().join("node-link");
        std::os::unix::fs::symlink(&real_root.path, &symlink_root).unwrap();

        let root_via_link = NodeRoot::at(symlink_root.clone()).unwrap();
        let ws_id = root_via_link.workspaces()[0].identity.ws_id.clone();
        let worktree = root_via_link.worktree_path("project", "mine");

        assert_eq!(
            workspace_from_dir(&worktree.join("src"), std::slice::from_ref(&symlink_root)),
            Some((symlink_root, ws_id))
        );
    }

    #[test]
    fn inner_state_workspaces_dir_in_repo_does_not_shadow_node_root() {
        let (_dir, root, ws_id) = node_with_workspace("mine");
        let worktree = root.worktree_path("project", "mine");
        std::fs::create_dir_all(worktree.join("state/workspaces")).unwrap();
        let roots = vec![root.path.clone()];
        assert_eq!(
            workspace_from_dir(&worktree.join("src/deep"), &roots),
            Some((root.path.clone(), ws_id))
        );
    }

    #[test]
    fn rejects_untrusted_ancestor_and_forged_worktree_paths() {
        let (_evil_dir, evil_root, _) = node_with_workspace("evil");
        let (_trusted_dir, trusted_root, ws_id) = node_with_workspace("mine");
        let trusted_roots = vec![trusted_root.path.clone()];

        assert_eq!(
            workspace_from_dir(&evil_root.worktree_path("project", "evil"), &trusted_roots),
            None
        );

        let mut forged = trusted_root.load(&ws_id).unwrap().unwrap();
        forged.worktree_path = PathBuf::from("/");
        trusted_root.save(&forged).unwrap();
        assert_eq!(
            workspace_from_dir(
                &trusted_root.worktree_path("project", "mine"),
                &trusted_roots
            ),
            None
        );
    }

    #[test]
    fn rejects_symlinked_worktree_escaping_node_root() {
        let (_dir, root, ws_id) = node_with_workspace("mine");
        let outside = tempfile::tempdir().unwrap();
        let wt = root.worktree_path("project", "mine");
        std::fs::remove_dir_all(&wt).unwrap();
        std::os::unix::fs::symlink(outside.path(), &wt).unwrap();

        let mut ws = root.load(&ws_id).unwrap().unwrap();
        ws.worktree_path = wt.clone();
        root.save(&ws).unwrap();

        assert_eq!(
            workspace_from_dir(&wt, std::slice::from_ref(&root.path)),
            None
        );
    }

    #[test]
    fn partial_env_and_sandbox_guest_never_fall_back_to_cwd() {
        let (_dir, root, _) = node_with_workspace("mine");
        let worktree = root.worktree_path("project", "mine");
        let roots = vec![root.path.clone()];

        assert_eq!(
            session_target_from(
                Some(root.path.clone().into()),
                None,
                false,
                || Some(worktree.clone()),
                || roots.clone(),
            ),
            None
        );
        assert_eq!(
            session_target_from(
                None,
                Some("w1".into()),
                false,
                || Some(worktree.clone()),
                || roots.clone(),
            ),
            None
        );
        assert_eq!(
            session_target_from(
                None,
                None,
                true,
                || Some(worktree.clone()),
                || roots.clone(),
            ),
            None
        );
    }

    #[test]
    fn ignores_directories_outside_node_worktrees() {
        let (_dir, root, _) = node_with_workspace("mine");
        let local = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.repo_path("project")).unwrap();
        let roots = vec![root.path.clone()];
        assert_eq!(workspace_from_dir(local.path(), &roots), None);
        assert_eq!(workspace_from_dir(&root.repo_path("project"), &roots), None);
        assert_eq!(workspace_from_dir(&root.path, &roots), None);
    }
}
