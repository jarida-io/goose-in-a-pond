//! The background thinking behind My Goose, on the inference lane: each member's portrait (and
//! the pictures of their interests), the ideas researched for them, and the filing of older
//! memories into their profiles. Each acquires the lane like every idle job and yields to a
//! returning member between model calls.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::interest::{topic_key, OFFERED_AT};
use pond_core::user_data::domain::memory::{
    MemoryEventKind, MemoryFragment, MemoryLifecycle, MemorySegment,
};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::ideas::IdeaRepository;
use pond_core::user_data::ports::interests::InterestRepository;
use pond_core::user_data::ports::memory_repository::MemoryRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::services::consolidation_schedule as sched;
use pond_core::user_data::services::idea_generation::{
    self as ideas, IdeaSeed, ReadPage, SeedFrom,
};
use pond_core::user_data::services::inference_lane::LaneJob;
use pond_core::user_data::services::member_portrait as portrait;
use pond_core::user_data::services::memory_resort::{self as resort, Verdict};

use crate::inference_lane_runner::{Cadence, InferenceLane, LaneSlot};

/// How often each job asks whether a member is due; the gates decide whether it runs.
const PORTRAIT_POLL: Duration = Duration::from_secs(20 * 60);
const IDEAS_POLL: Duration = Duration::from_secs(15 * 60);
/// Interests whose picture one portrait pass looks for at most.
const PICTURES_PER_PASS: usize = 5;
/// Search results tried per seed for readable pages.
const HITS_TRIED: usize = 6;

type Provider = Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>;

fn interest_repository(db: &pond_infra::db::Database) -> Arc<dyn InterestRepository> {
    Arc::new(pond_infra::sqlite_interests::SqliteInterestRepository::new(
        db.system.clone(),
    ))
}

fn idea_repository(db: &pond_infra::db::Database) -> Arc<dyn IdeaRepository> {
    Arc::new(pond_infra::sqlite_ideas::SqliteIdeaRepository::new(
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

// ── The portrait ───────────────────────────────────────────────────────

pub fn spawn_portrait_job(deps: MindDeps) {
    let wake = deps.lane.claim(LaneJob::Portrait);
    let started_at = std::time::Instant::now();
    let started_at_utc = Utc::now();
    tokio::spawn(async move {
        let interests = interest_repository(&deps.db);
        loop {
            let tick = crate::inference_lane_runner::wait_for_tick(PORTRAIT_POLL, &wake).await;
            let now = Utc::now();
            let mut due = Vec::new();
            for (id, name) in deps.member_ids().await {
                let known = interests.portrait(&id).await.ok().flatten();
                let changed = interests.last_changed(&id).await.ok().flatten();
                if tick.waives() || portrait::portrait_is_due(known.as_ref(), changed, now) {
                    due.push((id, name));
                }
            }
            if due.is_empty() {
                continue;
            }
            let (saw_activity, idle_for) = deps.activity(started_at, started_at_utc).await;
            let cadence = Cadence::new(
                PORTRAIT_POLL,
                Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS),
                false,
            );
            let enabled = deps.provider.read().await.is_some();
            let Some(slot) = deps
                .lane
                .acquire(
                    LaneJob::Portrait,
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
            for (member, name) in due {
                if slot.is_cancelled() {
                    tracing::info!("[my-goose] portrait pass preempted by user activity");
                    break;
                }
                write_portrait(&deps, interests.as_ref(), &slot, &member, &name).await;
                find_pictures(interests.as_ref(), &member).await;
            }
            drop(slot);
        }
    });
}

async fn write_portrait(
    deps: &MindDeps,
    interests: &dyn InterestRepository,
    slot: &LaneSlot<'_>,
    member: &str,
    name: &str,
) {
    let rows = interests.list(member).await.unwrap_or_default();
    let confirmed: Vec<_> = rows
        .iter()
        .filter(|i| i.confirmed && !i.avoid)
        .cloned()
        .collect();
    let guesses: Vec<_> = rows
        .iter()
        .filter(|i| !i.confirmed && !i.avoid && i.evidence >= OFFERED_AT)
        .cloned()
        .collect();
    let memories = deps
        .memories
        .search_recent(&ProfileScope::Owner(member.to_string()), 150)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|m| m.profile_id.as_deref() == Some(member))
        .filter(|m| m.segment != Some(MemorySegment::Knowledge))
        .filter(|m| {
            !pond_core::user_data::services::memory_extraction::describes_a_question(&m.content)
        })
        .collect();
    let memories = portrait::top_memories(memories, portrait::PORTRAIT_MEMORIES);
    let summaries = recent_summaries(deps.sessions.as_ref(), member).await;

    let started = std::time::Instant::now();
    let Some(reply) = deps
        .complete(
            slot,
            &portrait::render_system(name),
            portrait::build_user_message(&confirmed, &guesses, &memories, &summaries),
        )
        .await
    else {
        return;
    };
    match portrait::parse_reply(&reply, Utc::now()) {
        Some(written) => {
            if let Err(e) = interests.save_portrait(member, &written).await {
                tracing::warn!(error = %e, "[my-goose] the portrait could not be saved");
                return;
            }
            tracing::info!(
                target: "giap::trace",
                kind = "portrait_written",
                into = written.into.len(),
                working_on = written.working_on.len(),
                likes = written.likes.len(),
                ms = started.elapsed().as_millis() as u64,
                "a member's portrait was rewritten"
            );
        }
        None => tracing::warn!(
            target: "giap::trace",
            kind = "portrait_unparseable",
            "the portrait reply held no usable summary; the old portrait stays"
        ),
    }
}

/// The member's newest conversation summaries, each with its title.
async fn recent_summaries(sessions: &dyn SessionStorage, member: &str) -> Vec<String> {
    let Ok(mut all) = sessions.list_sessions().await else {
        return Vec::new();
    };
    all.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let mut out = Vec::new();
    for session in all.into_iter().take(60) {
        if out.len() >= portrait::PORTRAIT_SUMMARIES {
            break;
        }
        let owner = match session.profile_id.clone() {
            Some(owner) => Some(owner),
            None => sessions
                .get_session_identity(&session.id)
                .await
                .ok()
                .and_then(|i| i.profile_id),
        };
        if owner.as_deref() != Some(member) {
            continue;
        }
        if let Ok((Some(summary), _)) = sessions.get_rolling_summary(&session.id).await {
            let title = session.title.unwrap_or_default();
            out.push(if title.is_empty() {
                summary
            } else {
                format!("{title}: {summary}")
            });
        }
    }
    out
}

/// Best effort: a picture for each interest the phone would show, from its Wikipedia article.
async fn find_pictures(interests: &dyn InterestRepository, member: &str) {
    let Some(images) = pond_core::shared::services::report_images::installed() else {
        return;
    };
    let rows = interests
        .without_picture(member, PICTURES_PER_PASS * 2)
        .await
        .unwrap_or_default();
    for row in rows
        .into_iter()
        .filter(|r| !r.avoid && (r.confirmed || r.evidence >= OFFERED_AT))
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

// ── Ideas ──────────────────────────────────────────────────────────────

fn site_of(address: &str) -> String {
    reqwest::Url::parse(address)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_string())
        })
        .unwrap_or_default()
}

pub fn spawn_ideas_job(deps: MindDeps) {
    let wake = deps.lane.claim(LaneJob::Ideas);
    let started_at = std::time::Instant::now();
    let started_at_utc = Utc::now();
    tokio::spawn(async move {
        let ideas_repo = idea_repository(&deps.db);
        let interests = interest_repository(&deps.db);
        loop {
            let tick = crate::inference_lane_runner::wait_for_tick(IDEAS_POLL, &wake).await;
            let now = Utc::now();
            // Asked-for members first; then anyone a day past their last ideas.
            let mut asked = Vec::new();
            let mut daily = Vec::new();
            for (id, name) in deps.member_ids().await {
                let Ok(run) = ideas_repo.run(&id).await else {
                    continue;
                };
                if run.is_pending() {
                    asked.push((id, name));
                } else if ideas::ideas_are_due(false, run.finished_at, now) {
                    daily.push((id, name));
                }
            }
            let Some((member, name)) = asked.into_iter().chain(daily).next() else {
                continue;
            };
            let (saw_activity, idle_for) = deps.activity(started_at, started_at_utc).await;
            let cadence = Cadence::new(
                IDEAS_POLL,
                Duration::from_secs(sched::INACTIVITY_THRESHOLD_SECS),
                false,
            );
            let enabled = deps.provider.read().await.is_some();
            let Some(slot) = deps
                .lane
                .acquire(
                    LaneJob::Ideas,
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
            let _ = ideas_repo.mark_started(&member, Utc::now()).await;
            let made = think_of_ideas(
                &deps,
                ideas_repo.as_ref(),
                interests.as_ref(),
                &slot,
                &member,
                &name,
            )
            .await;
            // A pass a member cut short is finished too: the next due tick starts it again.
            let _ = ideas_repo.mark_finished(&member, Utc::now()).await;
            tracing::info!(
                target: "giap::trace",
                kind = "ideas_written",
                ideas = made,
                preempted = slot.is_cancelled(),
                "a member's ideas were thought of"
            );
            drop(slot);
        }
    });
}

/// One run for one member: up to two seeds, each searched, read and written up. Returns how many
/// ideas were stored.
async fn think_of_ideas(
    deps: &MindDeps,
    ideas_repo: &dyn IdeaRepository,
    interests: &dyn InterestRepository,
    slot: &LaneSlot<'_>,
    member: &str,
    name: &str,
) -> usize {
    let now = Utc::now();
    let rest_since = now - ideas::TOPIC_REST;
    let topic_map = ideas_repo
        .research_seeds(member, rest_since)
        .await
        .unwrap_or_default();
    let rows = interests.list(member).await.unwrap_or_default();
    let known = interests.portrait(member).await.ok().flatten();
    let used = ideas_repo
        .topics_since(member, rest_since)
        .await
        .unwrap_or_default();
    let seeds = ideas::pick_seeds(&topic_map, &rows, known.as_ref(), &used);
    let mut made = 0;
    for seed in seeds {
        if slot.is_cancelled() {
            break;
        }
        match idea_from(deps, slot, member, name, known.as_ref(), &seed).await {
            Ok(Some(idea)) => {
                if let Err(e) = ideas_repo.add(&idea).await {
                    tracing::warn!(error = %e, "[my-goose] an idea could not be stored");
                    continue;
                }
                made += 1;
            }
            Ok(None) => {}
            // No search key, or the search refused: no seed will fare better this run.
            Err(why) => {
                tracing::info!(target: "giap::trace", kind = "ideas_search_unavailable", %why);
                break;
            }
        }
        // Used whether or not an idea came of it, so the next run tries another topic.
        if let SeedFrom::TopicMap(id) = &seed.from {
            let _ = ideas_repo.mark_seed_used(member, id, Utc::now()).await;
        }
    }
    made
}

/// One seed to one idea. `Err` when searching itself is unavailable.
async fn idea_from(
    deps: &MindDeps,
    slot: &LaneSlot<'_>,
    member: &str,
    name: &str,
    known: Option<&pond_core::user_data::domain::interest::MemberPortrait>,
    seed: &IdeaSeed,
) -> Result<Option<pond_core::user_data::domain::idea::Idea>, String> {
    let query = match &seed.query {
        Some(query) => Some(query.clone()),
        None => deps
            .complete(
                slot,
                &ideas::render_query_system(name),
                ideas::build_query_message(known, seed),
            )
            .await
            .and_then(|reply| ideas::parse_query(&reply)),
    };
    let Some(query) = query else {
        return Ok(None);
    };
    let hits = pond_mcp_server::web_search::search_hits(&query).await?;
    let mut pages: Vec<ReadPage> = Vec::new();
    for hit in hits.iter().take(HITS_TRIED) {
        if pages.len() >= ideas::PAGES_PER_SEED || slot.is_cancelled() {
            break;
        }
        match pond_mcp_server::web_page::read_page_text(&hit.url).await {
            Ok((url, title, text)) if text.chars().count() >= 200 => pages.push(ReadPage {
                site: site_of(&url),
                title: if title.is_empty() {
                    hit.title.clone()
                } else {
                    title
                },
                url,
                text,
            }),
            _ => {}
        }
    }
    if pages.is_empty() {
        tracing::debug!("[my-goose] no page for an idea's search could be read");
        return Ok(None);
    }
    let Some(reply) = deps
        .complete(
            slot,
            &ideas::render_idea_system(name),
            ideas::build_idea_message(known, seed, &pages),
        )
        .await
    else {
        return Ok(None);
    };
    let Some(mut idea) = ideas::parse_idea(&reply, member, seed, &pages, Utc::now()) else {
        return Ok(None);
    };
    if let Some(images) = pond_core::shared::services::report_images::installed() {
        let addresses: Vec<String> = idea.sources.iter().map(|s| s.url.clone()).collect();
        idea.picture = images
            .for_pages(&addresses, 1)
            .await
            .into_iter()
            .next()
            .map(|image| pond_core::user_data::domain::interest::picture_path(&image.key));
    }
    Ok(Some(idea))
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
