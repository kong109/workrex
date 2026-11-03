//! Manifest of the skill files this binary wrote, so uninstall removes only those.
//!
//! The bundled skills share upstream workmux's names, and some are byte-identical
//! to upstream's copies, so content alone cannot tell whose file a path holds.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::BIN;

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    files: BTreeMap<PathBuf, Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    sha256: String,
    /// What the path held before this binary overwrote it, restored on uninstall.
    replaced: Option<String>,
}

fn hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn manifest_path() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(path) = tests::MANIFEST.with(|m| m.borrow().clone()) {
        return Ok(path);
    }
    Ok(crate::xdg::state_dir()?.join(format!("{BIN}-skills.json")))
}

fn load(path: &Path) -> Manifest {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(path: &Path, manifest: &Manifest) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    fs::write(path, serde_json::to_string_pretty(manifest)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Record that `path` now holds `content` written by this binary, replacing `previous`.
pub fn record(path: &Path, content: &str, previous: Option<String>) -> Result<()> {
    let manifest_path = manifest_path()?;
    let mut manifest = load(&manifest_path);
    // A second overwrite keeps the content first replaced, which is the user's.
    let replaced = match manifest.files.remove(path) {
        Some(entry) => entry.replaced,
        None => previous,
    };
    let sha256 = hash(content);
    manifest
        .files
        .insert(path.to_path_buf(), Entry { sha256, replaced });
    save(&manifest_path, &manifest)
}

/// Undo this binary's write of `path` if the file still holds what it wrote.
///
/// Returns false, leaving the file alone, when this binary did not write it or it
/// was changed since.
pub fn remove(path: &Path) -> Result<bool> {
    let manifest_path = manifest_path()?;
    let mut manifest = load(&manifest_path);
    let Some(entry) = manifest.files.get(path) else {
        return Ok(false);
    };
    if !fs::read_to_string(path).is_ok_and(|content| hash(&content) == entry.sha256) {
        return Ok(false);
    }
    match &entry.replaced {
        Some(previous) => fs::write(path, previous)?,
        None => fs::remove_dir_all(path.parent().context("skill file has no directory")?)?,
    }
    manifest.files.remove(path);
    save(&manifest_path, &manifest)?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        pub(crate) static MANIFEST: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// Point this thread's manifest at a file under `dir`.
    pub(crate) fn use_manifest_in(dir: &Path) {
        MANIFEST.with(|m| *m.borrow_mut() = Some(dir.join("manifest.json")));
    }

    #[test]
    fn remove_undoes_only_unchanged_writes() {
        let tmp = tempfile::tempdir().unwrap();
        use_manifest_in(tmp.path());
        let ours = tmp.path().join("ours/SKILL.md");
        let edited = tmp.path().join("edited/SKILL.md");
        let replaced = tmp.path().join("replaced/SKILL.md");
        let theirs = tmp.path().join("theirs/SKILL.md");
        for path in [&ours, &edited, &replaced, &theirs] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "bundled").unwrap();
        }
        record(&ours, "bundled", None).unwrap();
        record(&edited, "bundled", None).unwrap();
        fs::write(&edited, "user edit").unwrap();
        record(&replaced, "bundled", Some("upstream".into())).unwrap();

        assert!(remove(&ours).unwrap());
        assert!(!remove(&edited).unwrap());
        assert!(remove(&replaced).unwrap());
        assert!(!remove(&theirs).unwrap());

        assert!(!ours.parent().unwrap().exists());
        assert_eq!(fs::read_to_string(&edited).unwrap(), "user edit");
        assert_eq!(fs::read_to_string(&replaced).unwrap(), "upstream");
        assert_eq!(fs::read_to_string(&theirs).unwrap(), "bundled");
    }

    #[test]
    fn uninstall_keeps_upstream_skills_identical_to_bundled() {
        let tmp = tempfile::tempdir().unwrap();
        use_manifest_in(tmp.path());
        let base = tmp.path().join("skills");
        let skill = |name: &str| {
            let skill = crate::skills::BUNDLED_SKILLS
                .iter()
                .find(|s| s.name == name);
            (base.join(name).join("SKILL.md"), skill.unwrap().content)
        };
        let (upstream, upstream_content) = skill("open-pr");
        let (ours, our_content) = skill(BIN);
        for (path, content) in [(&upstream, upstream_content), (&ours, our_content)] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        record(&ours, our_content, None).unwrap();

        crate::skills::remove_skills_at(base.clone(), "Claude Code").unwrap();

        assert_eq!(fs::read_to_string(&upstream).unwrap(), upstream_content);
        assert!(!ours.parent().unwrap().exists());
    }
}
