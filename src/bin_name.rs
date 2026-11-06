//! Workrex executable, release artifacts, and owned paths.

use anyhow::{Result, bail};

use std::path::{Path, PathBuf};

pub mod skills;

pub const BIN: &str = env!("CARGO_BIN_NAME");

/// The file `remote setup` keeps in a node root's `bin/`, next to the pinned
/// binaries, so `update` can tell a pin from a versioned install.
pub const NODE_ROOT_MARKER: &str = concat!(".", env!("CARGO_BIN_NAME"), "-node");

/// The GitHub repository this binary is released from.
pub const REPO: &str = "kong109/workrex";

/// Release platforms published for Workrex.
const PUBLISHED_PLATFORMS: &[&str] = &["darwin-arm64", "linux-amd64"];

/// Key prefix of the Workrex Claude Code status plugin.
pub const PLUGIN_PREFIX: &str = concat!(env!("CARGO_BIN_NAME"), "-status@");

/// Release artifact name of this binary for `platform` (e.g. `workrex-linux-amd64`).
pub fn release_artifact(platform: &str) -> Result<String> {
    if !PUBLISHED_PLATFORMS.contains(&platform) {
        bail!(
            "no {BIN} build for {platform}; build from source with \
             cargo install --git https://github.com/{REPO} --bin {BIN}"
        );
    }
    Ok(format!("{BIN}-{platform}"))
}

/// URL of the release `version` download directory.
pub fn release_base(version: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/v{version}")
}

/// The binary `cargo build --target <target>` writes for the sandbox guest.
pub fn target_binary(workspace: &Path, target: &str, profile_dir: &str) -> PathBuf {
    workspace.join(format!("target/{target}/{profile_dir}/{BIN}"))
}

/// Shell command that moves a copied dev build into place in a Lima VM.
pub const LIMA_INSTALL: &str = concat!(
    "install -m 755 /tmp/workrex.new \"$HOME/.local/bin/",
    env!("CARGO_BIN_NAME"),
    "\""
);

/// Overlay Dockerfile that adds a dev build, copied into the context as [`BIN`], to `image`.
pub fn overlay_dockerfile(image: &str) -> String {
    format!("FROM {image}\nCOPY {BIN} /usr/local/bin/{BIN}\n")
}

/// Default Workrex node root on a remote server.
pub const NODE_ROOT: &str = concat!("~/.", env!("CARGO_BIN_NAME"), "-node");

/// Workrex update-check cache.
pub fn update_cache_file() -> Result<PathBuf> {
    Ok(crate::xdg::cache_dir()?.join(format!("{BIN}-update_check.json")))
}

/// Workrex state directory.
pub fn state_dir() -> Result<PathBuf> {
    Ok(crate::xdg::state_dir()?.with_file_name(BIN))
}

/// Name of the node pin for `version`, a release version or `dev-<hash>`.
pub fn pin_name(version: &str) -> String {
    format!("{BIN}-{version}")
}

/// The version part of a Workrex node pin name.
pub fn pin_version(name: &str) -> Option<&str> {
    name.strip_prefix(concat!(env!("CARGO_BIN_NAME"), "-"))
}

/// Find a real Workrex executable after the sandbox shim directory.
const GUEST_SHIM: &str = concat!(
    "#!/bin/sh\nset -f\nIFS=:\n",
    "entries=$PATH:\n",
    "[ \"$WORKREX_SHIM_PID\" = $$ ] || for dir in $entries; do\n",
    "  bin=${dir:-.}/",
    env!("CARGO_BIN_NAME"),
    "\n  [ ! \"$bin\" -ef \"$0\" ] && [ -f \"$bin\" ] && [ -x \"$bin\" ] && ",
    "WORKREX_SHIM_PID=$$ && export WORKREX_SHIM_PID && exec \"$bin\" \"$@\"\n",
    "done\nprintf '%s\\n' 'Workrex executable not found after sandbox shim' >&2\nexit 127\n"
);

/// Write the [`GUEST_SHIM`] into a sandbox shim directory.
pub fn write_guest_shim(shim_bin: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = shim_bin.join(BIN);
    crate::util::write_atomic(&path, GUEST_SHIM.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Print a completion script generated for Workrex.
pub fn print_completion(script: &str) {
    print!("{script}");
}

/// The default dashboard merge action, sent to the agent as a shell command.
pub const MERGE_ACTION: &str = "!workrex merge";

/// Parse the native Workrex CLI.
pub fn parse<T: clap::Parser>() -> Result<T, clap::Error> {
    T::try_parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_artifact_uses_workrex() {
        assert_eq!(
            release_artifact("darwin-arm64").unwrap(),
            "workrex-darwin-arm64"
        );
        assert!(release_base("1.0.0").contains("kong109/workrex/releases"));
        assert!(release_artifact("unsupported").is_err());
    }

    #[test]
    fn node_pins_only_accept_workrex() {
        assert_eq!(pin_name("1.0.0"), "workrex-1.0.0");
        assert_eq!(pin_version("workrex-1.0.0"), Some("1.0.0"));
        assert_eq!(pin_version("other-tool-1.0.0"), None);
    }

    #[test]
    fn guest_shim_executes_workrex_after_itself() {
        let dir = tempfile::tempdir().unwrap();
        let shims = dir.path().join("shim");
        let real = dir.path().join("real");
        std::fs::create_dir_all(&shims).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        write_guest_shim(&shims).unwrap();
        crate::test_support::write_script(&real.join(BIN), "#!/bin/sh\nprintf '%s\\n' \"$*\"\n");
        let output = std::process::Command::new(shims.join(BIN))
            .args(["set-window-status", "done"])
            .env("PATH", format!("{}:{}", shims.display(), real.display()))
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            "set-window-status done\n"
        );
    }

    #[test]
    fn guest_shim_without_real_executable_fails() {
        let dir = tempfile::tempdir().unwrap();
        write_guest_shim(dir.path()).unwrap();
        let output = std::process::Command::new(dir.path().join(BIN))
            .env("PATH", dir.path())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(127));
    }
}
