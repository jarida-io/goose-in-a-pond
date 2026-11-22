//! The heartbeat's intentions on the server: what a review's answer leaves waiting, the tick
//! and the events that find a waiting one met or late, and what carrying one out does. Whether
//! a condition holds is read from the clock and from what happened, never asked of a model.

mod carry;
mod pond;
mod review;

#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::StreamExt as _;
use tokio::sync::broadcast::error::RecvError;

use pond_core::shared::domain::goose_activity::GooseActivity;
use pond_core::shared::domain::todo::{hook_event_from_bus, zone, Tz};
use pond_core::shared::ports::agent_notes::AgentNotesRepository;
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::shared::ports::goose_activity::GooseActivityRepository;
use pond_core::shared::ports::todo::TodoRepository;
use pond_core::user_data::domain::intention::{ConditionKind, Intention};
use pond_core::user_data::ports::intentions::IntentionRepository;
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use pond_core::user_data::services::condition_events;
use pond_core::user_data::services::intentions::{
    fires_on, settled_line, ConditionEvent, ThingChoice,
};

use crate::engine_runner::EngineDeps;

pub use pond::{HelperErrands, PondSchedules, PondThings};
pub use review::{close_review, review_context, Kept, ReviewContext};

/// How often due times and passed deadlines are looked for; a job may fire up to this late.
const TICK_EVERY: Duration = Duration::from_secs(30);
/// The first tick waits for startup's own work to settle.
const FIRST_TICK_AFTER: Duration = Duration::from_secs(25);
const PRESENCE: &str = "presence";
const EXPIRED: &str = "Its time ran out before what it waited for came.";
const NOT_CARRIED_OUT: &str = "It could not be carried out.";

/// What looking one thing up came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Looked {
    Found(String),
    /// The look-up ran and came back with no answer.
    Nothing,
    /// The look-up did not run; why, as the member reads it.
    NotRun(String),
}

/// Finds one thing out for a member, reading only.
#[async_trait]
pub trait Errands: Send + Sync {
    async fn look_into(
        &self,
        profile_id: &str,
        text: &str,
        why: &str,
        now: DateTime<Utc>,
        zone: Tz,
    ) -> Looked;
}

/// The cameras, sensors and devices a job may wait on, as the hook catalogue names them.
#[async_trait]
pub trait ThingSources: Send + Sync {
    async fn things(&self) -> Vec<ThingChoice>;
}

/// The words of the schedules that run for one member or for the whole household.
#[async_trait]
pub trait ScheduleWords: Send + Sync {
    async fn words_for(&self, profile_id: &str) -> anyhow::Result<Vec<String>>;
}

/// The intentions being carried out or let go right now, so a tick that overlaps a slow one
/// settles none of them twice.
#[derive(Clone, Default)]
pub struct InHand(Arc<Mutex<HashSet<String>>>);

struct Held {
    all: InHand,
    id: String,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.all
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

impl InHand {
    fn take(&self, id: &str) -> Option<Held> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string())
            .then(|| Held {
                all: self.clone(),
                id: id.to_string(),
            })
    }
}

/// What the runner and the review read, write and call.
#[derive(Clone)]
pub struct IntentionDeps {
    pub engine: EngineDeps,
    pub intentions: Arc<dyn IntentionRepository>,
    pub todos: Arc<dyn TodoRepository>,
    pub reminders: Arc<dyn ReminderRepository>,
    pub notes: Arc<dyn AgentNotesRepository>,
    pub activity: Arc<dyn GooseActivityRepository>,
    pub things: Arc<dyn ThingSources>,
    pub schedules: Arc<dyn ScheduleWords>,
    pub errands: Arc<dyn Errands>,
    pub in_hand: InHand,
}

impl IntentionDeps {
    /// The household's timezone; UTC when it is unset or unreadable.
    async fn zone(&self) -> Tz {
        self.engine
            .settings
            .get()
            .await
            .map_or_else(|_| zone(""), |settings| zone(&settings.timezone))
    }
}

/// What one tick did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TickReport {
    pub expired: usize,
    pub fired: usize,
}

/// Writes a settled intention and its line in the member's activity log. `found` is what a
/// look-up came back with, empty when it found nothing.
async fn settled(
    deps: &IntentionDeps,
    job: &Intention,
    outcome: &str,
    found: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    match deps.intentions.save(job).await {
        Ok(true) => {}
        Ok(false) => return false,
        Err(e) => {
            tracing::warn!(error = %e, "a settled intention could not be saved");
            return false;
        }
    }
    let entry = GooseActivity::intention(
        &job.profile_id,
        &settled_line(job, outcome),
        &job.id,
        found,
        now,
    );
    if let Err(e) = deps.activity.add(&entry).await {
        tracing::warn!(error = %e, "a settled intention could not be logged for its member");
    }
    tracing::info!(
        target: "giap::trace",
        kind = "intention_settled",
        act = job.act.as_str(),
        state = job.state.as_str(),
        condition = job.condition.kind().as_str(),
        member = %job.profile_id,
        "an intention was settled"
    );
    true
}

/// Carries out an intention whose condition holds and marks it done.
async fn carry_out(deps: &IntentionDeps, mut job: Intention, now: DateTime<Utc>) -> bool {
    let Some(_held) = deps.in_hand.take(&job.id) else {
        return false;
    };
    let carried = match carry::out(deps, &job, now).await {
        Ok(carried) => carried,
        Err(e) => {
            tracing::warn!(act = job.act.as_str(), error = %e, "an intention could not be carried out");
            carry::Carried::said(NOT_CARRIED_OUT)
        }
    };
    if job.done(&carried.outcome, now).is_err() {
        return false;
    }
    settled(deps, &job, &carried.outcome, carried.found.as_deref(), now).await
}

/// Lets go of an intention whose deadline passed before its condition held.
async fn expire(deps: &IntentionDeps, mut job: Intention, now: DateTime<Utc>) -> bool {
    let Some(_held) = deps.in_hand.take(&job.id) else {
        return false;
    };
    if job.expire(now).is_err() {
        return false;
    }
    job.outcome = Some(EXPIRED.to_string());
    settled(deps, &job, "", None, now).await
}

/// One tick: lets go of every intention past its deadline, then carries out the time
/// intentions that have come due.
pub async fn tick(deps: &IntentionDeps, now: DateTime<Utc>) -> TickReport {
    let mut report = TickReport::default();
    match deps.intentions.waiting_past(now).await {
        Ok(late) => {
            for job in late {
                report.expired += usize::from(expire(deps, job, now).await);
            }
        }
        Err(e) => tracing::warn!(error = %e, "late intentions could not be read"),
    }
    match deps.intentions.waiting(ConditionKind::Time).await {
        Ok(timed) => {
            for job in timed.into_iter().filter(|job| job.is_due(now)) {
                report.fired += usize::from(carry_out(deps, job, now).await);
            }
        }
        Err(e) => tracing::warn!(error = %e, "timed intentions could not be read"),
    }
    report
}

/// Carries out every waiting intention `event` meets; returns how many.
pub async fn on_event(deps: &IntentionDeps, event: &ConditionEvent, now: DateTime<Utc>) -> usize {
    let kind = match event {
        ConditionEvent::Place { .. } => ConditionKind::Place,
        ConditionEvent::Thing { .. } => ConditionKind::Thing,
        ConditionEvent::Chat { .. } => ConditionKind::Chat,
    };
    let waiting = match deps.intentions.waiting(kind).await {
        Ok(waiting) => waiting,
        Err(e) => {
            tracing::warn!(error = %e, "waiting intentions could not be read");
            return 0;
        }
    };
    let mut fired = 0;
    for job in waiting
        .into_iter()
        .filter(|job| fires_on(job, event) && !job.is_expired(now))
    {
        fired += usize::from(carry_out(deps, job, now).await);
    }
    fired
}

/// A camera, sensor or device event as a job's condition names it, by the hook catalogue's
/// source id; `None` for anything else the bus carries.
pub fn thing_event(event: &BusEvent, now: DateTime<Utc>) -> Option<ConditionEvent> {
    let raised = hook_event_from_bus(event, now)?;
    (raised.source_kind != PRESENCE).then(|| ConditionEvent::Thing {
        source_id: format!("{}:{}", raised.source_kind, raised.source_id),
        event: raised.event,
    })
}

// Each event is handled on its own task: looking something up takes minutes, and a listener
// that falls behind loses what the bus raised meanwhile.
fn handle(deps: &IntentionDeps, event: ConditionEvent) {
    let deps = deps.clone();
    tokio::spawn(async move {
        on_event(&deps, &event, Utc::now()).await;
    });
}

fn hear_the_bus(deps: IntentionDeps, bus: Arc<dyn EventBus>) {
    let mut events = bus.subscribe();
    tokio::spawn(async move {
        while let Some(event) = events.next().await {
            if let Some(event) = thing_event(&event, Utc::now()) {
                handle(&deps, event);
            }
        }
    });
}

fn hear_places_and_turns(deps: IntentionDeps) {
    let mut events = condition_events::listen();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(event) => handle(&deps, event),
                Err(RecvError::Lagged(missed)) => {
                    tracing::warn!(missed, "the intention runner fell behind what was raised")
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}

/// Starts the tick and the listeners: the bus for cameras, sensors and devices, and what the
/// routes and members' turns raise for place crossings and chats.
pub fn spawn(deps: IntentionDeps, bus: Arc<dyn EventBus>) -> tokio::task::JoinHandle<()> {
    hear_the_bus(deps.clone(), bus);
    hear_places_and_turns(deps.clone());
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_TICK_AFTER).await;
        let mut clock = tokio::time::interval(TICK_EVERY);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!("intentions active: times, places, things and chats");
        loop {
            clock.tick().await;
            let deps = deps.clone();
            tokio::spawn(async move {
                let report = tick(&deps, Utc::now()).await;
                if report != TickReport::default() {
                    tracing::info!(
                        target: "giap::trace",
                        kind = "intentions_tick",
                        fired = report.fired,
                        expired = report.expired,
                        "the intention tick settled something"
                    );
                }
            });
        }
    })
}
