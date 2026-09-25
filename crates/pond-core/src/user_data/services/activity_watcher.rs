//! Cancels a background pass once human activity appears after its admission baseline.
//!
//! Baseline, not recency: the admitting gate already judged recency. The in-process clock is
//! read every tick, the DB (where a voice turn lands) every Nth; a failed read is no activity.
//! Only [`human_activity`] counts, so the pond's own `sched-` sessions never cancel a pass.

use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::shared::domain::session_activity::human_activity;
use crate::user_data::ports::session_storage::SessionStorage;

/// The activity reading a pass was admitted against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivityBaseline {
    /// `last_user_activity` as it stood when this pass was admitted.
    pub in_process_at: Instant,
    /// Newest human `sessions.updated_at` then; `None` if there was no human conversation yet.
    pub db_activity: Option<DateTime<Utc>>,
}

/// Whether activity has happened since `baseline`.
pub fn resumed_since(
    baseline: ActivityBaseline,
    in_process_now: Instant,
    db_now: Option<DateTime<Utc>>,
) -> bool {
    let in_process = in_process_now > baseline.in_process_at;
    let in_db = match db_now {
        // No baseline row: any human row now is somebody arriving.
        Some(latest) => baseline.db_activity.is_none_or(|seen| latest > seen),
        // A failed or empty read is no evidence, not activity.
        None => false,
    };
    in_process || in_db
}

/// Newest human session activity; `None` if there is none or storage errors.
pub async fn newest_human_activity(storage: &dyn SessionStorage) -> Option<DateTime<Utc>> {
    let sessions = storage.list_sessions().await.ok()?;
    human_activity(&sessions).newest_activity
}

/// Read both sources now, for a caller about to admit a pass.
pub async fn baseline_now(
    in_process: &Arc<RwLock<Instant>>,
    storage: &dyn SessionStorage,
) -> ActivityBaseline {
    ActivityBaseline {
        in_process_at: *in_process.read().await,
        db_activity: newest_human_activity(storage).await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivitySources {
    /// The in-process clock only, for passes that must react before a turn reaches the DB.
    InProcessOnly,
    InProcessAndDatabase {
        db_every_n_ticks: u32,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct WatchConfig {
    pub tick: Duration,
    pub sources: ActivitySources,
    /// Named in the INFO line a cancellation logs.
    pub what_is_cancelled: &'static str,
}

impl WatchConfig {
    pub fn chore(what_is_cancelled: &'static str) -> Self {
        Self {
            tick: Duration::from_millis(500),
            sources: ActivitySources::InProcessAndDatabase {
                db_every_n_ticks: 4,
            },
            what_is_cancelled,
        }
    }

    pub fn in_process_only(tick: Duration, what_is_cancelled: &'static str) -> Self {
        Self {
            tick,
            sources: ActivitySources::InProcessOnly,
            what_is_cancelled,
        }
    }
}

/// A running watcher; dropping it stops the task.
pub struct ActivityWatcher {
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for ActivityWatcher {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Cancel `pass` once activity appears after `baseline`, taken by the caller at admission:
/// re-reading it here would skip activity during lane-slot acquisition.
pub fn watch_for_return(
    baseline: ActivityBaseline,
    in_process: Arc<RwLock<Instant>>,
    storage: Arc<dyn SessionStorage>,
    pass: CancellationToken,
    config: WatchConfig,
) -> ActivityWatcher {
    let handle = tokio::spawn(async move {
        let mut tick: u32 = 0;
        loop {
            tokio::time::sleep(config.tick).await;
            if pass.is_cancelled() {
                return;
            }
            tick = tick.wrapping_add(1);

            let db_now = match config.sources {
                ActivitySources::InProcessOnly => None,
                ActivitySources::InProcessAndDatabase { db_every_n_ticks } => {
                    let due = db_every_n_ticks > 0 && tick % db_every_n_ticks == 0;
                    if due {
                        newest_human_activity(storage.as_ref()).await
                    } else {
                        None
                    }
                }
            };

            if resumed_since(baseline, *in_process.read().await, db_now) {
                tracing::info!(
                    what = config.what_is_cancelled,
                    "somebody is back — yielding the engine"
                );
                pass.cancel();
                return;
            }
        }
    });
    ActivityWatcher { handle }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs as i64, 0).unwrap()
    }

    fn baseline(db: Option<DateTime<Utc>>) -> ActivityBaseline {
        ActivityBaseline {
            in_process_at: Instant::now(),
            db_activity: db,
        }
    }

    #[test]
    fn nothing_new_is_not_a_reason_to_stop() {
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, Some(at(10))));
    }

    #[test]
    fn a_newer_in_process_reading_stops_the_pass() {
        let b = baseline(Some(at(10)));
        let later = b.in_process_at + Duration::from_millis(1);
        assert!(resumed_since(b, later, Some(at(10))));
    }

    /// Voice turns run in another process and reach only the database.
    #[test]
    fn a_turn_visible_only_in_the_database_stops_the_pass() {
        let b = baseline(Some(at(10)));
        assert!(resumed_since(b, b.in_process_at, Some(at(11))));
    }

    #[test]
    fn an_unreadable_database_is_no_evidence_rather_than_activity() {
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, None));
    }

    #[test]
    fn a_first_ever_conversation_counts_against_an_empty_baseline() {
        let b = baseline(None);
        assert!(resumed_since(b, b.in_process_at, Some(at(1))));
        assert!(!resumed_since(b, b.in_process_at, None));
    }

    /// Clocks and replicas can move backwards.
    #[test]
    fn an_older_database_row_does_not_stop_the_pass() {
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, Some(at(9))));
    }

    #[test]
    fn recent_activity_before_the_baseline_is_not_a_return() {
        let b = baseline(Some(at(10)));
        assert!(!resumed_since(b, b.in_process_at, Some(at(10))));
    }

    #[tokio::test]
    async fn the_watcher_cancels_when_the_in_process_clock_moves() {
        let clock = Arc::new(RwLock::new(Instant::now()));
        let pass = CancellationToken::new();
        let _watcher = watch_for_return(
            ActivityBaseline {
                in_process_at: *clock.read().await,
                db_activity: None,
            },
            clock.clone(),
            Arc::new(crate::user_data::mocks::mock_session::InMemorySessionStorage::new()),
            pass.clone(),
            WatchConfig::in_process_only(Duration::from_millis(10), "the test pass"),
        );

        assert!(!pass.is_cancelled());
        *clock.write().await = Instant::now() + Duration::from_millis(1);
        tokio::time::timeout(Duration::from_secs(2), pass.cancelled())
            .await
            .expect("the watcher never noticed the clock move");
    }

    #[tokio::test]
    async fn dropping_the_guard_stops_the_watcher() {
        let clock = Arc::new(RwLock::new(Instant::now()));
        let pass = CancellationToken::new();
        let watcher = watch_for_return(
            ActivityBaseline {
                in_process_at: *clock.read().await,
                db_activity: None,
            },
            clock.clone(),
            Arc::new(crate::user_data::mocks::mock_session::InMemorySessionStorage::new()),
            pass.clone(),
            WatchConfig::in_process_only(Duration::from_millis(10), "the test pass"),
        );
        drop(watcher);

        // Activity after the drop must NOT cancel: the task is gone.
        *clock.write().await = Instant::now() + Duration::from_millis(1);
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            !pass.is_cancelled(),
            "the dropped watcher was still running"
        );
    }
}
