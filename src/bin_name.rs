//! The name this binary is installed and invoked as.
//!
//! The package stays `workmux` so the fork rebases cleanly on upstream; only the
//! binary target is renamed. Release artifacts, hooks and skills that call back
//! into the binary use [`BIN`], and embedded upstream text goes through
//! [`rebrand!`](crate::rebrand) so its `workmux` invocations call [`BIN`] instead.

use anyhow::{Result, bail};

use crate::agent_setup::Agent;
use std::path::{Path, PathBuf};

pub mod skills;

pub const BIN: &str = env!("CARGO_BIN_NAME");

/// The GitHub repository this binary is released from.
pub const REPO: &str = "kong109/workrex";

/// Release platforms the fork publishes; upstream also ships darwin-amd64 and linux-arm64.
const PUBLISHED_PLATFORMS: &[&str] = &["darwin-arm64", "linux-amd64"];

/// Key prefix of this binary's Claude Code plugin; `workmux-status@…` is upstream's.
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
    "install -m 755 /tmp/workmux.new \"$HOME/.local/bin/",
    env!("CARGO_BIN_NAME"),
    "\""
);

/// Overlay Dockerfile that adds a dev build, copied into the context as [`BIN`], to `image`.
pub fn overlay_dockerfile(image: &str) -> String {
    format!("FROM {image}\nCOPY {BIN} /usr/local/bin/{BIN}\n")
}

/// Default node root on a remote. Upstream's is `~/.workmux-node`; a separate
/// root keeps each binary's pins, state and gc out of the other's.
pub const NODE_ROOT: &str = concat!("~/.", env!("CARGO_BIN_NAME"), "-node");

/// File name of the update-check cache, beside upstream's `update_check.json`.
pub const UPDATE_CACHE_FILE: &str = concat!(env!("CARGO_BIN_NAME"), "-update_check.json");

/// This binary's own state directory. Upstream's uninstall deletes the shared one.
pub fn state_dir() -> Result<PathBuf> {
    Ok(crate::xdg::state_dir()?.with_file_name(BIN))
}

/// Name of the node pin for `version`, a release version or `dev-<hash>`.
pub fn pin_name(version: &str) -> String {
    format!("{BIN}-{version}")
}

/// The part of a node pin name after `{BIN}-`; upstream's `workmux-…` pins are not ours.
pub fn pin_version(name: &str) -> Option<&str> {
    name.strip_prefix(concat!(env!("CARGO_BIN_NAME"), "-"))
}

/// Guest `PATH` entry named [`BIN`]: sandbox images ship upstream's `workmux`, while the
/// mounted agent hooks call [`BIN`]. It runs a [`BIN`] that install-dev put in the guest,
/// else the guest's `workmux`, which speaks the same sandbox RPC.
///
/// A PATH entry holding a copy of the shim would exec the shim again. exec keeps
/// the pid, so a shim that finds its own pid in the sentinel was re-entered and stops
/// searching; processes the binary starts later have other pids.
const GUEST_SHIM: &str = concat!(
    "#!/bin/sh\nset -f\nIFS=:\n",
    // The extra colon keeps a trailing empty entry, which is the current directory.
    "entries=$PATH:\n",
    "[ \"$WORKREX_SHIM_PID\" = $$ ] || for dir in $entries; do\n",
    "  bin=${dir:-.}/",
    env!("CARGO_BIN_NAME"),
    "\n  [ ! \"$bin\" -ef \"$0\" ] && [ -f \"$bin\" ] && [ -x \"$bin\" ] && ",
    "WORKREX_SHIM_PID=$$ && export WORKREX_SHIM_PID && exec \"$bin\" \"$@\"\n",
    "done\nexec workmux \"$@\"\n"
);

/// Write the [`GUEST_SHIM`] into a sandbox shim directory.
pub fn write_guest_shim(shim_bin: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let path = shim_bin.join(BIN);
    crate::util::write_atomic(&path, GUEST_SHIM.as_bytes())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// Print a completion script with upstream's command and function names renamed to [`BIN`].
pub fn print_completion(script: &str) {
    print!("{}", script.replace("workmux", BIN));
}

/// The default dashboard merge action, sent to the agent as a shell command.
pub const MERGE_ACTION: &str = crate::rebrand!("!workmux merge");

/// Runtime [`rebrand!`](crate::rebrand) for text clap builds at run time, such as help text.
fn rebrand_text(text: &str) -> String {
    let src = text.as_bytes();
    let mut out = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if is_command_at(src, i) {
            out.extend_from_slice(BIN.as_bytes());
            i += UPSTREAM.len();
        } else {
            out.push(src[i]);
            i += 1;
        }
    }
    String::from_utf8(out).expect("only ASCII was replaced")
}

/// `cmd` with the help text of it and every subcommand and argument rebranded.
pub fn rebrand_command(mut cmd: clap::Command) -> clap::Command {
    let text = |s: Option<&clap::builder::StyledStr>| s.map(|s| rebrand_text(&s.to_string()));
    if let Some(about) = text(cmd.get_about()) {
        cmd = cmd.about(about);
    }
    if let Some(about) = text(cmd.get_long_about()) {
        cmd = cmd.long_about(about);
    }
    cmd = cmd.mut_args(|mut arg| {
        if let Some(help) = text(arg.get_help()) {
            arg = arg.help(help);
        }
        if let Some(help) = text(arg.get_long_help()) {
            arg = arg.long_help(help);
        }
        arg
    });
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_string())
        .collect();
    for name in names {
        cmd = cmd.mut_subcommand(name, rebrand_command);
    }
    cmd
}

/// [`clap::Parser::try_parse`] over the [`rebrand_command`] help.
pub fn parse<T: clap::Parser>() -> Result<T, clap::Error> {
    let command = || rebrand_command(T::command());
    let mut matches = command().try_get_matches()?;
    T::from_arg_matches_mut(&mut matches).map_err(|err| err.format(&mut command()))
}

const UPSTREAM: &[u8] = b"workmux";

const fn joins(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'@' | b':')
}

/// Whether the command name `workmux` starts at `i`, as opposed to a compound
/// such as `workmux-status`, `.workmux.yaml`, `raine/workmux` or `workmux.dev`.
const fn is_command_at(src: &[u8], i: usize) -> bool {
    let end = i + UPSTREAM.len();
    if !matches_at(src, i, UPSTREAM) {
        return false;
    }
    if i > 0 && joins(src[i - 1]) {
        // `/workmux` after a space or quote names the skill as a slash command.
        let slash_command = src[i - 1] == b'/'
            && (i == 1 || matches!(src[i - 2], b' ' | b'\n' | b'"' | b'\'' | b'`' | b'('));
        if !slash_command {
            return false;
        }
    }
    if end < src.len() && joins(src[end]) {
        // A sentence may end on the name; a dotted name may not.
        let sentence_end =
            src[end] == b'.' && (end + 1 == src.len() || !src[end + 1].is_ascii_alphanumeric());
        // The OpenCode plugin ids `workmux.status…` would collide with upstream's plugin.
        if !sentence_end && !matches_at(src, end, b".status") {
            return false;
        }
    }
    true
}

const fn matches_at(src: &[u8], i: usize, pat: &[u8]) -> bool {
    if i + pat.len() > src.len() {
        return false;
    }
    let mut k = 0;
    while k < pat.len() {
        if src[i + k] != pat[k] {
            return false;
        }
        k += 1;
    }
    true
}

pub const fn rebranded_len(src: &str) -> usize {
    let src = src.as_bytes();
    let mut len = src.len();
    let mut i = 0;
    while i < src.len() {
        if is_command_at(src, i) {
            len = len - UPSTREAM.len() + BIN.len();
            i += UPSTREAM.len();
        } else {
            i += 1;
        }
    }
    len
}

pub const fn rebrand<const N: usize>(src: &str) -> [u8; N] {
    let src = src.as_bytes();
    let bin = BIN.as_bytes();
    let mut out = [0u8; N];
    let mut i = 0;
    let mut o = 0;
    while i < src.len() {
        if is_command_at(src, i) {
            let mut k = 0;
            while k < bin.len() {
                out[o + k] = bin[k];
                k += 1;
            }
            o += bin.len();
            i += UPSTREAM.len();
        } else {
            out[o] = src[i];
            o += 1;
            i += 1;
        }
    }
    out
}

/// Text found only in upstream workmux's hooks and plugins: its binary called by name.
const UPSTREAM_CALLS: &[&str] = &[
    "workmux set-window-status",
    "workmux register-agent",
    "\"workmux\"",
    "'workmux'",
    "\"workmux-status@",
];

/// Files of `agent`'s config that hold upstream workmux's status hooks.
///
/// workrex installs its own hooks beside them and never edits or removes them.
pub fn upstream_hook_files(agent: Agent) -> Vec<PathBuf> {
    use crate::agent_setup::*;
    let ext = |dir: Option<PathBuf>| dir.and_then(|p| Some(p.parent()?.join("workmux-status.ts")));
    let candidates: Vec<Option<PathBuf>> = match agent {
        Agent::Claude => vec![claude::settings_path()],
        Agent::Codex => vec![codex::hooks_path()],
        Agent::Gemini => vec![gemini::settings_path()],
        Agent::Antigravity => vec![antigravity::hooks_path()],
        Agent::Copilot => vec![copilot::copilot_dir().map(|d| d.join("hooks/workmux-status.json"))],
        Agent::Grok => vec![grok::hooks_dir().map(|d| d.join("workmux-status.json"))],
        Agent::Pi => vec![ext(pi::extension_path())],
        Agent::Omp => vec![ext(omp::extension_path())],
        Agent::OpenCode => [
            "plugins/workmux-status.ts",
            "plugin/workmux-status.ts",
            "plugins/workmux-status/tui.ts",
        ]
        .iter()
        .map(|rel| opencode::opencode_config_dir().map(|d| d.join(rel)))
        .collect(),
    };
    candidates
        .into_iter()
        .flatten()
        .filter(|path| {
            std::fs::read_to_string(path)
                .is_ok_and(|text| UPSTREAM_CALLS.iter().any(|call| text.contains(call)))
        })
        .collect()
}

/// Print one warning naming the agents that also have upstream workmux's hooks installed.
pub fn warn_upstream_hooks(agents: impl IntoIterator<Item = Agent>) {
    let names: Vec<&str> = agents
        .into_iter()
        .filter(|agent| !upstream_hook_files(*agent).is_empty())
        .map(|agent| agent.name())
        .collect();
    if !names.is_empty() {
        println!(
            "  {} Upstream workmux status hooks are also installed for {}. {BIN} leaves them in \
             place; remove them with `workmux uninstall`.\n",
            console::style("!").yellow(),
            names.join(", ")
        );
    }
}

/// [`rebrand!`](crate::rebrand) applied to `include_str!($path)`, resolved from the calling file.
#[macro_export]
macro_rules! rebrand_include {
    ($path:literal) => {
        $crate::rebrand!(include_str!($path))
    };
}

/// Rewrite the command name `workmux` in a constant string to [`BIN`] at compile time.
#[macro_export]
macro_rules! rebrand {
    ($src:expr) => {{
        const SRC: &str = $src;
        const BYTES: [u8; $crate::bin_name::rebranded_len(SRC)] = $crate::bin_name::rebrand(SRC);
        const OUT: &str = match ::std::str::from_utf8(&BYTES) {
            Ok(s) => s,
            Err(_) => panic!("rebranded text is not UTF-8"),
        };
        OUT
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn rebrand_rewrites_invocations_and_keeps_compound_names() {
        let src = rebrand!(
            "workmux add x; $`workmux set-window-status`; spawn(\"workmux\", a); use workmux.\n\
             \"/workmux add\" \
             .workmux.yaml workmux-status@workmux raine/workmux workmux.dev ~/.config/workmux/ \
             WORKMUX_X workmux_pipe workmux: 'workmux.status.server'"
        );
        assert_eq!(
            src,
            format!(
                "{BIN} add x; $`{BIN} set-window-status`; spawn(\"{BIN}\", a); use {BIN}.\n\
                 \"/{BIN} add\" \
                 .workmux.yaml workmux-status@workmux raine/workmux workmux.dev ~/.config/workmux/ \
                 WORKMUX_X workmux_pipe workmux: '{BIN}.status.server'"
            )
        );
    }

    #[test]
    fn release_url_points_at_fork_artifact() {
        let platform = crate::command::update::release_platform("linux", "x86_64").unwrap();
        assert_eq!(
            format!(
                "{}/{}.tar.gz",
                release_base("0.1.278"),
                release_artifact(platform).unwrap()
            ),
            "https://github.com/kong109/workrex/releases/download/v0.1.278/workrex-linux-amd64.tar.gz"
        );
    }

    #[test]
    fn unpublished_platforms_point_at_a_source_build() {
        for (os, arch) in [("linux", "aarch64"), ("macos", "x86_64")] {
            let platform = crate::command::update::release_platform(os, arch).unwrap();
            let err = release_artifact(platform).unwrap_err().to_string();
            assert!(
                err.starts_with(&format!("no {BIN} build for {platform};")),
                "{err}"
            );
            assert!(err.contains("cargo install --git https://github.com/kong109/workrex"));
        }
    }

    #[test]
    fn fork_prereleases_order_between_upstream_releases() {
        use crate::command::update::is_newer_version;
        assert!(is_newer_version("0.1.278-rex.1", "0.1.278"));
        assert!(is_newer_version("0.1.279", "0.1.278-rex.1"));
        assert!(is_newer_version("0.1.278-rex.2", "0.1.278-rex.1"));
        assert!(!is_newer_version("0.1.278", "0.1.278-rex.1"));
        assert!(!is_newer_version("0.1.278-rex.1", "0.1.279"));
    }

    #[test]
    fn installed_claude_hooks_call_bin() {
        let tmp = tempfile::tempdir().unwrap();
        crate::agent_setup::claude::install_dir(tmp.path()).unwrap();
        let settings: Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        let commands: Vec<&str> = settings["hooks"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|groups| groups.as_array().unwrap())
            .flat_map(|group| group["hooks"].as_array().unwrap())
            .map(|hook| hook["command"].as_str().unwrap())
            .collect();
        assert!(!commands.is_empty());
        for command in commands {
            assert!(command.starts_with("workrex "), "{command}");
        }
    }

    #[test]
    fn sandbox_dev_install_uses_bin() {
        assert_eq!(
            target_binary(Path::new("/ws"), "x86_64-unknown-linux-musl", "release"),
            Path::new("/ws/target/x86_64-unknown-linux-musl/release/workrex")
        );
        assert_eq!(
            LIMA_INSTALL,
            "install -m 755 /tmp/workmux.new \"$HOME/.local/bin/workrex\""
        );
        assert_eq!(
            overlay_dockerfile("img:latest"),
            "FROM img:latest\nCOPY workrex /usr/local/bin/workrex\n"
        );
    }

    #[test]
    fn sandbox_guest_hooks_reach_a_guest_binary() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let shims = crate::sandbox::shims::create_shim_directory(tmp.path(), &[]).unwrap();
        let fake = |dir: &str, name: &str| {
            let dir = tmp.path().join(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\necho {name} \"$@\"\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            dir
        };
        let image = fake("image", "workmux");
        let hook_in = |cwd: &Path, path: String| {
            let out = std::process::Command::new(shims.join(BIN))
                .args(["set-window-status", "done"])
                .env("PATH", path)
                .current_dir(cwd)
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap()
        };
        let hook = |path: String| hook_in(tmp.path(), path);
        // No /usr/bin: guest images need not ship coreutils.
        let path = format!("{}:{}", shims.display(), image.display());
        assert_eq!(hook(path.clone()), "workmux set-window-status done\n");
        let dev = fake("dev", BIN);
        assert_eq!(
            hook(format!("{path}:{}", dev.display())),
            format!("{BIN} set-window-status done\n")
        );
        // An empty PATH entry, inner or trailing, is the current directory.
        for empty in [
            format!("{}::{}", shims.display(), image.display()),
            format!("{path}:"),
        ] {
            assert_eq!(
                hook_in(&dev, empty),
                format!("{BIN} set-window-status done\n")
            );
        }
        // A directory named like the binary is not one.
        let checkout = tmp.path().join("checkout");
        std::fs::create_dir_all(checkout.join(BIN)).unwrap();
        assert_eq!(
            hook(format!("{}:{path}:{}", checkout.display(), dev.display())),
            format!("{BIN} set-window-status done\n")
        );
        // Another path to the shim itself is skipped, not taken for a real binary.
        let alias = tmp.path().join("alias");
        std::os::unix::fs::symlink(&shims, &alias).unwrap();
        assert_eq!(
            hook(format!("{}:{path}:{}", alias.display(), dev.display())),
            format!("{BIN} set-window-status done\n")
        );
    }

    #[test]
    fn guest_shim_reached_again_through_a_copy_falls_back_instead_of_looping() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};
        let tmp = tempfile::tempdir().unwrap();
        let shims = crate::sandbox::shims::create_shim_directory(tmp.path(), &[]).unwrap();
        let image = tmp.path().join("image");
        std::fs::create_dir_all(&image).unwrap();
        std::fs::write(image.join("workmux"), "#!/bin/sh\necho workmux \"$@\"\n").unwrap();
        std::fs::set_permissions(
            image.join("workmux"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let copy = tmp.path().join("copy");
        std::fs::create_dir_all(&copy).unwrap();
        std::fs::copy(shims.join(BIN), copy.join(BIN)).unwrap();
        let shells = ["/bin/sh", "/bin/dash", "/bin/busybox"];
        for shell in shells.iter().filter(|shell| Path::new(shell).exists()) {
            for entries in [
                format!("{}/", shims.display()),
                format!("{}:{}", shims.display(), copy.display()),
            ] {
                let mut cmd = std::process::Command::new(shell);
                if shell.ends_with("busybox") {
                    cmd.arg("sh");
                }
                let mut child = cmd
                    .args(["-c", &format!("{BIN} set-window-status done")])
                    .env(
                        "PATH",
                        format!("{entries}:{}:/usr/bin:/bin", image.display()),
                    )
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                let deadline = Instant::now() + Duration::from_secs(10);
                while child.try_wait().unwrap().is_none() {
                    if Instant::now() > deadline {
                        child.kill().unwrap();
                        panic!("{shell}: the shim looped with PATH entries {entries}");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let out = child.wait_with_output().unwrap();
                assert_eq!(
                    String::from_utf8(out.stdout).unwrap(),
                    "workmux set-window-status done\n",
                    "{shell} {entries}"
                );
            }
        }
    }

    #[test]
    fn dashboard_merge_action_calls_bin() {
        assert_eq!(
            crate::config::DashboardConfig::default().merge(),
            "!workrex merge"
        );
    }

    #[test]
    fn node_root_and_pins_are_not_upstreams() {
        assert_eq!(crate::remote::DEFAULT_NODE_PATH, "~/.workrex-node");
        assert_eq!(pin_name("dev-ba7816bf8f01"), "workrex-dev-ba7816bf8f01");
        assert_eq!(pin_version("workrex-0.1.278"), Some("0.1.278"));
        assert_eq!(pin_version("workmux-0.1.278"), None);
        assert_eq!(pin_version("workmux-dev-ba7816bf8f01"), None);
    }

    #[test]
    fn update_cache_is_not_shared_with_upstream() {
        assert_eq!(UPDATE_CACHE_FILE, "workrex-update_check.json");
    }
}
