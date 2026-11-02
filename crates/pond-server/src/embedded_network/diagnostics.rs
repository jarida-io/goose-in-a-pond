//! What the Pond's log says about its tailnet node: an outage that outlasts a grace period, the
//! node's recovery from it, and the networking helper's own stderr.
//!
//! The household hears about an outage through `system_notices`; this is the engineer's side.
//! Before it existed, a node that could not reach its coordinator sat in `NoState` for hours and
//! the log held one state change and nothing else, because the helper's backend log is discarded
//! and its stderr went only to the service journal.

use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::ChildStderr;

use super::{helper_complaint, Status};

/// How long the node may stay short of `Running` before the log says so. Longer than a normal
/// start, which takes seconds, and than the backend's own wait between attempts to reach the
/// control server, which is at most 45 s (a 30 s cap with up to 50% jitter).
pub(super) const DOWN_GRACE: Duration = Duration::from_secs(120);

/// Most helper stderr lines logged in one minute. Anything more is counted, and the count logged.
const HELPER_LINES_PER_MINUTE: u32 = 20;

/// The longest helper stderr line read in one piece; a longer one is logged in pieces.
const HELPER_LINE_BYTES: u64 = 4096;

/// What an outage of the node calls for in the log.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DownLog {
    /// Short of `Running` for longer than [`DOWN_GRACE`]: this long so far. Once per outage.
    StillDown(Duration),
    /// `Running` again after an outage that was reported, which lasted this long.
    Recovered(Duration),
}

/// One outage of the node, measured on the monotonic clock. A board with no working real-time
/// clock boots with the time it last recorded and jumps hours forward when a time server first
/// answers; that jump must not change how long remote access was down.
#[derive(Default)]
pub(super) struct DownWatch {
    since: Option<Instant>,
    warned: bool,
}

impl DownWatch {
    /// Record the state the node reported at `now`; returns what the log should say, if anything.
    pub(super) fn observe(&mut self, state: &str, now: Instant) -> Option<DownLog> {
        match state {
            "Running" => {
                let since = self.since.take()?;
                std::mem::take(&mut self.warned)
                    .then(|| DownLog::Recovered(now.saturating_duration_since(since)))
            }
            // Switched off, or the Pond is stopping: the household's doing, not an outage.
            "Stopped" => {
                *self = Self::default();
                None
            }
            _ => {
                let down = now.saturating_duration_since(*self.since.get_or_insert(now));
                if self.warned || down < DOWN_GRACE {
                    return None;
                }
                self.warned = true;
                Some(DownLog::StillDown(down))
            }
        }
    }
}

/// Write what [`DownWatch::observe`] decided, with what the node said about itself.
pub(super) fn log_outage(event: DownLog, status: &Status) {
    match event {
        DownLog::StillDown(down) => {
            let reason = if status.health.is_empty() {
                "the node gave no reason".to_string()
            } else {
                status.health.join("; ")
            };
            let clock_synchronized = match status.clock_synchronized {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            };
            if status.clock_synchronized == Some(false) {
                tracing::warn!(
                    kind = "remote_access_not_running",
                    state = status.state,
                    down_secs = down.as_secs(),
                    %reason,
                    clock_synchronized,
                    "remote access is not running, so a phone away from home cannot reach this Pond; \
                     this host's clock is not synchronised either, and a clock far enough off fails \
                     TLS to the coordinator (bash scripts/giap.sh doctor)"
                );
            } else {
                tracing::warn!(
                    kind = "remote_access_not_running",
                    state = status.state,
                    down_secs = down.as_secs(),
                    %reason,
                    clock_synchronized,
                    "remote access is not running, so a phone away from home cannot reach this Pond"
                );
            }
        }
        DownLog::Recovered(down) => tracing::info!(
            kind = "remote_access_running_again",
            down_secs = down.as_secs(),
            "remote access is running again"
        ),
    }
}

/// Whether one helper stderr line is logged.
#[derive(Debug, PartialEq, Eq)]
enum Admit {
    /// Log it, saying how many lines were dropped since the last one logged.
    Line { dropped: u32 },
    /// The first line over this minute's budget: say that lines are being dropped, then drop it.
    FirstDropped,
    /// Drop it; it is counted.
    Drop,
}

/// A per-minute budget for helper stderr, so a helper stuck printing cannot fill the log.
#[derive(Default)]
struct LineBudget {
    window: Option<Instant>,
    used: u32,
    dropped: u32,
}

impl LineBudget {
    fn admit(&mut self, now: Instant) -> Admit {
        let window = *self.window.get_or_insert(now);
        if now.saturating_duration_since(window) >= Duration::from_secs(60) {
            self.window = Some(now);
            self.used = 0;
        }
        if self.used < HELPER_LINES_PER_MINUTE {
            self.used += 1;
            return Admit::Line {
                dropped: std::mem::take(&mut self.dropped),
            };
        }
        self.dropped += 1;
        if self.dropped == 1 {
            Admit::FirstDropped
        } else {
            Admit::Drop
        }
    }
}

/// Copy the helper's stderr into the Pond's log until the helper exits.
///
/// The helper writes there only when something is wrong (it could not start, its listener
/// stopped, it panicked), so each line is a warning. Every line goes through [`helper_complaint`]:
/// one line, bounded, and with any node-authorisation URL redacted.
pub(super) fn forward_helper_stderr(stderr: ChildStderr) {
    tokio::spawn(async move {
        let mut reader = BufReader::new(stderr);
        let mut budget = LineBudget::default();
        loop {
            let mut line = Vec::new();
            match (&mut reader)
                .take(HELPER_LINE_BYTES)
                .read_until(b'\n', &mut line)
                .await
            {
                Ok(0) => break,
                Ok(_) => (),
                Err(error) => {
                    tracing::warn!(%error, "stopped reading the networking helper's stderr");
                    break;
                }
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match budget.admit(Instant::now()) {
                Admit::Line { dropped } => tracing::warn!(
                    kind = "network_helper_output",
                    dropped,
                    line = %helper_complaint(&line),
                    "the networking helper reported a problem"
                ),
                Admit::FirstDropped => tracing::warn!(
                    kind = "network_helper_output_limited",
                    per_minute = HELPER_LINES_PER_MINUTE,
                    "the networking helper is writing more than the log takes; the rest of this \
                     minute's lines are counted, not logged"
                ),
                Admit::Drop => (),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_normal_start_says_nothing_beyond_its_state_changes() {
        let t0 = Instant::now();
        let mut watch = DownWatch::default();
        assert_eq!(watch.observe("Starting", t0), None);
        assert_eq!(watch.observe("NoState", t0 + 2 * SECOND), None);
        assert_eq!(watch.observe("Running", t0 + 10 * SECOND), None);
    }

    #[test]
    fn an_outage_past_the_grace_is_logged_once_and_its_end_with_its_length() {
        let t0 = Instant::now();
        let mut watch = DownWatch::default();
        assert_eq!(watch.observe("Starting", t0), None);
        assert_eq!(watch.observe("NoState", t0 + DOWN_GRACE - SECOND), None);
        assert_eq!(
            watch.observe("NoState", t0 + DOWN_GRACE),
            Some(DownLog::StillDown(DOWN_GRACE))
        );
        // Every two seconds for four hours, as the helper reports, and nothing more is said.
        for tick in 1..7200 {
            assert_eq!(
                watch.observe("NoState", t0 + DOWN_GRACE + 2 * tick * SECOND),
                None
            );
        }
        let back = t0 + Duration::from_secs(4 * 60 * 60);
        assert_eq!(
            watch.observe("Running", back),
            Some(DownLog::Recovered(Duration::from_secs(4 * 60 * 60)))
        );
        assert_eq!(watch.observe("Running", back + SECOND), None);
    }

    #[test]
    fn every_state_short_of_running_is_down_and_a_restart_continues_the_outage() {
        let t0 = Instant::now();
        let mut watch = DownWatch::default();
        assert_eq!(watch.observe("NeedsLogin", t0), None);
        // The helper died and was started again: one outage, measured from its start.
        assert_eq!(watch.observe("Unavailable", t0 + 30 * SECOND), None);
        assert_eq!(watch.observe("Starting", t0 + 60 * SECOND), None);
        assert_eq!(
            watch.observe("NoState", t0 + DOWN_GRACE + SECOND),
            Some(DownLog::StillDown(DOWN_GRACE + SECOND))
        );
    }

    #[test]
    fn switching_remote_access_off_ends_the_outage_without_a_recovery() {
        let t0 = Instant::now();
        let mut watch = DownWatch::default();
        watch.observe("NoState", t0);
        assert!(watch.observe("NoState", t0 + DOWN_GRACE).is_some());
        assert_eq!(watch.observe("Stopped", t0 + DOWN_GRACE + SECOND), None);
        // Switched on again: a fresh outage with a fresh grace, and no stale recovery line.
        let again = t0 + 10 * DOWN_GRACE;
        assert_eq!(watch.observe("Starting", again), None);
        assert_eq!(watch.observe("Running", again + SECOND), None);
    }

    #[test]
    fn an_unreported_outage_ends_quietly() {
        let t0 = Instant::now();
        let mut watch = DownWatch::default();
        watch.observe("NoState", t0);
        assert_eq!(watch.observe("Running", t0 + DOWN_GRACE - SECOND), None);
    }

    #[test]
    fn helper_output_is_capped_per_minute_and_the_dropped_lines_are_counted() {
        let t0 = Instant::now();
        let mut budget = LineBudget::default();
        for _ in 0..HELPER_LINES_PER_MINUTE {
            assert_eq!(budget.admit(t0), Admit::Line { dropped: 0 });
        }
        assert_eq!(budget.admit(t0 + SECOND), Admit::FirstDropped);
        assert_eq!(budget.admit(t0 + 2 * SECOND), Admit::Drop);
        assert_eq!(budget.admit(t0 + 3 * SECOND), Admit::Drop);
        // The next minute logs again, and says how much was dropped.
        assert_eq!(budget.admit(t0 + 60 * SECOND), Admit::Line { dropped: 3 });
        assert_eq!(budget.admit(t0 + 61 * SECOND), Admit::Line { dropped: 0 });
    }
}
