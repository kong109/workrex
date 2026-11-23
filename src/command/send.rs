use std::io::{IsTerminal, Read};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};

use crate::agent_error::{AgentError, AgentErrorKind};
use crate::bin_name::BIN;
use crate::config;
use crate::multiplexer::{AgentPane, AgentStatus, Multiplexer, create_backend, detect_backend};
use crate::remote::agents::SentAgent;
use crate::remote::cache::{self, WorkspaceRecord};
use crate::state::sent::{TurnFence, same_second, sleep_to_next_second};
use crate::state::{PaneKey, StateStore};
use crate::util;
use crate::workflow::{self, AgentTarget};

const ACTIVITY_WINDOW: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(250);

pub struct WaitFor {
    pub until: Vec<AgentStatus>,
    pub timeout: Option<Duration>,
}

pub fn run(
    name: &str,
    text: Option<&str>,
    file: Option<&str>,
    wait: Option<WaitFor>,
) -> Result<()> {
    let mux = create_backend(detect_backend());
    let target = workflow::resolve_agent_target(name, mux.as_ref())?;
    let content = read_content(text, file)?;
    let start = Instant::now();
    let deadline = wait
        .as_ref()
        .and_then(|wait| wait.timeout)
        .map(|timeout| start + timeout);
    let sent = match target {
        AgentTarget::Remote { record, pane } => {
            let (cfg, _) = record.creator_config()?;
            send_remote(&cfg, record, pane, &content, wait.is_some())?
        }
        AgentTarget::Local(agent) => {
            let cfg = config::Config::load(None).unwrap_or_default();
            send_local(&cfg, mux.as_ref(), agent.pane_id, &content, wait.is_some())?
        }
    };
    let (Some(wait), Some(mut sent)) = (wait, sent) else {
        return Ok(());
    };
    let status = await_turn(name, &mut sent, &wait.until, deadline)?;
    let elapsed = util::format_elapsed_duration(start.elapsed());
    eprintln!("{name}: {} ({elapsed})", status.name());
    Ok(())
}

struct Sent<'a> {
    fence: TurnFence,
    observe: Box<dyn FnMut() -> Result<Seen> + 'a>,
}

enum Seen {
    Unobserved,
    Gone,
    Agent(Option<AgentStatus>),
}

fn send_local<'a>(
    cfg: &config::Config,
    mux: &'a dyn Multiplexer,
    pane_id: String,
    content: &str,
    wait: bool,
) -> Result<Option<Sent<'a>>> {
    let store = StateStore::new()?;
    let agent = sendable_agent(&pane_id, wait, || reload(&store, mux, &pane_id))?;
    let baseline = !same_second(agent.status, agent.status_ts, cache::now());
    let agent_identity = agent
        .agent_kind
        .as_deref()
        .or(cfg.agent.as_deref())
        .or(agent.agent_command.as_deref());
    mux.send_prompt_to_agent(&pane_id, content, agent_identity)?;
    let fence = TurnFence {
        status: agent.status.filter(|_| baseline),
        status_ts: agent.status_ts.filter(|_| baseline),
        sent_at: cache::now(),
    };
    let key = PaneKey {
        backend: mux.name().to_string(),
        instance: mux.instance_id(),
        pane_id: pane_id.clone(),
    };
    if let Err(err) = store.set_sent_turn(&key, &fence) {
        tracing::warn!(pane_id, "failed to record the sent turn: {err:#}");
    }
    if !wait {
        return Ok(None);
    }
    let observe_fence = fence.clone();
    Ok(Some(Sent {
        fence,
        observe: Box::new(move || Ok(seen(reload(&store, mux, &pane_id)?, &observe_fence))),
    }))
}

/// The agent's current state, once it may take a prompt. `reload` reads it
/// fresh: the target was resolved before the content was read, which can
/// block on stdin while the agent exits or opens a dialog. A `done` or
/// `waiting` state from the current second is left behind first, so every
/// status the prompt causes carries a later timestamp.
fn sendable_agent(
    pane_id: &str,
    wait: bool,
    mut reload: impl FnMut() -> Result<Option<AgentPane>>,
) -> Result<AgentPane> {
    let mut agent = reload()?.ok_or_else(|| not_found(pane_id))?;
    check_sendable(&agent, wait)?;
    for _ in 0..3 {
        if !same_second(agent.status, agent.status_ts, cache::now()) {
            break;
        }
        sleep_to_next_second();
        agent = reload()?.ok_or_else(|| not_found(pane_id))?;
    }
    check_sendable(&agent, wait)?;
    Ok(agent)
}

/// What a wait sees of the agent a turn went to. An agent back at its
/// launch shell has exited, though its launch record outlives its state.
fn seen(agent: Option<AgentPane>, fence: &TurnFence) -> Seen {
    match agent {
        None => Seen::Gone,
        Some(agent) if agent.launched.is_some_and(|launch| launch.at_shell) => Seen::Gone,
        Some(agent) => Seen::Agent(
            agent
                .status
                .filter(|_| fence.admits(agent.status, agent.status_ts)),
        ),
    }
}

fn reload(store: &StateStore, mux: &dyn Multiplexer, pane_id: &str) -> Result<Option<AgentPane>> {
    Ok(store
        .load_reconciled_agents(mux)?
        .into_iter()
        .find(|agent| agent.pane_id == pane_id))
}

fn check_sendable(agent: &AgentPane, wait: bool) -> Result<()> {
    if agent.launched.is_some_and(|launch| launch.at_shell) {
        return Err(AgentError::error(
            AgentErrorKind::NotFound,
            format!(
                "the agent in pane {} is not running: its pane's shell has the foreground; \
                 `{BIN} capture {}` shows why",
                agent.pane_id, agent.pane_id
            ),
        ));
    }
    if agent.status == Some(AgentStatus::Waiting) {
        return Err(AgentError::error(
            AgentErrorKind::Waiting,
            format!(
                "the agent in pane {} is waiting on a dialog; nothing sent",
                agent.pane_id
            ),
        ));
    }
    if wait && agent.status.is_none() {
        return Err(AgentError::error(
            AgentErrorKind::StatusUnknown,
            format!(
                "the agent in pane {} has not reported a status; nothing sent",
                agent.pane_id
            ),
        ));
    }
    Ok(())
}

fn not_found(pane_id: &str) -> anyhow::Error {
    AgentError::error(
        AgentErrorKind::NotFound,
        format!("the agent in pane {pane_id} exited; nothing sent"),
    )
}

fn send_remote(
    cfg: &config::Config,
    record: WorkspaceRecord,
    pane: Option<usize>,
    content: &str,
    wait: bool,
) -> Result<Option<Sent<'static>>> {
    if wait {
        crate::remote::observe::spawn(cfg, std::slice::from_ref(&record.remote));
    }
    let turn = crate::remote::agents::send(cfg, &record, pane, content, wait)?;
    if !wait {
        return Ok(None);
    }
    let turn = turn.ok_or_else(|| {
        anyhow!(
            "the node on '{}' cannot report the turn a send starts; `{BIN} remote setup {}` updates it",
            record.remote,
            record.remote
        )
    })?;
    Ok(Some(Sent {
        fence: turn.fence.clone(),
        observe: Box::new(move || {
            Ok(match crate::remote::agents::sent_agent(&record, &turn) {
                SentAgent::Unobserved => Seen::Unobserved,
                SentAgent::Gone => Seen::Gone,
                SentAgent::Present(agent) => {
                    Seen::Agent(agent.status.filter(|_| turn.admits(&agent)))
                }
            })
        }),
    }))
}

fn await_turn(
    name: &str,
    sent: &mut Sent<'_>,
    until: &[AgentStatus],
    deadline: Option<Instant>,
) -> Result<AgentStatus> {
    let mut active = sent.fence.status == Some(AgentStatus::Working);
    let mut seen_since: Option<Instant> = None;
    loop {
        match (sent.observe)()? {
            Seen::Unobserved => {}
            Seen::Gone => {
                return Err(AgentError::error(
                    AgentErrorKind::Gone,
                    format!("the agent of '{name}' exited before its turn settled"),
                ));
            }
            Seen::Agent(status) => {
                seen_since.get_or_insert_with(Instant::now);
                if let Some(status) = status {
                    active = true;
                    if until.contains(&status) {
                        return Ok(status);
                    }
                }
            }
        }
        if !active && seen_since.is_some_and(|since| since.elapsed() >= ACTIVITY_WINDOW) {
            return Err(AgentError::error(
                AgentErrorKind::Stalled,
                format!(
                    "the agent of '{name}' showed no activity within {}s of the send",
                    ACTIVITY_WINDOW.as_secs()
                ),
            ));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(AgentError::error(
                AgentErrorKind::Timeout,
                format!("timed out waiting for the agent of '{name}'"),
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// The text to send: positional arg > --file > stdin, without its trailing
/// newlines.
fn read_content(text: Option<&str>, file: Option<&str>) -> Result<String> {
    let content = if let Some(t) = text {
        t.to_string()
    } else if let Some(f) = file {
        std::fs::read_to_string(f)?
    } else {
        // Guard: don't block on interactive TTY
        if std::io::stdin().is_terminal() {
            return Err(anyhow!(
                "No content to send. Provide text argument, --file, or pipe stdin"
            ));
        }
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    };

    let content = content.trim_end_matches('\n');
    if content.is_empty() {
        return Err(anyhow!("No content to send"));
    }
    Ok(content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(was: Option<AgentStatus>, seen: Vec<Seen>) -> Sent<'static> {
        let mut seen = seen.into_iter();
        Sent {
            fence: TurnFence {
                status: was,
                status_ts: Some(1),
                sent_at: 1,
            },
            observe: Box::new(move || Ok(seen.next().unwrap_or(Seen::Unobserved))),
        }
    }

    fn kind(result: Result<AgentStatus>) -> AgentErrorKind {
        crate::agent_error::kind_of(&result.unwrap_err()).unwrap()
    }

    const SETTLED: &[AgentStatus] = &[AgentStatus::Done, AgentStatus::Waiting];

    fn agent(status: Option<AgentStatus>, at_shell: Option<bool>) -> AgentPane {
        AgentPane {
            session: String::new(),
            window_name: String::new(),
            pane_id: "block:a".into(),
            window_id: String::new(),
            window_index: None,
            path: Default::default(),
            pane_title: None,
            status,
            status_ts: Some(1),
            activity_ts: None,
            updated_ts: None,
            window_cmd: None,
            agent_command: None,
            agent_kind: None,
            prompt: None,
            remote: None,
            launched: at_shell.map(|at_shell| crate::multiplexer::Launch {
                at: 1,
                prompted: true,
                at_shell,
            }),
        }
    }

    /// `sendable_agent` over `states`, read one per reload.
    fn sendable(states: Vec<Option<AgentPane>>) -> Result<AgentPane> {
        let mut states = states.into_iter();
        sendable_agent("block:a", false, || Ok(states.next().unwrap()))
    }

    #[test]
    fn a_send_judges_the_agent_as_it_is_after_the_content_is_read() {
        let error_kind =
            |result: Result<AgentPane>| crate::agent_error::kind_of(&result.unwrap_err()).unwrap();
        // The target resolved while the agent was done; by the time stdin
        // was read it had opened a dialog, exited to its shell, or left.
        assert_eq!(
            error_kind(sendable(vec![Some(agent(
                Some(AgentStatus::Waiting),
                None
            ))])),
            AgentErrorKind::Waiting
        );
        assert_eq!(
            error_kind(sendable(vec![Some(agent(None, Some(true)))])),
            AgentErrorKind::NotFound
        );
        assert_eq!(error_kind(sendable(vec![None])), AgentErrorKind::NotFound);
        let fresh = sendable(vec![Some(agent(Some(AgentStatus::Working), None))]).unwrap();
        assert_eq!(fresh.status, Some(AgentStatus::Working));
    }

    #[test]
    fn an_agent_that_leaves_during_the_same_second_wait_was_never_sent_to() {
        let mut now_done = agent(Some(AgentStatus::Done), None);
        now_done.status_ts = Some(cache::now() + 5);
        assert_eq!(
            crate::agent_error::kind_of(&sendable(vec![Some(now_done), None]).unwrap_err()),
            Some(AgentErrorKind::NotFound)
        );
    }

    #[test]
    fn a_waited_agent_back_at_its_shell_is_gone() {
        let fence = TurnFence {
            status: Some(AgentStatus::Done),
            status_ts: Some(1),
            sent_at: 1,
        };
        assert!(matches!(
            seen(Some(agent(None, Some(true))), &fence),
            Seen::Gone
        ));
        assert!(matches!(seen(None, &fence), Seen::Gone));
        assert!(matches!(
            seen(Some(agent(None, Some(false))), &fence),
            Seen::Agent(None)
        ));
    }

    #[test]
    fn the_wait_ends_at_the_first_until_status_of_the_turn() {
        let done = Some(AgentStatus::Done);
        let mut turn = sent(
            done,
            vec![
                Seen::Unobserved,
                Seen::Agent(None),
                Seen::Agent(Some(AgentStatus::Working)),
                Seen::Agent(done),
            ],
        );
        assert_eq!(
            await_turn("f", &mut turn, SETTLED, None).unwrap(),
            AgentStatus::Done
        );

        let mut turn = sent(done, vec![Seen::Agent(Some(AgentStatus::Working))]);
        assert_eq!(
            await_turn("f", &mut turn, &[AgentStatus::Working], None).unwrap(),
            AgentStatus::Working
        );
    }

    #[test]
    fn an_agent_that_leaves_is_gone() {
        let mut turn = sent(Some(AgentStatus::Done), vec![Seen::Agent(None), Seen::Gone]);
        assert_eq!(
            kind(await_turn("f", &mut turn, SETTLED, None)),
            AgentErrorKind::Gone
        );
    }

    #[test]
    fn the_deadline_ends_a_wait_for_a_working_agent() {
        let working = Some(AgentStatus::Working);
        let mut turn = sent(working, (0..100).map(|_| Seen::Agent(working)).collect());
        let deadline = Some(Instant::now() + Duration::from_millis(300));
        assert_eq!(
            kind(await_turn("f", &mut turn, SETTLED, deadline)),
            AgentErrorKind::Timeout
        );
    }
}
