//! The server event stream, `rex events --all --json`.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdout, Command, Stdio};

use super::RexBackend;

/// The server events workmux acts on. Client connections, layer changes and
/// every other event parse to `None`: each `rex` CLI call connects a client,
/// so a consumer reacting to those would feed on its own calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    SessionCreated {
        session_id: String,
    },
    SessionDestroyed {
        session_id: String,
    },
    WindowCreated {
        session_id: String,
        window_id: String,
    },
    WindowClosed {
        session_id: String,
        window_id: String,
    },
    BlockClosed {
        session_id: String,
        block_id: String,
    },
    /// The layout, a label or the active window changed.
    ViewChanged {
        session_id: String,
    },
    SizeChanged {
        session_id: String,
        block_id: String,
        columns: u32,
        rows: u32,
    },
    ProcessChanged {
        session_id: String,
        block_id: String,
        foreground: Option<Foreground>,
    },
    TitleChanged {
        session_id: String,
        block_id: String,
        title: String,
    },
    /// An OSC 7501 report. `state` is the record's `state`, such as
    /// `working` or `blocked`, and `kind` qualifies it.
    ProgramStatusChanged {
        session_id: String,
        block_id: String,
        state: String,
        kind: Option<String>,
        app: Option<String>,
    },
    ProgramStatusRemoved {
        session_id: String,
        block_id: String,
    },
}

/// The foreground process of a terminal block.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct Foreground {
    #[serde(default)]
    pub pid: Option<u32>,
    pub name: String,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

impl Event {
    pub(crate) fn parse(line: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(line).ok()?;
        let payload = value.get("payload")?;
        match value.get("type")?.as_str()? {
            "session_created" => Some(Self::SessionCreated {
                session_id: text(payload.get("session")?, "session_id")?,
            }),
            "session_destroyed" => Some(Self::SessionDestroyed {
                session_id: text(payload, "session_id")?,
            }),
            "session_event" => {
                let session_id = text(payload, "session_id")?;
                let event = payload.get("event")?;
                let body = event.get("payload");
                match event.get("type")?.as_str()? {
                    "window_created" => Some(Self::WindowCreated {
                        session_id,
                        window_id: text(body?, "window_id")?,
                    }),
                    "window_closed" => Some(Self::WindowClosed {
                        session_id,
                        window_id: text(body?, "window_id")?,
                    }),
                    "block_closed" => Some(Self::BlockClosed {
                        session_id,
                        block_id: text(body?, "block_id")?,
                    }),
                    "session_view_changed"
                    | "active_window_changed"
                    | "window_label_changed"
                    | "session_label_changed"
                    | "block_created" => Some(Self::ViewChanged { session_id }),
                    "block_event" => Self::block_event(session_id, body?),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn block_event(session_id: String, body: &Value) -> Option<Self> {
        let block_id = text(body, "block_id")?;
        let data = body.get("payload");
        match body.get("name")?.as_str()? {
            "size_changed" => {
                let data = data?;
                let dimension = |key| data.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok());
                Some(Self::SizeChanged {
                    session_id,
                    block_id,
                    columns: dimension("columns")?,
                    rows: dimension("rows")?,
                })
            }
            "process_changed" => Some(Self::ProcessChanged {
                session_id,
                block_id,
                foreground: data
                    .and_then(|data| data.get("foreground"))
                    .and_then(|value| Foreground::deserialize(value).ok()),
            }),
            "title_changed" => Some(Self::TitleChanged {
                session_id,
                block_id,
                title: text(data?, "title")?,
            }),
            "program_status_changed" => {
                let record = data?.get("record")?;
                Some(Self::ProgramStatusChanged {
                    session_id,
                    block_id,
                    state: text(record, "state")?,
                    kind: text(record, "kind"),
                    app: text(record, "app"),
                })
            }
            "program_status_removed" => Some(Self::ProgramStatusRemoved {
                session_id,
                block_id,
            }),
            _ => None,
        }
    }
}

/// A running `rex events` process. Dropping it kills the process.
pub(crate) struct EventStream {
    child: Child,
    lines: std::io::Lines<BufReader<ChildStdout>>,
}

impl EventStream {
    /// The next event workmux acts on, or `None` once the stream ends.
    pub(crate) fn next_event(&mut self) -> Option<Event> {
        for line in self.lines.by_ref() {
            if let Some(event) = Event::parse(&line.ok()?) {
                return Some(event);
            }
        }
        None
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl RexBackend {
    /// Follow every server event. Terminal events such as `size_changed`
    /// and `program_status_changed` need `--all`; the unscoped stream
    /// carries only session and window changes.
    pub(crate) fn events(&self) -> Result<EventStream> {
        self.ensure_supported()?;
        let mut command = Command::new(&self.program);
        for var in Self::INHERITED_SCOPE_VARS {
            command.env_remove(var);
        }
        let mut child = command
            .args(self.global_args())
            .args(["events", "--all", "--json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to start rex events")?;
        let stdout = child.stdout.take().context("rex events has no stdout")?;
        Ok(EventStream {
            child,
            lines: BufReader::new(stdout).lines(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW_CREATED: &str = include_str!("../testdata/rex_events_window_created.jsonl");
    const SIZE_CHANGED: &str = include_str!("../testdata/rex_events_size_changed.jsonl");
    const PROGRAM_STATUS: &str = include_str!("../testdata/rex_events_program_status.jsonl");
    const OSC_PROBE: &str = include_str!("../testdata/rex_events_osc7501.jsonl");

    fn parsed(lines: &str) -> Vec<Event> {
        lines.lines().filter_map(Event::parse).collect()
    }

    #[test]
    fn window_creation_and_view_changes_are_kept() {
        let events = parsed(WINDOW_CREATED);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::WindowCreated { window_id, .. } if window_id.starts_with("window:")
        )));
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::ViewChanged { .. }))
        );
    }

    #[test]
    fn terminal_events_carry_size_and_program_status() {
        assert_eq!(
            parsed(SIZE_CHANGED),
            [Event::SizeChanged {
                session_id: "session:01a19e92-e5aa-7c77-a9b2-9042bbdbf620".into(),
                block_id: "block:01a19e93-b76d-7c44-9a83-651cbd8da21e".into(),
                columns: 30,
                rows: 40,
            }]
        );
        assert_eq!(
            parsed(PROGRAM_STATUS),
            [Event::ProgramStatusChanged {
                session_id: "session:01a19e92-e5aa-7c77-a9b2-9042bbdbf620".into(),
                block_id: "block:01a19e9e-15ce-732c-831b-0e8164133173".into(),
                state: "working".into(),
                kind: None,
                app: Some("test".into()),
            }]
        );
    }

    #[test]
    fn process_changes_carry_the_foreground_process() {
        let events = parsed(OSC_PROBE);
        let foregrounds: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::ProcessChanged { foreground, .. } => Some(foreground.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(foregrounds.len(), 3);
        assert_eq!(foregrounds[0], None);
        let sleep = foregrounds[2].as_ref().unwrap();
        assert_eq!(sleep.name, "sleep");
        assert_eq!(
            sleep.cwd.as_deref(),
            Some(std::path::Path::new("/Users/dev"))
        );
        let states: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                Event::ProgramStatusChanged { state, kind, .. } => {
                    Some((state.as_str(), kind.as_deref()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(states, [("working", None), ("blocked", Some("permission"))]);
    }

    #[test]
    fn session_and_close_events_parse() {
        let session = r#"{"type":"session_created","payload":{"session":{"session_id":"session:3","label":"wm-proj2"}}}"#;
        assert_eq!(
            Event::parse(session),
            Some(Event::SessionCreated {
                session_id: "session:3".into()
            })
        );
        let closed = r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"window_closed","payload":{"window_id":"window:2"}}}}"#;
        assert_eq!(
            Event::parse(closed),
            Some(Event::WindowClosed {
                session_id: "session:1".into(),
                window_id: "window:2".into()
            })
        );
        assert_eq!(
            Event::parse(r#"{"type":"session_destroyed","payload":{"session_id":"session:3"}}"#),
            Some(Event::SessionDestroyed {
                session_id: "session:3".into()
            })
        );
        let title = r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:1","name":"title_changed","payload":{"title":"claude","owner":{"pid":1,"name":"zsh"}}}}}}"#;
        assert_eq!(
            Event::parse(title),
            Some(Event::TitleChanged {
                session_id: "session:1".into(),
                block_id: "block:1".into(),
                title: "claude".into()
            })
        );
        let removed = r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:1","name":"program_status_removed","payload":{"ids":[""],"reason":"exit"}}}}}"#;
        assert_eq!(
            Event::parse(removed),
            Some(Event::ProgramStatusRemoved {
                session_id: "session:1".into(),
                block_id: "block:1".into()
            })
        );
    }

    #[test]
    fn client_traffic_and_garbage_are_ignored() {
        for ignored in [
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"client_connected","payload":{"client_id":"client:1"}}}}"#,
            r#"{"type":"client_left","payload":{"client_id":"client:1"}}"#,
            r#"{"type":"session_event","payload":{"session_id":"session:1","event":{"type":"block_event","payload":{"block_id":"block:1","name":"cwd_changed","payload":{}}}}}"#,
            "not json",
        ] {
            assert_eq!(Event::parse(ignored), None, "{ignored}");
        }
    }
}
