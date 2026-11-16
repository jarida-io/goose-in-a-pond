//! What the Pond tells the remote-access coordinator about itself, so a phone away from home can
//! learn why the Pond does not answer (`docs/remote-access.md`, "Why the Pond is offline").
//!
//! The Pond reports three kinds of thing: a heartbeat while it runs, `booted` once after it
//! starts, and `going_down` when it stops on purpose. The coordinator stamps each with its own
//! clock and works out the rest. This module decides what is sent and when; the transport is the
//! [`PondStatusReporter`](crate::network::ports::pond_status_reporter::PondStatusReporter) port.
//!
//! Nothing here reads the Pond's wall clock. A Jetson's clock restarts after a power cut at the
//! last time it saved and lags until its time server answers, so every duration is either the
//! coordinator's (from its receive times) or the host's monotonic uptime.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How often a running Pond reports. The coordinator's `HeartbeatInterval`
/// (`native/pondnet/enrollment/status.go`) must match: it calls a Pond unreachable after two
/// missed heartbeats.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// The first wait after a report that did not arrive. It doubles up to [`RETRY_CEILING`].
pub const RETRY_FLOOR: Duration = Duration::from_secs(30);

/// The longest wait between attempts while reports are not arriving.
pub const RETRY_CEILING: Duration = Duration::from_secs(2 * 60);

/// How long after a start the power-loss notice waits for the coordinator's account of how long
/// the Pond was off, before it is announced without one. After the 2026-11-12 cut the home
/// internet took about eight minutes to come back.
pub const NOTICE_WAIT: Duration = Duration::from_secs(15 * 60);

/// Why the Pond is stopping on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The household asked the Pond to restart.
    Restart,
    /// New software is being installed.
    Update,
    /// Stopped with no reason given: the service manager, or the host shutting down.
    Shutdown,
}

impl StopReason {
    /// The reason a stop-intent file names, if it names one this Pond knows. The file holds one
    /// word, written by whatever asked for the stop.
    pub fn from_intent(text: &str) -> Option<Self> {
        match text.trim() {
            "restart" => Some(Self::Restart),
            "update" => Some(Self::Update),
            "shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }

    /// The word for this reason, as the stop-intent file and the coordinator spell it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restart => "restart",
            Self::Update => "update",
            Self::Shutdown => "shutdown",
        }
    }
}

/// How the Pond's previous run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviousShutdown {
    /// It stopped through its graceful path.
    Clean,
    /// It stopped without one: the power went, the host hung, or the process was killed.
    Unclean,
}

/// What the run marker says about the previous run. The marker is written when the Pond starts,
/// holding the host's boot id, and removed at the end of its graceful stop; a marker found at
/// start means the previous run never reached that end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreviousRun {
    /// How the previous run ended. A first start, with no marker, counts as clean.
    pub shutdown: PreviousShutdown,
    /// Whether the host restarted since the previous run started; `None` where either boot id
    /// is unknown.
    pub rebooted: Option<bool>,
}

impl PreviousRun {
    /// Reads the marker the previous run left, if any, against this boot's id.
    pub fn from_marker(marker: Option<&str>, current_boot: Option<&str>) -> Self {
        let Some(marker) = marker else {
            return Self {
                shutdown: PreviousShutdown::Clean,
                rebooted: None,
            };
        };
        let previous_boot = Some(marker.trim()).filter(|id| !id.is_empty());
        let rebooted = match (previous_boot, current_boot) {
            (Some(previous), Some(current)) => Some(previous != current),
            _ => None,
        };
        Self {
            shutdown: PreviousShutdown::Unclean,
            rebooted,
        }
    }

    /// Whether the previous run ended because the host lost power: it never stopped gracefully
    /// and the host restarted under it. A hard reset or a kernel hang looks the same; a process
    /// that crashed on a host that kept running does not.
    pub fn lost_power(&self) -> bool {
        self.shutdown == PreviousShutdown::Unclean && self.rebooted == Some(true)
    }
}

/// A boot id as the kernel writes it (`/proc/sys/kernel/random/boot_id`), or `None` for anything
/// else, so a malformed one is never sent.
pub fn boot_id(raw: &str) -> Option<String> {
    let id = raw.trim();
    let groups: Vec<&str> = id.split('-').collect();
    let shaped = groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, len)| group.len() == len)
        && id
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    shaped.then(|| id.to_string())
}

/// What one report says. The household and the signature's expiry are added by the helper that
/// signs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusReport {
    /// `heartbeat`, `going_down` or `booted`.
    pub kind: ReportKind,
    /// Sent with `going_down` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<StopReason>,
    /// Sent with `booted` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_shutdown: Option<PreviousShutdown>,
    /// The host's boot id, where it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_id: Option<String>,
    /// This run of the Pond; with `sequence` it lets the coordinator refuse a replay.
    pub run: String,
    /// Counts this run's attempts from one, retries included.
    pub sequence: u64,
    /// The host's monotonic uptime; at least the process's own where the host's is unknown.
    pub uptime_seconds: u64,
    /// Whether the host's clock was synchronised to a time source, where the helper can tell.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clock_synchronized: Option<bool>,
    /// What the Pond knows about its connection.
    pub network: NetworkFacts,
}

/// The kind of a [`StatusReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportKind {
    /// The Pond is running.
    Heartbeat,
    /// The Pond is about to stop on purpose.
    GoingDown,
    /// The Pond has started; the first report of every run.
    Booted,
}

/// What the Pond knows about how a report reaches the coordinator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetworkFacts {
    /// How many attempts at reporting failed before this one.
    pub failed_attempts: u64,
    /// How long this Pond process had run.
    pub process_seconds: u64,
    /// The remote-access node's state, such as `Running`; omitted unless it is a plain word.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub node_state: String,
}

/// One look at the host, taken just before a report is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReading {
    /// Remote access is on and the household is registered, so there is a coordinator to tell.
    pub reporting: bool,
    /// The host's boot id, where it has one.
    pub boot_id: Option<String>,
    /// The host's uptime from its monotonic clock, where it can be read.
    pub uptime_seconds: Option<u64>,
    /// How long this Pond process has run.
    pub process_seconds: u64,
    /// Whether the clock is synchronised, where the helper can tell.
    pub clock_synchronized: Option<bool>,
    /// The remote-access node's state.
    pub node_state: String,
}

/// The coordinator's answer to a report: what it now believes about the Pond.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoordinatorView {
    /// `online`, `back`, `planned_restart`, `unreachable` or `unknown`.
    pub state: String,
    /// The most recent outage that has ended, as the coordinator timed it.
    #[serde(default)]
    pub last_outage: Option<Outage>,
}

/// One outage as the coordinator timed it, in its own Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outage {
    /// `power_loss`, `network_loss`, `planned` or `unknown`.
    pub cause: String,
    /// When the coordinator last heard from the Pond before the outage.
    pub started_at: i64,
    /// When the outage ended: for a power loss, when the host came back up.
    pub ended_at: i64,
}

impl Outage {
    /// How long it lasted, or `None` for a span that makes no sense.
    pub fn duration(&self) -> Option<Duration> {
        let seconds = self.ended_at.checked_sub(self.started_at)?;
        (seconds >= 0).then(|| Duration::from_secs(seconds as u64))
    }
}

/// Which report is due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// The first report of this run, still not delivered.
    Booted,
    /// The next heartbeat.
    Heartbeat,
}

/// The power-loss notice, once it is decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerNotice {
    /// Announce that the Pond lost power, with how long it was off when that is known.
    Announce { off_for: Option<Duration> },
}

/// When each report is due, and when the power-loss notice is ready. Pure: the caller supplies the
/// time and says how each attempt went.
#[derive(Debug, Clone)]
pub struct ReportSchedule {
    previous: PreviousRun,
    started: Instant,
    booted_delivered: bool,
    next_due: Option<Instant>,
    failed_attempts: u64,
    sequence: u64,
    stopping: bool,
    notice_settled: bool,
}

impl ReportSchedule {
    /// A schedule for a run that started at `started` after `previous`.
    pub fn new(previous: PreviousRun, started: Instant) -> Self {
        Self {
            previous,
            started,
            booted_delivered: false,
            next_due: None,
            failed_attempts: 0,
            sequence: 0,
            // A Pond that did not lose power has nothing to announce.
            notice_settled: !previous.lost_power(),
            stopping: false,
        }
    }

    /// What the previous run's end was.
    pub fn previous(&self) -> PreviousRun {
        self.previous
    }

    /// The report due at `now`, if any. Nothing is due while reporting is off or once the Pond
    /// is stopping.
    pub fn due(&self, reporting: bool, now: Instant) -> Option<Due> {
        if !reporting || self.stopping || self.next_due.is_some_and(|at| now < at) {
            return None;
        }
        Some(if self.booted_delivered {
            Due::Heartbeat
        } else {
            Due::Booted
        })
    }

    /// The sequence number for the next attempt. Every attempt gets its own, so a retry is
    /// never refused as a replay of the attempt before it.
    pub fn next_sequence(&mut self) -> u64 {
        self.sequence += 1;
        self.sequence
    }

    /// How many attempts failed since the last delivery.
    pub fn failed_attempts(&self) -> u64 {
        self.failed_attempts
    }

    /// The report due as `due` arrived at `now`.
    pub fn delivered(&mut self, due: Due, now: Instant) {
        if due == Due::Booted {
            self.booted_delivered = true;
        }
        self.failed_attempts = 0;
        self.next_due = Some(now + HEARTBEAT_INTERVAL);
    }

    /// An attempt at `now` did not arrive; the next waits longer, up to [`RETRY_CEILING`].
    pub fn failed(&mut self, now: Instant) {
        let wait = RETRY_FLOOR
            .saturating_mul(1 << self.failed_attempts.min(8))
            .min(RETRY_CEILING);
        self.failed_attempts += 1;
        self.next_due = Some(now + wait);
    }

    /// The Pond is stopping: nothing more is due.
    pub fn stop(&mut self) {
        self.stopping = true;
    }

    /// Whether the power-loss notice is ready at `now`, given the coordinator's answer to the
    /// `booted` report when it has arrived. Ready once: with the coordinator's duration when its
    /// answer names a power loss; without one when it names something else, when there is no
    /// coordinator to ask, or when [`NOTICE_WAIT`] has passed without an answer.
    pub fn power_notice(
        &mut self,
        reporting: bool,
        answer: Option<&CoordinatorView>,
        now: Instant,
    ) -> Option<PowerNotice> {
        if self.notice_settled {
            return None;
        }
        let off_for = match answer {
            Some(view) => view
                .last_outage
                .as_ref()
                .filter(|outage| outage.cause == "power_loss")
                .and_then(Outage::duration),
            None if !reporting || now.duration_since(self.started) >= NOTICE_WAIT => None,
            None => return None,
        };
        self.notice_settled = true;
        Some(PowerNotice::Announce { off_for })
    }
}

/// The report for `kind`, from a reading of the host.
pub fn report(
    kind: ReportKind,
    reading: &HostReading,
    run: &str,
    sequence: u64,
    failed_attempts: u64,
    previous: PreviousRun,
    reason: Option<StopReason>,
) -> StatusReport {
    let plain_word = !reading.node_state.is_empty()
        && reading.node_state.len() <= 32
        && reading.node_state.bytes().all(|b| b.is_ascii_alphabetic());
    StatusReport {
        kind,
        reason: (kind == ReportKind::GoingDown).then(|| reason.unwrap_or(StopReason::Shutdown)),
        previous_shutdown: (kind == ReportKind::Booted).then_some(previous.shutdown),
        boot_id: reading.boot_id.clone(),
        run: run.to_string(),
        sequence,
        uptime_seconds: reading
            .uptime_seconds
            .unwrap_or(0)
            .max(reading.process_seconds),
        clock_synchronized: reading.clock_synchronized,
        network: NetworkFacts {
            failed_attempts,
            process_seconds: reading.process_seconds,
            node_state: if plain_word {
                reading.node_state.clone()
            } else {
                String::new()
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const BOOT_A: &str = "0b5e1c2a-1111-4222-8333-944455556666";
    const BOOT_B: &str = "7f00aa10-2222-4333-8444-a55566667777";

    fn reading() -> HostReading {
        HostReading {
            reporting: true,
            boot_id: Some(BOOT_A.into()),
            uptime_seconds: Some(500),
            process_seconds: 40,
            clock_synchronized: Some(false),
            node_state: "Running".into(),
        }
    }

    #[test]
    fn a_marker_left_behind_on_another_boot_is_a_power_loss() {
        let lost = PreviousRun::from_marker(Some(BOOT_A), Some(BOOT_B));
        assert_eq!(lost.shutdown, PreviousShutdown::Unclean);
        assert!(lost.lost_power());

        let crashed = PreviousRun::from_marker(Some(&format!("{BOOT_A}\n")), Some(BOOT_A));
        assert_eq!(crashed.shutdown, PreviousShutdown::Unclean);
        assert!(
            !crashed.lost_power(),
            "a crash on a running host is not a power cut"
        );

        let unknowable = PreviousRun::from_marker(Some(""), Some(BOOT_A));
        assert_eq!(unknowable.rebooted, None);
        assert!(!unknowable.lost_power());

        let clean = PreviousRun::from_marker(None, Some(BOOT_A));
        assert_eq!(clean.shutdown, PreviousShutdown::Clean);
        assert!(!clean.lost_power());
    }

    #[test]
    fn only_a_kernel_shaped_boot_id_is_kept() {
        assert_eq!(boot_id(&format!("{BOOT_A}\n")).as_deref(), Some(BOOT_A));
        for bad in [
            "",
            "not-a-boot-id",
            "0B5E1C2A-1111-4222-8333-944455556666",
            "0b5e1c2a11114222833394445555666600",
        ] {
            assert_eq!(boot_id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn stop_intents_name_only_known_reasons() {
        assert_eq!(
            StopReason::from_intent("update\n"),
            Some(StopReason::Update)
        );
        assert_eq!(
            StopReason::from_intent("restart"),
            Some(StopReason::Restart)
        );
        assert_eq!(StopReason::from_intent("nap"), None);
        assert_eq!(StopReason::Update.as_str(), "update");
    }

    #[test]
    fn booted_is_sent_until_it_arrives_then_heartbeats_every_interval() {
        let start = Instant::now();
        let mut schedule = ReportSchedule::new(PreviousRun::from_marker(None, None), start);
        assert_eq!(
            schedule.due(false, start),
            None,
            "nothing while reporting is off"
        );
        assert_eq!(schedule.due(true, start), Some(Due::Booted));

        schedule.failed(start);
        assert_eq!(schedule.due(true, start + Duration::from_secs(29)), None);
        assert_eq!(schedule.due(true, start + RETRY_FLOOR), Some(Due::Booted));
        let mut at = start + RETRY_FLOOR;
        for _ in 0..6 {
            schedule.failed(at);
            at += RETRY_CEILING;
        }
        assert_eq!(schedule.failed_attempts(), 7);
        assert_eq!(
            schedule.due(true, at),
            Some(Due::Booted),
            "the wait never passes the ceiling"
        );

        schedule.delivered(Due::Booted, at);
        assert_eq!(schedule.failed_attempts(), 0);
        assert_eq!(schedule.due(true, at + Duration::from_secs(60)), None);
        assert_eq!(
            schedule.due(true, at + HEARTBEAT_INTERVAL),
            Some(Due::Heartbeat)
        );

        schedule.stop();
        assert_eq!(schedule.due(true, at + 10 * HEARTBEAT_INTERVAL), None);
    }

    #[test]
    fn every_attempt_has_its_own_sequence() {
        let mut schedule =
            ReportSchedule::new(PreviousRun::from_marker(None, None), Instant::now());
        assert_eq!((schedule.next_sequence(), schedule.next_sequence()), (1, 2));
    }

    fn view(cause: &str, started_at: i64, ended_at: i64) -> CoordinatorView {
        CoordinatorView {
            state: "back".into(),
            last_outage: Some(Outage {
                cause: cause.into(),
                started_at,
                ended_at,
            }),
        }
    }

    #[test]
    fn the_power_notice_carries_the_coordinators_duration_once() {
        let start = Instant::now();
        let mut schedule =
            ReportSchedule::new(PreviousRun::from_marker(Some(BOOT_A), Some(BOOT_B)), start);
        assert_eq!(
            schedule.power_notice(true, None, start),
            None,
            "waits for the answer"
        );
        let answer = view("power_loss", 1_000, 1_000 + 90 * 60);
        assert_eq!(
            schedule.power_notice(true, Some(&answer), start),
            Some(PowerNotice::Announce {
                off_for: Some(Duration::from_secs(90 * 60))
            })
        );
        assert_eq!(
            schedule.power_notice(true, Some(&answer), start),
            None,
            "once"
        );
    }

    #[test]
    fn the_power_notice_goes_without_a_duration_when_none_can_be_had() {
        let start = Instant::now();
        let lost = PreviousRun::from_marker(Some(BOOT_A), Some(BOOT_B));

        let mut waited = ReportSchedule::new(lost, start);
        assert_eq!(
            waited.power_notice(true, None, start + NOTICE_WAIT - Duration::from_secs(1)),
            None
        );
        assert_eq!(
            waited.power_notice(true, None, start + NOTICE_WAIT),
            Some(PowerNotice::Announce { off_for: None })
        );

        let mut no_coordinator = ReportSchedule::new(lost, start);
        assert_eq!(
            no_coordinator.power_notice(false, None, start),
            Some(PowerNotice::Announce { off_for: None })
        );

        let mut disagrees = ReportSchedule::new(lost, start);
        assert_eq!(
            disagrees.power_notice(true, Some(&view("planned", 1, 2)), start),
            Some(PowerNotice::Announce { off_for: None }),
            "the coordinator's duration is used only for the outage it calls a power loss"
        );

        let mut clean = ReportSchedule::new(PreviousRun::from_marker(None, Some(BOOT_B)), start);
        assert_eq!(clean.power_notice(false, None, start + NOTICE_WAIT), None);
    }

    #[test]
    fn reports_serialise_as_the_coordinator_reads_them() {
        let previous = PreviousRun::from_marker(Some(BOOT_A), Some(BOOT_B));
        let booted = report(
            ReportKind::Booted,
            &reading(),
            "run-0123456789abcdef",
            1,
            3,
            previous,
            None,
        );
        assert_eq!(
            serde_json::to_value(&booted).unwrap(),
            json!({
                "kind": "booted",
                "previousShutdown": "unclean",
                "bootId": BOOT_A,
                "run": "run-0123456789abcdef",
                "sequence": 1,
                "uptimeSeconds": 500,
                "clockSynchronized": false,
                "network": {"failedAttempts": 3, "processSeconds": 40, "nodeState": "Running"},
            })
        );

        let mut odd = reading();
        odd.node_state = "Needs Login; really".into();
        odd.uptime_seconds = None;
        odd.boot_id = None;
        odd.clock_synchronized = None;
        let down = report(
            ReportKind::GoingDown,
            &odd,
            "run-0123456789abcdef",
            9,
            0,
            previous,
            Some(StopReason::Update),
        );
        assert_eq!(
            serde_json::to_value(&down).unwrap(),
            json!({
                "kind": "going_down",
                "reason": "update",
                "run": "run-0123456789abcdef",
                "sequence": 9,
                "uptimeSeconds": 40,
                "network": {"failedAttempts": 0, "processSeconds": 40},
            })
        );
        let heartbeat = report(
            ReportKind::Heartbeat,
            &reading(),
            "run-0123456789abcdef",
            2,
            0,
            previous,
            Some(StopReason::Update),
        );
        let value = serde_json::to_value(&heartbeat).unwrap();
        assert!(value.get("reason").is_none() && value.get("previousShutdown").is_none());
    }

    #[test]
    fn the_coordinators_answer_reads_with_or_without_an_outage() {
        let plain: CoordinatorView =
            serde_json::from_value(json!({"state": "online", "lastSeen": 5, "now": 6})).unwrap();
        assert_eq!(plain.last_outage, None);
        let back: CoordinatorView = serde_json::from_value(json!({
            "state": "back",
            "lastOutage": {"cause": "power_loss", "startedAt": 100, "endedAt": 5500},
            "now": 5600
        }))
        .unwrap();
        assert_eq!(
            back.last_outage.unwrap().duration(),
            Some(Duration::from_secs(5400))
        );
        let backwards = Outage {
            cause: "unknown".into(),
            started_at: 10,
            ended_at: 5,
        };
        assert_eq!(backwards.duration(), None);
    }
}
