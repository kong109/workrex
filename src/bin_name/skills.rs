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

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    files: BTreeMap<PathBuf, Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    sha256: String,
}

fn hash(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}

fn manifest_path() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(path) = tests::MANIFEST.with(|m| m.borrow().clone()) {
        return Ok(path);
    }
    Ok(super::state_dir()?.join("skills.json"))
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

/// Write `content` to `path`. Each write records the path and the sha256 of the
/// text written. The record comes first, so a write it cannot record never happens.
pub fn write(path: &Path, content: &str) -> Result<()> {
    let manifest_path = manifest_path()?;
    let mut manifest = load(&manifest_path);
    let entry = Entry {
        sha256: hash(content),
    };
    manifest.files.insert(path.to_path_buf(), entry);
    save(&manifest_path, &manifest)?;
    fs::write(path, content).with_context(|| format!("Failed to write {}", path.display()))
}

/// Uninstall deletes a skill file only when the manifest records it and the file
/// still holds the recorded text. It then removes the skill's directory if that
/// directory is empty. It restores nothing and touches no other file. A recorded
/// path that is missing or changed loses its record, so a full uninstall leaves no
/// manifest behind. Returns whether it deleted the file.
pub fn remove(path: &Path) -> Result<bool> {
    let manifest_path = manifest_path()?;
    let mut manifest = load(&manifest_path);
    let Some(entry) = manifest.files.remove(path) else {
        return Ok(false);
    };
    let unchanged = fs::read_to_string(path).is_ok_and(|content| hash(&content) == entry.sha256);
    if unchanged {
        fs::remove_file(path)?;
        if let Some(dir) = path.parent() {
            let _ = fs::remove_dir(dir);
        }
    }
    save(&manifest_path, &manifest)?;
    Ok(unchanged)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::skills::BUNDLED_SKILLS;
    use std::cell::RefCell;

    thread_local! {
        pub(crate) static MANIFEST: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }

    /// Point this thread's manifest at a file under `dir`.
    pub(crate) fn use_manifest_in(dir: &Path) {
        MANIFEST.with(|m| *m.borrow_mut() = Some(dir.join("manifest.json")));
    }

    struct Home {
        tmp: tempfile::TempDir,
    }

    impl Home {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            use_manifest_in(tmp.path());
            Self { tmp }
        }

        fn skills(&self) -> PathBuf {
            self.tmp.path().join("skills")
        }

        fn skill(&self, name: &str) -> PathBuf {
            self.skills().join(name).join("SKILL.md")
        }

        fn put(&self, name: &str, text: &str) {
            let path = self.skill(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }

        fn setup(&self, answer: bool) {
            crate::skills::install_skills_at(self.skills(), "Claude Code", |_| Ok(answer)).unwrap();
        }

        fn uninstall(&self) -> String {
            let summary = crate::skills::remove_skills_at(self.skills(), "Claude Code").unwrap();
            summary.rsplit(": ").next().unwrap().to_string()
        }

        fn manifest(&self) -> PathBuf {
            self.tmp.path().join("manifest.json")
        }
    }

    fn bundled(name: &str) -> &'static str {
        BUNDLED_SKILLS
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .content
    }

    #[test]
    fn a_written_skill_is_removed_with_its_empty_directory() {
        let home = Home::new();
        home.setup(false);
        assert!(home.manifest().exists());

        assert_eq!(home.uninstall(), "6 removed");

        for skill in BUNDLED_SKILLS {
            assert!(!home.skill(skill.name).parent().unwrap().exists());
        }
        assert!(!home.manifest().exists());
    }

    #[test]
    fn a_written_skill_the_user_edited_stays() {
        let home = Home::new();
        home.setup(false);
        home.put("merge", "user edit");

        assert_eq!(home.uninstall(), "5 removed, 1 kept");

        assert_eq!(
            fs::read_to_string(home.skill("merge")).unwrap(),
            "user edit"
        );
        assert!(!home.manifest().exists());
    }

    #[test]
    fn a_path_already_holding_the_bundled_text_is_not_recorded() {
        let home = Home::new();
        home.put("open-pr", bundled("open-pr"));
        home.setup(false);

        assert_eq!(home.uninstall(), "5 removed, 1 kept");

        assert_eq!(
            fs::read_to_string(home.skill("open-pr")).unwrap(),
            bundled("open-pr")
        );
    }

    #[test]
    fn a_recorded_skill_that_is_gone_loses_its_record() {
        let home = Home::new();
        home.setup(false);
        fs::remove_dir_all(home.skill("merge").parent().unwrap()).unwrap();

        assert_eq!(home.uninstall(), "5 removed");

        assert!(!home.manifest().exists());
    }

    #[test]
    fn a_user_file_beside_the_skill_keeps_its_directory() {
        let home = Home::new();
        home.setup(false);
        let notes = home.skill("merge").with_file_name("notes.md");
        fs::write(&notes, "user notes").unwrap();

        assert_eq!(home.uninstall(), "6 removed");

        assert!(!home.skill("merge").exists());
        assert_eq!(fs::read_to_string(notes).unwrap(), "user notes");
    }

    #[test]
    fn an_overwrite_answered_yes_is_deleted_and_not_restored() {
        let home = Home::new();
        home.put("merge", "upstream text");
        home.setup(true);
        assert_eq!(
            fs::read_to_string(home.skill("merge")).unwrap(),
            bundled("merge")
        );

        assert_eq!(home.uninstall(), "6 removed");

        assert!(!home.skill("merge").parent().unwrap().exists());
    }

    #[test]
    fn an_overwrite_answered_no_leaves_the_file_alone() {
        let home = Home::new();
        home.put("merge", "upstream text");
        home.setup(false);

        assert_eq!(home.uninstall(), "5 removed, 1 kept");

        assert_eq!(
            fs::read_to_string(home.skill("merge")).unwrap(),
            "upstream text"
        );
    }

    #[test]
    fn a_manifest_that_remembers_replaced_text_still_loads() {
        let home = Home::new();
        home.put("merge", bundled("merge"));
        let old = serde_json::json!({ "files": { home.skill("merge").to_str().unwrap(): {
            "sha256": hash(bundled("merge")),
            "replaced": "upstream text",
        }}});
        fs::write(home.manifest(), old.to_string()).unwrap();

        assert_eq!(home.uninstall(), "1 removed");

        assert!(!home.skill("merge").exists());
        assert!(!home.manifest().exists());
    }

    #[test]
    fn a_write_it_cannot_record_never_happens() {
        let tmp = tempfile::tempdir().unwrap();
        let unwritable = tmp.path().join("state");
        fs::write(&unwritable, "").unwrap();
        use_manifest_in(&unwritable);
        let path = tmp.path().join("SKILL.md");

        assert!(write(&path, "ours").is_err());

        assert!(!path.exists());
    }
}
