//! Sends the Pond's status reports on their schedule, settles the power-loss notice and hands over
//! internet cuts the coordinator timed. The
//! decisions are [`pond_status`](crate::network::domain::pond_status)'s; this drives them
//! through the [`PondStatusReporter`] port. `serve` calls [`PondStatusReports::tick`] every
//! [`TICK`] and [`PondStatusReports::going_down`] on its graceful stop.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use crate::network::domain::pond_status::{
    report, CoordinatorView, Due, HostReading, InternetCuts, PowerNotice, PreviousRun, ReportKind,
    ReportSchedule, StopReason,
};
use crate::network::ports::pond_status_reporter::PondStatusReporter;

/// How often `serve` should call [`PondStatusReports::tick`]: the shortest retry wait.
pub const TICK: Duration = crate::network::domain::pond_status::RETRY_FLOOR;

/// The Pond's reports to the coordinator for one run of the process.
pub struct PondStatusReports {
    reporter: Arc<dyn PondStatusReporter>,
    run: String,
    schedule: Mutex<Settling>,
}

struct Settling {
    schedule: ReportSchedule,
    /// The coordinator's answer to `booted`, once it arrived.
    booted_answer: Option<CoordinatorView>,
    cuts: InternetCuts,
    /// An internet cut the coordinator timed that has not been handed to the caller yet.
    untold_cut: Option<Duration>,
}

impl PondStatusReports {
    /// Reports for the run named `run` (16 to 80 letters, digits, `-` or `_`), which started at
    /// `started` after `previous`.
    pub fn new(
        reporter: Arc<dyn PondStatusReporter>,
        run: String,
        previous: PreviousRun,
        started: Instant,
    ) -> Self {
        Self {
            reporter,
            run,
            schedule: Mutex::new(Settling {
                schedule: ReportSchedule::new(previous, started),
                booted_answer: None,
                cuts: InternetCuts::default(),
                untold_cut: None,
            }),
        }
    }

    /// Sends the report due at `now`, if one is, and returns the power-loss notice once it is
    /// ready to announce.
    pub async fn tick(&self, reading: &HostReading, now: Instant) -> Option<PowerNotice> {
        let mut settling = self.schedule.lock().await;
        if let Some(due) = settling.schedule.due(reading.reporting, now) {
            let kind = match due {
                Due::Booted => ReportKind::Booted,
                Due::Heartbeat => ReportKind::Heartbeat,
            };
            let failed_before = settling.schedule.failed_attempts();
            let sequence = settling.schedule.next_sequence();
            let sent = report(
                kind,
                reading,
                &self.run,
                sequence,
                failed_before,
                settling.schedule.previous(),
                None,
            );
            match self.reporter.report(&sent).await {
                Ok(answer) => {
                    settling.schedule.delivered(due, now);
                    if failed_before > 0 || due == Due::Booted {
                        tracing::info!(
                            target: "giap::trace",
                            kind = "pond_status_delivered",
                            report = ?kind,
                            failed_attempts = failed_before,
                            coordinator_state = %answer.state,
                            outage_cause = answer.last_outage.as_ref().map(|o| o.cause.as_str()).unwrap_or(""),
                            "the coordinator has the Pond's status"
                        );
                    }
                    if let Some(lasted) = settling.cuts.newly_ended(&answer) {
                        settling.untold_cut = Some(lasted);
                    }
                    if due == Due::Booted {
                        settling.booted_answer = Some(answer);
                    }
                }
                Err(error) => {
                    settling.schedule.failed(now);
                    // Once per streak: the next attempts fail the same way until the home's
                    // connection or the coordinator comes back.
                    if failed_before == 0 {
                        tracing::warn!(
                            target: "giap::trace",
                            kind = "pond_status_undelivered",
                            report = ?kind,
                            error = %format!("{error:#}"),
                            "the Pond's status did not reach the coordinator; retrying"
                        );
                    }
                }
            }
        }
        let Settling {
            schedule,
            booted_answer,
            ..
        } = &mut *settling;
        schedule.power_notice(reading.reporting, booted_answer.as_ref(), now)
    }

    /// How long the internet cut the coordinator last reported lasted, once per cut: a network
    /// loss of at least two minutes that ended during this run. `serve` asks after every
    /// [`tick`](Self::tick).
    pub async fn take_internet_cut(&self) -> Option<Duration> {
        self.schedule.lock().await.untold_cut.take()
    }

    /// Tells the coordinator the Pond is stopping on purpose, and stops every later report.
    /// Returns whether the coordinator has it; the caller bounds how long it waits.
    pub async fn going_down(&self, reason: StopReason, reading: &HostReading) -> bool {
        let mut settling = self.schedule.lock().await;
        settling.schedule.stop();
        if !reading.reporting {
            return false;
        }
        let sequence = settling.schedule.next_sequence();
        let sent = report(
            ReportKind::GoingDown,
            reading,
            &self.run,
            sequence,
            settling.schedule.failed_attempts(),
            settling.schedule.previous(),
            Some(reason),
        );
        match self.reporter.report(&sent).await {
            Ok(_) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "pond_status_going_down",
                    reason = reason.as_str(),
                    "the coordinator knows the Pond is stopping"
                );
                true
            }
            Err(error) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "pond_status_going_down_undelivered",
                    reason = reason.as_str(),
                    error = %format!("{error:#}"),
                    "the coordinator was not told the Pond is stopping; phones will see it go silent"
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::domain::pond_status::{Outage, PreviousShutdown, HEARTBEAT_INTERVAL};
    use crate::network::mocks::mock_pond_status_reporter::MockPondStatusReporter;

    const BOOT_A: &str = "0b5e1c2a-1111-4222-8333-944455556666";
    const BOOT_B: &str = "7f00aa10-2222-4333-8444-a55566667777";

    fn reading(reporting: bool) -> HostReading {
        HostReading {
            reporting,
            boot_id: Some(BOOT_B.into()),
            uptime_seconds: Some(600),
            process_seconds: 60,
            clock_synchronized: Some(true),
            node_state: "Running".into(),
        }
    }

    fn reports(
        previous: PreviousRun,
        start: Instant,
    ) -> (Arc<MockPondStatusReporter>, PondStatusReports) {
        let reporter = Arc::new(MockPondStatusReporter::new());
        let reports = PondStatusReports::new(
            reporter.clone(),
            "run-0123456789abcdef".into(),
            previous,
            start,
        );
        (reporter, reports)
    }

    #[tokio::test]
    async fn after_a_power_cut_the_notice_waits_for_the_coordinators_duration() {
        let start = Instant::now();
        let (reporter, reports) =
            reports(PreviousRun::from_marker(Some(BOOT_A), Some(BOOT_B)), start);
        reporter.set_failing(true);
        assert_eq!(
            reports.tick(&reading(true), start).await,
            None,
            "home internet still down"
        );
        assert!(reporter.sent().is_empty());

        reporter.set_failing(false);
        reporter.set_answer(CoordinatorView {
            state: "back".into(),
            last_outage: Some(Outage {
                cause: "power_loss".into(),
                started_at: 10_000,
                ended_at: 10_000 + 85 * 60,
            }),
        });
        let notice = reports.tick(&reading(true), start + TICK).await;
        assert_eq!(
            notice,
            Some(PowerNotice::Announce {
                off_for: Some(Duration::from_secs(85 * 60))
            })
        );
        let sent = reporter.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].kind, ReportKind::Booted);
        assert_eq!(sent[0].previous_shutdown, Some(PreviousShutdown::Unclean));
        assert_eq!(sent[0].network.failed_attempts, 1);
        assert_eq!(sent[0].sequence, 2, "the failed attempt spent sequence 1");

        // Heartbeats follow on the interval, and the notice is not repeated.
        assert_eq!(
            reports
                .tick(&reading(true), start + TICK + HEARTBEAT_INTERVAL)
                .await,
            None
        );
        let sent = reporter.sent();
        assert_eq!(sent.last().unwrap().kind, ReportKind::Heartbeat);
        assert_eq!(sent.last().unwrap().previous_shutdown, None);
    }

    #[tokio::test]
    async fn an_internet_cut_the_coordinator_timed_is_handed_over_once() {
        let start = Instant::now();
        let (reporter, reports) = reports(PreviousRun::from_marker(None, Some(BOOT_B)), start);
        reporter.set_answer(CoordinatorView {
            state: "back".into(),
            last_outage: Some(Outage {
                cause: "planned".into(),
                started_at: 1_000,
                ended_at: 1_030,
            }),
        });
        reports.tick(&reading(true), start).await;
        assert_eq!(
            reports.take_internet_cut().await,
            None,
            "the restart was planned"
        );

        reporter.set_answer(CoordinatorView {
            state: "online".into(),
            last_outage: Some(Outage {
                cause: "network_loss".into(),
                started_at: 5_000,
                ended_at: 5_000 + 9 * 60,
            }),
        });
        reports
            .tick(&reading(true), start + HEARTBEAT_INTERVAL)
            .await;
        assert_eq!(
            reports.take_internet_cut().await,
            Some(Duration::from_secs(9 * 60))
        );
        assert_eq!(reports.take_internet_cut().await, None, "handed over once");
        reports
            .tick(&reading(true), start + 2 * HEARTBEAT_INTERVAL)
            .await;
        assert_eq!(
            reports.take_internet_cut().await,
            None,
            "the same cut again is not news"
        );
    }

    #[tokio::test]
    async fn with_remote_access_off_nothing_is_sent_and_the_notice_goes_at_once() {
        let start = Instant::now();
        let (reporter, reports) =
            reports(PreviousRun::from_marker(Some(BOOT_A), Some(BOOT_B)), start);
        assert_eq!(
            reports.tick(&reading(false), start).await,
            Some(PowerNotice::Announce { off_for: None })
        );
        assert!(reporter.sent().is_empty());
    }

    #[tokio::test]
    async fn going_down_names_its_reason_and_ends_the_heartbeats() {
        let start = Instant::now();
        let (reporter, reports) = reports(PreviousRun::from_marker(None, Some(BOOT_B)), start);
        assert_eq!(reports.tick(&reading(true), start).await, None);
        assert!(reports.going_down(StopReason::Update, &reading(true)).await);
        assert_eq!(
            reports
                .tick(&reading(true), start + 3 * HEARTBEAT_INTERVAL)
                .await,
            None
        );
        let sent = reporter.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].kind, ReportKind::GoingDown);
        assert_eq!(sent[1].reason, Some(StopReason::Update));

        reporter.set_failing(true);
        assert!(
            !reports
                .going_down(StopReason::Shutdown, &reading(true))
                .await
        );
        assert!(
            !reports
                .going_down(StopReason::Shutdown, &reading(false))
                .await
        );
    }
}
