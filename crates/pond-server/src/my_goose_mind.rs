//! The background thinking behind My Goose, on the inference lane: each member's web watches
//! judged and older memories filed into their profiles. Each acquires the lane like every idle
//! job and yields to a returning member between model calls. The pictures of their interests
//! need no model and keep their own tick.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::shared::domain::todo::{zone, Tz};
use pond_core::shared::domain::web_watch::WatchChange;
use pond_core::shared::ports::web_watch::{
    StoredChange, WatchNotifier, WebWatch, WebWatchRepository,
};
use pond_core::shared::services::web_watch_judge as judge;
use pond_core::user_data::domain::interest::topic_key;
use pond_core::user_data::domain::memory::{
    MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment,
};
use pond_core::user_data::ports::interests::InterestRepository;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::consolidation_schedule as sched;
use pond_core::user_data::services::inference_lane::LaneJob;
use pond_core::user_data::services::memory_resort::{self as resort, Verdict};

use crate::inference_lane_runner::{Cadence, InferenceLane, LaneSlot};

/// How often interests are looked through for one with no picture looked for yet.
const PICTURES_POLL: Duration = Duration::from_secs(20 * 60);
/// Interests whose picture one pass looks for at most.
const PICTURES_PER_PASS: usize = 5;
/// Rows with no picture read at once, to find those a member is shown among the candidates.
const PICTURE_LOOKAHEAD: usize = 200;

type Provider = Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>;

fn interest_repository(db: &pond_infra::db::Database) -> Arc<dyn InterestRepository> {
    Arc::new(pond_infra::sqlite_interests::SqliteInterestRepository::new(
        db.system.clone(),
    ))
}

/// What both jobs read and write.
#[derive(Clone)]
pub struct MindDeps {
    pub lane: Arc<InferenceLane>,
    pub db: Arc<pond_infra::db::Database>,
    pub profiles: Arc<dyn ProfileRepository + Send + Sync>,
    pub memories: Arc<dyn MemoryRepository + Send + Sync>,
    pub sessions: Arc<dyn SessionStorage>,
    pub provider: Provider,
    pub last_user_activity: Arc<tokio::sync::RwLock<std::time::Instant>>,
    pub settings: Arc<dyn SettingsRepository + Send + Sync>,
}

impl MindDeps {
    /// The lane's quiet and activity gates, read as the other idle jobs read them.
    async fn activity(
        &self,
        started_at: std::time::Instant,
        started_at_utc: chrono::DateTime<Utc>,
    ) -> (bool, Duration) {
        let db_activity = crate::newest_session_activity(self.sessions.as_ref()).await;
        let in_process_at = *self.last_user_activity.read().await;
        let saw =
            sched::saw_activity_since_start(started_at, in_process_at, started_at_utc, db_activity);
        (
            saw,
            sched::combined_idle_for(in_process_at, db_activity, Utc::now()),
        )
    }

    async fn member_ids(&self) -> Vec<(String, String)> {
        self.profiles
            .list()
            .await
            .map(|profiles| {
                profiles
                    .into_iter()
                    .map(|p| (p.id, p.display_name))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// One model call that gives way to a returning member.
    async fn complete(&self, slot: &LaneSlot<'_>, system: &str, user: String) -> Option<String> {
        let provider = self.provider.read().await.clone()?;
        tokio::select! {
            _ = slot.cancelled() => None,
            reply = provider.complete(system, vec![ChatMessage::user(user)]) => match reply {
                Ok(reply) => Some(reply.content),
                Err(e) => {
                    tracing::warn!(error = %e, "[my-goose] a model call failed");
                    None
                }
            },
        }
    }
}

// ── Interest pictures ──────────────────────────────────────────────────

/// Looks for a picture of each interest a member is shown. No model is called, so it keeps
/// its own tick rather than a place on the lane.
pub fn spawn_interest_pictures_job(deps: MindDeps) {
    tokio::spawn(async move {
        let interests = interest_repository(&deps.db);
        loop {
            tokio::time::sleep(PICTURES_POLL).await;
            let tz = deps
                .settings
                .get()
                .await
                .map_or_else(|_| zone(""), |s| zone(&s.timezone));
            for (member, _) in deps.member_ids().await {
                find_pictures(interests.as_ref(), &member, tz).await;
            }
        }
    });
}

/// Best effort: a picture for each interest the member confirmed or is about to be asked
/// about, from its Wikipedia article.
async fn find_pictures(interests: &dyn InterestRepository, member: &str, tz: Tz) {
    let Some(images) = pond_core::shared::services::report_images::installed() else {
        return;
    };
    let rows = interests
        .without_picture(member, PICTURE_LOOKAHEAD)
        .await
        .unwrap_or_default();
    if rows.is_empty() {
        return;
    }
    let offered: Vec<String> = interests
        .ready_to_offer(member, tz)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.id)
        .collect();
    for row in rows
        .into_iter()
        .filter(|r| !r.avoid && (r.confirmed || offered.contains(&r.id)))
        .take(PICTURES_PER_PASS)
    {
        let key = match wikipedia_page(&row.topic).await {
            Some(page) => images
                .for_pages(&[page], 1)
                .await
                .into_iter()
                .next()
                .map(|image| image.key),
            None => None,
        };
        let _ = interests.set_picture(member, &row.id, key.as_deref()).await;
    }
}

/// The address of the topic's own Wikipedia article: the exact title, else the best search hit
/// when its title names the topic.
async fn wikipedia_page(topic: &str) -> Option<String> {
    use pond_mcp_server::knowledge::WikiFetchError;
    let client = pond_net::client_builder().build().ok()?;
    let wiki = pond_mcp_server::KnowledgeMcpServer::new(client);
    let text = match wiki.fetch_article_summary(topic).await {
        Ok(text) => text,
        Err(WikiFetchError::NotFound) => wiki.search_and_fetch_best(topic).await.ok()?,
        Err(WikiFetchError::Mcp(_)) => return None,
    };
    let title = text.lines().next()?.strip_prefix("# ")?.trim().to_string();
    let source = text
        .lines()
        .rev()
        .find_map(|l| l.strip_prefix("Source: "))?
        .trim()
        .to_string();
    let (title_key, wanted) = (topic_key(&title), topic_key(topic));
    (title_key.contains(&wanted) || wanted.contains(&title_key)).then_some(source)
}

// ── The watch judge ────────────────────────────────────────────────────

/// Held changes read at once: enough for every watch a pass could have checked.
const HELD_LOOKAHEAD: usize = judge::CHANGES_PER_CALL * 8;

/// Release `changes` of one watch: `told` are pushed to its owner as one notice, the rest stay
/// in its history untold.
async fn release_watch_changes(
    watches: &dyn WebWatchRepository,
    notifier: &dyn WatchNotifier,
    watch: &WebWatch,
    changes: &[StoredChange],
    told: &[usize],
) {
    let ids: Vec<i64> = changes.iter().map(|c| c.id).collect();
    let told_ids: Vec<i64> = told.iter().filter_map(|i| ids.get(*i).copied()).collect();
    if let Err(e) = watches.judged(&ids, &told_ids).await {
        tracing::warn!(error = %e, "[watch judge] a judgement could not be kept");
        return;
    }
    let lines: Vec<&WatchChange> = told
        .iter()
        .filter_map(|i| changes.get(*i).map(|c| &c.change))
        .collect();
    if let Some(notice) = judge::notice(watch, &lines) {
        if let Err(e) = notifier.notify(notice).await {
            tracing::warn!(error = %e, "[watch judge] the owner could not be told");
        }
    }
}

/// Asks the model which of a watch's held changes are what its owner asked for, tells them of
/// those and lets the rest go. A change no model could judge in time is told anyway.
pub fn spawn_watch_judge_job(
    deps: MindDeps,
    watches: Arc<dyn WebWatchRepository>,
    notifier: Arc<dyn WatchNotifier>,
) {
    let wake = deps.lane.claim(LaneJob::WatchJudge);
    let started_at = std::time::Instant::now();
    let started_at_utc = Utc::now();
    tokio::spawn(async move {
        loop {
            let tick = crate::inference_lane_runner::wait_for_tick(judge::JUDGE_POLL, &wake).await;
            let held = watches.held(HELD_LOOKAHEAD).await.unwrap_or_default();
            if held.is_empty() {
                continue;
            }
            let mut by_watch: std::collections::BTreeMap<String, Vec<StoredChange>> =
                std::collections::BTreeMap::new();
            for change in held {
                by_watch
                    .entry(change.watch_id.clone())
                    .or_default()
                    .push(change);
            }
            let now = Utc::now();
            let mut waiting: Vec<(WebWatch, Vec<StoredChange>)> = Vec::new();
            for (watch_id, changes) in by_watch {
                let Some(watch) = watches.get(&watch_id).await.ok().flatten() else {
                    continue;
                };
                let (late, rest): (Vec<StoredChange>, Vec<StoredChange>) = changes
                    .into_iter()
                    .partition(|change| judge::waited_out(change, now));
                if !late.is_empty() {
                    let all: Vec<usize> = (0..late.len()).collect();
                    release_watch_changes(watches.as_ref(), notifier.as_ref(), &watch, &late, &all)
                        .await;
                    tracing::info!(
                        target: "giap::trace",
                        kind = "watch_changes_judged",
                        changes = late.len(),
                        told = late.len(),
                        by = "wait",
                        "held changes were told unjudged: no model was free in time"
                    );
                }
                if !rest.is_empty() {
                    waiting.push((watch, rest));
                }
            }
            if waiting.is_empty() {
                continue;
            }
            let (saw_activity, idle_for) = deps.activity(started_at, started_at_utc).await;
            let cadence = Cadence::new(
                judge::JUDGE_POLL,
                Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS),
                false,
            );
            let enabled = deps.provider.read().await.is_some();
            let Some(slot) = deps
                .lane
                .acquire(
                    LaneJob::WatchJudge,
                    enabled,
                    cadence,
                    tick.waives(),
                    saw_activity,
                    idle_for,
                )
                .await
            else {
                continue;
            };
            'watches: for (watch, changes) in waiting {
                let name = deps
                    .profiles
                    .get(&watch.owner)
                    .await
                    .ok()
                    .flatten()
                    .map(|p| p.display_name.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "the person".to_string());
                for batch in changes.chunks(judge::CHANGES_PER_CALL) {
                    if slot.is_cancelled() {
                        break 'watches;
                    }
                    let started = std::time::Instant::now();
                    let shown: Vec<WatchChange> = batch.iter().map(|c| c.change.clone()).collect();
                    let Some(reply) = deps
                        .complete(
                            &slot,
                            &judge::render_system(&name),
                            judge::build_user_message(&watch, &shown),
                        )
                        .await
                    else {
                        break 'watches;
                    };
                    // An unusable reply leaves them held, to be asked about again.
                    let Some((told, thinking)) =
                        judge::parse_reply_with_thinking(&reply, batch.len())
                    else {
                        continue;
                    };
                    if !thinking.is_empty() {
                        if let Some(notes) = pond_mcp_server::memory::agent_notes() {
                            let _ = notes
                                .add(pond_core::shared::domain::agent_note::AgentNote {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    profile_id: Some(watch.owner.clone()),
                                    kind: pond_core::shared::domain::agent_note::NoteKind::Note,
                                    text: format!("[watch:{}] {}", watch.id, thinking),
                                    created_at: now,
                                })
                                .await;
                        }
                    }
                    release_watch_changes(
                        watches.as_ref(),
                        notifier.as_ref(),
                        &watch,
                        batch,
                        &told,
                    )
                    .await;
                    // Counts only: what a member watches stays out of the log.
                    tracing::info!(
                        target: "giap::trace",
                        kind = "watch_changes_judged",
                        changes = batch.len(),
                        told = told.len(),
                        by = "model",
                        ms = started.elapsed().as_millis() as u64,
                        "a watch's changes were sorted by what its owner asked for"
                    );
                }
            }
            drop(slot);
        }
    });
}

// ── The re-sort ────────────────────────────────────────────────────────

/// Unfiled rows read at once, to find the next member's batch among them.
const RESORT_LOOKAHEAD: usize = resort::NOTES_PER_CALL * 4;

/// Files memories kept before the profile under what each is about, a batch a model call, for
/// as long as the house stays quiet. It has nothing to do once every older row is filed.
pub fn spawn_memory_resort_job(deps: MindDeps) {
    let wake = deps.lane.claim(LaneJob::MemoryResort);
    let started_at = std::time::Instant::now();
    let started_at_utc = Utc::now();
    tokio::spawn(async move {
        loop {
            let tick =
                crate::inference_lane_runner::wait_for_tick(resort::RESORT_POLL, &wake).await;
            if deps
                .memories
                .unfiled(1)
                .await
                .unwrap_or_default()
                .is_empty()
            {
                continue;
            }
            let (saw_activity, idle_for) = deps.activity(started_at, started_at_utc).await;
            let cadence = Cadence::new(
                resort::RESORT_POLL,
                Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS),
                false,
            );
            let enabled = deps.provider.read().await.is_some();
            let Some(slot) = deps
                .lane
                .acquire(
                    LaneJob::MemoryResort,
                    enabled,
                    cadence,
                    tick.waives(),
                    saw_activity,
                    idle_for,
                )
                .await
            else {
                continue;
            };
            while !slot.is_cancelled() {
                let pending = deps
                    .memories
                    .unfiled(RESORT_LOOKAHEAD)
                    .await
                    .unwrap_or_default();
                let Some(owner) = pending.first().map(|row| row.profile_id.clone()) else {
                    tracing::info!(
                        target: "giap::trace",
                        kind = "memory_resort_done",
                        "every older memory is filed or archived"
                    );
                    break;
                };
                let batch: Vec<MemoryFragment> = pending
                    .into_iter()
                    .filter(|row| row.profile_id == owner)
                    .take(resort::NOTES_PER_CALL)
                    .collect();
                if !resort_batch(&deps, &slot, owner.as_deref(), &batch).await {
                    break;
                }
            }
            drop(slot);
        }
    });
}

/// Replace a note that runs several facts together with its sentences, unfiled, and archive
/// the note. Archived even when no sentence survives, so it is not unpacked again.
async fn unpack_paragraph(deps: &MindDeps, row: &MemoryFragment, name: Option<&str>) {
    let mut pieces = 0usize;
    for sentence in resort::unpacked(&row.content, name) {
        let mut piece = MemoryFragment::from_extraction(
            uuid::Uuid::new_v4().to_string(),
            None,
            sentence,
            row.segment.clone().unwrap_or(MemorySegment::Knowledge),
            row.importance.unwrap_or(0.5),
            None,
        );
        piece.profile_id = row.profile_id.clone();
        let id = piece.id.clone();
        if let Err(e) = deps.memories.add(piece).await {
            tracing::warn!(error = %e, "[re-sort] a sentence of a paragraph could not be kept");
            continue;
        }
        pieces += 1;
        let _ = deps
            .memories
            .log_event(
                MemoryEventKind::Consolidated,
                &id,
                None,
                Some(&format!(
                    "{{\"resort\":\"unpacked\",\"from\":\"{}\"}}",
                    row.id
                )),
            )
            .await;
    }
    if let Err(e) = deps
        .memories
        .update_lifecycle(&row.id, MemoryLifecycle::Archived)
        .await
    {
        tracing::warn!(error = %e, "[re-sort] a paragraph could not be archived");
        return;
    }
    let _ = deps
        .memories
        .log_event(
            MemoryEventKind::Archived,
            &row.id,
            None,
            Some(&format!("{{\"resort\":\"unpacked\",\"into\":{pieces}}}")),
        )
        .await;
    tracing::info!(
        target: "giap::trace",
        kind = "memory_paragraph_unpacked",
        sentences = pieces,
        "a memory that ran several facts together was split into its sentences"
    );
}

/// File one member's batch. `false` when the model gave no reply, so the pass stops instead of
/// counting an attempt against rows it never judged.
async fn resort_batch(
    deps: &MindDeps,
    slot: &LaneSlot<'_>,
    owner: Option<&str>,
    batch: &[MemoryFragment],
) -> bool {
    let (name, aliases) = match owner {
        Some(id) => {
            let profile = deps.profiles.get(id).await.ok().flatten();
            let mut names: Vec<String> = profile
                .iter()
                .flat_map(|p| {
                    [
                        Some(p.display_name.trim().to_string()),
                        p.preferences
                            .get("preferred_name")
                            .map(|n| n.trim().to_string()),
                    ]
                })
                .flatten()
                .filter(|n| !n.is_empty())
                .collect();
            names.dedup();
            let name = names
                .first()
                .cloned()
                .unwrap_or_else(|| "the user".to_string());
            (name, names)
        }
        None => ("the household".to_string(), Vec::new()),
    };
    let started = std::time::Instant::now();
    // A paragraph is not asked about: its sentences come round as notes of their own.
    let (paragraphs, batch): (Vec<&MemoryFragment>, Vec<&MemoryFragment>) = batch
        .iter()
        .partition(|row| resort::is_a_paragraph(&row.content));
    for row in &paragraphs {
        unpack_paragraph(deps, row, owner.map(|_| name.as_str())).await;
    }
    if batch.is_empty() {
        return true;
    }
    let batch: Vec<MemoryFragment> = batch.into_iter().cloned().collect();
    let batch = batch.as_slice();
    let Some(reply) = deps
        .complete(
            slot,
            &resort::render_system(&name),
            resort::build_user_message(batch),
        )
        .await
    else {
        return false;
    };
    let said = resort::parse_reply(&reply, batch.len());
    let (mut filed, mut archived, mut unanswered) = (0usize, 0usize, 0usize);
    for (row, said) in batch.iter().zip(said) {
        match resort::verdict(row, said, &aliases) {
            Verdict::File(about) => {
                if let Err(e) = deps.memories.file_row(&row.id, about).await {
                    tracing::warn!(error = %e, "[re-sort] a memory could not be filed");
                    continue;
                }
                filed += 1;
                let _ = deps
                    .memories
                    .log_event(
                        MemoryEventKind::Consolidated,
                        &row.id,
                        None,
                        Some(&format!(
                            "{{\"resort\":\"filed\",\"about\":\"{}\"}}",
                            about.as_str()
                        )),
                    )
                    .await;
            }
            Verdict::Archive(reason) => {
                if let Err(e) = deps
                    .memories
                    .update_lifecycle(&row.id, MemoryLifecycle::Archived)
                    .await
                {
                    tracing::warn!(error = %e, "[re-sort] a memory could not be archived");
                    continue;
                }
                archived += 1;
                let _ = deps
                    .memories
                    .log_event(
                        MemoryEventKind::Archived,
                        &row.id,
                        None,
                        Some(&format!("{{\"resort\":\"{reason}\"}}")),
                    )
                    .await;
            }
            Verdict::Unanswered => {
                unanswered += 1;
                let _ = deps.memories.note_unfiled_attempt(&row.id).await;
            }
        }
    }
    // Counts only: a memory's words never reach the on-disk log.
    tracing::info!(
        target: "giap::trace",
        kind = "memory_resort_batch",
        notes = batch.len(),
        filed,
        archived,
        unanswered,
        ms = started.elapsed().as_millis() as u64,
        "older memories were filed into a profile"
    );
    true
}
