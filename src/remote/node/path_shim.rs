//! `path-shim`: `~/.local/bin/workrex`, which puts the node's pinned binary on
//! the node user's login `PATH` for shells and scripts they run themselves.
//!
//! The shim runs the pin with the default state, as a normal install would.
//! Inside a node execution's session it defers to that root's `run/bin`
//! wrapper, which keeps the node store, so a login subshell that puts
//! `~/.local/bin` ahead of the wrapper still reaches the workspace's store.
//!
//! A file at the shim path is ours only when it carries [`MARKER`]; anything
//! else there is the user's and is never touched.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use super::NodeRoot;
use super::spawn::SESSION_ROOT_ENV;
use crate::bin_name::BIN;
use crate::shell::shell_quote;

/// The line that marks a shim as written by `remote setup`.
pub const MARKER: &str = concat!("# Written by ", env!("CARGO_BIN_NAME"), " remote setup.");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathShimRequest {
    /// Write the shim when it is missing or ours and outdated, instead of only
    /// reporting it.
    pub install: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathShimResponse {
    /// Absolute path of the shim.
    pub path: String,
    pub state: ShimState,
    /// The pin our shim runs, after any write.
    pub target: Option<String>,
    /// `target` is an executable file, so the shim can run it. `None` from a
    /// node that predates the field.
    pub target_runs: Option<bool>,
    /// This call wrote the shim.
    pub changed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShimState {
    Missing,
    /// Carries [`MARKER`].
    Ours,
    /// Any other file, symlink or directory.
    Foreign,
}

pub fn path_shim(root: &NodeRoot, request: PathShimRequest) -> Result<PathShimResponse> {
    let exe = std::env::current_exe().context("Could not determine the node binary")?;
    let name = exe
        .file_name()
        .ok_or_else(|| anyhow!("node binary has no file name: {}", exe.display()))?;
    let pin = root.path.join("bin").join(name);
    if !pin.is_file() {
        return Err(super::refuse(
            crate::remote::protocol::code::BAD_REQUEST,
            format!("{} is not a pinned node binary", pin.display()),
        ));
    }
    ensure(&shim_path()?, &pin, request.install)
}

/// `~/.local/bin/workrex` of the node user.
pub fn shim_path() -> Result<PathBuf> {
    let home = home::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    Ok(home.join(".local").join("bin").join(BIN))
}

/// The shim that runs `pin`.
pub fn script(pin: &Path) -> String {
    let wrapper = format!("\"${SESSION_ROOT_ENV}/run/bin/{BIN}\"");
    format!(
        "#!/bin/sh\n{MARKER}\n\
         [ -n \"${SESSION_ROOT_ENV}\" ] && [ -x {wrapper} ] && exec {wrapper} \"$@\"\n\
         exec {} \"$@\"\n",
        shell_quote(&pin.to_string_lossy())
    )
}

/// The pin `text` runs when it is our shim.
pub fn target(text: &str) -> Option<String> {
    if !text.lines().any(|line| line == MARKER) {
        return None;
    }
    let quoted = text
        .lines()
        .rev()
        .find_map(|line| line.strip_prefix("exec ")?.strip_suffix(" \"$@\""))?;
    Some(
        match quoted.strip_prefix('\'').and_then(|q| q.strip_suffix('\'')) {
            Some(inner) => inner.replace("'\\''", "'"),
            None => quoted.to_string(),
        },
    )
}

/// The pin of our shim at `shim`, if one is there.
pub fn installed_target(shim: &Path) -> Option<String> {
    if !fs::symlink_metadata(shim).ok()?.is_file() {
        return None;
    }
    target(&fs::read_to_string(shim).ok()?)
}

/// Report the shim at `shim`, and with `install` point ours, or a new one, at
/// `pin`. Rewrites only when the content changes.
pub fn ensure(shim: &Path, pin: &Path, install: bool) -> Result<PathShimResponse> {
    let want = script(pin);
    let (state, current) = match fs::symlink_metadata(shim) {
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => (ShimState::Missing, None),
        Err(err) => return Err(err).with_context(|| format!("Failed to read {}", shim.display())),
        Ok(meta) if !meta.is_file() => (ShimState::Foreign, None),
        Ok(_) => {
            let text =
                fs::read(shim).with_context(|| format!("Failed to read {}", shim.display()))?;
            let text = String::from_utf8_lossy(&text).into_owned();
            match target(&text) {
                Some(_) => (ShimState::Ours, Some(text)),
                None => (ShimState::Foreign, None),
            }
        }
    };
    let mut response = PathShimResponse {
        path: shim.to_string_lossy().into_owned(),
        state,
        target: current.as_deref().and_then(target),
        target_runs: None,
        changed: false,
    };
    if install && state != ShimState::Foreign && current.as_deref() != Some(want.as_str()) {
        write_executable(shim, &want)?;
        response.state = ShimState::Ours;
        response.target = target(&want);
        response.changed = true;
    }
    response.target_runs = Some(response.target.as_deref().is_some_and(is_executable));
    Ok(response)
}

fn is_executable(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Replace `path` with an executable file holding `text` in one rename.
fn write_executable(path: &Path, text: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().expect("the shim path has a parent");
    fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let mut tmp = tempfile::Builder::new()
        .prefix(&format!(".{BIN}.shim."))
        .tempfile_in(dir)
        .with_context(|| format!("Failed to create a temp file in {}", dir.display()))?;
    tmp.write_all(text.as_bytes())?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o755))?;
    tmp.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let shim = tmp.path().join(".local/bin").join(BIN);
        let pin = tmp
            .path()
            .join("node/bin")
            .join(crate::bin_name::pin_name("0.1.2"));
        (tmp, shim, pin)
    }

    #[test]
    fn a_missing_shim_is_created_executable() {
        let (_tmp, shim, pin) = setup();

        let response = ensure(&shim, &pin, true).unwrap();

        assert_eq!(response.state, ShimState::Ours);
        assert!(response.changed);
        assert_eq!(response.target.as_deref(), Some(pin.to_str().unwrap()));
        assert_eq!(fs::read_to_string(&shim).unwrap(), script(&pin));
        assert_eq!(
            fs::metadata(&shim).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn reporting_writes_nothing() {
        let (_tmp, shim, pin) = setup();

        let response = ensure(&shim, &pin, false).unwrap();

        assert_eq!(response.state, ShimState::Missing);
        assert!(!response.changed);
        assert!(!shim.exists());
    }

    #[test]
    fn an_unchanged_shim_is_not_rewritten() {
        let (_tmp, shim, pin) = setup();
        ensure(&shim, &pin, true).unwrap();
        let before = fs::metadata(&shim).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let response = ensure(&shim, &pin, true).unwrap();

        let after = fs::metadata(&shim).unwrap();
        assert!(!response.changed);
        assert_eq!(response.state, ShimState::Ours);
        assert_eq!(
            (before.ino(), before.modified().unwrap()),
            (after.ino(), after.modified().unwrap())
        );
    }

    #[test]
    fn our_shim_is_repointed_at_the_new_pin() {
        let (_tmp, shim, old) = setup();
        ensure(&shim, &old, true).unwrap();
        let new = old.with_file_name(crate::bin_name::pin_name("0.1.3"));

        let response = ensure(&shim, &new, true).unwrap();

        assert!(response.changed);
        assert_eq!(fs::read_to_string(&shim).unwrap(), script(&new));
        assert_eq!(
            installed_target(&shim).as_deref(),
            Some(new.to_str().unwrap())
        );
    }

    #[test]
    fn a_hand_written_shim_with_the_marker_is_taken_over() {
        let (_tmp, shim, pin) = setup();
        fs::create_dir_all(shim.parent().unwrap()).unwrap();
        fs::write(
            &shim,
            format!("#!/bin/sh\n{MARKER}\nexec /home/u/.{BIN}-node/bin/{BIN}-0.1.1 \"$@\"\n"),
        )
        .unwrap();

        let reported = ensure(&shim, &pin, false).unwrap();
        let response = ensure(&shim, &pin, true).unwrap();

        assert_eq!(reported.state, ShimState::Ours);
        assert_eq!(
            reported.target,
            Some(format!("/home/u/.{BIN}-node/bin/{BIN}-0.1.1"))
        );
        assert!(response.changed);
        assert_eq!(fs::read_to_string(&shim).unwrap(), script(&pin));
    }

    #[test]
    fn other_files_and_symlinks_are_left_alone() {
        let (tmp, shim, pin) = setup();
        fs::create_dir_all(shim.parent().unwrap()).unwrap();
        let own = "#!/bin/sh\n# my own install\nexec /opt/workrex \"$@\"\n";
        let real = tmp.path().join("real");
        fs::write(&real, script(&pin)).unwrap();
        for kind in ["file", "symlink", "directory"] {
            match kind {
                "file" => fs::write(&shim, own).unwrap(),
                // A symlink to a file with the marker is still not ours.
                "symlink" => std::os::unix::fs::symlink(&real, &shim).unwrap(),
                _ => fs::create_dir(&shim).unwrap(),
            }
            let before = fs::symlink_metadata(&shim).unwrap();

            let response = ensure(&shim, &pin, true).unwrap();

            let after = fs::symlink_metadata(&shim).unwrap();
            assert_eq!(response.state, ShimState::Foreign, "{kind}");
            assert!(!response.changed, "{kind}");
            assert_eq!(response.target, None, "{kind}");
            assert_eq!(before.ino(), after.ino(), "{kind}");
            assert_eq!(installed_target(&shim), None, "{kind}");
            if kind == "file" {
                assert_eq!(fs::read_to_string(&shim).unwrap(), own);
            }
            if after.is_dir() {
                fs::remove_dir(&shim).unwrap();
            } else {
                fs::remove_file(&shim).unwrap();
            }
        }
        assert_eq!(fs::read_to_string(&real).unwrap(), script(&pin));
    }

    #[test]
    fn the_marker_must_be_a_whole_line() {
        let text = format!("#!/bin/sh\n# not {MARKER}\nexec /x/bin/{BIN}-1 \"$@\"\n");
        assert_eq!(target(&text), None);
    }

    #[test]
    fn targets_with_quoted_paths_round_trip() {
        let pin = Path::new("/home/o'neil/my node/bin/workrex-1");
        assert_eq!(target(&script(pin)).as_deref(), pin.to_str());
    }

    #[test]
    fn the_shim_runs_the_pin_outside_executions_and_the_wrapper_inside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("o'neil's node");
        let pin = root.join("bin").join(crate::bin_name::pin_name("0.1.2"));
        let wrapper = root.join("run/bin").join(BIN);
        for (path, name) in [(&pin, "pin"), (&wrapper, "wrapper")] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            crate::test_support::write_script(
                path,
                &format!("#!/bin/sh\necho {name} \"$XDG_STATE_HOME\" \"$@\"\n"),
            );
        }
        let shim = tmp.path().join("shim");
        crate::test_support::write_script(&shim, &script(&pin));
        let run = |root: Option<&Path>| {
            let mut cmd = std::process::Command::new(&shim);
            cmd.args(["a b", "c"]).env_remove("XDG_STATE_HOME");
            match root {
                Some(root) => cmd.env(SESSION_ROOT_ENV, root),
                None => cmd.env_remove(SESSION_ROOT_ENV),
            };
            String::from_utf8(cmd.output().unwrap().stdout).unwrap()
        };

        assert_eq!(run(None), "pin  a b c\n");
        assert_eq!(run(Some(&root)), "wrapper  a b c\n");
        // A root without a wrapper, such as one whose executions run in containers.
        assert_eq!(run(Some(&tmp.path().join("other"))), "pin  a b c\n");
    }

    #[test]
    fn a_reply_from_a_node_without_target_runs_still_parses() {
        let reply = r#"{"path":"/h/.local/bin/workrex","state":"ours","target":"/n/bin/workrex-1","changed":false}"#;
        let response: PathShimResponse = serde_json::from_str(reply).unwrap();
        assert_eq!(response.target_runs, None);
    }

    #[test]
    fn a_shim_whose_pin_is_gone_is_reported_as_not_running() {
        let (_tmp, shim, pin) = setup();
        fs::create_dir_all(pin.parent().unwrap()).unwrap();
        crate::test_support::write_script(&pin, "#!/bin/sh\n");
        ensure(&shim, &pin, true).unwrap();
        assert_eq!(ensure(&shim, &pin, false).unwrap().target_runs, Some(true));

        fs::set_permissions(&pin, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(ensure(&shim, &pin, false).unwrap().target_runs, Some(false));
        fs::remove_file(&pin).unwrap();
        let response = ensure(&shim, &pin, false).unwrap();

        assert_eq!(response.state, ShimState::Ours);
        assert_eq!(response.target.as_deref(), Some(pin.to_str().unwrap()));
        assert_eq!(response.target_runs, Some(false));
    }
}
