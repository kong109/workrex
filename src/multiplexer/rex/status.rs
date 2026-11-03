//! Agent status from the Rex Program Status Protocol (OSC 7501), and status
//! icons in window labels.

use anyhow::Result;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tracing::warn;

use super::events::Event;
use super::{BlockId, ProcessEntry, ProcessReport, RexBackend, found};
use crate::multiplexer::{AgentStatus, ReportedStatus};

/// The one table from Rex program states to the status workmux shows (`None`
/// shows none), with each state's urgency when a block holds several records.
/// Unknown states count as idle, as the `program_status` schema says.
fn shown_status(state: &str) -> (Option<AgentStatus>, u8) {
    match state {
        "blocked" => (Some(AgentStatus::Waiting), 4),
        "working" => (Some(AgentStatus::Working), 3),
        // AgentStatus has no failed state, so an error shows as done.
        "error" => (Some(AgentStatus::Done), 2),
        "done" => (Some(AgentStatus::Done), 1),
        _ => (None, 0),
    }
}

#[derive(Debug, Default, Deserialize)]
struct ProgramStatusReport {
    #[serde(default)]
    records: Vec<ProgramRecord>,
}

#[derive(Debug, Deserialize)]
struct ProgramRecord {
    state: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    owner: Option<RecordOwner>,
}

#[derive(Debug, Deserialize)]
struct RecordOwner {
    pid: u32,
    #[serde(default)]
    name: Option<String>,
}

impl ProgramStatusReport {
    /// Rex keeps `done`, `error` and `idle` records after their program
    /// exits. Only the program in the foreground speaks for the agent now.
    /// The name matters too: a shell that `exec`s keeps the agent's pid.
    fn owned_by_foreground(mut self, foreground: Option<(u32, &str)>) -> Self {
        self.records.retain(|record| {
            record.owner.as_ref().is_none_or(|owner| {
                foreground.is_some_and(|(pid, name)| {
                    owner.pid == pid && owner.name.as_deref().is_none_or(|owner| owner == name)
                })
            })
        });
        self
    }

    /// The most urgent record decides; among equals, the latest report.
    fn reported(&self) -> Option<ReportedStatus> {
        self.records
            .iter()
            .map(|record| {
                let (status, urgency) = shown_status(&record.state);
                let since = unix_seconds(&record.updated_at).unwrap_or_else(now);
                (urgency, since, status)
            })
            .max_by_key(|&(urgency, since, _)| (urgency, since))
            .map(|(_, since, status)| match status {
                Some(status) => ReportedStatus::Agent { status, since },
                None => ReportedStatus::Idle,
            })
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// Parse an RFC 3339 timestamp such as `2026-11-02T15:22:36.943312-08:00`.
fn unix_seconds(timestamp: &str) -> Option<u64> {
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let digits = timestamp.get(range)?;
        digits
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let zone = timestamp
        .get(19..)?
        .trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match zone {
        "Z" | "z" => 0,
        _ => {
            let sign = match zone.get(..1)? {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            let (hours, minutes) = zone.get(1..)?.split_once(':')?;
            sign * (hours.parse::<i64>().ok()? * 3600 + minutes.parse::<i64>().ok()? * 60)
        }
    };
    // Days since 1970-01-01 in the proleptic Gregorian calendar.
    let (y, m) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400;
    let day_of_year = (153 * m + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second - offset).ok()
}

/// Separates a status icon from the window name. A plain space would be
/// ambiguous, because window prefixes like the nerdfont one end in a space.
const ICON_SEPARATOR: char = '\u{2002}';

/// A window label without the status icon workmux put in front of it.
pub(super) fn base_label(label: &str) -> &str {
    label
        .split_once(ICON_SEPARATOR)
        .map_or(label, |(_, name)| name)
}

pub(super) fn deserialize_base_label<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let label = String::deserialize(deserializer)?;
    Ok(base_label(&label).to_string())
}

fn label_with_icon(name: &str, icon: Option<&str>) -> String {
    match icon.filter(|icon| !icon.is_empty()) {
        Some(icon) => format!("{icon}{ICON_SEPARATOR}{name}"),
        None => name.to_string(),
    }
}

/// A `program_status` call made beside the block's `process` call.
pub(super) struct ProgramStatusCall(Result<Option<ProgramStatusReport>>);

impl ProgramStatusCall {
    /// What `block` reported, given its foreground process, or `None` when
    /// the call failed. A block that closed reports nothing.
    fn reported(self, block: &BlockId, foreground: Option<&ProcessEntry>) -> Option<Entry> {
        match self.0 {
            Ok(report) => {
                let report = report.unwrap_or_default();
                let recorded = !report.records.is_empty();
                let foreground = foreground.map(|fg| (fg.pid, fg.name.as_str()));
                let reported = report.owned_by_foreground(foreground).reported();
                Some(Entry { reported, recorded })
            }
            Err(error) => {
                warn!(block = block.as_str(), %error, "rex: program_status failed");
                None
            }
        }
    }
}

/// What each block reported, as its backend last read it.
#[derive(Debug, Default)]
pub(crate) struct ProgramStatuses(Mutex<Statuses>);

#[derive(Debug, Default)]
struct Statuses {
    /// Set while an event stream drops a block's entry each time the block
    /// may have changed, so every read is kept. Otherwise only a pane scan's
    /// reads are kept, for a command that scans again before it reads again.
    followed: bool,
    /// Counts drops, so a read that overlapped one is not kept.
    drops: u64,
    reported: HashMap<BlockId, Entry>,
}

/// What one block reported.
#[derive(Debug, Clone, Copy)]
struct Entry {
    reported: Option<ReportedStatus>,
    /// Whether Rex held any record for the block. A new foreground process
    /// changes only what a record says, so it leaves an entry without one.
    recorded: bool,
}

/// How a read reached a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Read {
    /// Part of a scan of every pane.
    Scan,
    /// A look at one block.
    Look,
}

/// A read of one block's status, begun before its calls.
pub(super) struct Reading {
    read: Read,
    drops: u64,
}

impl ProgramStatuses {
    fn lock(&self) -> std::sync::MutexGuard<'_, Statuses> {
        self.0.lock().expect("rex program status cache poisoned")
    }

    fn get(&self, block: &BlockId) -> Option<Option<ReportedStatus>> {
        self.lock().reported.get(block).map(|entry| entry.reported)
    }

    pub(super) fn begin(&self, read: Read) -> Reading {
        Reading {
            read,
            drops: self.lock().drops,
        }
    }

    fn keep(&self, block: &BlockId, reading: Reading, entry: Entry) {
        let mut statuses = self.lock();
        if statuses.drops == reading.drops && (statuses.followed || reading.read == Read::Scan) {
            statuses.reported.insert(block.clone(), entry);
        }
    }

    /// Start or stop following events. Either way, drop every entry and
    /// every read under way: an entry holds only while one stream follows it.
    pub(crate) fn follow(&self, followed: bool) {
        self.lock().followed = followed;
        self.expire();
    }

    /// Drop every entry and every read under way. Rex does not announce
    /// every change: a program that execs another keeps its pid and sends no
    /// `process_changed`, and a change made before a new stream subscribes
    /// reaches no stream. Expiring on a period bounds how long either lasts.
    pub(crate) fn expire(&self) {
        let mut statuses = self.lock();
        statuses.drops += 1;
        statuses.reported.clear();
    }

    /// Drop the entry of a block whose status `event` may have changed, and
    /// every read under way. A new foreground process leaves an entry read
    /// from no records, and the reads under way, alone.
    pub(crate) fn apply(&self, event: &Event) {
        let (Event::ProgramStatusChanged { block_id, .. }
        | Event::ProgramStatusRemoved { block_id, .. }
        | Event::ProcessChanged { block_id, .. }
        | Event::ChildExited { block_id, .. }
        | Event::BlockClosed { block_id, .. }) = event
        else {
            return;
        };
        let mut statuses = self.lock();
        let block = BlockId(block_id.clone());
        let unrecorded = statuses
            .reported
            .get(&block)
            .is_some_and(|entry| !entry.recorded);
        if unrecorded && matches!(event, Event::ProcessChanged { .. }) {
            return;
        }
        statuses.drops += 1;
        statuses.reported.remove(&block);
    }
}

impl RexBackend {
    pub(super) fn query_program_status(&self, block: &BlockId) -> ProgramStatusCall {
        ProgramStatusCall(found(self.block_call::<ProgramStatusReport>(
            block,
            "program_status",
            json!({}),
        )))
    }

    /// What `block` reported, given its foreground process, kept when
    /// `reading` allows. A block that closed reports nothing, and a failed
    /// call keeps nothing.
    pub(super) fn observe_program_status(
        &self,
        block: &BlockId,
        reading: Reading,
        call: ProgramStatusCall,
        foreground: Option<&ProcessEntry>,
    ) -> Option<ReportedStatus> {
        let entry = call.reported(block, foreground)?;
        self.program_status.keep(block, reading, entry);
        entry.reported
    }

    /// What this backend keeps for `pane_id`, or else what Rex says now.
    pub(super) fn program_status_of(&self, pane_id: &str) -> Option<ReportedStatus> {
        let block = BlockId::parse(pane_id).ok()?;
        if let Some(reported) = self.program_status.get(&block) {
            return reported;
        }
        let reading = self.program_status.begin(Read::Look);
        let process = found(self.block_call::<ProcessReport>(&block, "process", json!({})))
            .ok()
            .flatten()
            .filter(|process| !process.has_exited())?;
        let call = self.query_program_status(&block);
        self.observe_program_status(&block, reading, call, process.foreground.as_ref())
    }

    /// The statuses this backend keeps, for the event stream that follows
    /// them.
    pub(crate) fn program_statuses(&self) -> Arc<ProgramStatuses> {
        self.program_status.clone()
    }

    /// Show `icon` in front of the label of the window holding `pane_id`, or
    /// no icon when `None`. A pane that no longer exists is left alone.
    pub(super) fn set_window_icon(&self, pane_id: &str, icon: Option<&str>) -> Result<()> {
        let block = BlockId::parse(pane_id)?;
        let Some(target) = self.resolve("block", block.as_str())? else {
            return Ok(());
        };
        let Some(window_id) = target.window_id else {
            return Ok(());
        };
        let Some(view) = found(self.view(&target.session_id))? else {
            return Ok(());
        };
        let Some(window) = view.window(&window_id) else {
            return Ok(());
        };
        self.api::<Value>(
            "session.set_window_label",
            json!({
                "session_id": target.session_id.as_str(),
                "window_id": window_id.as_str(),
                "label": label_with_icon(&window.label, icon),
            }),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working() -> Option<ReportedStatus> {
        Some(ReportedStatus::Agent {
            status: AgentStatus::Working,
            since: 1,
        })
    }

    fn recorded(reported: Option<ReportedStatus>) -> Entry {
        Entry {
            reported,
            recorded: true,
        }
    }

    fn block_event(name: &str, block: &str) -> Event {
        let line = format!(
            r#"{{"type":"session_event","payload":{{"session_id":"session:1","event":{{"type":"block_event","payload":{{"block_id":"{block}","name":"{name}","payload":{{"record":{{"state":"done"}},"title":"t"}}}}}}}}}}"#
        );
        Event::parse(&line).unwrap_or_else(|| panic!("{name} parses"))
    }

    #[test]
    fn without_a_stream_only_a_scan_is_kept() {
        let statuses = ProgramStatuses::default();
        let block = BlockId("block:1".into());
        statuses.keep(&block, statuses.begin(Read::Look), recorded(working()));
        assert_eq!(statuses.get(&block), None);
        statuses.keep(&block, statuses.begin(Read::Scan), recorded(working()));
        assert_eq!(statuses.get(&block), Some(working()));
    }

    #[test]
    fn a_followed_entry_lasts_until_its_block_may_have_changed() {
        let statuses = ProgramStatuses::default();
        statuses.follow(true);
        let (block, other) = (BlockId("block:1".into()), BlockId("block:2".into()));
        statuses.keep(&other, statuses.begin(Read::Look), recorded(working()));
        for name in [
            "program_status_changed",
            "program_status_removed",
            "process_changed",
            "child_exited",
        ] {
            statuses.keep(&block, statuses.begin(Read::Look), recorded(working()));
            statuses.apply(&block_event("title_changed", "block:1"));
            assert_eq!(statuses.get(&block), Some(working()), "after title_changed");
            statuses.apply(&block_event(name, "block:1"));
            assert_eq!(statuses.get(&block), None, "after {name}");
        }
        statuses.keep(&block, statuses.begin(Read::Look), recorded(working()));
        statuses.apply(&Event::BlockClosed {
            session_id: "session:1".into(),
            block_id: "block:1".into(),
        });
        assert_eq!(statuses.get(&block), None, "after block_closed");
        assert_eq!(statuses.get(&other), Some(working()));
    }

    #[test]
    fn a_new_foreground_process_keeps_an_entry_read_from_no_records() {
        let statuses = ProgramStatuses::default();
        statuses.follow(true);
        let (block, agent) = (BlockId("block:1".into()), BlockId("block:2".into()));
        let unrecorded = Entry {
            reported: None,
            recorded: false,
        };
        statuses.keep(&block, statuses.begin(Read::Look), unrecorded);
        let reading = statuses.begin(Read::Look);
        statuses.apply(&block_event("process_changed", "block:1"));
        statuses.keep(&agent, reading, recorded(working()));
        assert_eq!(statuses.get(&agent), Some(working()), "a read across it");
        statuses.apply(&block_event("process_changed", "block:2"));
        assert_eq!(statuses.get(&block), Some(None), "no record to change");
        assert_eq!(
            statuses.get(&agent),
            None,
            "its record may not be the foreground's"
        );
        for name in [
            "program_status_changed",
            "program_status_removed",
            "child_exited",
        ] {
            statuses.keep(&block, statuses.begin(Read::Look), unrecorded);
            statuses.apply(&block_event(name, "block:1"));
            assert_eq!(statuses.get(&block), None, "after {name}");
        }
        statuses.keep(&block, statuses.begin(Read::Look), unrecorded);
        statuses.apply(&Event::BlockClosed {
            session_id: "session:1".into(),
            block_id: "block:1".into(),
        });
        assert_eq!(statuses.get(&block), None, "after block_closed");
    }

    #[test]
    fn a_read_across_a_drop_or_a_change_of_stream_is_not_kept() {
        let statuses = ProgramStatuses::default();
        let block = BlockId("block:1".into());
        statuses.follow(true);
        let reading = statuses.begin(Read::Look);
        statuses.apply(&block_event("program_status_changed", "block:1"));
        statuses.keep(&block, reading, recorded(working()));
        assert_eq!(statuses.get(&block), None, "across an event");

        statuses.keep(&block, statuses.begin(Read::Look), recorded(working()));
        statuses.follow(false);
        assert_eq!(statuses.get(&block), None, "past the stream's end");
        let reading = statuses.begin(Read::Scan);
        statuses.follow(true);
        statuses.keep(&block, reading, recorded(working()));
        assert_eq!(statuses.get(&block), None, "across a new stream");
    }

    const FIXTURES: &str = include_str!("../testdata/rex_program_status.json");

    fn fixture(name: &str) -> ProgramStatusReport {
        let fixtures: std::collections::HashMap<String, Value> =
            serde_json::from_str(FIXTURES).unwrap();
        serde_json::from_value(fixtures[name].clone()).unwrap()
    }

    fn shown(name: &str) -> Option<Option<AgentStatus>> {
        fixture(name).reported().map(|reported| match reported {
            ReportedStatus::Agent { status, .. } => Some(status),
            ReportedStatus::Idle => None,
        })
    }

    #[test]
    fn every_rex_state_maps_through_one_table() {
        use AgentStatus::{Done, Waiting, Working};
        let cases = [
            ("working", Some(Some(Working))),
            ("working_with_progress", Some(Some(Working))),
            ("blocked_permission", Some(Some(Waiting))),
            ("blocked_question", Some(Some(Waiting))),
            ("blocked_auth", Some(Some(Waiting))),
            ("blocked_without_kind", Some(Some(Waiting))),
            ("blocked_unknown_kind", Some(Some(Waiting))),
            ("done", Some(Some(Done))),
            ("error", Some(Some(Done))),
            ("idle", Some(None)),
            ("unknown_state", Some(None)),
            ("empty", None),
            ("claude_code_idle", Some(None)),
            ("claude_code_blocked", Some(Some(Waiting))),
            ("claude_code_working", Some(Some(Working))),
            ("claude_code_done", Some(Some(Done))),
            ("claude_code_after_esc", Some(None)),
        ];
        for (name, expected) in cases {
            assert_eq!(shown(name), expected, "{name}");
        }
    }

    #[test]
    fn program_status_overrides_a_stale_hook_status() {
        use crate::state::reported::tests::hooked;

        let mut stale_waiting = hooked("block:a", AgentStatus::Waiting, 100);
        let working = fixture("working").reported().unwrap();
        assert!(crate::state::reported::apply(&mut stale_waiting, working));
        assert_eq!(stale_waiting.status, Some(AgentStatus::Working));

        let mut stale_working = hooked("block:a", AgentStatus::Working, 100);
        let idle = fixture("claude_code_after_esc").reported().unwrap();
        assert!(crate::state::reported::apply(&mut stale_working, idle));
        assert_eq!(stale_working.status, None);
    }

    #[test]
    fn the_most_urgent_record_of_a_block_wins() {
        assert_eq!(
            shown("idle_root_blocked_child"),
            Some(Some(AgentStatus::Waiting))
        );
        assert_eq!(
            shown("records_left_after_exit"),
            Some(Some(AgentStatus::Done))
        );
    }

    #[test]
    fn records_of_a_program_that_left_the_foreground_are_ignored() {
        let left = fixture("records_left_after_exit");
        assert!(
            left.owned_by_foreground(Some((4242, "zsh")))
                .reported()
                .is_none()
        );
        let running =
            fixture("claude_code_blocked").owned_by_foreground(Some((85917, "claude.exe")));
        assert!(matches!(
            running.reported(),
            Some(ReportedStatus::Agent {
                status: AgentStatus::Waiting,
                ..
            })
        ));
        assert!(
            fixture("done")
                .owned_by_foreground(None)
                .reported()
                .is_none()
        );
    }

    #[test]
    fn a_shell_that_took_over_the_agents_pid_does_not_inherit_its_records() {
        let records = || fixture("records_left_after_exit");
        assert!(
            records()
                .owned_by_foreground(Some((58052, "zsh")))
                .reported()
                .is_none()
        );
        assert!(
            records()
                .owned_by_foreground(Some((58052, "Python")))
                .reported()
                .is_some()
        );
    }

    #[test]
    fn report_time_is_the_latest_record_of_the_winning_urgency() {
        let Some(ReportedStatus::Agent { since, .. }) = fixture("done").reported() else {
            panic!("done fixture reports done");
        };
        assert_eq!(since, unix_seconds("2026-11-02T15:22:54-08:00").unwrap());
    }

    #[test]
    fn report_time_is_parsed_with_its_offset() {
        assert_eq!(
            unix_seconds("2026-11-02T15:22:36.943312-08:00"),
            Some(1_793_661_756)
        );
        assert_eq!(unix_seconds("2026-11-02T23:22:36Z"), Some(1_793_661_756));
        assert_eq!(unix_seconds("1970-01-01T00:00:00+00:00"), Some(0));
        assert_eq!(unix_seconds("not a time"), None);
    }

    #[test]
    fn window_labels_drop_the_status_icon() {
        assert_eq!(base_label("💬\u{2002}wm-feat-a"), "wm-feat-a");
        assert_eq!(base_label("wm-feat-a"), "wm-feat-a");
        assert_eq!(base_label("\u{f418} feat-a"), "\u{f418} feat-a");
        assert_eq!(
            base_label(&label_with_icon("\u{f418} feat-a", Some("✅"))),
            "\u{f418} feat-a"
        );
        assert_eq!(
            label_with_icon("wm-feat-a", Some("💬")),
            "💬\u{2002}wm-feat-a"
        );
        assert_eq!(label_with_icon("wm-feat-a", Some("")), "wm-feat-a");
        assert_eq!(label_with_icon("wm-feat-a", None), "wm-feat-a");
    }
}
