//! `add_todo`, `list_todos` and `complete_todo` for `giap-schedule`: the speaker's own to-dos and
//! reminders in My Goose, which the Pond pushes to their phone at a time, a repeat or a place.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Duration, NaiveDateTime, NaiveTime, Utc};
use pond_core::shared::domain::todo::{
    local_instant, new_todo, parse_clock, rfc3339, zone, Answer, HookCatalog, HookError,
    HookSource, Place, StateFilter, Todo, TodoSource, TodoState, Transition, Tz, PRESENCE_HOME,
};
use pond_core::shared::domain::when_said::due_from_words;
use pond_core::shared::ports::todo::TodoRepository;
use pond_core::user_data::domain::profile::ProfileScope;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ErrorData, Meta},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::format::format_dead_end;
use crate::schedule::ScheduleMcpServer;

/// A `when` this far in the past is a slip of the clock, not a time that has gone.
const PAST_SLACK_MINUTES: i64 = 2;
/// To-dos `list_todos` names.
const LISTED: usize = 20;

static TODO_DEPS: OnceLock<Arc<dyn TodoRepository>> = OnceLock::new();

/// Install the store once, at startup; later calls are ignored.
pub fn init_todo_deps(repo: Arc<dyn TodoRepository>) {
    let _ = TODO_DEPS.set(repo);
}

/// A repeat, as `add_todo`'s `repeat` takes it.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RepeatParam {
    /// "day", "weekday" (Monday to Friday), "week" or "month".
    pub every: String,
    /// The time of day, 24-hour "HH:MM", e.g. "18:00".
    pub at: String,
    /// For "week": the weekdays as numbers, 1 Monday to 7 Sunday, e.g. [2, 4]. For "month": the
    /// days of the month, 1 to 31. Null for "day" and "weekday".
    pub days: Option<Vec<u32>>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AddTodoParams {
    /// What to do or remember, in the person's words, without the time or place: "call Emmanuel".
    #[serde(default)]
    pub text: String,
    /// "reminder" when they said "remind me"; "todo" when they asked to add a task to their
    /// to-dos. Null: "todo".
    pub kind: Option<String>,
    /// One moment, as an RFC 3339 time with the person's UTC offset, worked out from the turn's
    /// stated current date and time: "2026-11-10T18:00:00+03:00". A day said without a time is
    /// that day at 09:00. Null when they gave no time, or when it repeats.
    pub when: Option<String>,
    /// Only when they said "every ...": {"every": "weekday", "at": "18:00", "days": null}.
    /// Null otherwise.
    #[schemars(with = "Option<RepeatParam>")]
    pub repeat: Option<Value>,
    /// A place they named for it, as they named it: "home", "work", "the gym". Null when they
    /// named no place.
    pub place: Option<String>,
    /// "arrive" for "when I get to" a place, "leave" for "when I leave" it. Null: "arrive".
    pub place_event: Option<String>,
    /// true to have Goose ask afterwards whether it was done. Null: true for a todo, false for a
    /// reminder.
    pub ask: Option<bool>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListTodosParams {
    /// "open" (default), "done" or "all".
    pub state: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CompleteTodoParams {
    /// The to-do's id from list_todos. Null when you only have its words.
    pub id: Option<String>,
    /// Words of the to-do the person says they did, e.g. "garage". Null when you have the id.
    pub text: Option<String>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

#[tool_router(router = todo_tool_router, vis = "pub")]
impl ScheduleMcpServer {
    #[tool(description = "\
Put a to-do or reminder on the person's own list in My Goose; the Pond pushes it to their phone \
at a time, on a repeat, or at a place. For \"remind me to call Emmanuel when I get home\", \
\"add renew the logbook to my to-dos\", \"remind me on Friday at 9 to call the garage\", \
\"every weekday at 18:00 remind me to stretch\". Not for a countdown (\"set a timer for 10 \
minutes\"): that is set_timer.")]
    async fn add_todo(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<AddTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("add_todo");
        Ok(said(add(&ctx.meta, params.0).await))
    }

    #[tool(description = "\
List the person's to-dos and reminders in My Goose, with what fires each one.")]
    async fn list_todos(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ListTodosParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("list_todos");
        Ok(said(list(&ctx.meta, params.0).await))
    }

    #[tool(description = "\
Mark one of the person's to-dos or reminders done when they say they did it (\"I called the \
garage\").")]
    async fn complete_todo(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<CompleteTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("complete_todo");
        Ok(said(complete(&ctx.meta, params.0).await))
    }
}

fn said(text: String) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text)])
}

/// The household member speaking in this call: the turn's owner, else a one-member household's
/// only member. A guest keeps no list.
async fn speaker(meta: &Meta) -> Option<String> {
    let session = crate::session_from_meta(meta)?;
    let owner = crate::installed_orchestrator_deps()
        .and_then(|deps| deps.authorities().authority_for_engine_session(&session))
        .and_then(|a| a.profile_scope().owner_id().map(str::to_string));
    if owner.is_some() {
        return owner;
    }
    let authority = crate::speaker_authority()?;
    match authority.actor_for_engine_session(&session).await {
        Some((ProfileScope::Owner(id), _)) => Some(id),
        Some((ProfileScope::Household, _)) => authority.sole_member().await,
        _ => None,
    }
}

async fn household_zone() -> Tz {
    match crate::schedule::scheduler_and_settings() {
        Some((_, settings)) => settings
            .get()
            .await
            .map_or_else(|_| zone(""), |s| zone(&s.timezone)),
        None => zone(""),
    }
}

const LOCAL_FORMATS: [&str; 4] = [
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%d %H:%M",
];

/// `when` as the model wrote it: RFC 3339, else a local date and time in the member's timezone,
/// else words ("18:00", "Friday", "tomorrow at 9") read as a person's.
fn moment(raw: &str, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Some(t.with_timezone(&Utc));
    }
    for format in LOCAL_FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, format) {
            return local_instant(tz, naive.date(), naive.time());
        }
    }
    due_from_words(raw, now, tz)
}

/// The clock time written in `text`, as written: "18:00" in "2026-11-09T18:00:00+00:00", "6pm"
/// in "every weekday at 6pm". The offset is ignored: a repeat is kept in member time.
fn clock_in(text: &str) -> Option<NaiveTime> {
    if let Some((_, after)) = text.split_once('T') {
        if let Some(t) = after.get(..5).and_then(parse_clock) {
            return Some(t);
        }
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    words.iter().enumerate().find_map(|(i, w)| {
        parse_clock(w).or_else(|| {
            let next = words.get(i + 1)?;
            matches!(*next, "am" | "pm").then(|| parse_clock(&format!("{w}{next}")))?
        })
    })
}

/// A repeat as the model sent it: the object the schema asks for, or the words ("every
/// weekday"), with its time taken from `when` when it gave none.
fn repeat_spec(repeat: &Value, when: Option<&str>) -> Result<Value, String> {
    let (every, at, days) = match repeat {
        Value::Object(fields) => {
            let text = |k: &str| fields.get(k).and_then(Value::as_str).map(str::to_string);
            (
                text("every"),
                text("at"),
                fields.get("days").cloned().unwrap_or(Value::Null),
            )
        }
        Value::String(words) => {
            let lower = words.to_lowercase();
            let named: Vec<u32> = [
                "monday",
                "tuesday",
                "wednesday",
                "thursday",
                "friday",
                "saturday",
                "sunday",
            ]
            .iter()
            .zip(1u32..)
            .filter(|(day, _)| lower.contains(*day))
            .map(|(_, n)| n)
            .collect();
            let every = if lower.contains("weekday") {
                "weekday"
            } else if !named.is_empty() || lower.contains("week") {
                "week"
            } else if lower.contains("month") {
                "month"
            } else if lower.contains("day") || lower.contains("daily") || lower.contains("night") {
                "day"
            } else {
                return Err(format!(
                    "`repeat` \"{words}\" names no repeat. Send {{\"every\": \"day\"|\"weekday\"|\
                     \"week\"|\"month\", \"at\": \"HH:MM\", \"days\": [..] or null}}."
                ));
            };
            (
                Some(every.to_string()),
                clock_in(words).map(|t| t.format("%H:%M").to_string()),
                if named.is_empty() {
                    Value::Null
                } else {
                    json!(named)
                },
            )
        }
        _ => return Err("`repeat` must be an object".to_string()),
    };
    let at = at.filter(|a| parse_clock(a).is_some()).or_else(|| {
        when.and_then(clock_in)
            .map(|t| t.format("%H:%M").to_string())
    });
    let Some(at) = at else {
        return Err(
            "A repeat needs its time of day: `repeat.at` as \"HH:MM\". Ask them what time if \
             they did not say."
                .to_string(),
        );
    };
    Ok(json!({"every": every, "at": at, "days": days}))
}

/// The place a member named: one of theirs by its label, else home as the Pond sees it.
fn place_hook(said: &str, transition: Transition, places: &[Place]) -> Result<Value, String> {
    let wanted = said.trim().trim_start_matches("the ").trim().to_lowercase();
    if let Some(place) = places
        .iter()
        .find(|p| p.label.trim().to_lowercase() == wanted)
        .or_else(|| {
            places
                .iter()
                .find(|p| p.label.to_lowercase().contains(&wanted) && !wanted.is_empty())
        })
    {
        return Ok(
            json!({"kind": "place", "spec": {"place_id": place.id, "transition": transition.as_str()}}),
        );
    }
    if matches!(
        wanted.as_str(),
        "home" | "house" | "my house" | "my home" | "the house" | "nyumbani"
    ) {
        return Ok(
            json!({"kind": "event", "spec": {"source_id": PRESENCE_HOME, "event": transition.as_str()}}),
        );
    }
    let known: Vec<&str> = places.iter().map(|p| p.label.as_str()).collect();
    Err(if known.is_empty() {
        format!(
            "They have no place called \"{said}\" yet. Places are added on their phone in My \
             Goose; until then offer a time instead. \"home\" works without one."
        )
    } else {
        format!(
            "They have no place called \"{said}\". Their places: {}. \"home\" also works.",
            known.join(", ")
        )
    })
}

async fn add(meta: &Meta, p: AddTodoParams) -> String {
    let what = "adding that to their to-dos";
    let Some(repo) = TODO_DEPS.get().cloned() else {
        return format_dead_end(what, "To-dos are not set up on this Pond yet.");
    };
    let Some(owner) = speaker(meta).await else {
        return format_dead_end(
            what,
            "Only a household member the Pond knows keeps a to-do list. Ask them who they are.",
        );
    };
    let text = p.text.trim();
    if text.is_empty() {
        return format_dead_end(what, "Give `text`: what to do, in their words.");
    }
    let (now, tz) = (Utc::now(), household_zone().await);
    let mut hooks: Vec<Value> = Vec::new();
    let mut due_at: Option<DateTime<Utc>> = None;
    let repeat = p.repeat.as_ref().filter(|r| !r.is_null());
    let when = p.when.as_deref().filter(|w| !w.trim().is_empty());

    // With a repeat, `when` is only the repeat's time of day.
    if let Some(raw) = when.filter(|_| repeat.is_none()) {
        let Some(at) = moment(raw, now, tz) else {
            return format_dead_end(
                what,
                &format!(
                    "`when` \"{raw}\" is not a time. Send RFC 3339 with the offset, worked out \
                     from the turn's current date and time, e.g. \"2026-11-10T18:00:00+03:00\"."
                ),
            );
        };
        if at < now - Duration::minutes(PAST_SLACK_MINUTES) {
            let local = now.with_timezone(&tz).format("%A %-d %B %Y %H:%M");
            return format_dead_end(
                what,
                &format!(
                    "`when` {} has already passed: it is now {local} there. Work the time out \
                     again from the current date.",
                    rfc3339(at)
                ),
            );
        }
        due_at = Some(at);
        hooks.push(json!({"kind": "time", "spec": {"at": rfc3339(at)}}));
    }
    if let Some(repeat) = repeat {
        match repeat_spec(repeat, when) {
            Ok(spec) => hooks.push(json!({"kind": "repeat", "spec": spec})),
            Err(why) => return format_dead_end(what, &why),
        }
    }
    let places = match repo.places(&owner).await {
        Ok(places) => places,
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            return format_dead_end(what, "The Pond could not read their places just now.");
        }
    };
    if let Some(said) = p.place.as_deref().filter(|s| !s.trim().is_empty()) {
        let transition = p
            .place_event
            .as_deref()
            .and_then(Transition::parse)
            .unwrap_or(Transition::Arrive);
        match place_hook(said, transition, &places) {
            Ok(hook) => hooks.push(hook),
            Err(why) => return format_dead_end(what, &why),
        }
    }
    let kind = p
        .kind
        .as_deref()
        .filter(|k| !k.trim().is_empty())
        .unwrap_or("todo");
    if kind.eq_ignore_ascii_case("reminder") && hooks.is_empty() {
        return format_dead_end(
            what,
            "A reminder needs a moment to remind them: `when`, `repeat` or `place`. Ask them when.",
        );
    }
    let body = json!({
        "kind": kind,
        "text": text,
        "due_at": due_at.map(rfc3339),
        "ask": p.ask,
        "hooks": hooks,
    });
    let catalog = HookCatalog {
        places,
        sources: vec![HookSource::presence()],
    };
    let todo = match new_todo(&owner, &body, &catalog, TodoSource::Chat, None, now, tz) {
        Ok(todo) => todo,
        Err(HookError(why)) => return format_dead_end(what, &why),
    };
    if let Err(e) = repo.create(&todo).await {
        tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
        return format_dead_end(
            what,
            "The Pond could not save it just now; try again shortly.",
        );
    }
    tracing::info!(target: "giap::trace", kind = "todo_added", hooks = todo.hooks.len());
    let fires: Vec<&str> = todo.hooks.iter().map(|h| h.label.as_str()).collect();
    let when = if fires.is_empty() {
        "It has no time or place, so it waits on their list.".to_string()
    } else {
        format!("Their phone gets it: {}.", fires.join("; "))
    };
    format!(
        "Added to their {}: \"{}\". {when} Tell them in one short sentence.",
        if todo.kind.as_str() == "reminder" {
            "reminders"
        } else {
            "to-dos"
        },
        todo.text
    )
}

fn line(todo: &Todo) -> String {
    let fires: Vec<&str> = todo.hooks.iter().map(|h| h.label.as_str()).collect();
    let mut out = format!("- [{}] {} ({})", todo.id, todo.text, todo.kind.as_str());
    if !fires.is_empty() {
        out.push_str(&format!(": {}", fires.join("; ")));
    } else if let Some(due) = todo.due_at {
        out.push_str(&format!(": due {}", rfc3339(due)));
    }
    if todo.state != TodoState::Open {
        out.push_str(&format!(" [{}]", todo.state.as_str()));
    }
    out
}

async fn list(meta: &Meta, p: ListTodosParams) -> String {
    let what = "reading their to-dos";
    let Some(repo) = TODO_DEPS.get().cloned() else {
        return format_dead_end(what, "To-dos are not set up on this Pond yet.");
    };
    let Some(owner) = speaker(meta).await else {
        return format_dead_end(
            what,
            "Only a household member the Pond knows keeps a to-do list.",
        );
    };
    let filter = StateFilter::parse(p.state.as_deref()).unwrap_or(StateFilter::Open);
    match repo.list(&owner, filter).await {
        Ok(todos) if todos.is_empty() => "Their list is empty.".to_string(),
        Ok(todos) => {
            let lines: Vec<String> = todos.iter().take(LISTED).map(line).collect();
            format!("Their to-dos and reminders:\n{}", lines.join("\n"))
        }
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            format_dead_end(what, "The Pond could not read their list just now.")
        }
    }
}

async fn complete(meta: &Meta, p: CompleteTodoParams) -> String {
    let what = "ticking that off";
    let Some(repo) = TODO_DEPS.get().cloned() else {
        return format_dead_end(what, "To-dos are not set up on this Pond yet.");
    };
    let Some(owner) = speaker(meta).await else {
        return format_dead_end(
            what,
            "Only a household member the Pond knows keeps a to-do list.",
        );
    };
    let open = match repo.list(&owner, StateFilter::Open).await {
        Ok(open) => open,
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            return format_dead_end(what, "The Pond could not read their list just now.");
        }
    };
    let by_id =
        p.id.as_deref()
            .map(str::trim)
            .and_then(|id| open.iter().find(|t| t.id == id));
    let found: Vec<&Todo> = match (by_id, p.text.as_deref().map(str::trim)) {
        (Some(todo), _) => vec![todo],
        (None, Some(words)) if !words.is_empty() => {
            let words = words.to_lowercase();
            open.iter()
                .filter(|t| t.text.to_lowercase().contains(&words))
                .collect()
        }
        _ => Vec::new(),
    };
    let todo = match found.as_slice() {
        [one] => (*one).clone(),
        [] => {
            return format_dead_end(
                what,
                "No open to-do of theirs matches. Call list_todos and pass the `id`.",
            )
        }
        many => {
            let lines: Vec<String> = many.iter().map(|t| line(t)).collect();
            return format_dead_end(
                what,
                &format!(
                    "Several open to-dos match; pass the `id` of the one they mean:\n{}",
                    lines.join("\n")
                ),
            );
        }
    };
    let mut done = todo;
    let tz = household_zone().await;
    if done.answer(Answer::Done, None, Utc::now(), tz).is_err() {
        return format_dead_end(what, "That to-do is already closed.");
    }
    match repo.update(&done, false).await {
        Ok(()) => format!(
            "Marked done: \"{}\". Goose will not ask about it again.",
            done.text
        ),
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            format_dead_end(what, "The Pond could not save that just now.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn when_is_read_as_rfc3339_or_as_member_local_time() {
        let at = |raw: &str| moment(raw, now(), NAIROBI).map(rfc3339);
        assert_eq!(
            at("2026-11-10T18:00:00+03:00").as_deref(),
            Some("2026-11-10T15:00:00Z")
        );
        assert_eq!(
            at("2026-11-10T18:00").as_deref(),
            Some("2026-11-10T15:00:00Z")
        );
        assert_eq!(at("18:00").as_deref(), Some("2026-11-09T15:00:00Z"));
        // 09:00 has passed at 10:00 Nairobi, so tomorrow's.
        assert_eq!(at("9am").as_deref(), Some("2026-11-10T06:00:00Z"));
        // Gemma 4 sent these for "on Friday" and "tomorrow at 9".
        assert_eq!(at("Friday").as_deref(), Some("2026-11-13T06:00:00Z"));
        assert_eq!(
            at("2026-11-10T09:00:00").as_deref(),
            Some("2026-11-10T06:00:00Z")
        );
        assert_eq!(at("after the rains"), None);
    }

    #[test]
    fn a_repeat_is_read_from_the_object_or_the_words_gemma_sends() {
        // Gemma 4's own call for "every weekday at 6pm remind me to stretch".
        assert_eq!(
            repeat_spec(&json!("every weekday"), Some("2026-11-09T18:00:00+00:00")).unwrap(),
            json!({"every": "weekday", "at": "18:00", "days": null})
        );
        assert_eq!(
            repeat_spec(&json!("every weekday"), Some("18:00")).unwrap(),
            json!({"every": "weekday", "at": "18:00", "days": null})
        );
        assert_eq!(
            repeat_spec(&json!("every Monday and Thursday at 7 am"), None).unwrap(),
            json!({"every": "week", "at": "07:00", "days": [1, 4]})
        );
        assert_eq!(
            repeat_spec(&json!({"every": "month", "at": "09:00", "days": [1]}), None).unwrap(),
            json!({"every": "month", "at": "09:00", "days": [1]})
        );
        assert!(repeat_spec(&json!({"every": "day"}), None)
            .unwrap_err()
            .contains("time of day"));
        assert!(repeat_spec(&json!("fortnightly"), Some("09:00"))
            .unwrap_err()
            .contains("names no repeat"));
    }

    #[test]
    fn a_named_place_is_the_members_own_else_home_is_presence() {
        let home = Place::new("jerry", "Gym", -1.3, 36.8, None, now()).unwrap();
        let places = vec![home.clone()];
        assert_eq!(
            place_hook("the gym", Transition::Arrive, &places).unwrap()["spec"]["place_id"],
            json!(home.id)
        );
        assert_eq!(
            place_hook("home", Transition::Leave, &places).unwrap(),
            json!({"kind": "event", "spec": {"source_id": "presence:home", "event": "leave"}})
        );
        let why = place_hook("the office", Transition::Arrive, &places).unwrap_err();
        assert!(why.contains("Gym"), "{why}");
    }

    #[test]
    fn the_tool_block_carries_the_three_todo_tools() {
        let names: Vec<String> = ScheduleMcpServer::tool_defs()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for tool in ["add_todo", "list_todos", "complete_todo", "set_timer"] {
            assert!(
                names.iter().any(|n| n == tool),
                "{tool} missing from {names:?}"
            );
        }
    }
}
