//! Members' news on the inference lane: the topic map, the story passes and the digest, each a
//! lane job that takes the slot only when a member's news is due and the household is quiet, and
//! gives it up the moment someone comes back. The house speaker reads a digest at its time, under
//! the household's quiet hours and its unprompted-speech setting.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::RwLock;

use pond_core::mcp::ports::notification::{
    MemberDelivery, MemberNotifier, Notification, OpenScreen,
};
use pond_core::models::ports::provider::LlmProvider;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::shared::domain::news::{Digest, NewsSettings};
use pond_core::shared::ports::news::NewsStore;
use pond_core::shared::services::chat::{LocalTimeOfDay, UnpromptedSpeech};
use pond_core::shared::services::news::schedule::{self, StoryPass};
use pond_core::shared::services::news::topic_map::{map_due, MAP_MEMORIES, MAP_ROUTINES};
use pond_core::shared::services::news::writer::{MapExtras, NewsWriter, PassLimits, Today};
use pond_core::shared::services::news::{speaker, speech};
use pond_core::user_data::domain::memory::MemoryLifecycle;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::schedule::TaskKind;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::consolidation_schedule as sched;
use pond_core::user_data::services::inference_lane::LaneJob;
use pond_infra::sqlite_news::SqliteNewsStore;

use crate::inference_lane_runner::{wait_for_tick, Bells, Cadence, InferenceLane, LaneSlot, Tick};

/// How often each loop looks for a member whose news is due.
const POLL: Duration = Duration::from_secs(60);
/// Quiet before a scheduled pass: a pass holds the model for minutes on the Jetson.
const QUIET: Duration = Duration::from_secs(5 * 60);
/// Quiet before a pass the member asked for from their phone.
const QUIET_WHEN_ASKED: Duration = Duration::from_secs(20);
/// Shortest gap between two passes of one job, so members' passes can follow each other.
const BETWEEN_PASSES: Duration = Duration::from_secs(30);
/// Both jobs may run on a pond that has served no turn since it started: a member's digest is
/// due at its time whether or not anyone has talked to the pond since a restart.
const EXEMPT_FROM_ACTIVITY_GATE: bool = true;
/// A digest that came out empty is not tried again for this long; each try spends searches.
const DIGEST_RETRY: Duration = Duration::from_secs(2 * 3600);
/// A map rebuild that was cut short or failed waits this long, and stories use the old map.
const MAP_RETRY: Duration = Duration::from_secs(30 * 60);
/// Stories are forgotten after this long; digests keep their own copies.
const STORIES_KEPT: chrono::Duration = chrono::Duration::days(45);

/// Everything the news needs from the running pond.
pub struct NewsDeps {
    pub db: sqlx::Pool<sqlx::Sqlite>,
    pub settings: Arc<dyn SettingsRepository + Send + Sync>,
    pub profiles: Arc<dyn ProfileRepository + Send + Sync>,
    pub memories: Arc<dyn MemoryRepository + Send + Sync>,
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    pub provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
    pub lane: Arc<InferenceLane>,
    pub last_user_activity: Arc<RwLock<Instant>>,
    pub sessions: Arc<dyn SessionStorage>,
    pub members: Arc<dyn MemberNotifier>,
    pub voice: Option<Arc<dyn VoiceOutput>>,
    pub data_dir: PathBuf,
}

struct Runner {
    deps: NewsDeps,
    store: Arc<SqliteNewsStore>,
    writer: NewsWriter,
    started_at: Instant,
    started_at_utc: DateTime<Utc>,
}

/// What a story tick will do.
enum StoryWork {
    Map(String),
    Stories(String, StoryPass),
}

/// Starts the news: its two lane jobs and the speaker's clock, and installs the house speaker the
/// speak route reads on.
pub fn spawn(deps: NewsDeps) {
    let store = Arc::new(SqliteNewsStore::new(deps.db.clone()));
    let writer = NewsWriter::new(
        store.clone(),
        Arc::new(pond_mcp_server::news_web::BraveNewsWeb::default()),
        deps.profiles.clone(),
    );
    if let Some(voice) = &deps.voice {
        speaker::install(Arc::new(pond_audio_out::Announcer::new(voice.clone())));
    }
    let story_bells = deps.lane.claim(LaneJob::NewsTiles);
    let digest_bells = deps.lane.claim(LaneJob::NewsDigest);
    let runner = Arc::new(Runner {
        deps,
        store,
        writer,
        started_at: Instant::now(),
        started_at_utc: Utc::now(),
    });
    tokio::spawn(runner.clone().story_loop(story_bells));
    tokio::spawn(runner.clone().digest_loop(digest_bells));
    tokio::spawn(runner.speaker_loop());
    tracing::info!("news active -- topic maps, stories and digests on the inference lane");
}

/// A refresh from the phone or a Run-now press skips the quiet wait, but never starts a pass while
/// a conversation holds the model.
fn waives(tick: Tick) -> bool {
    tick.waives() && !pond_core::models::domain::inference_bus::turn_has_the_model()
}

impl Runner {
    async fn household_zone(&self) -> String {
        self.deps
            .settings
            .get()
            .await
            .map(|s| s.timezone)
            .unwrap_or_else(|_| "UTC".to_string())
    }

    /// The household's activity as the lane reads it.
    async fn activity(&self) -> (bool, Duration) {
        let db_activity = crate::newest_session_activity(self.deps.sessions.as_ref()).await;
        let in_process_at = *self.deps.last_user_activity.read().await;
        let saw = sched::saw_activity_since_start(
            self.started_at,
            in_process_at,
            self.started_at_utc,
            db_activity,
        );
        (
            saw,
            sched::combined_idle_for(in_process_at, db_activity, Utc::now()),
        )
    }

    /// The first member whose map or stories are due, and whether they asked for it.
    async fn story_work(
        &self,
        now: DateTime<Utc>,
        map_tried: &HashMap<String, Instant>,
    ) -> Option<(StoryWork, bool)> {
        let household = self.household_zone().await;
        let members = self.store.members().await.ok()?;
        let mut due: Option<(StoryWork, bool)> = None;
        for member in members {
            let settings = self.store.settings(&member).await.ok()?;
            if !(settings.tiles || settings.digest) {
                continue;
            }
            if !self.writer.has_interests(&member).await.unwrap_or(false) {
                continue;
            }
            let book = self.store.book(&member).await.ok()?;
            let zone = settings.zone(&household);
            let asked = book.refresh_requested_at.is_some();
            let map_resting = map_tried
                .get(&member)
                .is_some_and(|at| at.elapsed() < MAP_RETRY);
            let work = if !map_resting
                && map_due(now, &zone, book.map_built_at, book.interests_changed_at)
            {
                Some(StoryWork::Map(member.clone()))
            } else {
                schedule::story_pass_due(now, &zone, &settings, &book)
                    .map(|pass| StoryWork::Stories(member.clone(), pass))
            };
            if let Some(work) = work {
                // A member who asked goes before the schedule.
                if asked {
                    return Some((work, true));
                }
                due.get_or_insert((work, false));
            }
        }
        due
    }

    async fn story_loop(self: Arc<Self>, bells: Bells) {
        let mut pruned_on: Option<NaiveDate> = None;
        let mut map_tried: HashMap<String, Instant> = HashMap::new();
        loop {
            let tick = wait_for_tick(POLL, &bells).await;
            let now = Utc::now();
            if pruned_on != Some(now.date_naive()) {
                pruned_on = Some(now.date_naive());
                if let Err(e) = self.store.prune_stories(now - STORIES_KEPT).await {
                    tracing::warn!(target: "giap::trace", kind = "news_prune_failed", error = %e);
                }
            }
            let work = self.story_work(now, &map_tried).await;
            let asked = work.as_ref().is_some_and(|(_, asked)| *asked);
            let quiet = if asked { QUIET_WHEN_ASKED } else { QUIET };
            let (saw, idle_for) = self.activity().await;
            let Some(slot) = self
                .deps
                .lane
                .acquire(
                    LaneJob::NewsTiles,
                    work.is_some(),
                    Cadence::new(BETWEEN_PASSES, quiet, EXEMPT_FROM_ACTIVITY_GATE),
                    waives(tick),
                    saw,
                    idle_for,
                )
                .await
            else {
                continue;
            };
            let Some((work, _)) = work else {
                continue;
            };
            let Some(model) = self.deps.provider.read().await.clone() else {
                tracing::debug!("[news] no language model is configured");
                continue;
            };
            match work {
                StoryWork::Map(member) => {
                    map_tried.insert(member.clone(), Instant::now());
                    let extras = self.map_extras(&member).await;
                    let zone = self
                        .store
                        .settings(&member)
                        .await
                        .unwrap_or_default()
                        .zone(&self.household_zone().await);
                    let today = Today::of(now, zone);
                    let limits = PassLimits::stories();
                    tokio::select! {
                        _ = slot.cancelled() => {
                            tracing::info!("[news] topic map preempted by user activity");
                        }
                        report = self.writer.build_map(model.as_ref(), &member, extras, now, &today, &limits) => {
                            match report {
                                Ok(r) => tracing::info!(
                                    target: "giap::trace",
                                    kind = "news_map_built",
                                    areas = r.areas,
                                    topics = r.topics,
                                    newsworthy = r.newsworthy,
                                    fallback = r.fallback,
                                ),
                                Err(e) => tracing::warn!(target: "giap::trace", kind = "news_map_failed", error = %e),
                            }
                        }
                    }
                }
                StoryWork::Stories(member, pass) => {
                    self.story_pass(&slot, model, &member, pass, now).await;
                }
            }
            drop(slot);
        }
    }

    async fn story_pass(
        &self,
        slot: &LaneSlot<'_>,
        model: Arc<dyn LlmProvider>,
        member: &str,
        pass: StoryPass,
        now: DateTime<Utc>,
    ) {
        let settings = self.store.settings(member).await.unwrap_or_default();
        let today = Today::of(now, settings.zone(&self.household_zone().await));
        if let Err(e) = self.store.begin_writing(member, now).await {
            tracing::warn!(target: "giap::trace", kind = "news_book_failed", error = %e);
            return;
        }
        let limits = PassLimits::stories();
        let outcome = tokio::select! {
            _ = slot.cancelled() => None,
            report = self.writer.write_stories(model.as_ref(), member, pass, &settings, now, &today, &limits) => Some(report),
        };
        let finished = match outcome {
            None => {
                tracing::info!(
                    pass = pass.as_str(),
                    "[news] story pass preempted by user activity"
                );
                if pass == StoryPass::Refresh {
                    let _ = self.store.request_refresh(member, now).await;
                }
                None
            }
            Some(Ok(report)) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "news_stories_written",
                    pass = pass.as_str(),
                    groups = report.groups,
                    searches = report.searches,
                    candidates = report.candidates,
                    written = report.written,
                    refused = report.refused,
                    duplicates = report.duplicates,
                    unread = report.unread,
                    long_read = report.long_read,
                    out_of_time = report.out_of_time,
                    blocked = ?report.blocked,
                );
                Some(Utc::now())
            }
            Some(Err(e)) => {
                tracing::warn!(target: "giap::trace", kind = "news_stories_failed", error = %e);
                Some(Utc::now())
            }
        };
        if let Err(e) = self.store.end_writing(member, finished).await {
            tracing::warn!(target: "giap::trace", kind = "news_book_failed", error = %e);
        }
    }

    /// What a topic map is written from besides the news's own tables: the member's most
    /// important memories, and the household's scheduled prompts, which show their routines.
    async fn map_extras(&self, member: &str) -> MapExtras {
        let mut memories = self
            .deps
            .memories
            .search_recent(&ProfileScope::Owner(member.to_string()), 200)
            .await
            .unwrap_or_default();
        memories.retain(|m| matches!(m.lifecycle, None | Some(MemoryLifecycle::Active)));
        memories.sort_by(|a, b| {
            b.importance
                .unwrap_or(0.5)
                .total_cmp(&a.importance.unwrap_or(0.5))
        });
        let routines = match &self.deps.scheduler {
            Some(scheduler) => scheduler
                .list_tasks()
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|s| !s.paused)
                .filter_map(|s| match s.kind {
                    TaskKind::AgentPrompt { prompt } => Some(format!("{}: {}", s.label, prompt)),
                    _ => None,
                })
                .take(MAP_ROUTINES)
                .collect(),
            None => Vec::new(),
        };
        MapExtras {
            memories: memories
                .into_iter()
                .take(MAP_MEMORIES)
                .map(|m| m.content)
                .collect(),
            routines,
        }
    }

    /// The first member whose digest is due, and the day it is for.
    async fn digest_work(
        &self,
        now: DateTime<Utc>,
        tried: &HashMap<(String, NaiveDate), Instant>,
    ) -> Option<(String, NaiveDate, NewsSettings)> {
        let household = self.household_zone().await;
        for member in self.store.members().await.ok()? {
            let settings = self.store.settings(&member).await.ok()?;
            if !settings.digest || !self.writer.has_interests(&member).await.unwrap_or(false) {
                continue;
            }
            let zone = settings.zone(&household);
            let Some(day) = schedule::digest_day(now, &zone, &settings) else {
                continue;
            };
            if tried
                .get(&(member.clone(), day))
                .is_some_and(|at| at.elapsed() < DIGEST_RETRY)
            {
                continue;
            }
            let written = self.store.digest_on(&member, day).await.ok()?.is_some();
            let book = self.store.book(&member).await.ok()?;
            if schedule::digest_due(now, &zone, &settings, &book, day, written) {
                return Some((member, day, settings));
            }
        }
        None
    }

    async fn digest_loop(self: Arc<Self>, bells: Bells) {
        let mut tried: HashMap<(String, NaiveDate), Instant> = HashMap::new();
        loop {
            let tick = wait_for_tick(POLL, &bells).await;
            let now = Utc::now();
            tried.retain(|_, at| at.elapsed() < DIGEST_RETRY);
            let work = self.digest_work(now, &tried).await;
            let (saw, idle_for) = self.activity().await;
            let Some(slot) = self
                .deps
                .lane
                .acquire(
                    LaneJob::NewsDigest,
                    work.is_some(),
                    Cadence::new(BETWEEN_PASSES, QUIET, EXEMPT_FROM_ACTIVITY_GATE),
                    waives(tick),
                    saw,
                    idle_for,
                )
                .await
            else {
                continue;
            };
            let Some((member, day, settings)) = work else {
                continue;
            };
            let Some(model) = self.deps.provider.read().await.clone() else {
                continue;
            };
            let zone = settings.zone(&self.household_zone().await);
            let mut today = Today::of(now, zone);
            today.date = day;
            let limits = PassLimits::digest();
            let written = tokio::select! {
                _ = slot.cancelled() => {
                    tracing::info!("[news] digest preempted by user activity");
                    continue;
                }
                digest = self.writer.write_digest(model.as_ref(), &member, &settings, now, &today, &limits) => digest,
            };
            match written {
                Ok(Some(digest)) => {
                    tracing::info!(
                        target: "giap::trace",
                        kind = "news_digest_written",
                        sections = digest.sections.len(),
                    );
                    self.tell(&member, &digest).await;
                    self.record(&digest).await;
                }
                Ok(None) => {
                    tracing::info!(target: "giap::trace", kind = "news_digest_empty");
                    tried.insert((member, day), Instant::now());
                }
                Err(e) => {
                    tracing::warn!(target: "giap::trace", kind = "news_digest_failed", error = %e);
                    tried.insert((member, day), Instant::now());
                }
            }
            drop(slot);
        }
    }

    /// Pushes "Your news is ready" to the member's own devices; a tap opens the digest.
    async fn tell(&self, member: &str, digest: &Digest) {
        let notification = Notification {
            id: format!("news-digest-{}", digest.id),
            target: String::new(),
            category: "info".to_string(),
            title: "Your news is ready".to_string(),
            body: digest.title.clone(),
            timestamp: Utc::now().to_rfc3339(),
            data: Some(
                OpenScreen::News {
                    digest_id: Some(digest.id.clone()),
                }
                .data(),
            ),
        };
        let outcome = match self.deps.members.notify_member(member, notification).await {
            MemberDelivery::Reached(devices) => format!("queued for {}", devices.len()),
            MemberDelivery::NoPhone => "no_phone".to_string(),
            MemberDelivery::Failed(_) => "failed".to_string(),
        };
        tracing::info!(target: "giap::trace", kind = "news_digest_notice", outcome = %outcome);
    }

    /// Records the digest for Listen while the slot is still held, so the phone plays it at once.
    async fn record(&self, digest: &Digest) {
        let Some(voice) = &self.deps.voice else {
            return;
        };
        let path = pond_infra::news_audio::path_for(&self.deps.data_dir, &digest.id);
        match speech::listen_wav(voice.as_ref(), digest).await {
            Ok(Some(wav)) => pond_infra::news_audio::write(&path, &wav).await,
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(target: "giap::trace", kind = "news_listen_failed", error = %e)
            }
        }
    }

    /// Reads each member's digest on the house speaker at its time, when they asked for that and
    /// the household allows it. No model: not a lane job.
    async fn speaker_loop(self: Arc<Self>) {
        let mut clock = tokio::time::interval(POLL);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            clock.tick().await;
            let Some(announcer) = speaker::installed() else {
                continue;
            };
            let household = self.deps.settings.get().await.ok();
            let household_zone = household
                .as_ref()
                .map_or("UTC".to_string(), |s| s.timezone.clone());
            let Ok(members) = self.store.members().await else {
                continue;
            };
            for member in members {
                let Ok(settings) = self.store.settings(&member).await else {
                    continue;
                };
                if !settings.spoken_on_speaker {
                    continue;
                }
                let now = Utc::now();
                let zone = settings.zone(&household_zone);
                let day = now.with_timezone(&zone).date_naive();
                let Ok(Some(digest)) = self.store.digest_on(&member, day).await else {
                    continue;
                };
                let Ok(book) = self.store.book(&member).await else {
                    continue;
                };
                if !schedule::speak_due(now, &zone, &settings, &book, day, true) {
                    continue;
                }
                let local = LocalTimeOfDay::in_zone(&household_zone, now);
                let turn = pond_core::models::domain::inference_bus::turn_has_the_model();
                match speech::house_may_speak(household.as_ref(), local, turn) {
                    UnpromptedSpeech::Spoken => {
                        if self.store.mark_spoken(&member, day).await.is_err() {
                            continue;
                        }
                        let name = self.writer.member_name(&member).await;
                        let text = speech::speaker_text(&digest, name.as_deref());
                        if let Err(e) = announcer.announce(&text).await {
                            tracing::warn!(target: "giap::trace", kind = "news_speak_failed", error = %e);
                        }
                    }
                    UnpromptedSpeech::Refused(why) => {
                        tracing::debug!(reason = why.as_str(), "[news] digest not read aloud yet");
                    }
                }
            }
        }
    }
}
