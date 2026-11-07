//! Muse Code status tracking setup.
//!
//! Detects Muse via its config directory, `$XDG_CONFIG_HOME/muse/` or
//! `~/.config/muse/`. Muse strips the environment of the hooks it runs
//! from `settings.json`, which hides the pane from `set-window-status`.
//! Managed hooks keep the variables `managed_hooks_env_vars` names, so the
//! hooks go in a hooks file of their own, and `settings.json` points
//! `managed_hooks_path` at it and names the variables. A record file beside
//! the hooks lists the names setup added, so uninstall can restore the
//! user's own list exactly.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
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

const HOOKS_JSON: &str = include_str!("../../resources/muse/workrex-status.json");

const ENV_VARS_KEY: &str = "managed_hooks_env_vars";
const HOOKS_PATH_KEY: &str = "managed_hooks_path";

/// Variables `register-agent` and `set-window-status` read to find the
/// agent's pane and the state store.
const HOOK_ENV_VARS: &[&str] = &[
    "REX_SERVER",
    "REX_SESSION",
    "REX_BLOCK",
    "REX_BIN_DIR",
    "WORKREX_BACKEND",
    "WORKREX_STATUS_BACKEND",
    "WORKREX_STATUS_INSTANCE",
    "WORKREX_STATUS_PANE_ID",
    "WORKREX_DISABLE_SET_WINDOW_STATUS",
    "WORKREX_SANDBOX_GUEST",
    "WORKREX_RPC_HOST",
    "WORKREX_RPC_PORT",
    "WORKREX_RPC_TOKEN",
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

fn record_path_in(dir: &Path) -> PathBuf {
    dir.join(concat!(env!("CARGO_BIN_NAME"), "-env-vars.json"))
}

/// What setup added to `managed_hooks_env_vars`.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct EnvVarsRecord {
    added: Vec<String>,
    created_key: bool,
}

pub fn detect() -> Option<&'static str> {
    detect_from_env(home::home_dir(), std::env::var_os("XDG_CONFIG_HOME"))
}

fn detect_from_env(
    home: Option<PathBuf>,
    xdg_config_home: Option<OsString>,
) -> Option<&'static str> {
    let from_xdg = xdg_config_home.as_ref().is_some_and(|dir| !dir.is_empty());
    let dir = muse_dir_from_env(home, xdg_config_home)?;
    dir.is_dir().then_some(if from_xdg {
        "found $XDG_CONFIG_HOME/muse/"
    } else {
        "found ~/.config/muse/"
    })
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
    } else if hooks::has_workrex_hooks(&config) {
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
    let settings_path = dir.join("settings.json");
    let settings_path = settings_path.display();
    let hooks_path_shown = hooks_path.display();
    match configured_hooks_path(settings) {
        Some(path) if path.starts_with('~') => anyhow::bail!(
            "{settings_path} sets {HOOKS_PATH_KEY} to {path}. {BIN} matches only an absolute \
             path: set it to {hooks_path_shown} to use {BIN}'s hooks file, or write your own \
             file's absolute path"
        ),
        Some(path) if Path::new(path) != hooks_path => anyhow::bail!(
            "{settings_path} sets {HOOKS_PATH_KEY} to {path}, and Muse reads only one managed \
             hooks file. Copy {BIN}'s Muse hooks into {path}, or remove the key and run setup \
             again"
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

/// Point settings at the hooks file and pass the hooks' variables. Returns
/// what this call added to `managed_hooks_env_vars`.
fn wire(settings: &mut Value, hooks_path: &Path) -> Result<EnvVarsRecord> {
    let hooks_path = hooks_path
        .to_str()
        .context("Muse hooks path is not valid UTF-8")?;
    let settings = settings
        .as_object_mut()
        .context("Muse settings.json is not an object")?;
    let mut record = EnvVarsRecord {
        created_key: !settings.contains_key(ENV_VARS_KEY),
        ..Default::default()
    };
    let names = settings
        .entry(ENV_VARS_KEY)
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .with_context(|| format!("Muse {ENV_VARS_KEY} is not an array"))?;
    for name in HOOK_ENV_VARS {
        if !names.iter().any(|n| n == name) {
            names.push(Value::from(*name));
            record.added.push(name.to_string());
        }
    }
    settings.insert(HOOKS_PATH_KEY.to_string(), Value::from(hooks_path));
    Ok(record)
}

fn read_record(dir: &Path) -> Result<Option<EnvVarsRecord>> {
    let path = record_path_in(dir);
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&path).context("Failed to read Muse env vars record")?;
    let record = serde_json::from_str(&content).with_context(|| {
        format!(
            "{} is not valid. Delete it and run the command again",
            path.display()
        )
    })?;
    Ok(Some(record))
}

/// Add what one setup run changed to `earlier`, the record of the runs
/// that wired the current settings.
fn save_record(dir: &Path, earlier: Option<EnvVarsRecord>, new: EnvVarsRecord) -> Result<()> {
    let record = match earlier {
        Some(mut record) => {
            for name in new.added {
                if !record.added.contains(&name) {
                    record.added.push(name);
                }
            }
            record.created_key |= new.created_key;
            record
        }
        None if new == EnvVarsRecord::default() => {
            // A record left from an install these settings no longer carry.
            if record_path_in(dir).exists() {
                fs::remove_file(record_path_in(dir))
                    .context("Failed to remove Muse env vars record")?;
            }
            return Ok(());
        }
        None => new,
    };
    fs::write(
        record_path_in(dir),
        serde_json::to_string_pretty(&record)? + "\n",
    )
    .context("Failed to write Muse env vars record")
}

/// Undo [`wire`] once the hooks file is gone: remove the path, and the
/// variable names `record` says setup added. Returns whether anything changed.
fn unwire(settings: &mut Value, hooks_path: &Path, record: Option<&EnvVarsRecord>) -> bool {
    if !configured_hooks_path(settings).is_some_and(|path| Path::new(path) == hooks_path) {
        return false;
    }
    let Some(settings) = settings.as_object_mut() else {
        return false;
    };
    settings.remove(HOOKS_PATH_KEY);
    let Some(record) = record else {
        return true;
    };
    if let Some(names) = settings.get_mut(ENV_VARS_KEY).and_then(Value::as_array_mut) {
        names.retain(|n| !record.added.iter().any(|name| n == name.as_str()));
        if record.created_key && names.is_empty() {
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
    // Read the record first, so a bad one stops uninstall before it removes
    // the hooks file.
    let record = read_record(dir)?;
    let result = json_config::json_hook_uninstall(
        &hooks_path,
        &JsonHookUninstallSpec {
            messages: json_config::JsonHookUninstallMessages {
                file_missing: "No Muse workrex-status.json found",
                not_found: "No workrex hooks found in Muse hooks file",
                soft_read_error: None,
                soft_parse_error: None,
            },
            delete_if_no_hooks_remain: true,
            remove_plugins: false,
            soft_errors: false,
        },
    )?;
    if hooks_path.exists() {
        return Ok(result);
    }
    if let Some(mut settings) = read_settings(dir)?
        && unwire(&mut settings, &hooks_path, record.as_ref())
    {
        write_settings(dir, &settings)?;
    }
    if record.is_some() {
        fs::remove_file(record_path_in(dir)).context("Failed to remove Muse env vars record")?;
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
    // A record describes the settings it was made with. Settings that no
    // longer point at the hooks file start a new record.
    let earlier = match configured_hooks_path(&settings) {
        Some(path) if Path::new(path) == hooks_path => read_record(dir)?,
        _ => None,
    };
    // Wire in memory first, so a settings.json Muse could not use stops
    // setup before it writes anything.
    let added = wire(&mut settings, &hooks_path)?;

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
    save_record(dir, earlier, added)?;
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
            ("SessionStart", "workrex register-agent"),
            ("UserPromptSubmit", "workrex set-window-status working"),
            ("Notification", "workrex set-window-status waiting"),
            ("ToolUseStart", "workrex set-window-status working"),
            ("PostToolUse", "workrex set-window-status working"),
            ("PostToolUseFailure", "workrex set-window-status working"),
            ("Stop", "workrex set-window-status done"),
            ("Interrupt", "workrex set-window-status done"),
            ("SessionEnd", "workrex set-window-status done"),
        ];
        assert_eq!(config["hooks"].as_object().unwrap().len(), expected.len());
        for (event, command) in expected {
            assert_eq!(hook_commands(&config, event), [command], "{event}");
        }
    }

    #[test]
    fn test_interrupt_and_tool_start_hooks_are_async() {
        // Muse refuses synchronous Interrupt and ToolUseStart hooks.
        let config: Value = serde_json::from_str(HOOKS_JSON).unwrap();
        for event in ["Interrupt", "ToolUseStart"] {
            assert_eq!(
                config["hooks"][event][0]["hooks"][0]["async"], true,
                "{event}"
            );
        }
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
        assert!(names.contains(&"REX_BLOCK") && names.contains(&"REX_SESSION"));
        assert!(matches!(check_dir(&dir).unwrap(), StatusCheck::Installed));
    }

    #[test]
    fn test_wiring_passes_rex_pane_variables() {
        let tmp = tempfile::tempdir().unwrap();
        install_dir(tmp.path()).unwrap();
        let settings = settings_in(tmp.path());
        let names = settings[ENV_VARS_KEY].as_array().unwrap();
        for name in [
            "REX_SERVER",
            "REX_SESSION",
            "REX_BLOCK",
            "REX_BIN_DIR",
            "WORKREX_STATUS_BACKEND",
            "WORKREX_STATUS_INSTANCE",
            "WORKREX_STATUS_PANE_ID",
            "XDG_STATE_HOME",
        ] {
            assert!(
                names.iter().any(|n| n == name),
                "{name} is not passed to hooks"
            );
        }
    }

    #[test]
    fn test_detect_requires_the_config_dir_and_names_it() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let xdg = tmp.path().join("xdg");
        assert_eq!(detect_from_env(Some(home.clone()), None), None);
        assert_eq!(detect_from_env(None, None), None);

        fs::create_dir_all(home.join(".config/muse")).unwrap();
        assert_eq!(
            detect_from_env(Some(home.clone()), None),
            Some("found ~/.config/muse/")
        );
        // Muse ignores ~/.config/muse while XDG_CONFIG_HOME is set.
        assert_eq!(
            detect_from_env(Some(home.clone()), Some(xdg.clone().into())),
            None
        );
        fs::create_dir_all(xdg.join("muse")).unwrap();
        assert_eq!(
            detect_from_env(Some(home), Some(xdg.into())),
            Some("found $XDG_CONFIG_HOME/muse/")
        );
    }

    #[test]
    fn test_install_is_idempotent_and_keeps_user_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"model":"m","managed_hooks_env_vars":["MY_VAR","REX_SERVER"]}"#,
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
        let record = read_record(dir).unwrap().unwrap();
        assert!(!record.created_key);
        assert_eq!(record.added.len(), HOOK_ENV_VARS.len() - 1);
        assert!(!record.added.iter().any(|n| n == "REX_SERVER"));
        let settings = settings_in(dir);
        assert_eq!(settings["model"], "m");
        let names = settings[ENV_VARS_KEY].as_array().unwrap();
        assert_eq!(names[0], "MY_VAR");
        assert_eq!(names.iter().filter(|n| *n == "REX_SERVER").count(), 1);
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
    fn test_uninstall_restores_the_users_env_vars_exactly() {
        for original in [
            r#"{"schema_version":1,"managed_hooks_env_vars":["MY_VAR","REX_SERVER","REX_BLOCK"]}"#,
            r#"{"schema_version":1,"managed_hooks_env_vars":[]}"#,
            r#"{"schema_version":1}"#,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path();
            fs::write(dir.join("settings.json"), original).unwrap();
            install_dir(dir).unwrap();
            install_dir(dir).unwrap();

            uninstall_dir(dir).unwrap();

            let original: Value = serde_json::from_str(original).unwrap();
            assert_eq!(settings_in(dir), original);
            assert!(!record_path_in(dir).exists());
        }
    }

    #[test]
    fn test_bad_record_stops_uninstall_before_it_removes_anything() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        install_dir(dir).unwrap();
        let settings = settings_in(dir);
        fs::write(record_path_in(dir), "not json").unwrap();

        let err = uninstall_dir(dir).unwrap_err();

        assert!(
            format!("{err:#}").contains(&record_path_in(dir).display().to_string()),
            "{err:#}"
        );
        assert!(hooks_path_in(dir).exists());
        assert_eq!(settings_in(dir), settings);
    }

    #[test]
    fn test_bad_record_stops_setup_before_it_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        install_dir(dir).unwrap();
        let mut config: Value =
            serde_json::from_str(&fs::read_to_string(hooks_path_in(dir)).unwrap()).unwrap();
        config["hooks"].as_object_mut().unwrap().remove("Stop");
        fs::write(hooks_path_in(dir), config.to_string()).unwrap();
        let hooks = fs::read_to_string(hooks_path_in(dir)).unwrap();
        fs::write(record_path_in(dir), "not json").unwrap();

        let err = install_dir(dir).unwrap_err();

        assert!(
            format!("{err:#}").contains(&record_path_in(dir).display().to_string()),
            "{err:#}"
        );
        assert_eq!(fs::read_to_string(hooks_path_in(dir)).unwrap(), hooks);
    }

    #[test]
    fn test_setup_ignores_a_record_its_settings_no_longer_carry() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(dir.join("settings.json"), r#"{"schema_version":1}"#).unwrap();
        install_dir(dir).unwrap();
        // The user removes setup's changes by hand and keeps the record.
        fs::remove_file(hooks_path_in(dir)).unwrap();
        let own = r#"{"schema_version":1,"managed_hooks_env_vars":["REX_SERVER","MY_VAR"]}"#;
        fs::write(dir.join("settings.json"), own).unwrap();
        install_dir(dir).unwrap();

        uninstall_dir(dir).unwrap();

        assert_eq!(
            settings_in(dir),
            serde_json::from_str::<Value>(own).unwrap()
        );
    }

    #[test]
    fn test_rerun_setup_adds_to_the_record() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"managed_hooks_env_vars":["REX_SERVER"]}"#,
        )
        .unwrap();
        install_dir(dir).unwrap();
        // The user drops the list, REX_SERVER included; the next setup recreates it.
        let mut settings = settings_in(dir);
        settings.as_object_mut().unwrap().remove(ENV_VARS_KEY);
        write_settings(dir, &settings).unwrap();
        install_dir(dir).unwrap();

        uninstall_dir(dir).unwrap();

        assert_eq!(settings_in(dir), serde_json::json!({"schema_version": 1}));
    }

    #[test]
    fn test_install_without_a_record_leaves_the_names_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"managed_hooks_env_vars":["MY_VAR"]}"#,
        )
        .unwrap();
        install_dir(dir).unwrap();
        // An install made before setup kept a record.
        fs::remove_file(record_path_in(dir)).unwrap();
        install_dir(dir).unwrap();
        assert!(!record_path_in(dir).exists());
        let names = settings_in(dir)[ENV_VARS_KEY].clone();

        uninstall_dir(dir).unwrap();

        let settings = settings_in(dir);
        assert_eq!(settings[ENV_VARS_KEY], names);
        assert!(settings.get(HOOKS_PATH_KEY).is_none());
    }

    #[test]
    fn test_uninstall_keeps_names_the_user_adds_after_setup() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        install_dir(dir).unwrap();
        let mut settings = settings_in(dir);
        settings[ENV_VARS_KEY]
            .as_array_mut()
            .unwrap()
            .push("MY_VAR".into());
        write_settings(dir, &settings).unwrap();

        uninstall_dir(dir).unwrap();

        assert_eq!(
            settings_in(dir),
            serde_json::json!({"schema_version": 1, "managed_hooks_env_vars": ["MY_VAR"]})
        );
    }

    #[test]
    fn test_unusable_settings_stop_setup_before_it_writes() {
        for original in [
            "[1]",
            r#"{"schema_version":1,"managed_hooks_env_vars":"REX_SERVER"}"#,
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let dir = tmp.path();
            fs::write(dir.join("settings.json"), original).unwrap();

            assert!(install_dir(dir).is_err(), "{original}");

            assert_eq!(
                fs::read_to_string(dir.join("settings.json")).unwrap(),
                original
            );
            assert!(!hooks_path_in(dir).exists(), "{original}");
            assert!(!record_path_in(dir).exists(), "{original}");
        }
    }

    #[test]
    fn test_tilde_hooks_path_asks_for_an_absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        fs::write(
            dir.join("settings.json"),
            r#"{"schema_version":1,"managed_hooks_path":"~/.config/muse/workrex-status.json"}"#,
        )
        .unwrap();

        let error = install_dir(dir).unwrap_err().to_string();

        assert!(error.contains("absolute"), "{error}");
        assert!(
            error.contains(hooks_path_in(dir).to_str().unwrap()),
            "{error}"
        );
        assert!(!hooks_path_in(dir).exists());
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

        assert!(result.contains("Removed workrex hooks"), "{result}");
        let config: Value =
            serde_json::from_str(&fs::read_to_string(hooks_path_in(dir)).unwrap()).unwrap();
        assert_eq!(config["hooks"].as_object().unwrap().len(), 1);
        assert_eq!(hook_commands(&config, "Stop"), ["python3 my-hook.py"]);
        assert!(is_wired(&settings_in(dir), &hooks_path_in(dir)));
    }
}
