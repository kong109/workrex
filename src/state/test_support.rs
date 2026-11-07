use tempfile::TempDir;

use crate::state::{PaneKey, StateStore};

pub(crate) fn temp_store() -> (StateStore, TempDir) {
    let dir = TempDir::new().unwrap();
    let store = StateStore::with_path(dir.path().to_path_buf()).unwrap();
    (store, dir)
}

pub(crate) fn rex_pane_key(pane_id: &str) -> PaneKey {
    PaneKey {
        backend: "rex".into(),
        instance: "default".into(),
        pane_id: pane_id.into(),
    }
}

pub(crate) fn default_pane_key() -> PaneKey {
    rex_pane_key("%1")
}
