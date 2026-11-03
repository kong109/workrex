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
    if manifest.files.is_empty() {
        return match fs::remove_file(path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err.into()),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    }
    fs::write(path, serde_json::to_string_pretty(manifest)?)
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Write `content` to `path`, which held `previous`, recording it first so a
/// failed write never loses what uninstall must restore.
pub fn write(path: &Path, content: &str, previous: Option<String>) -> Result<()> {
    record(path, content, previous)?;
    fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
}

fn record(path: &Path, content: &str, previous: Option<String>) -> Result<()> {
    let manifest_path = manifest_path()?;
    let mut manifest = load(&manifest_path);
    let replaced = match manifest.files.remove(path) {
        // Still this binary's last write, so what that write replaced is the original.
        Some(entry) if previous.as_deref().is_some_and(|p| hash(p) == entry.sha256) => {
            entry.replaced
        }
        _ => previous,
    };
    let sha256 = hash(content);
    manifest
        .files
        .insert(path.to_path_buf(), Entry { sha256, replaced });
    save(&manifest_path, &manifest)
}

/// Undo this binary's write of `path` if the file still holds what it wrote.
///
/// Returns false, leaving the file alone, when this binary did not write it, it
/// was changed since, or upstream's identical skill shares it.
pub fn remove(path: &Path) -> Result<bool> {
    let manifest_path = manifest_path()?;
    let mut manifest = load(&manifest_path);
    let Some(entry) = manifest.files.get(path) else {
        return Ok(false);
    };
    if !fs::read_to_string(path).is_ok_and(|content| hash(&content) == entry.sha256) {
        return Ok(false);
    }
    let dir = path.parent().context("skill file has no directory")?;
    let removed = match &entry.replaced {
        Some(previous) => fs::write(path, previous).map(|()| true)?,
        None if upstream_uses(dir, &entry.sha256) => false,
        None => {
            fs::remove_file(path)?;
            // Files the user keeps beside the skill stay, and so does their directory.
            let _ = fs::remove_dir(dir);
            true
        }
    };
    manifest.files.remove(path);
    save(&manifest_path, &manifest)?;
    Ok(removed)
}

/// Whether upstream workmux's skills are installed beside `dir`, whose file
/// holds the bundled skill hashed `sha256`, and that skill is byte-identical
/// to upstream's: upstream setup then counts this file as its own.
fn upstream_uses(dir: &Path, sha256: &str) -> bool {
    let upstream_installed = dir.with_file_name("workmux").join("SKILL.md").exists();
    upstream_installed
        && crate::skills::BUNDLED_SKILLS
            .iter()
            .any(|skill| hash(skill.content) == sha256 && !skill.content.contains(BIN))
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
        let notes = tmp.path().join("notes/SKILL.md");
        fs::create_dir_all(notes.parent().unwrap()).unwrap();
        fs::write(notes.with_file_name("mine.md"), "user notes").unwrap();
        write(&notes, "bundled", None).unwrap();
        assert!(remove(&notes).unwrap());
        assert!(!notes.exists());
        assert_eq!(
            fs::read_to_string(notes.with_file_name("mine.md")).unwrap(),
            "user notes"
        );
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
        assert!(!tmp.path().join("manifest.json").exists());
    }

    #[test]
    fn rewriting_after_another_writer_restores_that_writer() {
        let tmp = tempfile::tempdir().unwrap();
        use_manifest_in(tmp.path());
        let retaken = tmp.path().join("retaken/SKILL.md");
        let rewritten = tmp.path().join("rewritten/SKILL.md");
        for path in [&retaken, &rewritten] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        write(&retaken, "ours", None).unwrap();
        fs::write(&retaken, "upstream").unwrap();
        write(&retaken, "ours", Some("upstream".into())).unwrap();
        write(&rewritten, "ours", Some("user".into())).unwrap();
        write(&rewritten, "ours", Some("ours".into())).unwrap();

        assert!(remove(&retaken).unwrap());
        assert!(remove(&rewritten).unwrap());

        assert_eq!(fs::read_to_string(&retaken).unwrap(), "upstream");
        assert_eq!(fs::read_to_string(&rewritten).unwrap(), "user");
    }

    #[test]
    fn a_failed_install_never_loses_the_original() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("merge/SKILL.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "upstream").unwrap();
        let unwritable = tmp.path().join("state");
        fs::write(&unwritable, "").unwrap();
        use_manifest_in(&unwritable);
        assert!(write(&path, "ours", Some("upstream".into())).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "upstream");

        use_manifest_in(tmp.path());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(write(&path, "ours", Some("upstream".into())).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let manifest = load(&tmp.path().join("manifest.json"));
        assert_eq!(manifest.files[&path].replaced.as_deref(), Some("upstream"));
    }

    #[test]
    fn uninstall_keeps_a_skill_upstream_installed_identically() {
        let tmp = tempfile::tempdir().unwrap();
        use_manifest_in(tmp.path());
        let base = tmp.path().join("skills");
        let open_pr = crate::skills::BUNDLED_SKILLS
            .iter()
            .find(|s| s.name == "open-pr")
            .unwrap();
        assert!(!open_pr.content.contains(BIN));
        let shared = base.join("open-pr/SKILL.md");
        let upstream = base.join("workmux/SKILL.md");
        for path in [&shared, &upstream] {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
        }
        fs::write(&upstream, "upstream workmux skill").unwrap();
        write(&shared, open_pr.content, None).unwrap();

        crate::skills::remove_skills_at(base.clone(), "Claude Code").unwrap();

        assert_eq!(fs::read_to_string(&shared).unwrap(), open_pr.content);
        assert!(!tmp.path().join("manifest.json").exists());
    }
}
