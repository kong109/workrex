//! The control plane's hold on its Rex server. A slow, paused or busy
//! server is retried for as long as it takes: giving up on a live server
//! would leave every new window without a sidebar. Only a server that is
//! certainly gone ends the daemon.

use std::time::{Duration, Instant};

const BACKOFF_FIRST: Duration = Duration::from_millis(250);
const BACKOFF_CAP: Duration = Duration::from_secs(5);
/// A connection that lasted this long resets the backoff.
const STABLE: Duration = Duration::from_secs(10);

/// What the rex CLI prints, with exit status 4, when the socket path is
/// missing or connecting to it is refused.
const NO_SERVER: &str = "no Rex server is running at";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Link {
    Connected {
        boot_id: String,
        since: Instant,
        attempt: u32,
    },
    Retrying {
        attempt: u32,
        since: Instant,
        /// The boot id of the last server seen, to tell a restarted server
        /// from the same one coming back.
        boot_id: Option<String>,
    },
    Gone,
}

/// Why a call to the server failed.
#[derive(Debug)]
pub(super) enum Failure {
    /// Nothing listens at the server's socket.
    Dead,
    /// Anything else, a timeout included: the server may be alive.
    Unsure(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        if format!("{error:#}").contains(NO_SERVER) {
            Failure::Dead
        } else {
            Failure::Unsure(error)
        }
    }
}

/// The server as the lifecycle drives it, so tests can run it on a fake.
pub(super) trait Server {
    /// Start following events and return the server's boot id.
    fn connect(&mut self) -> Result<String, Failure>;
    /// Bring the windows in line after connecting: every window on a server
    /// not seen before, otherwise only what changed while disconnected.
    fn resync(&mut self, full: bool);
    /// Act on events until the stream ends or fails.
    fn follow(&mut self);
    fn wait(&mut self, delay: Duration);
}

impl Link {
    pub fn start() -> Self {
        Link::Retrying {
            attempt: 0,
            since: Instant::now(),
            boot_id: None,
        }
    }

    pub fn step(self, server: &mut impl Server) -> Self {
        match self {
            Link::Gone => Link::Gone,
            Link::Connected {
                boot_id,
                since,
                attempt,
            } => {
                server.follow();
                let attempt = if since.elapsed() >= STABLE {
                    1
                } else {
                    attempt + 1
                };
                Link::Retrying {
                    attempt,
                    since: Instant::now(),
                    boot_id: Some(boot_id),
                }
            }
            Link::Retrying {
                attempt,
                since,
                boot_id,
            } => {
                if attempt > 0 {
                    server.wait(backoff(attempt));
                }
                match server.connect() {
                    Ok(now) => {
                        server.resync(boot_id.as_deref() != Some(now.as_str()));
                        Link::Connected {
                            boot_id: now,
                            since: Instant::now(),
                            attempt,
                        }
                    }
                    Err(Failure::Dead) => Link::Gone,
                    Err(Failure::Unsure(error)) => {
                        tracing::warn!(
                            %error,
                            attempt,
                            waited = ?since.elapsed(),
                            "rex server not answering; retrying"
                        );
                        Link::Retrying {
                            attempt: attempt + 1,
                            since,
                            boot_id,
                        }
                    }
                }
            }
        }
    }
}

pub(super) fn backoff(attempt: u32) -> Duration {
    let doublings = attempt.saturating_sub(1).min(8);
    BACKOFF_FIRST
        .saturating_mul(1 << doublings)
        .min(BACKOFF_CAP)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::collections::VecDeque;

    const TIMED_OUT: &str = "rex api call session.list - failed: Error: connecting to the \
                             server timed out after 10s; increase --timeout: context deadline exceeded";
    const REFUSED: &str = "rex whoami --json failed: Error: no Rex server is running at /tmp/x/s.sock, and autostart is disabled";

    #[derive(Default)]
    struct Fake {
        connects: VecDeque<Result<&'static str, &'static str>>,
        resyncs: Vec<bool>,
        follows: u32,
        waits: Vec<Duration>,
    }

    impl Server for Fake {
        fn connect(&mut self) -> Result<String, Failure> {
            match self
                .connects
                .pop_front()
                .expect("an outcome for each connect")
            {
                Ok(boot) => Ok(boot.to_string()),
                Err(message) => Err(anyhow!(message).into()),
            }
        }
        fn resync(&mut self, full: bool) {
            self.resyncs.push(full);
        }
        fn follow(&mut self) {
            self.follows += 1;
        }
        fn wait(&mut self, delay: Duration) {
            self.waits.push(delay);
        }
    }

    fn run(fake: &mut Fake, steps: usize) -> Link {
        let mut link = Link::start();
        for _ in 0..steps {
            link = link.step(fake);
        }
        link
    }

    #[test]
    fn a_server_that_keeps_timing_out_is_retried_without_end() {
        let mut fake = Fake::default();
        fake.connects
            .extend(std::iter::repeat_n(Err(TIMED_OUT), 200));
        let link = run(&mut fake, 200);
        assert!(
            matches!(link, Link::Retrying { attempt: 200, .. }),
            "{link:?}"
        );
        assert_eq!(fake.waits.len(), 199);
        assert!(fake.waits.iter().all(|wait| *wait <= BACKOFF_CAP));
        assert_eq!(fake.waits.last(), Some(&BACKOFF_CAP));
        assert!(fake.waits.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn a_paused_server_coming_back_is_resynced_for_what_changed() {
        let mut fake = Fake::default();
        fake.connects.push_back(Ok("boot-1"));
        fake.connects
            .extend(std::iter::repeat_n(Err(TIMED_OUT), 30));
        fake.connects.push_back(Ok("boot-1"));
        // start, connect, follow (stream ends), 30 timeouts, connect.
        let link = run(&mut fake, 1 + 1 + 30 + 1);
        assert!(
            matches!(&link, Link::Connected { boot_id, .. } if boot_id == "boot-1"),
            "{link:?}"
        );
        assert_eq!(fake.resyncs, vec![true, false]);
        assert_eq!(fake.follows, 1);
    }

    #[test]
    fn a_refused_connection_is_the_end() {
        let mut fake = Fake::default();
        fake.connects.push_back(Ok("boot-1"));
        fake.connects.push_back(Err(TIMED_OUT));
        fake.connects.push_back(Err(REFUSED));
        let link = run(&mut fake, 4);
        assert_eq!(link, Link::Gone);
        assert_eq!(Link::Gone.step(&mut fake), Link::Gone);
    }

    #[test]
    fn a_restarted_server_gets_a_full_resync() {
        let mut fake = Fake::default();
        fake.connects
            .extend([Ok("boot-1"), Ok("boot-1"), Ok("boot-2")]);
        run(&mut fake, 5);
        assert_eq!(fake.resyncs, vec![true, false, true]);
    }

    #[test]
    fn only_a_missing_or_refused_socket_counts_as_death() {
        assert!(matches!(Failure::from(anyhow!(REFUSED)), Failure::Dead));
        assert!(matches!(
            Failure::from(anyhow!(TIMED_OUT)),
            Failure::Unsure(_)
        ));
        assert!(matches!(
            Failure::from(anyhow!("Failed to parse rex whoami output")),
            Failure::Unsure(_)
        ));
    }

    #[test]
    fn a_connection_that_held_resets_the_backoff() {
        let connected = |held| Link::Connected {
            boot_id: "boot-1".into(),
            since: Instant::now() - held,
            attempt: 6,
        };
        let mut fake = Fake::default();
        assert!(matches!(
            connected(STABLE).step(&mut fake),
            Link::Retrying { attempt: 1, .. }
        ));
        assert!(matches!(
            connected(Duration::ZERO).step(&mut fake),
            Link::Retrying { attempt: 7, .. }
        ));
    }

    #[test]
    fn a_stream_that_ends_at_once_backs_off() {
        let mut fake = Fake::default();
        fake.connects.extend(std::iter::repeat_n(Ok("boot-1"), 20));
        run(&mut fake, 40);
        assert_eq!(fake.follows, 20);
        assert_eq!(fake.waits.last(), Some(&BACKOFF_CAP));
    }
}
