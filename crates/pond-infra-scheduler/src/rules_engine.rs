//! Fires enabled [`TaskKind::SensorTrigger`] rules matching a bus event via `run_now`, debounced
//! by `cooldown_secs`. Time windows use the server's local wall clock.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use pond_core::shared::ports::event_bus::{BusEvent, BusStream};
use pond_core::user_data::domain::schedule::{Schedule, TaskKind};
use pond_core::user_data::ports::scheduler::SchedulerPort;

/// Rules-list reuse window: flat per-event cost, while new rules still apply within seconds.
const RULES_CACHE_TTL: Duration = Duration::from_secs(5);

/// Which rules `event` fires now, stamping their cooldowns. `now` is wall-clock to compare with
/// persisted stamps; a clock stepped backwards suppresses rather than fires (fails closed).
fn rules_to_fire(
    rules: &[Schedule],
    event: &BusEvent,
    local_time: chrono::NaiveTime,
    now: DateTime<Utc>,
    cooldowns: &mut HashMap<String, DateTime<Utc>>,
) -> Vec<String> {
    // Non-device events (ticks, session changes) match nothing; a placeholder view would
    // match every rule with no device/signal filter.
    let Some(view) = event.trigger_view() else {
        return Vec::new();
    };
    let mut fired = Vec::new();
    for rule in rules {
        let TaskKind::SensorTrigger(spec) = &rule.kind else {
            continue;
        };
        if rule.paused || !spec.matches(&view, local_time) {
            continue;
        }
        // The stamp survives restarts but lags (cache TTL, async `run_now`); the map is current
        // but empty after a restart. Debounce against the later.
        let last_fired = [cooldowns.get(&rule.id).copied(), rule.last_run]
            .into_iter()
            .flatten()
            .max();
        if let Some(last) = last_fired {
            let elapsed = now.signed_duration_since(last);
            if elapsed < chrono::Duration::seconds(clamp_cooldown_secs(spec.cooldown_secs)) {
                continue; // debounced
            }
        }
        cooldowns.insert(rule.id.clone(), now);
        fired.push(rule.id.clone());
    }
    fired
}

/// Saturating `u64` -> `i64`: chrono panics past its limit, and wrapping would fire every event.
fn clamp_cooldown_secs(secs: u64) -> i64 {
    const MAX: u64 = i64::MAX as u64 / 1000; // chrono's own seconds ceiling
    secs.min(MAX) as i64
}

/// Run the rules engine until the bus stream ends (server shutdown).
/// Spawn once from `pond-server` startup:
///
/// ```rust,ignore
/// tokio::spawn(pond_infra_scheduler::run_rules_engine(
///     event_bus.subscribe(), scheduler.clone(),
/// ));
/// ```
pub async fn run_rules_engine(mut events: BusStream, scheduler: Arc<dyn SchedulerPort>) {
    let mut cooldowns: HashMap<String, DateTime<Utc>> = HashMap::new();
    let mut cache: Option<(Instant, Vec<Schedule>)> = None;

    while let Some(event) = events.next().await {
        // Cache age uses the monotonic clock: it is never persisted.
        let cached_at = Instant::now();
        let now = Utc::now();
        let stale = cache
            .as_ref()
            .map(|(at, _)| cached_at.duration_since(*at) >= RULES_CACHE_TTL)
            .unwrap_or(true);
        if stale {
            match scheduler.list_tasks().await {
                Ok(tasks) => cache = Some((cached_at, tasks)),
                Err(e) => {
                    tracing::warn!(error = %e, "rules engine: failed to list rules");
                    // Keep any previous snapshot rather than dropping rules.
                }
            }
        }
        let Some((_, rules)) = &cache else { continue };

        let local_time = chrono::Local::now().time();
        for rule_id in rules_to_fire(rules, &event, local_time, now, &mut cooldowns) {
            tracing::info!(rule = %rule_id, "rules engine: rule matched — firing");
            // `run_now` gives rule fires the same run records and broadcasts as cron fires.
            if let Err(e) = scheduler.run_now(&rule_id).await {
                tracing::warn!(rule = %rule_id, error = %e, "rules engine: fire failed");
            }
        }
    }
    tracing::info!("rules engine: event stream ended");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pond_core::user_data::domain::schedule::{
        SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource, TriggerSourceKind,
    };
    use pond_core::user_data::domain::sensor::SensorReading;

    fn motion_event(device: &str) -> BusEvent {
        BusEvent::Sensor(SensorReading {
            device_id: device.into(),
            sensor_type: "motion".into(),
            value: 1.0,
            unit: "bool".into(),
            recorded_at: Utc::now(),
        })
    }

    /// A rule with a persisted fire stamp, as rehydrated after a restart.
    fn rule_last_fired(
        id: &str,
        cooldown_secs: u64,
        last_run: Option<chrono::DateTime<Utc>>,
    ) -> Schedule {
        Schedule {
            last_run,
            ..rule(id, None, cooldown_secs, false)
        }
    }

    fn rule(id: &str, device: Option<&str>, cooldown_secs: u64, paused: bool) -> Schedule {
        Schedule {
            fire_at: None,
            id: id.into(),
            label: format!("rule {id}"),
            cron: "@event".into(),
            timezone: "UTC".into(),
            kind: TaskKind::SensorTrigger(SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: device.map(String::from),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions: vec![TriggerAction::Notify {
                    title: "t".into(),
                    body: "b".into(),
                }],
                cooldown_secs,
            }),
            paused,
            currently_running: false,
            last_run: None,
            next_run: None,
            created_at: Utc::now(),
        }
    }

    fn noon() -> chrono::NaiveTime {
        chrono::NaiveTime::parse_from_str("12:00", "%H:%M").unwrap()
    }

    #[test]
    fn fires_matching_rule_and_debounces_within_cooldown() {
        let rules = vec![rule("r1", Some("backyard-pir"), 60, false)];
        let mut cooldowns = HashMap::new();
        let now = Utc::now();

        // First event fires.
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            now,
            &mut cooldowns,
        );
        assert_eq!(fired, vec!["r1".to_string()]);

        // Second event inside the cooldown is debounced.
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            now,
            &mut cooldowns,
        );
        assert!(fired.is_empty(), "cooldown must suppress the re-fire");

        // After the cooldown has elapsed it fires again.
        let later = now + chrono::Duration::seconds(61);
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            later,
            &mut cooldowns,
        );
        assert_eq!(fired, vec!["r1".to_string()]);
    }

    #[test]
    fn zero_cooldown_fires_every_event() {
        let rules = vec![rule("r0", None, 0, false)];
        let mut cooldowns = HashMap::new();
        let now = Utc::now();
        assert_eq!(
            rules_to_fire(&rules, &motion_event("a"), noon(), now, &mut cooldowns).len(),
            1
        );
        assert_eq!(
            rules_to_fire(&rules, &motion_event("a"), noon(), now, &mut cooldowns).len(),
            1
        );
    }

    #[test]
    fn skips_paused_and_non_matching_rules() {
        let rules = vec![
            rule("paused", Some("backyard-pir"), 0, true),
            rule("other-device", Some("front-door"), 0, false),
        ];
        let mut cooldowns = HashMap::new();
        let fired = rules_to_fire(
            &rules,
            &motion_event("backyard-pir"),
            noon(),
            Utc::now(),
            &mut cooldowns,
        );
        assert!(fired.is_empty());
    }

    // ── Durable cooldowns ─────────────────────────────────────────────────
    // Each test starts with an empty `cooldowns` map, as a restarted process does.

    #[test]
    fn persisted_fire_stamp_debounces_a_restarted_process() {
        let now = Utc::now();
        let rules = vec![rule_last_fired(
            "r1",
            3600,
            Some(now - chrono::Duration::seconds(10)),
        )];
        let mut cooldowns = HashMap::new(); // restart: nothing in memory

        let fired = rules_to_fire(&rules, &motion_event("d"), noon(), now, &mut cooldowns);
        assert!(
            fired.is_empty(),
            "a rule that fired 10s ago with an hour's cooldown must stay \
             debounced across a restart — an in-memory-only cooldown lets a \
             crash loop re-fire it on every boot"
        );
    }

    #[test]
    fn persisted_fire_stamp_older_than_the_cooldown_still_fires() {
        // Vacuity control for the test above: same fixture, only the stamp's age differs.
        let now = Utc::now();
        let rules = vec![rule_last_fired(
            "r1",
            3600,
            Some(now - chrono::Duration::seconds(3601)),
        )];
        let mut cooldowns = HashMap::new();

        let fired = rules_to_fire(&rules, &motion_event("d"), noon(), now, &mut cooldowns);
        assert_eq!(fired, vec!["r1".to_string()]);
    }

    #[test]
    fn no_persisted_stamp_fires() {
        // Upgrade case: an old `schedules.json` has no stamp, which must read as never fired.
        let now = Utc::now();
        let rules = vec![rule_last_fired("r1", 3600, None)];
        let mut cooldowns = HashMap::new();
        let fired = rules_to_fire(&rules, &motion_event("d"), noon(), now, &mut cooldowns);
        assert_eq!(fired, vec!["r1".to_string()]);
    }

    #[test]
    fn the_later_of_the_two_stamps_wins() {
        // The map leads the cached stamp by up to the cache TTL; the stamp alone would re-fire.
        let now = Utc::now();
        let rules = vec![rule_last_fired(
            "r1",
            60,
            Some(now - chrono::Duration::seconds(300)), // stale snapshot
        )];
        let mut cooldowns = HashMap::new();
        cooldowns.insert("r1".to_string(), now - chrono::Duration::seconds(5));

        let fired = rules_to_fire(&rules, &motion_event("d"), noon(), now, &mut cooldowns);
        assert!(fired.is_empty(), "the in-memory stamp is the later one");

        // And symmetrically: a fresh persisted stamp beats a stale map entry.
        let rules = vec![rule_last_fired(
            "r2",
            60,
            Some(now - chrono::Duration::seconds(5)),
        )];
        let mut cooldowns = HashMap::new();
        cooldowns.insert("r2".to_string(), now - chrono::Duration::seconds(300));
        let fired = rules_to_fire(&rules, &motion_event("d"), noon(), now, &mut cooldowns);
        assert!(fired.is_empty(), "the persisted stamp is the later one");
    }

    #[test]
    fn a_clock_that_moved_backwards_suppresses_rather_than_fires() {
        // An NTP step on an RTC-less Jetson can put a stamp in the future.
        let now = Utc::now();
        let rules = vec![rule_last_fired(
            "r1",
            60,
            Some(now + chrono::Duration::seconds(600)),
        )];
        let mut cooldowns = HashMap::new();
        let fired = rules_to_fire(&rules, &motion_event("d"), noon(), now, &mut cooldowns);
        assert!(fired.is_empty(), "negative elapsed must debounce, not fire");
    }

    #[test]
    fn an_absurd_cooldown_does_not_wrap_into_firing_every_event() {
        let now = Utc::now();
        let rules = vec![rule_last_fired(
            "r1",
            u64::MAX,
            Some(now - chrono::Duration::seconds(10)),
        )];
        let mut cooldowns = HashMap::new();
        let fired = rules_to_fire(&rules, &motion_event("d"), noon(), now, &mut cooldowns);
        assert!(
            fired.is_empty(),
            "a u64 cooldown cast straight to i64 wrapped negative, so the \
             longest cooldown expressible became no cooldown at all"
        );
        assert_eq!(clamp_cooldown_secs(u64::MAX), i64::MAX / 1000);
        assert_eq!(clamp_cooldown_secs(60), 60);
    }

    // ── Acceptance: a rule fires end-to-end on a simulated sensor event ───────
    // The "after sunset" window is left to domain tests; a wall-clock window here would be flaky.
    mod end_to_end {
        use super::*;
        use anyhow::Result;
        use async_trait::async_trait;
        use pond_core::shared::ports::event_bus::EventBus;
        use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
        use pond_core::user_data::domain::schedule::RunStatus;
        use pond_core::user_data::ports::schedule_execution::ScheduleExecutor;
        use pond_core::user_data::ports::scheduler::CreateScheduleRequest;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct CountingExecutor(Arc<AtomicU32>);

        #[async_trait]
        impl ScheduleExecutor for CountingExecutor {
            async fn execute(&self, _id: &str, kind: &TaskKind) -> Result<String> {
                assert!(kind.is_event_triggered(), "engine must pass the rule kind");
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok("actions ran".into())
            }
        }

        /// Records which tasks fired, so the restart test can tell suppression from no delivery.
        struct RecordingExecutor(Arc<std::sync::Mutex<Vec<String>>>);

        #[async_trait]
        impl ScheduleExecutor for RecordingExecutor {
            async fn execute(&self, id: &str, _kind: &TaskKind) -> Result<String> {
                self.0.lock().unwrap().push(id.to_string());
                Ok("actions ran".into())
            }
        }

        #[tokio::test]
        async fn sensor_rule_fires_end_to_end_on_simulated_event() {
            let tmp = tempfile::tempdir().unwrap();
            let counter = Arc::new(AtomicU32::new(0));
            let exec: Arc<dyn ScheduleExecutor> = Arc::new(CountingExecutor(counter.clone()));
            let scheduler: Arc<dyn SchedulerPort> = Arc::new(
                crate::CronSchedulerAdapter::new(
                    tmp.path().join("schedules.json"),
                    tmp.path().join("runs.json"),
                    exec,
                )
                .await
                .unwrap(),
            );

            // "Motion on the backyard PIR → notify" (no time window: see above).
            let spec = SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: Some("backyard-pir".into()),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions: vec![TriggerAction::Notify {
                    title: "Motion".into(),
                    body: "Backyard motion".into(),
                }],
                cooldown_secs: 60,
            };
            scheduler
                .create_task(CreateScheduleRequest {
                    fire_at: None,
                    once: false,
                    id: "rule-e2e".into(),
                    label: "Backyard motion".into(),
                    cron: "@event".into(),
                    timezone: "UTC".into(),
                    kind: TaskKind::SensorTrigger(spec),
                })
                .await
                .expect("event rules must not require a valid cron");

            let bus = Arc::new(InProcessEventBus::new());
            tokio::spawn(run_rules_engine(bus.subscribe(), scheduler.clone()));
            // Let the engine subscribe before publishing.
            tokio::time::sleep(Duration::from_millis(50)).await;

            bus.publish(motion_event("backyard-pir"));
            // A second event inside the cooldown must be debounced.
            bus.publish(motion_event("backyard-pir"));

            // Wait (bounded) for the fire to propagate through run_now.
            let mut fired = 0;
            for _ in 0..40 {
                fired = counter.load(Ordering::SeqCst);
                if fired > 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert_eq!(fired, 1, "exactly one fire (second event debounced)");

            // It went through the scheduler's path: a completed run record exists.
            let mut completed = false;
            for _ in 0..40 {
                let runs = scheduler.get_runs("rule-e2e", 10).await.unwrap();
                if runs.iter().any(|r| {
                    r.status == RunStatus::Completed && r.result.as_deref() == Some("actions ran")
                }) {
                    completed = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            assert!(completed, "run record shows the completed rule fire");
        }

        #[tokio::test]
        async fn a_rule_cooldown_survives_a_restart() {
            let tmp = tempfile::tempdir().unwrap();
            let fired = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let exec: Arc<dyn ScheduleExecutor> = Arc::new(RecordingExecutor(fired.clone()));

            let spec = |cooldown_secs: u64| SensorTriggerSpec {
                source: TriggerSource {
                    kind: TriggerSourceKind::Sensor,
                    device_id: Some("backyard-pir".into()),
                    signal: Some("motion".into()),
                },
                condition: TriggerCondition::default(),
                actions: vec![TriggerAction::Notify {
                    title: "Motion".into(),
                    body: "Backyard motion".into(),
                }],
                cooldown_secs,
            };
            let new_adapter = || {
                crate::CronSchedulerAdapter::new(
                    tmp.path().join("schedules.json"),
                    tmp.path().join("runs.json"),
                    exec.clone(),
                )
            };

            // ── First lifetime: create the rule and let it fire once ────────
            {
                let scheduler: Arc<dyn SchedulerPort> = Arc::new(new_adapter().await.unwrap());
                scheduler
                    .create_task(CreateScheduleRequest {
                        fire_at: None,
                        once: false,
                        id: "hourly-rule".into(),
                        label: "Backyard motion".into(),
                        cron: "@event".into(),
                        timezone: "UTC".into(),
                        kind: TaskKind::SensorTrigger(spec(3600)),
                    })
                    .await
                    .unwrap();

                let bus = Arc::new(InProcessEventBus::new());
                tokio::spawn(run_rules_engine(bus.subscribe(), scheduler.clone()));
                tokio::time::sleep(Duration::from_millis(50)).await;
                bus.publish(motion_event("backyard-pir"));

                for _ in 0..40 {
                    if !fired.lock().unwrap().is_empty() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                assert_eq!(
                    fired.lock().unwrap().as_slice(),
                    ["hourly-rule".to_string()],
                    "the rule must fire the first time"
                );
            }

            // The stamp is on disk. Parsed, not grepped: `"last_run"` is present even when `null`.
            let on_disk = tokio::fs::read_to_string(tmp.path().join("schedules.json"))
                .await
                .unwrap();
            let records: serde_json::Value = serde_json::from_str(&on_disk).unwrap();
            let stamp = records
                .as_array()
                .and_then(|rs| rs.iter().find(|r| r["id"] == "hourly-rule"))
                .map(|r| r["last_run"].clone())
                .unwrap_or(serde_json::Value::Null);
            assert!(
                stamp.is_string(),
                "the fire stamp must be persisted as a timestamp, else the \
                 restart below cannot debounce. last_run was {stamp} in \
                 {on_disk}"
            );

            // ── Second lifetime: same files, new adapter, new engine ────────
            fired.lock().unwrap().clear();
            let scheduler: Arc<dyn SchedulerPort> = Arc::new(new_adapter().await.unwrap());

            let rehydrated = scheduler.list_tasks().await.unwrap();
            let rule = rehydrated
                .iter()
                .find(|t| t.id == "hourly-rule")
                .expect("the rule itself must survive the restart");
            assert!(
                rule.last_run.is_some(),
                "rehydration dropped the fire stamp"
            );

            // Control: a no-cooldown rule created after the restart proves the engine is live.
            scheduler
                .create_task(CreateScheduleRequest {
                    fire_at: None,
                    once: false,
                    id: "control-rule".into(),
                    label: "Control".into(),
                    cron: "@event".into(),
                    timezone: "UTC".into(),
                    kind: TaskKind::SensorTrigger(spec(0)),
                })
                .await
                .unwrap();

            let bus = Arc::new(InProcessEventBus::new());
            tokio::spawn(run_rules_engine(bus.subscribe(), scheduler.clone()));
            tokio::time::sleep(Duration::from_millis(50)).await;
            bus.publish(motion_event("backyard-pir"));

            for _ in 0..40 {
                if !fired.lock().unwrap().is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            // Give any (wrongly) un-debounced fire the same chance to land.
            tokio::time::sleep(Duration::from_millis(300)).await;

            let after_restart = fired.lock().unwrap().clone();
            assert!(
                after_restart.contains(&"control-rule".to_string()),
                "the restarted engine delivered nothing — the assertion below \
                 would be vacuous. Fired: {after_restart:?}"
            );
            assert!(
                !after_restart.contains(&"hourly-rule".to_string()),
                "the hour-long cooldown reset on restart: a pond that \
                 crash-loops would fire this rule every time it came back. \
                 Fired: {after_restart:?}"
            );
        }
    }
}
