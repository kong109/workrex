//! Folder-trust records for the agents workrex launches.
//!
//! Most agents ask whether to trust a folder the first time they start there,
//! even with permission checks skipped, and wait for an answer nobody is
//! there to give. Workrex created the worktree, so it records the answer in
//! the agent's own trust store before the agent starts.
//!
//! - Claude, Codex and Grok look up the main repository a worktree belongs
//!   to, so the repository gets the entry.
//! - Muse and Antigravity look up the exact folder and ignore a trusted
//!   parent, so each worktree gets an entry, and loses it when workrex
//!   removes the worktree.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

use crate::bin_name::BIN;
use crate::config::{AgentEnvValue, Config};
use crate::multiplexer::agent::{AgentCommand, resolve_selected_agent};

/// The folder an agent looks up when deciding whether it is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The main repository a worktree belongs to.
    Repo,
    /// The worktree itself.
    Worktree,
}

/// Where one agent keeps its trust decisions.
pub struct Store {
    pub kind: &'static str,
    pub key: Key,
    /// Environment variable naming the agent's config directory, and the
    /// directory under it that holds the store.
    dir_env: Option<(&'static str, &'static str)>,
    /// The config directory under the home directory when the variable is
    /// unset.
    home_dir: &'static str,
    pub file: &'static str,
}

const STORES: [Store; 5] = [
    Store {
        kind: "claude",
        key: Key::Repo,
        dir_env: Some(("CLAUDE_CONFIG_DIR", "")),
        home_dir: "",
        file: ".claude.json",
    },
    Store {
        kind: "codex",
        key: Key::Repo,
        dir_env: Some(("CODEX_HOME", "")),
        home_dir: ".codex",
        file: "config.toml",
    },
    Store {
        kind: "grok",
        key: Key::Repo,
        dir_env: Some(("GROK_HOME", "")),
        home_dir: ".grok",
        file: "trusted_folders.toml",
    },
    Store {
        kind: "muse",
        key: Key::Worktree,
        dir_env: Some(("XDG_CONFIG_HOME", "muse")),
        home_dir: ".config/muse",
        file: "trust.json",
    },
    Store {
        kind: "agy",
        key: Key::Worktree,
        dir_env: None,
        home_dir: ".gemini/antigravity-cli",
        file: "settings.json",
    },
];

/// The trust store of the agent `kind`, if it asks.
pub fn store(kind: &str) -> Option<&'static Store> {
    STORES.iter().find(|store| store.kind == kind)
}

impl Store {
    /// The store's file, reading the config-directory override through
    /// `env` (unquoted, with `~` and `~/` expanded, relative to
    /// `working_dir`). An override that needs more shell expansion than
    /// that is an error: the agent reads whatever the shell makes of it,
    /// so neither the literal path nor the default is the right file.
    pub fn path(
        &self,
        env: impl Fn(&str) -> Option<String>,
        working_dir: &Path,
    ) -> Result<PathBuf> {
        let home = home::home_dir().context("Could not determine the home directory")?;
        let from_env = self.dir_env.and_then(|(name, sub)| {
            env(name)
                .filter(|value| !value.is_empty())
                .map(|value| (name, value, sub))
        });
        let dir = match from_env {
            Some((name, value, sub)) => {
                let dir = value.trim_matches(['"', '\'']);
                let dir = if dir == "~" {
                    home
                } else if let Some(rest) = dir.strip_prefix("~/") {
                    home.join(rest)
                } else if dir.starts_with('~') || dir.contains(['$', '`']) {
                    bail!("{name}={value} needs shell expansion; not guessing where it points");
                } else {
                    working_dir.join(dir)
                };
                dir.join(sub)
            }
            None => home.join(self.home_dir),
        };
        Ok(dir.join(self.file))
    }

    /// The folders to mark: the repository or the worktree, as the agent
    /// looks them up. Muse and Antigravity also get the agent's working
    /// directory when it is inside the worktree.
    pub fn dirs(&self, repo: &Path, worktree: &Path, working_dir: &Path) -> Vec<String> {
        match self.key {
            Key::Repo => with_canonical(repo),
            Key::Worktree => {
                let mut dirs = with_canonical(worktree);
                for dir in with_canonical(working_dir) {
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
                dirs
            }
        }
    }

    /// Mark each of `dirs` as trusted in the store at `path`, keeping every
    /// other entry and an explicit "not trusted" decision.
    pub fn trust(&self, path: &Path, dirs: &[String]) -> Result<()> {
        match self.kind {
            "claude" => ensure_claude_trusted(path, dirs, "{}", WriteMode::Replace),
            "codex" => ensure_codex_trusted(path, dirs),
            "grok" => ensure_grok_trusted(path, dirs),
            "muse" => ensure_muse_trusted(path, dirs),
            "agy" => ensure_agy_trusted(path, dirs),
            _ => Ok(()),
        }
    }

    /// Drop the entries for `worktree` and the folders inside it from the
    /// store at `path`. Only stores keyed by worktree hold such entries. A
    /// file that never mentions the worktree is not locked or parsed, so
    /// removing a worktree no other agent ran in touches nothing.
    pub fn forget(&self, path: &Path, worktree: &Path) -> Result<()> {
        if self.key != Key::Worktree {
            return Ok(());
        }
        let roots = with_canonical(worktree);
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => {
                return Err(err).with_context(|| format!("Failed to read {}", path.display()));
            }
        };
        if !roots.iter().any(|root| text.contains(root.as_str())) {
            return Ok(());
        }
        let inside = |dir: &str| roots.iter().any(|root| Path::new(dir).starts_with(root));
        match self.kind {
            "muse" => edit_json(path, "{}", WriteMode::Replace, |object| {
                let Some(projects) = object.get_mut("projects") else {
                    return Ok(false);
                };
                let projects = projects
                    .as_object_mut()
                    .context("`projects` is not an object")?;
                let before = projects.len();
                projects.retain(|dir, _| !inside(dir));
                Ok(projects.len() != before)
            }),
            "agy" => edit_json(path, "{}", WriteMode::Replace, |object| {
                let Some(list) = object.get_mut(AGY_KEY) else {
                    return Ok(false);
                };
                let list = list
                    .as_array_mut()
                    .with_context(|| format!("`{AGY_KEY}` is not a list"))?;
                let before = list.len();
                list.retain(|dir| !dir.as_str().is_some_and(inside));
                Ok(list.len() != before)
            }),
            _ => Ok(()),
        }
    }
}

/// `name` in the environment `command` runs with: from its `env` (which the
/// command line sets last) or its inline assignments, else from `fallback`.
/// An `env` entry taken `from_env` reads that variable from `fallback`.
pub fn agent_env(
    command: Option<&AgentCommand>,
    name: &str,
    fallback: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut name = name.to_string();
    let from_command = command.and_then(|command| match command.env.get(&name) {
        Some(AgentEnvValue::Literal(value)) => Some(value.clone()),
        Some(AgentEnvValue::FromEnv { from_env }) => {
            name = from_env.clone();
            None
        }
        None => {
            let prefix = format!("{name}=");
            command
                .env_assignments
                .iter()
                .rev()
                .find_map(|assignment| assignment.strip_prefix(prefix.as_str()))
                .map(str::to_string)
        }
    });
    from_command
        .or_else(|| fallback(&name))
        .filter(|value| !value.is_empty())
}

/// Before workrex launches `agent` in a local worktree, record that the
/// folder it starts in is trusted. A failure is reported and leaves the file
/// alone; the agent then shows the dialog.
pub fn ensure_local(
    config: &Config,
    agent: Option<&str>,
    repo: &Path,
    worktree: &Path,
    working_dir: &Path,
) {
    let Some(selected) = resolve_selected_agent(config, agent) else {
        return;
    };
    let Some(store) = store(selected.kind()) else {
        return;
    };
    let env = |name: &str| std::env::var(name).ok();
    let result = store
        .path(
            |name| agent_env(Some(&selected.command), name, env),
            working_dir,
        )
        .and_then(|path| store.trust(&path, &store.dirs(repo, worktree, working_dir)));
    if let Err(err) = result {
        eprintln!("{BIN}: could not mark the worktree as trusted: {err:#}");
    }
}

/// Before workrex deletes a local worktree, drop its entries from the stores
/// keyed by worktree. A failure is reported and the removal goes on.
pub fn forget_local(worktree: &Path) {
    forget_everywhere(worktree, |name| std::env::var(name).ok());
}

/// Drop `worktree`'s entries from every store keyed by worktree, at the
/// paths `env` resolves.
pub fn forget_everywhere(worktree: &Path, env: impl Fn(&str) -> Option<String>) {
    for store in STORES.iter().filter(|store| store.key == Key::Worktree) {
        let result = store
            .path(&env, worktree)
            .and_then(|path| store.forget(&path, worktree));
        if let Err(err) = result {
            eprintln!("{BIN}: could not drop the worktree's trust entry: {err:#}");
        }
    }
}

/// `path` as given and, when it differs, with symlinks resolved.
pub fn with_canonical(path: &Path) -> Vec<String> {
    let mut dirs = vec![path.to_string_lossy().into_owned()];
    if let Ok(canonical) = path.canonicalize() {
        let canonical = canonical.to_string_lossy().into_owned();
        if !dirs.contains(&canonical) {
            dirs.push(canonical);
        }
    }
    dirs
}

/// How a changed file is written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    /// A temporary file renamed over the original, so a reader never sees
    /// half of it.
    Replace,
    /// The same file rewritten, for a file bind-mounted on its own, whose
    /// mount would keep showing the old inode after a rename.
    InPlace,
}

/// Mark each of `dirs` as trusted in a Claude config that is bind-mounted
/// into a container (see [`WriteMode::InPlace`]). A missing file starts
/// from `seed`.
pub fn ensure_trusted_in_place(path: &Path, dirs: &[String], seed: &str) -> Result<()> {
    ensure_claude_trusted(path, dirs, seed, WriteMode::InPlace)
}

/// Mark each of `dirs` as trusted in the Claude config at `path`. Reads
/// first and writes only when an entry is missing, keeping every other key.
/// A missing file starts from `seed`. An empty file, or one that is not a
/// JSON object, is left untouched.
fn ensure_claude_trusted(path: &Path, dirs: &[String], seed: &str, mode: WriteMode) -> Result<()> {
    edit_json(path, seed, mode, |object| {
        let projects = object
            .entry("projects")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .context("`projects` is not an object")?;
        let mut changed = false;
        for dir in dirs {
            let entry = projects
                .entry(dir.clone())
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .with_context(|| format!("`projects[{dir:?}]` is not an object"))?;
            if entry.get("hasTrustDialogAccepted") != Some(&Value::Bool(true)) {
                entry.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
                changed = true;
            }
        }
        Ok(changed)
    })
}

/// Give each of `dirs` `decision = "trusted"` under `projects` in Muse's
/// `trust.json`, unless it already has a decision.
fn ensure_muse_trusted(path: &Path, dirs: &[String]) -> Result<()> {
    edit_json(
        path,
        r#"{"schema_version":1}"#,
        WriteMode::Replace,
        |object| {
            let projects = object
                .entry("projects")
                .or_insert_with(|| Value::Object(Map::new()))
                .as_object_mut()
                .context("`projects` is not an object")?;
            let mut changed = false;
            for dir in dirs {
                let entry = projects
                    .entry(dir.clone())
                    .or_insert_with(|| Value::Object(Map::new()))
                    .as_object_mut()
                    .with_context(|| format!("`projects[{dir:?}]` is not an object"))?;
                if !entry.contains_key("decision") {
                    entry.insert("decision".into(), Value::from("trusted"));
                    changed = true;
                }
            }
            Ok(changed)
        },
    )
}

const AGY_KEY: &str = "trustedWorkspaces";

/// Add each of `dirs` to `trustedWorkspaces` in Antigravity's
/// `settings.json`.
fn ensure_agy_trusted(path: &Path, dirs: &[String]) -> Result<()> {
    edit_json(path, "{}", WriteMode::Replace, |object| {
        let list = object
            .entry(AGY_KEY)
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .with_context(|| format!("`{AGY_KEY}` is not a list"))?;
        let mut changed = false;
        for dir in dirs {
            if !list.iter().any(|item| item.as_str() == Some(dir)) {
                list.push(Value::from(dir.as_str()));
                changed = true;
            }
        }
        Ok(changed)
    })
}

/// Apply `edit` to the JSON object in `path` and write it back when it
/// reports a change. A missing file starts from `seed`; a file that is not
/// a JSON object, or whose shape `edit` rejects, is left untouched.
fn edit_json(
    path: &Path,
    seed: &str,
    mode: WriteMode,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<bool>,
) -> Result<()> {
    edit_locked(path, mode, |text| {
        let mut document: Value = serde_json::from_str(text.unwrap_or(seed))
            .with_context(|| format!("{} is not valid JSON; not changing it", path.display()))?;
        let Some(object) = document.as_object_mut() else {
            bail!("{} is not a JSON object; not changing it", path.display());
        };
        let changed = edit(object).with_context(|| {
            format!(
                "{} has an unexpected shape; not changing it",
                path.display()
            )
        })?;
        if !changed {
            return Ok(None);
        }
        let mut text = serde_json::to_string_pretty(&document)?;
        text.push('\n');
        Ok(Some(text))
    })
}

/// Give each of `dirs` `trust_level = "trusted"` under `[projects]` in the
/// Codex config at `path`, unless it already has a trust level (an
/// explicit "untrusted" stays). Reads first and writes only when something
/// is missing, keeping the rest of the file as written. A missing or empty
/// file starts empty; a file that is not valid TOML, or whose `projects`
/// has an unexpected shape, is left untouched.
pub fn ensure_codex_trusted(path: &Path, dirs: &[String]) -> Result<()> {
    edit_toml(path, |document| {
        mark_toml(document, "projects", dirs, |entry| {
            if entry.contains_key("trust_level") {
                return false;
            }
            entry.insert("trust_level", toml_edit::value("trusted"));
            true
        })
    })
}

/// Give each of `dirs` `trusted = true` and the time of the decision under
/// `[folders]` in Grok's `trusted_folders.toml`, unless it already has a
/// decision.
fn ensure_grok_trusted(path: &Path, dirs: &[String]) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let now = i64::try_from(now).unwrap_or(i64::MAX);
    edit_toml(path, |document| {
        mark_toml(document, "folders", dirs, |entry| {
            if entry.contains_key("trusted") {
                return false;
            }
            entry.insert("trusted", toml_edit::value(true));
            if !entry.contains_key("decided_at") {
                entry.insert("decided_at", toml_edit::value(now));
            }
            true
        })
    })
}

/// Apply `edit` to the TOML document in `path`, keeping the rest of the
/// file as written. A missing file starts empty; a file that is not valid
/// TOML, or whose shape `edit` rejects, is left untouched.
fn edit_toml(
    path: &Path,
    edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<bool>,
) -> Result<()> {
    edit_locked(path, WriteMode::Replace, |text| {
        let mut document: toml_edit::DocumentMut = text
            .unwrap_or("")
            .parse()
            .with_context(|| format!("{} is not valid TOML; not changing it", path.display()))?;
        let changed = edit(&mut document).with_context(|| {
            format!(
                "{} has an unexpected shape; not changing it",
                path.display()
            )
        })?;
        Ok(changed.then(|| document.to_string()))
    })
}

/// Run `mark` on the table `[table."<dir>"]` for each of `dirs`, creating
/// what is missing; returns whether any `mark` changed something.
fn mark_toml(
    document: &mut toml_edit::DocumentMut,
    table: &str,
    dirs: &[String],
    mut mark: impl FnMut(&mut dyn toml_edit::TableLike) -> bool,
) -> Result<bool> {
    let entries = document.entry(table).or_insert_with(|| {
        let mut table = toml_edit::Table::new();
        table.set_implicit(true);
        toml_edit::Item::Table(table)
    });
    let entries = entries
        .as_table_like_mut()
        .with_context(|| format!("`{table}` is not a table"))?;
    let mut changed = false;
    for dir in dirs {
        if entries.get(dir).is_none() {
            entries.insert(dir, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        let entry = entries
            .get_mut(dir)
            .and_then(|item| item.as_table_like_mut())
            .with_context(|| format!("`{table}.{dir:?}` is not a table"))?;
        changed |= mark(entry);
    }
    Ok(changed)
}

/// Read `path` (through any symlink) while holding the lock on its
/// directory, and write back what `edit` returns, if anything. `edit` gets
/// `None` for a missing file. An existing file that reads empty is left
/// alone: its agent may be partway through rewriting it.
fn edit_locked(
    path: &Path,
    mode: WriteMode,
    edit: impl FnOnce(Option<&str>) -> Result<Option<String>>,
) -> Result<()> {
    let target = match path.canonicalize() {
        Ok(target) => target,
        Err(_) if path.symlink_metadata().is_ok_and(|meta| meta.is_symlink()) => {
            bail!("{} is a broken symlink; not changing it", path.display());
        }
        Err(_) => path.to_path_buf(),
    };
    let dir = target
        .parent()
        .with_context(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let _lock = lock_dir(dir)?;
    let text = match std::fs::read_to_string(&target) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(err).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    if text.as_deref().is_some_and(|text| text.trim().is_empty()) {
        bail!("{} is empty; not changing it", path.display());
    }
    let Some(text) = edit(text.as_deref())? else {
        return Ok(());
    };
    match mode {
        WriteMode::Replace => replace(&target, &text),
        WriteMode::InPlace => write_in_place(&target, &text),
    }
    .with_context(|| format!("Failed to write {}", path.display()))
}

/// Write `text` to a temporary file beside `target` and rename it over
/// `target`, keeping its permissions (private for a new file).
fn replace(target: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let mode = match std::fs::metadata(target) {
        Ok(meta) => meta.permissions().mode() & 0o7777,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0o600,
        Err(err) => return Err(err),
    };
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let temp = target.with_file_name(format!(".{name}.{BIN}-{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
        std::fs::rename(&temp, target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Rewrite `target` itself, creating it private when missing.
fn write_in_place(target: &Path, text: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(target)?;
    file.write_all(text.as_bytes())?;
    file.set_len(text.len() as u64)?;
    file.sync_all()
}

/// Hold an advisory lock on `dir` so concurrent workrex writers of a file in
/// it take turns. The directory is locked rather than the file because a
/// replaced file is a new inode. Gives up after about two seconds rather
/// than hold up a launch or removal.
fn lock_dir(dir: &Path) -> Result<File> {
    const ATTEMPTS: u32 = 40;
    const PAUSE: std::time::Duration = std::time::Duration::from_millis(50);
    let file = File::open(dir).with_context(|| format!("Failed to open {}", dir.display()))?;
    for attempt in 1..=ATTEMPTS {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if attempt < ATTEMPTS => {
                std::thread::sleep(PAUSE);
            }
            Err(std::fs::TryLockError::WouldBlock) => break,
            Err(std::fs::TryLockError::Error(err)) => {
                return Err(err).with_context(|| format!("Failed to lock {}", dir.display()));
            }
        }
    }
    bail!("{} is locked by another writer; not waiting", dir.display())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    const SEED: &str = r#"{"hasCompletedOnboarding":true}"#;

    fn dirs() -> Vec<String> {
        vec!["/node/projects".to_string()]
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn trusted(value: &Value) -> bool {
        value["projects"]["/node/projects"]["hasTrustDialogAccepted"] == Value::Bool(true)
    }

    fn claude(path: &Path) -> Result<()> {
        store("claude").unwrap().trust(path, &dirs())
    }

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().mode() & 0o7777
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn missing_file_starts_from_the_seed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude-sandbox.json");
        ensure_trusted_in_place(&path, &dirs(), SEED).unwrap();
        let value = read(&path);
        assert!(trusted(&value));
        assert_eq!(value["hasCompletedOnboarding"], Value::Bool(true));
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn existing_empty_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, "  \n").unwrap();
        assert!(claude(&path).is_err());
        assert!(ensure_trusted_in_place(&path, &dirs(), SEED).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "  \n");

        let muse = dir.path().join("trust.json");
        std::fs::write(&muse, "").unwrap();
        assert!(store("muse").unwrap().trust(&muse, &dirs()).is_err());
        assert_eq!(std::fs::read_to_string(&muse).unwrap(), "");

        let codex = dir.path().join("config.toml");
        std::fs::write(&codex, "\n").unwrap();
        assert!(ensure_codex_trusted(&codex, &dirs()).is_err());
        assert_eq!(std::fs::read_to_string(&codex).unwrap(), "\n");
    }

    #[test]
    fn replaces_the_file_keeping_other_keys_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let original = r#"{"oauthAccount":{"x":1},"projects":{"/other":{"allowedTools":["a"]},"/node/projects":{"history":[1]}},"numStartups":3}"#;
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        claude(&path).unwrap();
        let value = read(&path);
        assert!(trusted(&value));
        assert_eq!(value["oauthAccount"]["x"], 1);
        assert_eq!(value["numStartups"], 3);
        assert_eq!(value["projects"]["/other"]["allowedTools"][0], "a");
        assert_eq!(value["projects"]["/node/projects"]["history"][0], 1);
        assert_eq!(mode(&path), 0o640);
        assert_eq!(entries(dir.path()), [".claude.json"]);
    }

    #[test]
    fn in_place_keeps_the_inode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude-sandbox.json");
        std::fs::write(&path, r#"{"numStartups":3}"#).unwrap();
        let inode = std::fs::metadata(&path).unwrap().ino();
        ensure_trusted_in_place(&path, &dirs(), SEED).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        let value = read(&path);
        assert!(trusted(&value));
        assert_eq!(value["numStartups"], 3);
    }

    #[test]
    fn present_entry_means_no_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        let original = "{\"projects\": {\"/node/projects\": {\"hasTrustDialogAccepted\": true}}}";
        std::fs::write(&path, original).unwrap();
        claude(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn invalid_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        for original in ["{not json", "[1, 2]", r#"{"projects": []}"#] {
            std::fs::write(&path, original).unwrap();
            assert!(claude(&path).is_err(), "{original}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        assert_eq!(entries(dir.path()), [".claude.json"]);
    }

    #[test]
    fn writes_through_a_symlinked_dotfile() {
        let dir = tempfile::tempdir().unwrap();
        let dotfiles = dir.path().join("dotfiles");
        std::fs::create_dir(&dotfiles).unwrap();
        let target = dotfiles.join("claude.json");
        std::fs::write(&target, "{}").unwrap();
        let link = dir.path().join(".claude.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        claude(&link).unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(trusted(&read(&target)));
        assert_eq!(entries(&dotfiles), ["claude.json"]);
    }

    #[test]
    fn a_held_lock_gives_up_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(&path, "{}").unwrap();
        let holder = File::open(dir.path()).unwrap();
        holder.lock().unwrap();
        assert!(claude(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}");
        holder.unlock().unwrap();
        claude(&path).unwrap();
        assert!(trusted(&read(&path)));
    }

    const REPO: &str = "/node/projects/app-0123456789ab/repo";

    fn codex_dirs() -> Vec<String> {
        vec![REPO.to_string()]
    }

    fn codex_trust(path: &Path) -> Option<String> {
        let document: toml_edit::DocumentMut =
            std::fs::read_to_string(path).unwrap().parse().unwrap();
        document["projects"][REPO]["trust_level"]
            .as_str()
            .map(str::to_string)
    }

    #[test]
    fn codex_missing_file_gets_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex/config.toml");
        ensure_codex_trusted(&path, &codex_dirs()).unwrap();
        assert_eq!(codex_trust(&path).as_deref(), Some("trusted"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("[projects.\"{REPO}\"]\ntrust_level = \"trusted\"\n")
        );
    }

    #[test]
    fn codex_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "# my settings\nmodel = \"o3\"  # pinned\n\n[features]\nhooks = true\n\n[projects.\"/other\"]\ntrust_level = \"trusted\"\n";
        std::fs::write(&path, original).unwrap();
        ensure_codex_trusted(&path, &codex_dirs()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(original), "{text}");
        assert_eq!(codex_trust(&path).as_deref(), Some("trusted"));

        // Trusted now: nothing to write.
        ensure_codex_trusted(&path, &codex_dirs()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn codex_fills_in_a_project_without_a_trust_level() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, format!("[projects.\"{REPO}\"]\n# note\n")).unwrap();
        ensure_codex_trusted(&path, &codex_dirs()).unwrap();
        assert_eq!(codex_trust(&path).as_deref(), Some("trusted"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("# note"));

        std::fs::write(&path, format!("projects = {{ \"{REPO}\" = {{}} }}\n")).unwrap();
        ensure_codex_trusted(&path, &codex_dirs()).unwrap();
        assert_eq!(codex_trust(&path).as_deref(), Some("trusted"));
    }

    #[test]
    fn codex_keeps_an_explicit_decision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = format!("[projects.\"{REPO}\"]\ntrust_level = \"untrusted\"\n");
        std::fs::write(&path, &original).unwrap();
        ensure_codex_trusted(&path, &codex_dirs()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn codex_invalid_files_are_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let bad = [
            "not = [valid".to_string(),
            "projects = 3\n".to_string(),
            format!("[projects]\n\"{REPO}\" = \"yes\"\n"),
        ];
        for original in bad {
            std::fs::write(&path, &original).unwrap();
            assert!(
                ensure_codex_trusted(&path, &codex_dirs()).is_err(),
                "{original}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
    }

    const WT: &str = "/repos/app/worktrees/feature";

    fn wt_dirs() -> Vec<String> {
        vec![WT.to_string()]
    }

    #[test]
    fn grok_missing_file_gets_a_trusted_folder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grok/trusted_folders.toml");
        let grok = store("grok").unwrap();
        grok.trust(&path, &codex_dirs()).unwrap();
        let document: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(document["folders"][REPO]["trusted"].as_bool(), Some(true));
        assert!(
            document["folders"][REPO]["decided_at"]
                .as_integer()
                .unwrap()
                > 0
        );
    }

    #[test]
    fn grok_keeps_the_file_and_an_explicit_decision() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted_folders.toml");
        let original = "# folders\n[folders.\"/other\"]\ntrusted = false\ndecided_at = 7\n";
        std::fs::write(&path, original).unwrap();
        let grok = store("grok").unwrap();
        grok.trust(&path, &codex_dirs()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with(original), "{text}");
        assert!(text.contains(&format!("[folders.\"{REPO}\"]\ntrusted = true\n")));

        grok.trust(&path, &codex_dirs()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        grok.trust(&path, &["/other".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn muse_missing_file_gets_the_minimal_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("muse/trust.json");
        store("muse").unwrap().trust(&path, &wt_dirs()).unwrap();
        assert_eq!(
            read(&path),
            serde_json::json!({
                "schema_version": 1,
                "projects": { WT: { "decision": "trusted" } }
            })
        );
    }

    #[test]
    fn muse_keeps_other_entries_and_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        std::fs::write(
            &path,
            r#"{"schema_version":1,"extra":true,"projects":{"/other":{"decision":"untrusted","at":3}}}"#,
        )
        .unwrap();
        let muse = store("muse").unwrap();
        muse.trust(&path, &wt_dirs()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let value = read(&path);
        assert_eq!(value["extra"], true);
        assert_eq!(value["projects"]["/other"]["decision"], "untrusted");
        assert_eq!(value["projects"]["/other"]["at"], 3);
        assert_eq!(value["projects"][WT]["decision"], "trusted");

        muse.trust(&path, &wt_dirs()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        muse.trust(&path, &["/other".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn agy_adds_the_worktree_and_keeps_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"model":"x","trustedWorkspaces":["/other"],"ui":{"theme":"dark"}}"#,
        )
        .unwrap();
        let agy = store("agy").unwrap();
        agy.trust(&path, &wt_dirs()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            read(&path),
            serde_json::json!({
                "model": "x",
                "trustedWorkspaces": ["/other", WT],
                "ui": { "theme": "dark" }
            })
        );

        agy.trust(&path, &wt_dirs()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn agy_missing_file_gets_the_minimal_shape() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("antigravity-cli/settings.json");
        store("agy").unwrap().trust(&path, &wt_dirs()).unwrap();
        assert_eq!(
            read(&path),
            serde_json::json!({ "trustedWorkspaces": [WT] })
        );
    }

    #[test]
    fn forget_drops_the_worktree_and_folders_inside_it_only() {
        let dir = tempfile::tempdir().unwrap();
        let muse_path = dir.path().join("trust.json");
        let agy_path = dir.path().join("settings.json");
        let keep = ["/repos/app", "/repos/app/worktrees/feature-2"];
        let all: Vec<String> = keep
            .iter()
            .map(|dir| dir.to_string())
            .chain([WT.to_string(), format!("{WT}/sub")])
            .collect();
        let muse = store("muse").unwrap();
        let agy = store("agy").unwrap();
        muse.trust(&muse_path, &all).unwrap();
        agy.trust(&agy_path, &all).unwrap();

        muse.forget(&muse_path, Path::new(WT)).unwrap();
        agy.forget(&agy_path, Path::new(WT)).unwrap();
        let muse_value = read(&muse_path);
        let projects = muse_value["projects"].as_object().unwrap();
        assert_eq!(projects.keys().collect::<Vec<_>>(), keep);
        assert_eq!(muse_value["schema_version"], 1);
        assert_eq!(
            read(&agy_path),
            serde_json::json!({ "trustedWorkspaces": keep })
        );

        let text = std::fs::read_to_string(&muse_path).unwrap();
        muse.forget(&muse_path, Path::new(WT)).unwrap();
        assert_eq!(std::fs::read_to_string(&muse_path).unwrap(), text);
    }

    #[test]
    fn forget_leaves_missing_files_and_repo_stores_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.json");
        store("muse").unwrap().forget(&path, Path::new(WT)).unwrap();
        assert!(!path.exists());

        let claude = dir.path().join(".claude.json");
        let original = format!("{{\"projects\":{{\"{WT}\":{{\"hasTrustDialogAccepted\":true}}}}}}");
        std::fs::write(&claude, &original).unwrap();
        store("claude")
            .unwrap()
            .forget(&claude, Path::new(WT))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), original);
    }

    #[test]
    fn store_paths_follow_the_config_dir_overrides() {
        let home = home::home_dir().unwrap();
        let wd = Path::new("/work/tree");
        let none = |_: &str| None::<String>;
        let path = |kind: &str, env: &dyn Fn(&str) -> Option<String>| {
            store(kind).unwrap().path(env, wd).unwrap()
        };
        assert_eq!(path("claude", &none), home.join(".claude.json"));
        assert_eq!(path("codex", &none), home.join(".codex/config.toml"));
        assert_eq!(path("grok", &none), home.join(".grok/trusted_folders.toml"));
        assert_eq!(path("muse", &none), home.join(".config/muse/trust.json"));
        assert_eq!(
            path("agy", &none),
            home.join(".gemini/antigravity-cli/settings.json")
        );

        let set = |name: &str| {
            Some(match name {
                "XDG_CONFIG_HOME" => "/xdg".to_string(),
                "GROK_HOME" => "~/g".to_string(),
                "CLAUDE_CONFIG_DIR" => "'rel'".to_string(),
                _ => "/ignored".to_string(),
            })
        };
        assert_eq!(path("muse", &set), Path::new("/xdg/muse/trust.json"));
        assert_eq!(path("grok", &set), home.join("g/trusted_folders.toml"));
        assert_eq!(path("claude", &set), wd.join("rel/.claude.json"));
        assert_eq!(
            path("agy", &set),
            home.join(".gemini/antigravity-cli/settings.json")
        );
        let tilde = |_: &str| Some("~".to_string());
        assert_eq!(path("muse", &tilde), home.join("muse/trust.json"));
    }

    #[test]
    fn overrides_needing_shell_expansion_resolve_to_no_store() {
        let wd = Path::new("/work/tree");
        for value in ["$HOME/.config", "${XDG}/x", "~other/.config", "`pwd`/x"] {
            let env = |_: &str| Some(value.to_string());
            assert!(store("muse").unwrap().path(env, wd).is_err(), "{value}");
        }
    }

    #[test]
    fn forget_skips_files_that_never_mention_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let jsonc =
            "// comments make this invalid JSON\n{\"trustedWorkspaces\": [\"/elsewhere\"]}\n";
        std::fs::write(&path, jsonc).unwrap();
        store("agy").unwrap().forget(&path, Path::new(WT)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), jsonc);

        let mentions = format!("// note\n{{\"trustedWorkspaces\": [\"{WT}\"]}}\n");
        std::fs::write(&path, &mentions).unwrap();
        assert!(store("agy").unwrap().forget(&path, Path::new(WT)).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mentions);
    }

    #[test]
    fn worktree_stores_mark_the_worktree_and_repo_stores_the_repo() {
        let repo = Path::new("/nonexistent/repo");
        let wt = Path::new("/nonexistent/wt");
        let sub = Path::new("/nonexistent/wt/pkg");
        assert_eq!(
            store("grok").unwrap().dirs(repo, wt, sub),
            vec!["/nonexistent/repo"]
        );
        assert_eq!(
            store("muse").unwrap().dirs(repo, wt, wt),
            vec!["/nonexistent/wt"]
        );
        assert_eq!(
            store("agy").unwrap().dirs(repo, wt, sub),
            vec!["/nonexistent/wt", "/nonexistent/wt/pkg"]
        );
    }

    #[test]
    fn ensure_local_uses_the_agent_env_override() {
        let dir = tempfile::tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        let repo = dir.path().join("repo");
        let wt = dir.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        let config: Config = serde_yaml::from_str(&format!(
            "agents:\n  m:\n    command: muse\n    type: muse\n    env:\n      XDG_CONFIG_HOME: {}\n",
            xdg.display()
        ))
        .unwrap();
        ensure_local(&config, Some("m"), &repo, &wt, &wt);
        let value = read(&xdg.join("muse/trust.json"));
        for dir in with_canonical(&wt) {
            assert_eq!(value["projects"][dir.as_str()]["decision"], "trusted");
        }
    }
}
