//! `plan_for` for `giap-schedule`: something the speaker asked Goose to plan, prepare for or keep
//! an eye on becomes one of their anchors, which Goose makes work for by itself.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Utc};
use pond_core::shared::domain::todo::Tz;
use pond_core::user_data::ports::anchors::AnchorRepository;
use pond_core::user_data::ports::dont_list::DontListRepository;
use pond_core::user_data::services::anchor_add::{
    add_asked, anchor_for_plan, plan_reply, AnchorAdded, PlanAsked,
};
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

/// The stores `plan_for` writes to.
pub struct PlanDeps {
    pub anchors: Arc<dyn AnchorRepository>,
    /// Where the rule a stopped anchor left is taken back when the member asks for it again.
    pub dont: Arc<dyn DontListRepository>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::anchor::{AnchorKind, AnchorOrigin, AnchorState};
    use pond_core::user_data::mocks::mock_anchors::InMemoryAnchorRepository;
    use pond_core::user_data::mocks::mock_dont_list::InMemoryDontListRepository;

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
}
