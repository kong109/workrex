//! Muse Code status tracking setup.
//!
//! Detects Muse via its config directory, `$XDG_CONFIG_HOME/muse/` or
//! `~/.config/muse/`. Muse strips the environment of the hooks it runs
//! from `settings.json`, which hides the pane from `set-window-status`.
//! Managed hooks keep the variables `managed_hooks_env_vars` names, so the
//! hooks go in a hooks file of their own, and `settings.json` points
//! `managed_hooks_path` at it and names the variables.

use anyhow::{Context, Result};
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use super::{StatusCheck, UpdatePreview};
use crate::agent_setup::hooks;
use crate::agent_setup::json_config::{
    self, EmptyJsonRoot, JsonHookInstallSpec, JsonHookUninstallSpec,
};
use crate::bin_name::BIN;

const HOOKS_JSON: &str = crate::rebrand_include!("../../resources/muse/workmux-status.json");

const ENV_VARS_KEY: &str = "managed_hooks_env_vars";
const HOOKS_PATH_KEY: &str = "managed_hooks_path";

/// Variables `register-agent` and `set-window-status` read to find the
/// agent's pane and the state store.
const HOOK_ENV_VARS: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "TMUX_TMPDIR",
    "REX_SERVER",
    "REX_SESSION",
    "REX_BLOCK",
    "REX_BIN_DIR",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "ZELLIJ",
    "ZELLIJ_PANE_ID",
    "ZELLIJ_SESSION_NAME",
    "KITTY_WINDOW_ID",
    "KITTY_LISTEN_ON",
    "WORKMUX_BACKEND",
    "WORKMUX_STATUS_BACKEND",
    "WORKMUX_STATUS_INSTANCE",
    "WORKMUX_STATUS_PANE_ID",
    "WORKMUX_DISABLE_SET_WINDOW_STATUS",
    "WM_SANDBOX_GUEST",
    "WM_RPC_HOST",
    "WM_RPC_PORT",
    "WM_RPC_TOKEN",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_CACHE_HOME",
];

fn muse_dir() -> Option<PathBuf> {
    muse_dir_from_env(home::home_dir(), std::env::var_os("XDG_CONFIG_HOME"))
}

fn muse_dir_from_env(home: Option<PathBuf>, xdg_config_home: Option<OsString>) -> Option<PathBuf> {
    xdg_config_home
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join("muse"))
        .or_else(|| home.map(|home| home.join(".config/muse")))
}

fn hooks_path_in(dir: &Path) -> PathBuf {
    dir.join(concat!(env!("CARGO_BIN_NAME"), "-status.json"))
}

pub fn detect() -> Option<&'static str> {
    if muse_dir().is_some_and(|d| d.is_dir()) {
        return Some("found ~/.config/muse/");
    }
    None
}

pub fn check() -> Result<StatusCheck> {
    let Some(dir) = muse_dir() else {
        return Ok(StatusCheck::NotInstalled);
    };
    check_dir(&dir)
}

fn check_dir(dir: &Path) -> Result<StatusCheck> {
    let hooks_path = hooks_path_in(dir);
    let settings = read_settings(dir)?;
    if let Some(settings) = &settings {
        ensure_own_hooks_path(dir, settings, &hooks_path)?;
    }
    if !hooks_path.exists() {
        return Ok(StatusCheck::NotInstalled);
    }

    let content = fs::read_to_string(&hooks_path).context("Failed to read Muse hooks file")?;
    let config: Value =
        serde_json::from_str(&content).context("Muse hooks file is not valid JSON")?;
    let wired = settings
        .as_ref()
        .is_some_and(|settings| is_wired(settings, &hooks_path));

    if wired && hooks::has_required_hook_commands(&config, &load_hooks()?) {
        Ok(StatusCheck::Installed)
    } else if hooks::has_workmux_hooks(&config) {
        Ok(StatusCheck::UpdateAvailable)
    } else {
        Ok(StatusCheck::NotInstalled)
    }
}

fn read_settings(dir: &Path) -> Result<Option<Value>> {
    let path = dir.join("settings.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).context("Failed to read Muse settings.json")?;
    let settings =
        serde_json::from_str(&content).context("Muse settings.json is not valid JSON")?;
    Ok(Some(settings))
}

fn write_settings(dir: &Path, settings: &Value) -> Result<()> {
    fs::write(
        dir.join("settings.json"),
        serde_json::to_string_pretty(settings)? + "\n",
    )
    .context("Failed to write Muse settings.json")
}

fn configured_hooks_path(settings: &Value) -> Option<&str> {
    settings.get(HOOKS_PATH_KEY).and_then(Value::as_str)
}

/// Muse reads one managed hooks file, so a path the user set is theirs.
fn ensure_own_hooks_path(dir: &Path, settings: &Value, hooks_path: &Path) -> Result<()> {
    match configured_hooks_path(settings) {
        Some(path) if Path::new(path) != hooks_path => anyhow::bail!(
            "{} sets {HOOKS_PATH_KEY} to {path}; add {BIN}'s hooks from {} to that file",
            dir.join("settings.json").display(),
            hooks_path.display()
        ),
        _ => Ok(()),
    }
}

fn is_wired(settings: &Value, hooks_path: &Path) -> bool {
    let names = settings.get(ENV_VARS_KEY).and_then(Value::as_array);
    configured_hooks_path(settings).is_some_and(|path| Path::new(path) == hooks_path)
        && HOOK_ENV_VARS
            .iter()
            .all(|name| names.is_some_and(|names| names.iter().any(|n| n == name)))
}

fn wire(settings: &mut Value, hooks_path: &Path) -> Result<()> {
    let hooks_path = hooks_path
        .to_str()
        .context("Muse hooks path is not valid UTF-8")?;
    let settings = settings
        .as_object_mut()
        .context("Muse settings.json is not an object")?;
    settings.insert(HOOKS_PATH_KEY.to_string(), Value::from(hooks_path));
    let names = settings
        .entry(ENV_VARS_KEY)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .with_context(|| format!("Muse {ENV_VARS_KEY} is not an array"))?;
    for name in HOOK_ENV_VARS {
        if !names.iter().any(|n| n == name) {
            names.push(Value::from(*name));
        }
    }
    Ok(())
}

/// Undo [`wire`] once the hooks file is gone. Returns whether anything changed.
fn unwire(settings: &mut Value, hooks_path: &Path) -> bool {
    if !configured_hooks_path(settings).is_some_and(|path| Path::new(path) == hooks_path) {
        return false;
    }
    let Some(settings) = settings.as_object_mut() else {
        return false;
    };
    settings.remove(HOOKS_PATH_KEY);
    if let Some(names) = settings.get_mut(ENV_VARS_KEY).and_then(Value::as_array_mut) {
        names.retain(|n| !HOOK_ENV_VARS.iter().any(|name| n == name));
        if names.is_empty() {
            settings.remove(ENV_VARS_KEY);
        }
    }
    true
}

pub fn uninstall() -> Result<String> {
    let Some(dir) = muse_dir() else {
        return Ok("Muse config dir not found, nothing to uninstall".to_string());
    };
    uninstall_dir(&dir)
}

fn uninstall_dir(dir: &Path) -> Result<String> {
    let hooks_path = hooks_path_in(dir);
    let result = json_config::json_hook_uninstall(
        &hooks_path,
        &JsonHookUninstallSpec {
            messages: json_config::JsonHookUninstallMessages {
                file_missing: crate::rebrand!("No Muse workmux-status.json found"),
                not_found: crate::rebrand!("No workmux hooks found in Muse hooks file"),
                soft_read_error: None,
                soft_parse_error: None,
            },
            delete_if_no_hooks_remain: true,
            remove_plugins: false,
            soft_errors: false,
        },
    )?;
    if !hooks_path.exists()
        && let Some(mut settings) = read_settings(dir)?
        && unwire(&mut settings, &hooks_path)
    {
        write_settings(dir, &settings)?;
    }
    Ok(result)
}

fn load_hooks() -> Result<Value> {
    json_config::hooks_from_embedded(HOOKS_JSON, "hooks config missing hooks key")
}

pub(crate) fn update_preview() -> Result<Option<UpdatePreview>> {
    let Some(path) = muse_dir()
        .map(|dir| hooks_path_in(&dir))
        .filter(|path| path.exists())
    else {
        return Ok(None);
    };
    let content = fs::read_to_string(&path).context("Failed to read Muse hooks file")?;
    let installed: Value =
        serde_json::from_str(&content).context("Muse hooks file is not valid JSON")?;
    let mut bundled = installed.clone();
    hooks::merge_missing_hook_commands(&mut bundled, &load_hooks()?)?;

    Ok(Some(UpdatePreview {
        label: path.display().to_string(),
        installed: serde_json::to_string_pretty(&installed)? + "\n",
        bundled: serde_json::to_string_pretty(&bundled)? + "\n",
    }))
}

pub fn install() -> Result<String> {
    let dir = muse_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    install_dir(&dir)
}

fn install_dir(dir: &Path) -> Result<String> {
    let hooks_path = hooks_path_in(dir);
    // Muse refuses to start on a settings.json without `schema_version`.
    let mut settings =
        read_settings(dir)?.unwrap_or_else(|| serde_json::json!({ "schema_version": 1 }));
    ensure_own_hooks_path(dir, &settings, &hooks_path)?;

    json_config::json_hook_install_with(
        &hooks_path,
        &load_hooks()?,
        &JsonHookInstallSpec {
            read_context: "Failed to read Muse hooks file",
            parse_context: "Muse hooks file is not valid JSON",
            write_context: "Failed to write Muse hooks file",
            mkdir_context: "Failed to create Muse config directory",
            empty_root: EmptyJsonRoot::HooksObject,
        },
        hooks::merge_missing_hook_commands,
    )?;
    wire(&mut settings, &hooks_path)?;
    write_settings(dir, &settings)?;

    Ok(format!("Installed hooks to {}", hooks_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_in(dir: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap()
    }

    fn hook_commands(config: &Value, event: &str) -> Vec<String> {
        config["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|group| group["hooks"].as_array().unwrap())
            .map(|hook| hook["command"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn muse_dir_prefers_xdg_config_home() {
        let home = Some(PathBuf::from("/home/tester"));
        assert_eq!(
            muse_dir_from_env(home.clone(), Some(OsString::from("/xdg"))),
            Some(PathBuf::from("/xdg/muse"))
        );
        assert_eq!(
            muse_dir_from_env(home.clone(), Some(OsString::new())),
            Some(PathBuf::from("/home/tester/.config/muse"))
        );
        assert_eq!(
            muse_dir_from_env(home, None),
            Some(PathBuf::from("/home/tester/.config/muse"))
        );
    }

    #[test]
    fn test_hooks_json_maps_events_to_statuses() {
        let config: Value = serde_json::from_str(HOOKS_JSON).unwrap();
        let expected = [
            ("SessionStart", crate::rebrand!("workmux register-agent")),
            (
                "UserPromptSubmit",
                crate::rebrand!("workmux set-window-status working"),
            ),
            (
                "Notification",
                crate::rebrand!("workmux set-window-status waiting"),
            ),
            (
                "PostToolUse",
                crate::rebrand!("workmux set-window-status working"),
            ),
            ("Stop", crate::rebrand!("workmux set-window-status done")),
            (
                "Interrupt",
                crate::rebrand!("workmux set-window-status done"),
            ),
            (
                "SessionEnd",
                crate::rebrand!("workmux set-window-status done"),
            ),
        ];
        assert_eq!(config["hooks"].as_object().unwrap().len(), expected.len());
        for (event, command) in expected {
            assert_eq!(hook_commands(&config, event), [command], "{event}");
        }
    }

    #[test]
    fn test_interrupt_hook_is_async() {
        // Muse refuses a synchronous Interrupt hook.
        let config: Value = serde_json::from_str(HOOKS_JSON).unwrap();
        assert_eq!(config["hooks"]["Interrupt"][0]["hooks"][0]["async"], true);
    }

    #[test]
    fn test_install_into_missing_dir_wires_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("muse");

        install_dir(&dir).unwrap();

        let settings = settings_in(&dir);
        let hooks_path = hooks_path_in(&dir);
        assert_eq!(settings["schema_version"], 1);
        assert_eq!(settings[HOOKS_PATH_KEY], hooks_path.to_str().unwrap(),);
        let names: Vec<&str> = settings[ENV_VARS_KEY]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert_eq!(names, HOOK_ENV_VARS);
        assert!(names.contains(&"REX_BLOCK") && names.contains(&"TMUX_PANE"));
        assert!(matches!(check_dir(&dir).unwrap(), StatusCheck::Installed));
    }

    #[test]
    fn test_install_is_idempotent_and_keeps_user_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"model":"m","managed_hooks_env_vars":["MY_VAR","TMUX"]}"#,
        )
        .unwrap();

        install_dir(dir).unwrap();
        let settings_first = fs::read_to_string(dir.join("settings.json")).unwrap();
        let hooks_first = fs::read_to_string(hooks_path_in(dir)).unwrap();
        install_dir(dir).unwrap();

        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            settings_first
        );
        assert_eq!(fs::read_to_string(hooks_path_in(dir)).unwrap(), hooks_first);
        let settings = settings_in(dir);
        assert_eq!(settings["model"], "m");
        let names = settings[ENV_VARS_KEY].as_array().unwrap();
        assert_eq!(names[0], "MY_VAR");
        assert_eq!(names.iter().filter(|n| *n == "TMUX").count(), 1);
        assert_eq!(names.len(), HOOK_ENV_VARS.len() + 1);
    }

    #[test]
    fn test_check_reports_update_until_settings_pass_the_environment() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        install_dir(dir).unwrap();

        let mut settings = settings_in(dir);
        settings[ENV_VARS_KEY]
            .as_array_mut()
            .unwrap()
            .retain(|n| n != "REX_BLOCK");
        write_settings(dir, &settings).unwrap();
        assert!(matches!(
            check_dir(dir).unwrap(),
            StatusCheck::UpdateAvailable
        ));

        install_dir(dir).unwrap();
        assert!(matches!(check_dir(dir).unwrap(), StatusCheck::Installed));
    }

    #[test]
    fn test_check_reports_update_for_partial_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        install_dir(dir).unwrap();
        let mut config: Value = serde_json::from_str(HOOKS_JSON).unwrap();
        config["hooks"].as_object_mut().unwrap().remove("Interrupt");
        fs::write(hooks_path_in(dir), config.to_string()).unwrap();

        assert!(matches!(
            check_dir(dir).unwrap(),
            StatusCheck::UpdateAvailable
        ));
    }

    #[test]
    fn test_users_own_managed_hooks_path_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let original = r#"{"schema_version":1,"managed_hooks_path":"/etc/muse/hooks.json"}"#;
        fs::write(dir.join("settings.json"), original).unwrap();

        let error = install_dir(dir).unwrap_err().to_string();

        assert!(error.contains("/etc/muse/hooks.json"), "{error}");
        assert!(check_dir(dir).is_err());
        assert_eq!(
            fs::read_to_string(dir.join("settings.json")).unwrap(),
            original
        );
        assert!(!hooks_path_in(dir).exists());
    }

    #[test]
    fn test_uninstall_removes_hooks_file_and_unwires_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"model":"m","managed_hooks_env_vars":["MY_VAR"]}"#,
        )
        .unwrap();
        install_dir(dir).unwrap();

        let result = uninstall_dir(dir).unwrap();

        assert!(result.contains("no hooks remain"), "{result}");
        assert!(!hooks_path_in(dir).exists());
        assert_eq!(
            settings_in(dir),
            serde_json::json!({"schema_version": 1, "model": "m", "managed_hooks_env_vars": ["MY_VAR"]})
        );
        let again = uninstall_dir(dir).unwrap();
        assert!(again.contains("No Muse"), "{again}");
    }

    #[test]
    fn test_uninstall_keeps_user_hooks_and_wiring() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        install_dir(dir).unwrap();
        let mut config: Value =
            serde_json::from_str(&fs::read_to_string(hooks_path_in(dir)).unwrap()).unwrap();
        config["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "hooks": [{"type": "command", "command": "python3 my-hook.py"}]
            }));
        fs::write(hooks_path_in(dir), config.to_string()).unwrap();

        let result = uninstall_dir(dir).unwrap();

        assert!(
            result.contains(crate::rebrand!("Removed workmux hooks")),
            "{result}"
        );
        let config: Value =
            serde_json::from_str(&fs::read_to_string(hooks_path_in(dir)).unwrap()).unwrap();
        assert_eq!(config["hooks"].as_object().unwrap().len(), 1);
        assert_eq!(hook_commands(&config, "Stop"), ["python3 my-hook.py"]);
        assert!(is_wired(&settings_in(dir), &hooks_path_in(dir)));
    }
}
