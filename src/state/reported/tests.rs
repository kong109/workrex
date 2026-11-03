use std::path::PathBuf;
use std::sync::Mutex;

use super::fake_mux::{FakeMux, INSTANCE};
use super::*;
use crate::multiplexer::AgentStatus;

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
    let other_record = record(&other, AgentStatus::Working, 100, None);
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
    assert_eq!(merged.len(), 1);
    merged[0].status
}

#[test]
fn a_record_applies_only_while_the_hook_has_the_status_it_was_merged_against() {
    let (_dir, store) = temp_store();
    let seen = hooked("block:a", AgentStatus::Working, 100);
    store.upsert_agent(&seen).unwrap();
    let done = record(&seen, AgentStatus::Done, 90, Some(HookVersion::of(&seen)));
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

#[test]
fn a_record_merged_against_a_hook_that_is_gone_is_not_read() {
    let (_dir, store) = temp_store();
    let seen = hooked("block:a", AgentStatus::Working, 100);
    store.upsert_agent(&seen).unwrap();
    store_record(
        &store,
        &record(&seen, AgentStatus::Done, 200, Some(HookVersion::of(&seen))),
    );
    let adopted = hooked("block:b", AgentStatus::Done, 200);
    store_record(&store, &record(&adopted, AgentStatus::Done, 200, None));
    store.delete_agent(&seen.pane_key).unwrap();

    let merged = with_reported(&store, store.list_all_agents().unwrap()).unwrap();
    let panes: Vec<_> = merged
        .iter()
        .map(|agent| agent.pane_key.pane_id.as_str())
        .collect();
    assert_eq!(panes, ["block:b"]);
}

#[test]
fn a_record_without_the_hook_it_was_merged_against_is_not_read() {
    let (_dir, store) = temp_store();
    let seen = hooked("block:a", AgentStatus::Working, 100);
    store.upsert_agent(&seen).unwrap();
    let mut old = serde_json::to_value(record(&seen, AgentStatus::Done, 200, None)).unwrap();
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
