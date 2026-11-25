//! `plan_for` for `giap-schedule`: something the speaker asked Goose to plan, prepare for or keep
//! an eye on becomes one of their anchors, which Goose makes work for by itself.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use pond_core::shared::domain::todo::Tz;
use pond_core::user_data::domain::anchor::{
    clean_title, title_key, Anchor, AnchorFact, AnchorState, DeliverableState,
};
use pond_core::user_data::domain::card::{
    Card, CardKind, CardNotes, CardSource, CardSourceKind, CardState, Why, WhySource,
};
use pond_core::user_data::domain::engine_job::JobClass;
use pond_core::user_data::ports::anchors::AnchorRepository;
use pond_core::user_data::ports::cards::CardRepository;
use pond_core::user_data::ports::dont_list::DontListRepository;
use pond_core::user_data::ports::engine_jobs::EngineJobRepository;
use pond_core::user_data::services::anchor_add::{
    add_asked, anchor_for_plan, parse_when_words, plan_reply, AnchorAdded, PlanAsked,
};
use pond_core::user_data::services::engine::refresh::request_refresh;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ErrorData, RequestMetaObject},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::format::format_not_done;
use crate::schedule::ScheduleMcpServer;

const WHAT: &str = "planning that";
const ADJUST_WHAT: &str = "updating that plan";
const IDEA_WHAT: &str = "saving that idea";

/// The stores `plan_for`, `adjust_plan` and `save_idea` write to.
pub struct PlanDeps {
    pub anchors: Arc<dyn AnchorRepository>,
    /// Where the rule a stopped anchor left is taken back when the member asks for it again.
    pub dont: Arc<dyn DontListRepository>,
    pub cards: Option<Arc<dyn CardRepository>>,
    pub engine_jobs: Option<Arc<dyn EngineJobRepository>>,
}

static PLAN_DEPS: OnceLock<PlanDeps> = OnceLock::new();

/// Install the stores once, at startup; later calls are ignored.
pub fn init_plan_deps(deps: PlanDeps) {
    let _ = PLAN_DEPS.set(deps);
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct PlanForParams {
    /// "event", "trip", "purchase" or "project".
    #[serde(default)]
    pub kind: String,
    /// What it is, in a few of their words: "Diani trip".
    #[serde(default)]
    pub title: String,
    /// When it starts, as they said it: "19 December". Null when they gave no date.
    pub when: Option<String>,
    /// When it ends, as they said it. Null when they gave none.
    pub until: Option<String>,
    /// Where, as they named it. Null when they named none.
    pub place: Option<String>,
    /// Anything else they said about it, in one line. Null when nothing.
    pub details: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

impl PlanForParams {
    /// The plan as asked, with a field a model sent under another common name taken from there.
    fn asked(self) -> PlanAsked {
        let other = |keys: &[&str]| {
            keys.iter()
                .find_map(|key| self.extra.get(*key).and_then(Value::as_str))
                .map(str::to_string)
        };
        let or = |sent: String, keys: &[&str]| match sent.trim() {
            "" => other(keys).unwrap_or_default(),
            _ => sent,
        };
        PlanAsked {
            kind: or(self.kind.clone(), &["type"]),
            title: or(self.title.clone(), &["name", "what"]),
            when: self.when.clone().or_else(|| other(&["date", "start"])),
            until: self.until.clone().or_else(|| other(&["end"])),
            place: self.place.clone().or_else(|| other(&["where", "location"])),
            details: self.details.clone().or_else(|| other(&["notes"])),
        }
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AdjustPlanParams {
    /// The plan's id, if known (from the plan card).
    pub id: Option<String>,
    /// The plan's current title, used when `id` is not given.
    pub current_title: Option<String>,
    /// New title for the plan, if renaming it.
    pub title: Option<String>,
    /// Updated start date/time in natural language (e.g. "20 December").
    pub when: Option<String>,
    /// Updated end date/time in natural language.
    pub until: Option<String>,
    /// Updated place or location.
    pub place: Option<String>,
    /// Updated notes or details to store on the plan.
    pub notes: Option<String>,
    /// Whether to queue a refresh of the plan's deliverables after updating (defaults to true when details change).
    pub refresh: Option<bool>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveIdeaParams {
    /// Short title for the idea.
    #[serde(default)]
    pub title: String,
    /// Why this idea is worth trying or interesting.
    pub why: Option<String>,
    /// Extra notes, steps, or details from the conversation.
    pub notes: Option<String>,
    /// Kind of idea: "do", "next_step", "reminder" or "ask" (defaults to "do").
    pub kind: Option<String>,
}

#[tool_router(router = plan_tool_router, vis = "pub")]
impl ScheduleMcpServer {
    #[tool(description = "\
Have Goose plan, prepare for or keep an eye on a trip, an event, something to buy or a project \
in the person's life. Goose researches it by itself, writes a plan, dossier or shortlist, and \
tells them when it is ready: \"plan my Diani trip on 19 December\", \"help me prepare for the AI \
summit\", \"keep an eye on used Fielders for me\".")]
    async fn plan_for(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<PlanForParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("plan_for");
        let text = plan(&ctx.meta, params.0).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "\
Update an existing plan or goal (title, dates, place, or notes) and optionally queue a refresh \
of its deliverables so Goose re-runs or refiles the plan with the new details.")]
    async fn adjust_plan(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<AdjustPlanParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("adjust_plan");
        let text = adjust(&ctx.meta, params.0).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(description = "\
Save a new idea to the member's saved ideas list when brainstorming or discussing ideas in chat.")]
    async fn save_idea(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<SaveIdeaParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("save_idea");
        let text = save_idea_cmd(&ctx.meta, params.0).await;
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}

async fn plan(meta: &RequestMetaObject, p: PlanForParams) -> String {
    let Some(deps) = PLAN_DEPS.get() else {
        return format_not_done(WHAT, "Planning is not set up on this Pond yet.");
    };
    let Some(member) = crate::todo_tools::speaker(meta).await else {
        return format_not_done(
            WHAT,
            "Goose plans only for a household member it knows. Ask them who they are.",
        );
    };
    plan_for_member(
        deps,
        &member,
        p,
        Utc::now(),
        crate::todo_tools::household_zone().await,
    )
    .await
}

/// `plan` for a member the Pond knows, at `now` in their timezone.
async fn plan_for_member(
    deps: &PlanDeps,
    member: &str,
    p: PlanForParams,
    now: DateTime<Utc>,
    tz: Tz,
) -> String {
    let anchor = match anchor_for_plan(member, &p.asked(), now, tz) {
        Ok(anchor) => anchor,
        Err(refused) => return format_not_done(WHAT, refused.advice()),
    };
    let added = match add_asked(deps.anchors.as_ref(), deps.dont.as_ref(), anchor, now).await {
        Ok(added) => added,
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "plan_for_failed", error = %e);
            return format_not_done(
                WHAT,
                "The Pond could not save it just now; try again shortly.",
            );
        }
    };
    let outcome = match &added {
        AnchorAdded::New(_) => "new",
        AnchorAdded::Held(_) => "held",
        AnchorAdded::Confirmed(_) => "confirmed",
        AnchorAdded::Revived(_) => "revived",
        AnchorAdded::Ended(_) => "ended",
    };
    tracing::info!(target: "giap::trace", kind = "plan_for", outcome);
    plan_reply(&added).unwrap_or_else(|| {
        format_not_done(
            WHAT,
            &format!(
                "They had one called \"{}\" and it has ended. Ask them for another name for this \
                 one.",
                added.anchor().title
            ),
        )
    })
}

async fn adjust(meta: &RequestMetaObject, p: AdjustPlanParams) -> String {
    let Some(deps) = PLAN_DEPS.get() else {
        return format_not_done(ADJUST_WHAT, "Planning is not set up on this Pond yet.");
    };
    let Some(member) = crate::todo_tools::speaker(meta).await else {
        return format_not_done(
            ADJUST_WHAT,
            "Goose only updates plans for a household member it knows. Ask them who they are.",
        );
    };
    adjust_plan_for_member(
        deps,
        &member,
        p,
        Utc::now(),
        crate::todo_tools::household_zone().await,
    )
    .await
}

fn one_line_opt(raw: Option<&str>, max_chars: usize) -> Option<String> {
    let joined: String = raw?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect();
    (!joined.is_empty()).then_some(joined)
}

async fn find_member_anchor(
    anchors: &dyn AnchorRepository,
    member: &str,
    id: Option<&str>,
    current_title: Option<&str>,
) -> anyhow::Result<Option<Anchor>> {
    if let Some(id) = id.map(str::trim).filter(|s| !s.is_empty()) {
        return anchors.get(member, id).await;
    }
    let Some(title) = current_title.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Some(found) = anchors.by_title(member, &title_key(title)).await? {
        return Ok(Some(found));
    }
    let lower = title.to_ascii_lowercase();
    let live = anchors.list(member, &AnchorState::LIVE).await?;
    Ok(live
        .into_iter()
        .find(|a| a.title.trim().to_ascii_lowercase() == lower))
}

async fn adjust_plan_for_member(
    deps: &PlanDeps,
    member: &str,
    p: AdjustPlanParams,
    now: DateTime<Utc>,
    tz: Tz,
) -> String {
    let has_id = p.id.as_deref().is_some_and(|s| !s.trim().is_empty());
    let has_title = p
        .current_title
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    if !has_id && !has_title {
        return format_not_done(
            ADJUST_WHAT,
            "Send `id` or `current_title` so I know which plan to adjust.",
        );
    }
    let mut anchor = match find_member_anchor(
        deps.anchors.as_ref(),
        member,
        p.id.as_deref(),
        p.current_title.as_deref(),
    )
    .await
    {
        Ok(Some(anchor)) => anchor,
        Ok(None) => return format_not_done(ADJUST_WHAT, "No matching plan was found."),
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "adjust_plan_failed", error = %e);
            return format_not_done(
                ADJUST_WHAT,
                "The Pond could not look up that plan just now; try again shortly.",
            );
        }
    };
    if anchor.profile_id != member {
        return format_not_done(
            ADJUST_WHAT,
            "That plan is shared with them; only the member who owns it can change it.",
        );
    }
    if !AnchorState::LIVE.contains(&anchor.state) {
        return format_not_done(ADJUST_WHAT, "This plan has ended or been stopped.");
    }

    let mut changed = false;
    if let Some(raw_title) = p.title.as_deref() {
        let Some(new_title) = clean_title(raw_title) else {
            return format_not_done(ADJUST_WHAT, "Send a non-empty `title`.");
        };
        match deps.anchors.by_title(member, &title_key(&new_title)).await {
            Ok(Some(other)) if other.id != anchor.id => {
                return format_not_done(
                    ADJUST_WHAT,
                    "They already have another plan with that name.",
                );
            }
            Err(e) => {
                tracing::warn!(target: "giap::trace", kind = "adjust_plan_failed", error = %e);
                return format_not_done(
                    ADJUST_WHAT,
                    "The Pond could not check that title just now; try again shortly.",
                );
            }
            _ => {}
        }
        anchor.title = new_title;
        changed = true;
    }
    let mut unparsed_dates = Vec::new();
    if let Some(when) = one_line_opt(p.when.as_deref(), 80) {
        match parse_when_words(&when, now, tz) {
            Some(dt) => {
                anchor.starts_at = Some(dt);
                changed = true;
            }
            None => unparsed_dates.push("when"),
        }
    }
    if let Some(until) = one_line_opt(p.until.as_deref(), 80) {
        match parse_when_words(&until, now, tz) {
            Some(dt) => {
                anchor.ends_at = Some(dt);
                changed = true;
            }
            None => unparsed_dates.push("until"),
        }
    }
    if matches!((anchor.starts_at, anchor.ends_at), (Some(start), Some(end)) if end < start) {
        return format_not_done(
            ADJUST_WHAT,
            "`until` falls before `when`. Ask them for the two dates again.",
        );
    }
    if let Some(raw_place) = p.place.as_deref() {
        anchor.place = one_line_opt(Some(raw_place), 120);
        changed = true;
    }
    if let Some(notes) = one_line_opt(p.notes.as_deref(), 1000) {
        anchor.set_fact(AnchorFact::new("Notes", &notes, "you"), now);
        changed = true;
    }

    if changed {
        anchor.updated_at = now;
        match deps.anchors.save(&anchor).await {
            Ok(true) => {}
            Ok(false) => return format_not_done(ADJUST_WHAT, "No matching plan was found."),
            Err(e) => {
                tracing::warn!(target: "giap::trace", kind = "adjust_plan_failed", error = %e);
                return format_not_done(
                    ADJUST_WHAT,
                    "The Pond could not save that plan just now; try again shortly.",
                );
            }
        }
    }

    let should_refresh = p.refresh.unwrap_or(changed);
    let mut refreshed = 0usize;
    if should_refresh {
        if let Some(jobs) = deps.engine_jobs.as_ref() {
            if let Ok(deliverables) = deps.anchors.deliverables(member, &anchor.id).await {
                for d in &deliverables {
                    if !matches!(
                        d.state,
                        DeliverableState::Rejected | DeliverableState::Archived
                    ) && request_refresh(jobs.as_ref(), &anchor, d, JobClass::Asked, now)
                        .await
                        .is_ok()
                    {
                        refreshed += 1;
                    }
                }
            }
        }
    }

    tracing::info!(
        target: "giap::trace",
        kind = "adjust_plan",
        changed,
        refreshed
    );
    let mut reply = if refreshed > 0 {
        format!(
            "Updated \"{}\" and queued {} deliverable(s) to refresh.",
            anchor.title, refreshed
        )
    } else if p.refresh == Some(true) {
        format!(
            "Updated \"{}\". No deliverables were queued for refresh.",
            anchor.title
        )
    } else {
        format!("Updated \"{}\".", anchor.title)
    };
    if !unparsed_dates.is_empty() {
        reply.push_str(&format!(
            " Could not parse {} date, so the existing timestamp was kept.",
            unparsed_dates.join(" and ")
        ));
    }
    reply
}

async fn save_idea_cmd(meta: &RequestMetaObject, p: SaveIdeaParams) -> String {
    let Some(deps) = PLAN_DEPS.get() else {
        return format_not_done(IDEA_WHAT, "Saving ideas is not set up on this Pond yet.");
    };
    let Some(member) = crate::todo_tools::speaker(meta).await else {
        return format_not_done(
            IDEA_WHAT,
            "Goose only saves ideas for a household member it knows. Ask them who they are.",
        );
    };
    save_idea_for_member(deps, &member, p, Utc::now()).await
}

async fn save_idea_for_member(
    deps: &PlanDeps,
    member: &str,
    p: SaveIdeaParams,
    now: DateTime<Utc>,
) -> String {
    let Some(cards) = deps.cards.as_ref() else {
        return format_not_done(IDEA_WHAT, "Saving ideas is not set up on this Pond yet.");
    };
    let title = p.title.trim().to_string();
    if title.is_empty() {
        return format_not_done(IDEA_WHAT, "Send `title`: a short name for the idea.");
    }
    let why_text = p
        .why
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Discussed in chat")
        .to_string();
    let notes_text = p.notes.as_deref().map(str::trim).unwrap_or("").to_string();
    let kind = p
        .kind
        .as_deref()
        .and_then(CardKind::parse)
        .unwrap_or(CardKind::Do);
    let card = Card {
        id: uuid::Uuid::new_v4().to_string(),
        profile_id: member.to_string(),
        batch_id: "user".to_string(),
        rank: 0,
        kind,
        title,
        why: Why {
            text: why_text,
            source: WhySource {
                kind: CardSourceKind::Memory,
                label: "From chat".to_string(),
                url: None,
            },
        },
        when: None,
        where_: None,
        cost: None,
        effort: None,
        action: None,
        sources: vec![CardSource {
            kind: CardSourceKind::Memory,
            title: "Saved from chat".to_string(),
            url: None,
        }],
        notes: CardNotes {
            found: notes_text,
            next: String::new(),
            caveats: String::new(),
        },
        anchor: None,
        state: CardState::Saved,
        created_at: now,
        fades_at: Card::fade_time(now),
    };
    if let Err(e) = cards.add_one(&card).await {
        tracing::warn!(target: "giap::trace", kind = "save_idea_failed", error = %e);
        return format_not_done(
            IDEA_WHAT,
            "The Pond could not save that idea just now; try again shortly.",
        );
    }
    tracing::info!(target: "giap::trace", kind = "save_idea", id = %card.id);
    format!("Saved idea \"{}\" (id: {}).", card.title, card.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::anchor::{
        AnchorKind, AnchorOrigin, AnchorState, Deliverable, DeliverableKind,
    };
    use pond_core::user_data::mocks::mock_anchors::InMemoryAnchorRepository;
    use pond_core::user_data::mocks::mock_cards::InMemoryCardRepository;
    use pond_core::user_data::mocks::mock_dont_list::InMemoryDontListRepository;
    use pond_core::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-17T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn deps() -> PlanDeps {
        PlanDeps {
            anchors: Arc::new(InMemoryAnchorRepository::new()),
            dont: Arc::new(InMemoryDontListRepository::new()),
            cards: Some(Arc::new(InMemoryCardRepository::new())),
            engine_jobs: Some(Arc::new(InMemoryEngineJobRepository::new())),
        }
    }

    fn params(raw: Value) -> PlanForParams {
        serde_json::from_value(raw).unwrap()
    }

    #[tokio::test]
    async fn a_trip_becomes_an_active_anchor_and_one_plain_line() {
        let deps = deps();
        let said = plan_for_member(
            &deps,
            "jerry",
            params(serde_json::json!({
                "kind": "trip", "title": "Diani trip", "when": "19 December",
                "until": "23 December", "place": "Diani", "details": "With Liz"
            })),
            now(),
            NAIROBI,
        )
        .await;
        assert_eq!(
            said,
            "I'll work on a plan for your Diani trip and tell you when it's ready."
        );
        let held = deps
            .anchors
            .list("jerry", &AnchorState::LIVE)
            .await
            .unwrap();
        assert_eq!(held.len(), 1);
        let trip = &held[0];
        assert_eq!(
            (trip.kind, trip.state, trip.origin),
            (AnchorKind::Trip, AnchorState::Active, AnchorOrigin::Goose)
        );
        assert!(trip.starts_at.is_some() && trip.ends_at.is_some());
        assert_eq!(trip.place.as_deref(), Some("Diani"));
        assert!(deps
            .anchors
            .list("liz", &AnchorState::LIVE)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn asking_again_is_answered_with_the_one_already_in_hand() {
        let deps = deps();
        for _ in 0..2 {
            plan_for_member(
                &deps,
                "jerry",
                params(serde_json::json!({"kind": "event", "title": "AI Summit Nairobi"})),
                now(),
                NAIROBI,
            )
            .await;
        }
        let again = plan_for_member(
            &deps,
            "jerry",
            params(serde_json::json!({"type": "event", "name": "ai summit nairobi"})),
            now(),
            NAIROBI,
        )
        .await;
        assert!(
            again.starts_with("I'm already working on your AI Summit Nairobi"),
            "{again}"
        );
        assert_eq!(
            deps.anchors
                .list("jerry", &AnchorState::LIVE)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn what_cannot_be_planned_is_refused_with_advice() {
        let deps = deps();
        let said = plan_for_member(
            &deps,
            "jerry",
            params(serde_json::json!({"kind": "holiday", "title": "Diani"})),
            now(),
            NAIROBI,
        )
        .await;
        assert!(said.starts_with("Not done: planning that."), "{said}");
        assert!(said.contains("\"trip\""), "{said}");
        let said = plan_for_member(
            &deps,
            "jerry",
            params(serde_json::json!({"kind": "trip"})),
            now(),
            NAIROBI,
        )
        .await;
        assert!(said.contains("`title`"), "{said}");
        assert!(deps
            .anchors
            .list("jerry", &AnchorState::LIVE)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn an_ended_one_asks_for_another_name() {
        let deps = deps();
        let mut ended = pond_core::user_data::domain::anchor::Anchor::new(
            "jerry",
            AnchorKind::Trip,
            "Diani trip",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        ended.end(now()).unwrap();
        deps.anchors.add(&ended).await.unwrap();
        let said = plan_for_member(
            &deps,
            "jerry",
            params(serde_json::json!({"kind": "trip", "title": "Diani trip"})),
            now(),
            NAIROBI,
        )
        .await;
        assert!(said.contains("has ended"), "{said}");
    }

    #[tokio::test]
    async fn nobody_the_pond_knows_gets_a_plain_refusal() {
        init_plan_deps(deps());
        let said = plan(
            &RequestMetaObject::new(),
            params(serde_json::json!({"kind": "trip", "title": "Diani trip"})),
        )
        .await;
        assert_eq!(
            said,
            "Not done: planning that. Goose plans only for a household member it knows. Ask \
             them who they are."
        );
    }

    #[tokio::test]
    async fn adjust_plan_updates_fields_and_queues_deliverable_refresh() {
        let deps = deps();
        plan_for_member(
            &deps,
            "jerry",
            params(serde_json::json!({
                "kind": "trip", "title": "Diani trip", "when": "19 December",
                "place": "Diani"
            })),
            now(),
            NAIROBI,
        )
        .await;
        let anchor = deps
            .anchors
            .by_title("jerry", &title_key("Diani trip"))
            .await
            .unwrap()
            .unwrap();
        let deliverable = Deliverable::new(
            &anchor.id,
            DeliverableKind::Plan,
            "plan",
            "Itinerary for Diani trip",
            now(),
        )
        .unwrap();
        deps.anchors.add_deliverable(&deliverable).await.unwrap();

        let reply = adjust_plan_for_member(
            &deps,
            "jerry",
            AdjustPlanParams {
                current_title: Some("diani trip".to_string()),
                place: Some("Watamu".to_string()),
                notes: Some("Flying from Wilson".to_string()),
                ..AdjustPlanParams::default()
            },
            now(),
            NAIROBI,
        )
        .await;
        assert!(reply.contains("Updated \"Diani trip\""), "{reply}");
        assert!(reply.contains("1 deliverable(s) to refresh"), "{reply}");

        let updated = deps
            .anchors
            .get("jerry", &anchor.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.place.as_deref(), Some("Watamu"));
        assert!(updated
            .facts
            .iter()
            .any(|f| f.label == "Notes" && f.value == "Flying from Wilson"));
        let jobs = deps.engine_jobs.as_ref().unwrap().open().await.unwrap();
        assert_eq!(jobs.len(), 1);
    }

    #[tokio::test]
    async fn save_idea_stores_saved_card_for_member() {
        let deps = deps();
        let reply = save_idea_for_member(
            &deps,
            "jerry",
            SaveIdeaParams {
                title: "Weekend pottery class".to_string(),
                why: Some("Good way to unwind".to_string()),
                notes: Some("Check studios in Kilimani".to_string()),
                kind: Some("next_step".to_string()),
            },
            now(),
        )
        .await;
        assert!(
            reply.starts_with("Saved idea \"Weekend pottery class\""),
            "{reply}"
        );
        let shown = deps
            .cards
            .as_ref()
            .unwrap()
            .shown("jerry", now())
            .await
            .unwrap();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].state, CardState::Saved);
        assert_eq!(shown[0].batch_id, "user");
        assert_eq!(shown[0].kind, CardKind::NextStep);
    }
}
