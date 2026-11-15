//! The to-do hook engine's loop: every 30 seconds it fires due times and repeats, asks again after
//! a not-yet, and matches queued events to place, event and connector hooks. Beside it, the bus
//! feeds presence, cameras, devices and sensors into the queue, and dated mentions from chats are
//! pushed to their member as to-do offers. The decisions are pond-core's
//! (`shared::services::todo_hooks`, `todo_offers`); this file keeps the clock.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use futures::StreamExt as _;
use pond_core::mcp::ports::notification::{MemberDelivery, MemberNotifier};
use pond_core::shared::domain::todo::{hook_event_from_bus, zone, EventKey, Tz};
use pond_core::shared::ports::event_bus::EventBus;
use pond_core::shared::ports::todo::HookEventQueue;
use pond_core::shared::services::todo_hooks::TodoHookEngine;
use pond_core::shared::services::todo_offers::{offer_due, offer_notice};
use pond_core::user_data::domain::reminder::CapturedReminder;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_infra::sqlite_todo::SqliteTodoRepository;

/// How often due hooks are looked for; a hook may fire up to this late.
const PASS_SECS: u64 = 30;
/// The first pass waits for startup's own work to settle.
const FIRST_PASS_AFTER: Duration = Duration::from_secs(20);
/// Handled events are pruned about this often.
const PRUNE_EVERY_PASSES: u64 = 120;
/// The same bus event from the same source inside this window is one event: a camera reports
/// a person many times a minute.
const SAME_EVENT_SECS: i64 = 60;

type Settings = Arc<dyn SettingsRepository + Send + Sync>;

async fn household_zone(settings: &Settings) -> Tz {
    settings
        .get()
        .await
        .map_or_else(|_| zone(""), |s| zone(&s.timezone))
}

/// Starts the engine and the bus feeder. Web watch matches reach the same queue through the
/// watch runner, which this installs the queue into.
pub fn spawn(
    pool: sqlx::SqlitePool,
    settings: Settings,
    notifier: Arc<dyn MemberNotifier>,
    bus: Arc<dyn EventBus>,
) -> tokio::task::JoinHandle<()> {
    let repo = Arc::new(SqliteTodoRepository::new(pool));
    pond_mcp_server::web_watch_runner::install_hook_events(repo.clone());
    let engine = TodoHookEngine::new(repo.clone(), repo.clone(), notifier);
    let wanted: Arc<RwLock<HashSet<EventKey>>> = Arc::default();
    feed_from_bus(bus, repo, wanted.clone());
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_PASS_AFTER).await;
        let mut clock = tokio::time::interval(Duration::from_secs(PASS_SECS));
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut passes: u64 = 0;
        tracing::info!("to-do hooks active: times, repeats, places, events and connectors");
        loop {
            clock.tick().await;
            let now = Utc::now();
            match engine.pass(now, household_zone(&settings).await).await {
                Ok(report) if report.pushed > 0 || report.events > 0 => tracing::info!(
                    target: "giap::trace",
                    kind = "todo_hooks_pass",
                    pushed = report.pushed,
                    events = report.events,
                ),
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(target: "giap::trace", kind = "todo_hooks_pass_failed", error = %e)
                }
            }
            match engine.listening_keys().await {
                Ok(keys) => {
                    if let Ok(mut held) = wanted.write() {
                        *held = keys;
                    }
                }
                Err(e) => tracing::debug!(error = %e, "to-do hooks: listening hooks unreadable"),
            }
            if passes.is_multiple_of(PRUNE_EVERY_PASSES) {
                if let Err(e) = engine.prune(now).await {
                    tracing::debug!(error = %e, "to-do hooks: pruning handled events failed");
                }
            }
            passes += 1;
        }
    })
}

/// Queues the bus events a hook listens for; nothing else is written, so a camera watching an
/// empty room costs no disk.
fn feed_from_bus(
    bus: Arc<dyn EventBus>,
    queue: Arc<SqliteTodoRepository>,
    wanted: Arc<RwLock<HashSet<EventKey>>>,
) {
    let mut events = bus.subscribe();
    tokio::spawn(async move {
        let mut last_raised: HashMap<(EventKey, Option<String>), chrono::DateTime<Utc>> =
            HashMap::new();
        while let Some(event) = events.next().await {
            let now = Utc::now();
            let Some(raised) = hook_event_from_bus(&event, now) else {
                continue;
            };
            let key = raised.key();
            if !wanted.read().is_ok_and(|w| w.contains(&key)) {
                continue;
            }
            let seen = (key, raised.profile_id.clone());
            if last_raised
                .get(&seen)
                .is_some_and(|at| (now - *at).num_seconds() < SAME_EVENT_SECS)
            {
                continue;
            }
            last_raised.retain(|_, at| (now - *at).num_seconds() < SAME_EVENT_SECS);
            last_raised.insert(seen, now);
            if let Err(e) = queue.raise(&raised).await {
                tracing::warn!(error = %e, "to-do hooks: a bus event could not be queued");
            }
        }
    });
}

/// Pushes each dated mention the extraction loop offered to its member as a to-do offer.
pub fn spawn_offer_notices(
    mut offers: tokio::sync::mpsc::Receiver<CapturedReminder>,
    notifier: Arc<dyn MemberNotifier>,
    settings: Settings,
) {
    tokio::spawn(async move {
        while let Some(reminder) = offers.recv().await {
            let Some(member) = reminder.profile_id.clone() else {
                continue;
            };
            let (now, tz) = (Utc::now(), household_zone(&settings).await);
            let due = offer_due(&reminder, now, tz);
            let delivery = notifier
                .notify_member(&member, offer_notice(&reminder, due, now, tz))
                .await;
            let outcome = match &delivery {
                MemberDelivery::Reached(devices) => format!("queued for {}", devices.len()),
                MemberDelivery::NoPhone => "no_phone".to_string(),
                MemberDelivery::Failed(why) => format!("failed: {why}"),
            };
            tracing::info!(
                target: "giap::trace",
                kind = "todo_offer_notice",
                reminder_id = %reminder.id,
                worked_out_a_time = due.is_some(),
                outcome = %outcome,
            );
        }
    });
}
