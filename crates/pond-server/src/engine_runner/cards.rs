//! A `cards` job: the maker step searches the web for what the member's brief needs and drafts
//! cards from numbered sources, the critic names cards to drop, and the settle stores the set
//! and pushes the cards that fall today.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use pond_core::mcp::ports::notification::{Notification, OpenScreen};
use pond_core::shared::domain::long_task::{ArtifactKind, ArtifactSummary, LongTask, PlanStep};
use pond_core::shared::domain::todo::{zone, StateFilter};
use pond_core::user_data::domain::anchor::{AnchorKind, AnchorState};
use pond_core::user_data::domain::card::Card;
use pond_core::user_data::domain::dont_list::Strength;
use pond_core::user_data::domain::engine_job::{EngineJob, JobClass, JobKind, JobState};
use pond_core::user_data::domain::fact_sheet::FactState;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::services::cards::{
    build_critic_message, build_maker_message, build_queries_message, is_timely, morning_set_due,
    parse_cards, parse_drops, parse_queries, pick, request_cards, sources, today_line, with_web,
    Numbered, Passage, SourceRows, CRITIC_SYSTEM, KEEP_DAYS, MAKER_SYSTEM, QUERIES_SYSTEM,
    RECENT_DAYS,
};
use pond_core::user_data::services::dont_list::for_brief;
use pond_core::user_data::services::engine::critic::Verdict;
use pond_core::user_data::services::engine::settle::job_outcome_line;
use pond_core::user_data::services::engine::steps::{
    critic_step, draft_artifact, latest_draft_step, verdict_artifact,
};

use super::finish::{close, stage, why_no_draft, Stage};
use super::inline::{body_of, failed, Saved, StepCtx};
use super::{notices, EngineDeps, ModelStop};

const SOURCES_TITLE: &str = "Sources";
const DRAFT_TITLE: &str = "Draft";
const CHECK_TITLE: &str = "Check";
const PASSAGES_PER_QUERY: usize = 3;
const MEMORY_ROWS: usize = 15;

/// The pond's web search, as a set's queries use it.
#[async_trait]
pub trait WebFinder: Send + Sync {
    async fn find(&self, query: &str) -> Result<Vec<Passage>, String>;
}

/// The built-in search the chat's `search_web` tool uses.
pub struct PondWebSearch;

#[async_trait]
impl WebFinder for PondWebSearch {
    async fn find(&self, query: &str) -> Result<Vec<Passage>, String> {
        let hits = pond_mcp_server::web_search::search_hits(query).await?;
        Ok(hits
            .into_iter()
            .map(|hit| {
                let mut text = hit.description;
                for extra in hit.extra_snippets {
                    text.push(' ');
                    text.push_str(&extra);
                }
                Passage {
                    title: hit.title,
                    url: hit.url,
                    text,
                }
            })
            .collect())
    }
}

/// What the maker and the critic are both told besides the sources.
struct Context {
    today: String,
    dont: String,
    recent: Vec<String>,
}

async fn context(deps: &EngineDeps, member: &str, now: DateTime<Utc>) -> anyhow::Result<Context> {
    let (timezone, place) = deps.clock_and_place().await;
    let never = deps.dont.list(member, Some(Strength::Never)).await?;
    let avoid = deps.dont.list(member, Some(Strength::Avoid)).await?;
    let recent = deps
        .cards
        .since(member, now - Duration::days(RECENT_DAYS))
        .await?
        .into_iter()
        .map(|card| card.title)
        .collect();
    Ok(Context {
        today: today_line(now, &timezone, place.as_deref()),
        dont: for_brief(&never, &avoid),
        recent,
    })
}

/// The member's active plans, leaving out the standing everyday one.
async fn plans_of(
    deps: &EngineDeps,
    member: &str,
) -> anyhow::Result<Vec<pond_core::user_data::domain::anchor::Anchor>> {
    Ok(deps
        .anchors
        .list(member, &[AnchorState::Active])
        .await?
        .into_iter()
        .filter(|anchor| anchor.profile_id == member && anchor.kind != AnchorKind::Standing)
        .collect())
}

async fn own_sources(deps: &EngineDeps, member: &str) -> anyhow::Result<Vec<Numbered>> {
    let (timezone, _) = deps.clock_and_place().await;
    let facts = deps.facts.list(member, FactState::Active).await?;
    let plans = plans_of(deps, member).await?;
    let todos = deps.todos.list(member, StateFilter::Open).await?;
    let interests = deps.interests.list(member).await?;
    let memories = deps
        .memories
        .search_recent(&ProfileScope::Owner(member.to_string()), MEMORY_ROWS)
        .await?;
    Ok(sources(
        SourceRows {
            facts: &facts,
            plans: &plans,
            todos: &todos,
            interests: &interests,
            memories: &memories,
        },
        &timezone,
    ))
}

/// The top passages of each search; a search that fails is left out.
async fn passages(deps: &EngineDeps, queries: &[String]) -> Vec<Passage> {
    let Some(web) = &deps.web else {
        return Vec::new();
    };
    let mut kept: Vec<Passage> = Vec::new();
    for query in queries {
        match web.find(query).await {
            Ok(found) => kept.extend(
                found
                    .into_iter()
                    .filter(|p| !kept.iter().any(|k| k.url == p.url))
                    .take(PASSAGES_PER_QUERY)
                    .collect::<Vec<_>>(),
            ),
            Err(e) => tracing::info!(error = %e, "a card set's web search found nothing"),
        }
    }
    kept
}

/// The numbered sources the maker step saved.
async fn shown(
    deps: &EngineDeps,
    task: &LongTask,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
) -> anyhow::Result<Vec<Numbered>> {
    let maker = latest_draft_step(steps).map(|step| step.key());
    let saved = artifacts
        .iter()
        .rev()
        .find(|artifact| artifact.step_key.as_ref() == maker && artifact.title == SOURCES_TITLE)
        .ok_or_else(|| anyhow::anyhow!("the draft's sources were not saved"))?;
    Ok(serde_json::from_str(&body_of(deps, task, saved).await?)?)
}

/// The maker: up to three searches, then up to twelve cards from the numbered sources.
pub(super) async fn make(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let member = ctx.member()?;
    let told = context(ctx.deps, member, ctx.now).await.map_err(failed)?;
    let own = own_sources(ctx.deps, member).await.map_err(failed)?;
    let queries = if ctx.deps.web.is_some() {
        let reply = ctx
            .ask(QUERIES_SYSTEM, build_queries_message(&told.today, &own))
            .await?;
        parse_queries(&reply)
    } else {
        Vec::new()
    };
    let found = passages(ctx.deps, &queries).await;
    let shown = with_web(own, &found);
    if shown.is_empty() {
        return Err(failed(
            "nothing is known about the member to build a card on",
        ));
    }
    let reply = ctx
        .ask(
            MAKER_SYSTEM,
            build_maker_message(&told.today, &shown, &told.dont, &told.recent),
        )
        .await?;
    let drafted = parse_cards(&reply, &shown, member, &ctx.job.id, ctx.now);
    let summary = format!(
        "Searched {} times and drafted {} cards; refused {}.",
        queries.len(),
        drafted.cards.len(),
        drafted.refused.len()
    );
    let kept = ctx
        .save(
            ArtifactKind::Data,
            SOURCES_TITLE,
            "The numbered sources the cards were drafted from",
            serde_json::to_string(&shown).map_err(failed)?,
        )
        .await?;
    // Saved last: the draft a step stands by is what it saved last.
    let draft = ctx
        .save(ArtifactKind::Data, DRAFT_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![kept, draft],
    })
}

/// The critic: names the drafted cards to drop.
pub(super) async fn critic(ctx: &StepCtx<'_>) -> Result<Saved, ModelStop> {
    let member = ctx.member()?;
    let shown = shown(ctx.deps, ctx.task, ctx.steps, ctx.artifacts)
        .await
        .map_err(failed)?;
    let (_, draft) = ctx.draft().await?;
    let cards = parse_cards(&draft, &shown, member, &ctx.job.id, ctx.now).cards;
    if cards.is_empty() {
        return Err(failed("the draft holds no card to check"));
    }
    let told = context(ctx.deps, member, ctx.now).await.map_err(failed)?;
    let reply = ctx
        .ask(
            CRITIC_SYSTEM,
            build_critic_message(&told.today, &shown, &told.dont, &cards),
        )
        .await?;
    let summary = match parse_drops(&reply, cards.len()) {
        None => "The check could not be read.".to_string(),
        Some(drops) => format!(
            "The check dropped {} of {} cards.",
            drops.len(),
            cards.len()
        ),
    };
    let saved = ctx
        .save(ArtifactKind::Data, CHECK_TITLE, &summary, reply)
        .await?;
    Ok(Saved {
        summary,
        artifacts: vec![saved],
    })
}

fn dropped(why: &str) -> (JobState, String) {
    (JobState::Dropped, format!("Dropped: {why}"))
}

/// The cards the check left, picked into the set.
async fn picked(
    deps: &EngineDeps,
    task: &LongTask,
    member: &str,
    cards: Vec<Card>,
    steps: &[PlanStep],
    artifacts: &[ArtifactSummary],
    now: DateTime<Utc>,
) -> anyhow::Result<Vec<Card>> {
    let reply = match verdict_artifact(steps, artifacts) {
        Some(found) => body_of(deps, task, found).await?,
        None => String::new(),
    };
    // Every card already passed the code's own checks, so an unreadable check drops nothing.
    let drops = parse_drops(&reply, cards.len()).unwrap_or_default();
    let kept: Vec<Card> = cards
        .into_iter()
        .enumerate()
        .filter(|(at, _)| !drops.contains(at))
        .map(|(_, card)| card)
        .collect();
    let has_plan = !plans_of(deps, member).await?.is_empty();
    let recent = context(deps, member, now).await?.recent;
    Ok(pick(kept, has_plan, &recent))
}

/// What follows a cards task's steps: its one check, then the set is stored and the cards that
/// fall today are pushed.
pub(super) async fn after_steps(
    deps: &EngineDeps,
    task: &LongTask,
    mut job: EngineJob,
    steps: Vec<PlanStep>,
    artifacts: Vec<ArtifactSummary>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let at = stage(&steps);
    let found = draft_artifact(&steps, &artifacts).filter(|_| !matches!(at, Stage::NoDraft));
    let (Some(found), Some(member)) = (found, job.profile_id.clone()) else {
        let outcome = why_no_draft(&steps);
        return close(deps, task, &mut job, JobState::Failed, &outcome, now).await;
    };
    let Ok(sources) = shown(deps, task, &steps, &artifacts).await else {
        let outcome = "Failed: what the draft was written from was not kept.";
        return close(deps, task, &mut job, JobState::Failed, outcome, now).await;
    };
    let draft = body_of(deps, task, found).await?;
    let cards = parse_cards(&draft, &sources, &member, &job.id, now).cards;
    let mut set = Vec::new();
    let (state, said) = match at {
        Stage::Unchecked if cards.is_empty() => {
            dropped("the draft held no card its sources support.")
        }
        Stage::Unchecked => match critic_step(task, &steps, &artifacts) {
            Ok(plan) => return deps.tasks.replace_open_steps(&plan).await,
            Err(_) => (JobState::Dropped, job_outcome_line(&job, None)),
        },
        Stage::NoDraft | Stage::CheckDied => (JobState::Dropped, job_outcome_line(&job, None)),
        Stage::Checked => {
            job.count_round(Vec::new());
            set = picked(deps, task, &member, cards, &steps, &artifacts, now).await?;
            if set.is_empty() {
                dropped("the check left no card worth showing.")
            } else {
                deps.cards.add_set(&member, &set, now).await?;
                let passed = Verdict {
                    pass: true,
                    objections: Vec::new(),
                };
                let said = format!(
                    "{} Made {} cards.",
                    job_outcome_line(&job, Some(&passed)),
                    set.len()
                );
                (JobState::Passed, said)
            }
        }
    };
    close(deps, task, &mut job, state, &said, now).await?;
    push_timely(deps, &member, &set, now).await;
    Ok(())
}

/// One push per card that falls today and has an action.
async fn push_timely(deps: &EngineDeps, member: &str, set: &[Card], now: DateTime<Utc>) {
    let (timezone, _) = deps.clock_and_place().await;
    for card in set
        .iter()
        .filter(|card| is_timely(card, zone(&timezone), now))
    {
        let mut fields = serde_json::Map::new();
        fields.insert("card_id".into(), card.id.as_str().into());
        let notice = Notification {
            id: format!("card-{}", card.id),
            target: String::new(),
            category: "info".to_string(),
            title: card.title.clone(),
            body: card.why.text.clone(),
            timestamp: now.to_rfc3339(),
            data: Some(OpenScreen::Cards.merged_into(fields)),
        };
        notices::tell_member(deps, member, notice, None).await;
    }
}

/// Queues the member's morning set when it is due; `true` when one was queued.
pub(super) async fn queue_morning(
    deps: &EngineDeps,
    member: &str,
    open_jobs: &[EngineJob],
    now: DateTime<Utc>,
) -> anyhow::Result<bool> {
    let one_open = open_jobs
        .iter()
        .any(|job| job.kind == JobKind::Cards && job.profile_id.as_deref() == Some(member));
    let last = deps.jobs.last_finished(member, JobKind::Cards).await?;
    let (timezone, _) = deps.clock_and_place().await;
    if !morning_set_due(last, one_open, zone(&timezone), now) {
        return Ok(false);
    }
    request_cards(deps.jobs.as_ref(), member, JobClass::Upkeep, now).await?;
    Ok(true)
}

/// Deletes unsaved cards older than the maker is ever told of.
pub(super) async fn purge(deps: &EngineDeps, now: DateTime<Utc>) {
    if let Err(e) = deps
        .cards
        .purge_before(now - Duration::days(KEEP_DAYS))
        .await
    {
        tracing::warn!(error = %e, "the engine could not purge old cards");
    }
}
