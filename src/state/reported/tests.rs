use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::fake_mux::{FakeMux, INSTANCE};
use super::*;
use crate::multiplexer::AgentStatus;
use crate::test_support::{
    ISOLATED_TEST_CANARY, init_repo, is_isolated_child, run_git, run_isolated_test,
};

pub(crate) fn hooked(pane_id: &str, status: AgentStatus, status_ts: u64) -> AgentState {
    AgentState {
        pane_key: PaneKey {
            backend: "rex".to_string(),
            instance: INSTANCE.to_string(),
            pane_id: pane_id.to_string(),
        },
        workdir: PathBuf::from("/repo/wt"),
        status: Some(status),
        status_ts: Some(status_ts),
        activity_ts: Some(status_ts),
        pane_title: None,
        pane_pid: 7,
        command: "claude".to_string(),
        updated_ts: status_ts,
        window_name: Some("wm-feat".to_string()),
        session_name: Some("main".to_string()),
        boot_id: None,
        agent_kind: Some("claude".to_string()),
        agent_session_id: None,
        prompt: None,
        remote: None,
    }
}

fn live(working_dir: &str) -> LivePaneInfo {
    LivePaneInfo {
        pid: Some(7),
        current_command: Some("claude".to_string()),
        working_dir: PathBuf::from(working_dir),
        title: None,
        session: Some("main".to_string()),
        window: Some("wm-feat".to_string()),
        session_id: None,
        window_id: None,
        window_index: None,
    }
}

fn reporting(pane_id: &str, reported: Option<ReportedStatus>) -> FakeMux {
    FakeMux {
        live: HashMap::from([(pane_id.to_string(), live("/repo/wt"))]),
        reported: reported
            .into_iter()
            .map(|reported| (pane_id.to_string(), reported))
            .collect(),
        fresh: HashMap::new(),
        fresh_fails: false,
        during_query: Mutex::new(None),
    }
}

fn working(since: u64) -> ReportedStatus {
    ReportedStatus::Agent {
        status: AgentStatus::Working,
        since,
    }
}

fn shown_status(store: &StateStore, mux: &FakeMux, pane_id: &str) -> Option<AgentStatus> {
    store
        .load_all_reconciled_agents(mux)
        .unwrap()
        .into_iter()
        .find(|agent| agent.pane_id == pane_id)
        .and_then(|agent| agent.status)
}

fn temp_store() -> (tempfile::TempDir, StateStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = StateStore::with_path(dir.path().to_path_buf()).unwrap();
    (dir, store)
}

#[test]
fn a_hook_write_during_a_readers_query_survives() {
    let (dir, store) = temp_store();
    let mut old = hooked("block:a", AgentStatus::Done, 100);
    old.prompt = Some("old prompt".to_string());
    store.upsert_agent(&old).unwrap();
    let mux = reporting("block:a", Some(working(200)));
    let mut new = hooked("block:a", AgentStatus::Working, 150);
    new.prompt = Some("new prompt".to_string());
    let hook_store = StateStore::with_path(dir.path().to_path_buf()).unwrap();
    *mux.during_query.lock().unwrap() =
        Some(Box::new(move || hook_store.upsert_agent(&new).unwrap()));

    assert_eq!(
        shown_status(&store, &mux, "block:a"),
        Some(AgentStatus::Working)
    );

    let stored = store.get_agent(&old.pane_key).unwrap().unwrap();
    assert_eq!(stored.prompt.as_deref(), Some("new prompt"));
    assert_eq!(stored.status_ts, Some(150));
}

#[test]
fn the_hook_status_returns_when_the_report_is_cleared() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Waiting, 100))
        .unwrap();

    let reporting_working = reporting("block:a", Some(working(200)));
    assert_eq!(
        shown_status(&store, &reporting_working, "block:a"),
        Some(AgentStatus::Working)
    );

    let cleared = reporting("block:a", None);
    assert_eq!(
        shown_status(&store, &cleared, "block:a"),
        Some(AgentStatus::Waiting)
    );
}

#[test]
fn readers_that_skip_the_live_query_see_the_last_report_until_the_pane_goes() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Working, 100))
        .unwrap();
    let done = ReportedStatus::Agent {
        status: AgentStatus::Done,
        since: 300,
    };
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(done)))
        .unwrap();

    let merged = with_reported(&store, store.list_all_agents().unwrap()).unwrap();
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].status, Some(AgentStatus::Done));
    assert_eq!(merged[0].status_ts, Some(300));

    let gone = FakeMux::default();
    store.load_all_reconciled_agents(&gone).unwrap();
    assert!(read_records(&store.reported_dir()).unwrap().is_empty());
}

#[test]
fn a_record_of_a_pane_that_stopped_reporting_is_dropped() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Waiting, 100))
        .unwrap();
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(working(200))))
        .unwrap();
    assert_eq!(read_records(&store.reported_dir()).unwrap().len(), 1);

    store
        .load_all_reconciled_agents(&reporting("block:a", Some(ReportedStatus::Idle)))
        .unwrap();

    assert!(read_records(&store.reported_dir()).unwrap().is_empty());
    let merged = with_reported(&store, store.list_all_agents().unwrap()).unwrap();
    assert_eq!(merged[0].status, Some(AgentStatus::Waiting));
}

#[test]
fn a_later_report_is_not_replaced_by_an_earlier_one() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Waiting, 100))
        .unwrap();
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(working(300))))
        .unwrap();
    let slower_reader = ReportedStatus::Agent {
        status: AgentStatus::Done,
        since: 200,
    };
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(slower_reader)))
        .unwrap();

    let records = read_records(&store.reported_dir()).unwrap();
    assert_eq!(records[0].1.status, AgentStatus::Working);
}

#[test]
fn a_reporting_pane_outside_the_current_repository_is_not_adopted() {
    let (_dir, store) = temp_store();
    let mux = FakeMux {
        live: HashMap::from([("block:b".to_string(), live("/elsewhere"))]),
        reported: HashMap::from([("block:b".to_string(), working(200))]),
        fresh: HashMap::new(),
        fresh_fails: false,
        during_query: Mutex::new(None),
    };

    assert!(store.load_all_reconciled_agents(&mux).unwrap().is_empty());
    assert!(store.list_all_agents().unwrap().is_empty());
}

#[test]
fn a_reported_status_replaces_a_stale_one_with_its_time() {
    let mut state = hooked("block:a", AgentStatus::Waiting, 100);

    assert!(apply(&mut state, working(200)));
    assert_eq!(state.status, Some(AgentStatus::Working));
    assert_eq!(state.status_ts, Some(200));
    assert_eq!(state.activity_ts(), Some(200));
}

#[test]
fn an_unchanged_status_keeps_its_hook_timestamp() {
    let mut state = hooked("block:a", AgentStatus::Working, 100);

    assert!(!apply(&mut state, working(200)));
    assert_eq!(state.status_ts, Some(100));
}

#[test]
fn idle_clears_the_hook_status() {
    let mut state = hooked("block:a", AgentStatus::Waiting, 100);

    assert!(apply(&mut state, ReportedStatus::Idle));
    assert_eq!(state.status, None);
    assert_eq!(state.status_ts, None);
}

#[test]
fn a_record_older_than_the_hook_status_does_not_replace_it() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Working, 100))
        .unwrap();
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(working(200))))
        .unwrap();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Done, 300))
        .unwrap();

    let merged = with_reported(&store, store.list_all_agents().unwrap()).unwrap();
    assert_eq!(merged[0].status, Some(AgentStatus::Done));
    assert_eq!(merged[0].status_ts, Some(300));
}

#[test]
fn a_record_from_the_hooks_second_keeps_the_hook_status() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Working, 100))
        .unwrap();
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(working(200))))
        .unwrap();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Done, 200))
        .unwrap();

    let merged = with_reported(&store, store.list_all_agents().unwrap()).unwrap();
    assert_eq!(merged[0].status, Some(AgentStatus::Done));
}

#[test]
fn a_reader_keeps_the_records_of_another_server() {
    let (_dir, store) = temp_store();
    let mut other = hooked("block:a", AgentStatus::Working, 100);
    other.pane_key.instance = "unix:///tmp/other.sock".to_string();
    let dir = store.reported_dir();
    fs::create_dir_all(&dir).unwrap();
    let other_path = dir.join(other.pane_key.to_filename());
    let other_record = record(&other, AgentStatus::Working, 100, None, None);
    fs::write(&other_path, serde_json::to_vec(&other_record).unwrap()).unwrap();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Waiting, 100))
        .unwrap();
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(working(200))))
        .unwrap();
    assert_eq!(read_records(&dir).unwrap().len(), 2);

    store
        .load_all_reconciled_agents(&FakeMux::default())
        .unwrap();

    let records = read_records(&dir).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].1, other_record);
}

fn store_record(store: &StateStore, record: &ReportedRecord) {
    let dir = store.reported_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(record.pane_key.to_filename()),
        serde_json::to_vec(record).unwrap(),
    )
    .unwrap();
}

fn last_done_status(store: &StateStore) -> Option<AgentStatus> {
    let merged = with_reported(store, store.list_all_agents().unwrap()).unwrap();
    assert!(merged.len() <= 1);
    merged.first().and_then(|agent| agent.status)
}

#[test]
fn a_record_applies_only_while_the_hook_has_the_status_it_was_merged_against() {
    let (_dir, store) = temp_store();
    let seen = hooked("block:a", AgentStatus::Working, 100);
    store.upsert_agent(&seen).unwrap();
    let done = record(
        &seen,
        AgentStatus::Done,
        90,
        Some(HookVersion::of(&seen)),
        None,
    );
    store_record(&store, &done);
    assert_eq!(last_done_status(&store), Some(AgentStatus::Done));

    store
        .upsert_agent(&hooked("block:a", AgentStatus::Working, 101))
        .unwrap();
    assert_eq!(last_done_status(&store), Some(AgentStatus::Working));

    store.upsert_agent(&seen).unwrap();
    store_record(
        &store,
        &ReportedRecord {
            hook_seen: None,
            ..done
        },
    );
    assert_eq!(last_done_status(&store), Some(AgentStatus::Working));
}

/// A repository in `dir` with one linked worktree, returned as git lists it.
fn linked_worktree(dir: &Path) -> (PathBuf, PathBuf) {
    let repo = dir.join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let worktree = dir.join("wt");
    run_git(
        &repo,
        &["worktree", "add", "-q", worktree.to_str().unwrap()],
    );
    (
        repo.canonicalize().unwrap(),
        worktree.canonicalize().unwrap(),
    )
}

type Unlink = fn(&Path, &Path);

fn remove_worktree(repo: &Path, worktree: &Path) {
    run_git(
        repo,
        &["worktree", "remove", "--force", worktree.to_str().unwrap()],
    );
}

fn adopted_in(worktree: &Path, workdir: PathBuf) -> ReportedRecord {
    let mut adopted = hooked("block:b", AgentStatus::Done, 200);
    adopted.workdir = workdir;
    record(
        &adopted,
        AgentStatus::Done,
        200,
        None,
        Some(worktree.to_path_buf()),
    )
}

#[test]
fn a_record_merged_against_a_hook_that_is_gone_is_not_read() {
    let (dir, store) = temp_store();
    let (_repo, worktree) = linked_worktree(dir.path());
    let mut seen = hooked("block:a", AgentStatus::Working, 100);
    seen.workdir = worktree.clone();
    store.upsert_agent(&seen).unwrap();
    let hook_seen = Some(HookVersion::of(&seen));
    store_record(
        &store,
        &record(
            &seen,
            AgentStatus::Done,
            200,
            hook_seen,
            Some(worktree.clone()),
        ),
    );
    store_record(&store, &adopted_in(&worktree, worktree.clone()));
    store.delete_agent(&seen.pane_key).unwrap();

    let merged = with_reported(&store, store.list_all_agents().unwrap()).unwrap();
    let panes: Vec<_> = merged
        .iter()
        .map(|agent| agent.pane_key.pane_id.as_str())
        .collect();
    assert_eq!(panes, ["block:b"]);
}

#[test]
fn an_adopted_record_is_read_while_its_worktree_is_linked_though_its_directory_is_gone() {
    let (dir, store) = temp_store();
    let (_repo, worktree) = linked_worktree(dir.path());
    let sub = worktree.join("sub");
    fs::create_dir(&sub).unwrap();
    store_record(&store, &adopted_in(&worktree, sub.clone()));

    fs::remove_dir(&sub).unwrap();

    assert_eq!(last_done_status(&store), Some(AgentStatus::Done));
}

#[test]
fn an_adopted_record_is_not_read_once_its_worktree_is_no_longer_linked() {
    let unlink: [(&str, Unlink); 9] = [
        ("removed", remove_worktree),
        ("moved", |repo, worktree| {
            let to = worktree.with_file_name("moved");
            let args = [
                "worktree",
                "move",
                worktree.to_str().unwrap(),
                to.to_str().unwrap(),
            ];
            run_git(repo, &args);
        }),
        ("unregistered, directory kept", |repo, worktree| {
            fs::remove_file(worktree.join(".git")).unwrap();
            run_git(repo, &["worktree", "prune"]);
        }),
        ("entry gone, .git file kept", |repo, _| {
            fs::remove_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        }),
        ("plain directory at the path", |repo, worktree| {
            remove_worktree(repo, worktree);
            fs::create_dir(worktree).unwrap();
        }),
        ("file at the path", |repo, worktree| {
            remove_worktree(repo, worktree);
            fs::write(worktree, "").unwrap();
        }),
        ("symlink to the main worktree", |repo, worktree| {
            remove_worktree(repo, worktree);
            std::os::unix::fs::symlink(repo, worktree).unwrap();
        }),
        ("symlink to another linked worktree", |repo, worktree| {
            remove_worktree(repo, worktree);
            let other = worktree.with_file_name("other");
            run_git(repo, &["worktree", "add", "-q", other.to_str().unwrap()]);
            std::os::unix::fs::symlink(other, worktree).unwrap();
        }),
        ("dangling symlink", |repo, worktree| {
            remove_worktree(repo, worktree);
            std::os::unix::fs::symlink(repo.join("missing"), worktree).unwrap();
        }),
    ];
    for (case, unlink) in unlink {
        let (dir, store) = temp_store();
        let (repo, worktree) = linked_worktree(dir.path());
        store_record(&store, &adopted_in(&worktree, worktree.clone()));
        assert_eq!(last_done_status(&store), Some(AgentStatus::Done), "{case}");

        unlink(&repo, &worktree);

        assert_eq!(last_done_status(&store), None, "{case}");
    }
}

#[test]
fn an_adopted_record_in_a_main_worktree_is_not_read() {
    let (dir, store) = temp_store();
    let (repo, _worktree) = linked_worktree(dir.path());

    store_record(&store, &adopted_in(&repo, repo.clone()));

    assert_eq!(last_done_status(&store), None);
}

#[test]
fn an_adopted_record_from_before_worktrees_were_stored_is_read_once_rewritten() {
    let (dir, store) = temp_store();
    let (_repo, worktree) = linked_worktree(dir.path());
    let current = adopted_in(&worktree, worktree.clone());
    let mut old = serde_json::to_value(&current).unwrap();
    old.as_object_mut().unwrap().remove("worktree");
    let reported = store.reported_dir();
    fs::create_dir_all(&reported).unwrap();
    fs::write(
        reported.join(current.pane_key.to_filename()),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    assert_eq!(last_done_status(&store), None);

    let mux = FakeMux {
        live: HashMap::from([("block:b".to_string(), live(worktree.to_str().unwrap()))]),
        reported: HashMap::from([(
            "block:b".to_string(),
            ReportedStatus::Agent {
                status: AgentStatus::Done,
                since: 200,
            },
        )]),
        fresh: HashMap::new(),
        fresh_fails: false,
        during_query: Mutex::new(None),
    };
    let mut reports = Reports::new(&mux, INSTANCE);
    reports.shown.push(current);
    reports.persist(&store, &mux.live).unwrap();

    assert_eq!(last_done_status(&store), Some(AgentStatus::Done));
}

#[test]
fn a_record_without_the_hook_it_was_merged_against_is_not_read() {
    let (_dir, store) = temp_store();
    let seen = hooked("block:a", AgentStatus::Working, 100);
    store.upsert_agent(&seen).unwrap();
    let mut old = serde_json::to_value(record(&seen, AgentStatus::Done, 200, None, None)).unwrap();
    old.as_object_mut().unwrap().remove("hook_seen");
    let dir = store.reported_dir();
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join(seen.pane_key.to_filename()),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();

    assert!(read_records(&dir).unwrap().is_empty());
    assert_eq!(last_done_status(&store), Some(AgentStatus::Working));
}

#[test]
fn a_hook_repeating_its_status_keeps_the_record() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Working, 100))
        .unwrap();
    let done = ReportedStatus::Agent {
        status: AgentStatus::Done,
        since: 200,
    };
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(done)))
        .unwrap();
    let mut repeated = hooked("block:a", AgentStatus::Working, 100);
    repeated.updated_ts = 300;
    store.upsert_agent(&repeated).unwrap();

    assert_eq!(last_done_status(&store), Some(AgentStatus::Done));
}

#[test]
fn a_hook_status_change_within_the_merged_second_keeps_the_hook_status() {
    let (_dir, store) = temp_store();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Waiting, 100))
        .unwrap();
    store
        .load_all_reconciled_agents(&reporting("block:a", Some(working(100))))
        .unwrap();
    store
        .upsert_agent(&hooked("block:a", AgentStatus::Done, 100))
        .unwrap();

    assert_eq!(last_done_status(&store), Some(AgentStatus::Done));
}

#[test]
fn an_adopted_record_is_read_while_its_worktree_is_linked_by_relative_paths() {
    let (dir, store) = temp_store();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let worktree = dir.path().join("wt");
    let args = [
        "-c",
        "worktree.useRelativePaths=true",
        "worktree",
        "add",
        "-q",
        worktree.to_str().unwrap(),
    ];
    run_git(&repo, &args);
    let worktree = worktree.canonicalize().unwrap();
    store_record(&store, &adopted_in(&worktree, worktree.clone()));

    assert_eq!(last_done_status(&store), Some(AgentStatus::Done));
}

#[test]
fn a_pane_is_adopted_in_a_worktree_whose_parent_became_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir(&repo).unwrap();
    init_repo(&repo);
    let parent = dir.path().join("worktrees");
    run_git(
        &repo,
        &["worktree", "add", "-q", parent.join("wt").to_str().unwrap()],
    );
    let moved = dir.path().join("moved");
    fs::rename(&parent, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &parent).unwrap();
    let pane_dir = moved.join("wt").canonicalize().unwrap();

    let worktrees = linked_worktrees_in(Some(&repo));

    assert_eq!(containing(&worktrees, &pane_dir), Some(&pane_dir));
}

/// Where an adopted agent's pane is now, given the repository and the
/// worktree it was adopted in.
type PaneDir = fn(&Path, &Path) -> PathBuf;

#[test]
fn a_pruning_reader_drops_an_adopted_record_once_its_pane_leaves_the_worktree() {
    let done = Some(AgentStatus::Done);
    let cases: [(&str, PaneDir, Option<AgentStatus>); 8] = [
        ("worktree root", |_, worktree| worktree.to_path_buf(), done),
        ("subdirectory", |_, worktree| worktree.join("sub"), done),
        (
            "trailing slash",
            |_, worktree| PathBuf::from(format!("{}/", worktree.display())),
            done,
        ),
        (
            "through a symlink",
            |_, worktree| {
                let alias = worktree.with_file_name("alias");
                std::os::unix::fs::symlink(worktree, &alias).unwrap();
                alias.join("sub")
            },
            done,
        ),
        (
            "deleted subdirectory",
            |_, worktree| {
                let sub = worktree.join("sub");
                fs::remove_dir(&sub).unwrap();
                sub
            },
            done,
        ),
        (
            "parent directory",
            |_, worktree| worktree.parent().unwrap().to_path_buf(),
            None,
        ),
        ("main worktree", |repo, _| repo.to_path_buf(), None),
        (
            "sibling sharing the name as a prefix",
            |_, worktree| {
                let sibling = worktree.with_file_name("wt2");
                fs::create_dir(&sibling).unwrap();
                sibling
            },
            None,
        ),
    ];
    for (case, pane_dir, expected) in cases {
        let (dir, store) = temp_store();
        let (repo, worktree) = linked_worktree(dir.path());
        fs::create_dir(worktree.join("sub")).unwrap();
        let adopted = adopted_in(&worktree, worktree.clone());
        store_record(&store, &adopted);

        prune_while_reporting_in(&store, &adopted, &pane_dir(&repo, &worktree));

        assert_eq!(last_done_status(&store), expected, "{case}");
    }
}

#[test]
fn a_pruning_reader_keeps_an_adopted_record_while_its_pane_moves_within_the_worktree() {
    let moves: [(&str, PaneDir); 2] = [
        ("to the worktree root", |_, worktree| worktree.to_path_buf()),
        ("to another subdirectory", |_, worktree| {
            worktree.join("other")
        }),
    ];
    for (case, moved) in moves {
        let (dir, store) = temp_store();
        let (repo, worktree) = linked_worktree(dir.path());
        fs::create_dir(worktree.join("sub")).unwrap();
        fs::create_dir(worktree.join("other")).unwrap();
        let adopted = adopted_in(&worktree, worktree.join("sub"));
        store_record(&store, &adopted);

        prune_while_reporting_in(&store, &adopted, &moved(&repo, &worktree));

        assert_eq!(last_done_status(&store), Some(AgentStatus::Done), "{case}");
    }
}

/// Adoption lists the worktrees of the repository the reader runs in, so the
/// test reruns itself in a child whose working directory is the main
/// worktree. Changing the working directory here would race other tests.
#[test]
fn a_reader_in_the_repository_adopts_only_panes_in_its_linked_worktrees() {
    const TEST_NAME: &str = "state::reported::tests::a_reader_in_the_repository_adopts_only_panes_in_its_linked_worktrees";
    if !is_isolated_child(TEST_NAME) {
        let dir = tempfile::tempdir().unwrap();
        let (repo, _worktree) = linked_worktree(dir.path());
        run_isolated_test(TEST_NAME, &repo, &[]);
        return;
    }

    println!("{ISOLATED_TEST_CANARY}");
    let repo = std::env::current_dir().unwrap();
    let worktree = repo.with_file_name("wt");
    fs::create_dir(repo.join("src")).unwrap();
    fs::create_dir(worktree.join("sub")).unwrap();
    let (_dir, store) = temp_store();
    let panes = [
        ("block:main", repo.clone()),
        ("block:src", repo.join("src")),
        ("block:sub", worktree.join("sub")),
    ];
    let done = ReportedStatus::Agent {
        status: AgentStatus::Done,
        since: 200,
    };
    let mux = FakeMux {
        live: panes
            .iter()
            .map(|(pane, dir)| (pane.to_string(), live(dir.to_str().unwrap())))
            .collect(),
        reported: panes
            .iter()
            .map(|(pane, _)| (pane.to_string(), done))
            .collect(),
        fresh: HashMap::new(),
        fresh_fails: false,
        during_query: Mutex::new(None),
    };

    let adopted: Vec<_> = store
        .load_all_reconciled_agents(&mux)
        .unwrap()
        .into_iter()
        .map(|agent| agent.pane_id)
        .collect();

    assert_eq!(adopted, ["block:sub"]);
    assert_eq!(last_done_status(&store), Some(AgentStatus::Done));
}

/// Run a pruning reader while the adopted agent's pane, in `pane_dir`, still
/// reports the status it was adopted with.
fn prune_while_reporting_in(store: &StateStore, adopted: &ReportedRecord, pane_dir: &Path) {
    let pane = adopted.pane_key.pane_id.clone();
    let mux = FakeMux {
        live: HashMap::from([(pane.clone(), live(pane_dir.to_str().unwrap()))]),
        reported: HashMap::from([(
            pane,
            ReportedStatus::Agent {
                status: adopted.status,
                since: adopted.since,
            },
        )]),
        fresh: HashMap::new(),
        fresh_fails: false,
        during_query: Mutex::new(None),
    };
    store.load_all_reconciled_agents(&mux).unwrap();
}

fn running(command: &str) -> LivePaneInfo {
    LivePaneInfo {
        current_command: Some(command.to_string()),
        ..live("/repo/wt")
    }
}

fn listed(store: &StateStore, mux: &FakeMux, pane_id: &str) -> bool {
    store
        .load_all_reconciled_agents(mux)
        .unwrap()
        .iter()
        .any(|agent| agent.pane_id == pane_id)
}

#[test]
fn an_agent_stored_by_an_older_build_under_its_executable_name_stays_listed() {
    let (_dir, store) = temp_store();
    let mut old = hooked("block:a", AgentStatus::Waiting, 100);
    old.command = "claude.exe".to_string();
    store.upsert_agent(&old).unwrap();
    let mux = FakeMux {
        live: HashMap::from([("block:a".to_string(), running("claude"))]),
        ..FakeMux::default()
    };

    assert!(listed(&store, &mux, "block:a"));
    assert!(store.get_agent(&old.pane_key).unwrap().is_some());
}

#[test]
fn an_agent_stored_by_an_older_build_drops_when_its_shell_returns() {
    let (_dir, store) = temp_store();
    let mut old = hooked("block:a", AgentStatus::Waiting, 100);
    old.command = "claude.exe".to_string();
    store.upsert_agent(&old).unwrap();
    let mux = FakeMux {
        live: HashMap::from([("block:a".to_string(), running("zsh"))]),
        fresh: HashMap::from([("block:a".to_string(), running("zsh"))]),
        ..FakeMux::default()
    };

    assert!(!listed(&store, &mux, "block:a"));
    assert!(store.get_agent(&old.pane_key).unwrap().is_none());
}

#[test]
fn an_agent_that_retitled_itself_after_the_snapshot_stays_listed() {
    let (_dir, store) = temp_store();
    let mut agent = hooked("block:a", AgentStatus::Working, 100);
    agent.command = "agy".to_string();
    agent.agent_kind = Some("agy".to_string());
    store.upsert_agent(&agent).unwrap();
    let mux = FakeMux {
        live: HashMap::from([("block:a".to_string(), running("node"))]),
        fresh: HashMap::from([("block:a".to_string(), running("agy"))]),
        ..FakeMux::default()
    };

    assert!(listed(&store, &mux, "block:a"));
    assert!(store.get_agent(&agent.pane_key).unwrap().is_some());
}

/// The re-read is the agent's last chance; a read that fails does not keep it.
#[test]
fn an_agent_whose_command_changed_drops_when_the_second_read_fails() {
    let (_dir, store) = temp_store();
    let mut agent = hooked("block:a", AgentStatus::Working, 100);
    agent.command = "agy".to_string();
    agent.agent_kind = Some("agy".to_string());
    store.upsert_agent(&agent).unwrap();
    let mux = FakeMux {
        live: HashMap::from([("block:a".to_string(), running("zsh"))]),
        fresh_fails: true,
        ..FakeMux::default()
    };

    assert!(!listed(&store, &mux, "block:a"));
}
