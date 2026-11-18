//! `add_reminder`, `list_todos` and `complete_todo` for `giap-schedule`: the speaker's own to-dos and
//! reminders in My Goose, which the Pond pushes to their phone at a time, a repeat or a place.

use std::sync::{Arc, OnceLock};

use chrono::{DateTime, Duration, NaiveDateTime, NaiveTime, Utc};
use pond_core::shared::domain::todo::{
    clock_label, local_instant, new_todo, parse_clock, rfc3339, zone, Answer, HookCatalog,
    HookError, HookSource, HookSpec, Place, Priority, StateFilter, Todo, TodoKind, TodoSource,
    TodoState, Transition, Tz, MAX_LIST_CHARS, MAX_STEPS, MAX_STEP_CHARS, PRESENCE_HOME,
};
use pond_core::shared::domain::when_repeats::{repeat_from_words, RepeatUnclear};
use pond_core::shared::domain::when_said::due_from_words;
use pond_core::shared::ports::todo::TodoRepository;
use pond_core::shared::services::todo_add::{add_once, AddedTodo};
use pond_core::user_data::domain::message_note::Receipt;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::prompt_template::PromptTemplateRepository;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ErrorData, RequestMetaObject},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::format::format_not_done;
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

static HEARTBEAT_DOCS: OnceLock<Arc<dyn PromptTemplateRepository>> = OnceLock::new();

/// Where the heartbeat's checklist is kept, for `add_to_heartbeat`.
pub fn init_heartbeat_docs(documents: Arc<dyn PromptTemplateRepository>) {
    let _ = HEARTBEAT_DOCS.set(documents);
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AddTodoParams {
    /// What to do, as a full action in the person's words, without the time or place:
    /// "Watch Avengers: Doomsday", not "Doomsday".
    #[serde(default)]
    pub text: String,
    /// "reminder" when they said "remind me"; "todo" when they asked to add a task to their
    /// to-dos. Null: "todo".
    pub kind: Option<String>,
    /// When, as they said it: "tomorrow at 6pm", "Friday 9am", "in 2 hours", "19 November at
    /// 10:00"; or a repeat: "every weekday at 18:00". Null when they gave no time.
    pub when: Option<String>,
    #[schemars(skip)]
    pub repeat: Option<Value>,
    /// A place they named for it, as they named it: "home", "work", "the gym". Null when they
    /// named no place.
    pub place: Option<String>,
    /// "arrive" for "when I get to" a place, "leave" for "when I leave" it. Null: "arrive".
    pub place_event: Option<String>,
    /// true to have Goose ask afterwards whether it was done. Null: true for a todo, false for a
    /// reminder.
    pub ask: Option<bool>,
    /// The list they named for it: "Work", "Personal". Null when they named none.
    pub list: Option<String>,
    /// "high" when they call it urgent, "low" when it can wait. Null: normal.
    pub priority: Option<String>,
    /// Its steps when they listed them, each a short action. Null when they gave none.
    #[schemars(with = "Option<Vec<String>>")]
    pub steps: Option<Value>,
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
Remind the person of something, or put it on their to-do list in My Goose: their phone gets it \
at a time, on a repeat, or at a place. For \"remind me to call Emmanuel when I get home\", \
\"remind me on Friday at 9 to call the garage\", \"every weekday at 18:00 remind me to \
stretch\", \"add renew the logbook to my to-dos\". Any reminder goes here, not to set_timer \
(a countdown) or create_schedule (work Goose does by itself).")]
    async fn add_reminder(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<AddTodoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("add_reminder");
        Ok(add(&ctx.meta, params.0).await.into_result())
    }

    #[tool(description = "\
Add something for Goose to check by itself each time it looks in on the house: \"keep \
checking whether the dollar is under 130\", \"add this to your heartbeat\". `check` is the \
question to ask each time. For one page use web_watch; for a time or place, add_reminder.")]
    async fn add_to_heartbeat(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<AddToHeartbeatParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("add_to_heartbeat");
        Ok(add_to_heartbeat(params.0).await)
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

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AddToHeartbeatParams {
    /// What to check each time, as one question.
    #[serde(default)]
    pub check: String,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

async fn add_to_heartbeat(p: AddToHeartbeatParams) -> CallToolResult {
    use pond_core::prompts::{
        add_heartbeat_item, HeartbeatItemRefused, HEARTBEAT, PROMPT_HEARTBEAT,
    };
    let what = "adding that to the heartbeat";
    let asked = match p.check.trim() {
        "" => ["item", "text", "question", "task"]
            .iter()
            .find_map(|k| p.extra.get(*k).and_then(Value::as_str))
            .unwrap_or_default()
            .trim()
            .to_string(),
        check => check.to_string(),
    };
    let refuse = |said: &str, why: &str| {
        crate::format::with_receipt(
            format_not_done(what, why),
            Receipt::not_set("standing", asked.clone(), said),
        )
    };
    let Some(documents) = HEARTBEAT_DOCS.get().cloned() else {
        return refuse(
            "The heartbeat is not set up on this Pond yet.",
            "The heartbeat's checklist is not available on this Pond.",
        );
    };
    let existing = documents.get(HEARTBEAT).await.ok().flatten();
    let checklist = existing
        .as_ref()
        .map(|t| t.content.clone())
        .unwrap_or_else(|| PROMPT_HEARTBEAT.to_string());
    let added = match add_heartbeat_item(&checklist, &asked) {
        Ok(added) => added,
        Err(HeartbeatItemRefused::Empty) => {
            return refuse(
                "I could not tell what to keep an eye on.",
                "Send `check`: what Goose should ask itself each time, in one line.",
            )
        }
        Err(HeartbeatItemRefused::AlreadyThere) => {
            return crate::format::with_receipt(
                "That is already on the heartbeat's checklist. Tell them it already is.",
                Receipt::set("standing", asked, None),
            )
        }
        Err(HeartbeatItemRefused::Full) => {
            return refuse(
                "The heartbeat's list is full. Remove something from it in My Goose first.",
                "The checklist is at its longest. Tell them to remove an item in My Goose, \
                 under Heartbeat, and ask again.",
            )
        }
    };
    let document = pond_core::user_data::domain::prompt_template::PromptTemplate {
        name: HEARTBEAT.to_string(),
        content: added,
        description: existing
            .as_ref()
            .map(|t| t.description.clone())
            .unwrap_or_default(),
        is_system: existing.as_ref().is_some_and(|t| t.is_system),
        is_customized: true,
        factory_version: existing.as_ref().map(|t| t.factory_version).unwrap_or(0),
        updated_at: Utc::now().to_rfc3339(),
    };
    if let Err(e) = documents.upsert(&document).await {
        tracing::warn!(target: "giap::trace", kind = "heartbeat_item_failed", error = %e);
        return refuse(
            "The Pond could not save it. Ask again in a moment.",
            "The Pond could not save it just now; try again shortly.",
        );
    }
    tracing::info!(target: "giap::trace", kind = "heartbeat_item_added");
    crate::format::with_receipt(
        format!(
            "Added to the heartbeat's checklist: \"{asked}\". Goose looks at it each time it \
             checks in on the house by itself, about once an hour, and tells them when it \
             needs them. Tell them in one short sentence."
        ),
        Receipt::set("standing", asked, None),
    )
}

/// The household member speaking in this call: the turn's owner, else a one-member household's
/// only member. A guest keeps no list.
pub(crate) async fn speaker(meta: &RequestMetaObject) -> Option<String> {
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

pub(crate) async fn household_zone() -> Tz {
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
            } else if lower
                .split(|c: char| !c.is_alphabetic())
                .any(|w| matches!(w, "day" | "days" | "daily" | "night" | "nightly"))
            {
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

/// How `add` says no: `why` is advice for the model; `said` is the short reason the person is
/// given.
fn refusal(p: &AddTodoParams) -> impl Fn(&str, &str) -> Added {
    let asked = p.text.trim().to_string();
    let listed_as = match p.kind.as_deref() {
        Some(kind) if kind.eq_ignore_ascii_case("reminder") => "reminder",
        _ => "todo",
    };
    move |said, why| Added {
        text: format_not_done("adding that to their to-dos", why),
        receipt: Receipt::not_set(listed_as, asked.clone(), said),
    }
}

fn clipped(text: &str, max: usize) -> String {
    text.trim().chars().take(max).collect()
}

/// Steps as the model sent them: strings, or objects with `text`; anything else is no step.
fn steps_from(sent: Option<&Value>) -> Vec<Value> {
    let items = match sent {
        Some(Value::Array(items)) => items.as_slice(),
        Some(one @ Value::String(_)) => std::slice::from_ref(one),
        _ => &[],
    };
    items
        .iter()
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("text").and_then(Value::as_str))
        })
        .map(|text| clipped(text, MAX_STEP_CHARS))
        .filter(|text| !text.is_empty())
        .take(MAX_STEPS)
        .map(|text| json!({ "text": text }))
        .collect()
}

pub(crate) async fn add(meta: &RequestMetaObject, p: AddTodoParams) -> Added {
    let refuse = refusal(&p);
    let Some(repo) = TODO_DEPS.get().cloned() else {
        return refuse(
            "Reminders are not set up on this Pond yet.",
            "To-dos are not set up on this Pond yet.",
        );
    };
    let Some(owner) = speaker(meta).await else {
        return refuse(
            "I do not know who is speaking, so there is no list to put it on.",
            "Only a household member the Pond knows keeps a to-do list. Ask them who they are.",
        );
    };
    add_for(repo.as_ref(), &owner, p, Utc::now(), household_zone().await).await
}

/// `add` for a member the Pond knows, at `now` in their timezone.
async fn add_for(
    repo: &dyn TodoRepository,
    owner: &str,
    p: AddTodoParams,
    now: DateTime<Utc>,
    tz: Tz,
) -> Added {
    let refuse = refusal(&p);
    let text = p.text.trim();
    if text.is_empty() {
        return refuse(
            "I could not tell what it is for.",
            "Give `text`: what to do, in their words.",
        );
    }
    let mut hooks: Vec<Value> = Vec::new();
    let mut due_at: Option<DateTime<Utc>> = None;
    let repeat = p.repeat.as_ref().filter(|r| !r.is_null());
    let when = p.when.as_deref().filter(|w| !w.trim().is_empty());

    // With a repeat sent on its own, `when` is only the repeat's time of day.
    if let Some(raw) = when.filter(|_| repeat.is_none()) {
        let today = now.with_timezone(&tz).date_naive();
        match repeat_from_words(raw, today) {
            Ok(found) => {
                match found.as_todo_repeat() {
                    Some((every, at, days)) => hooks.push(json!({"kind": "repeat", "spec": {
                        "every": every.as_str(),
                        "at": at.format("%H:%M").to_string(),
                        "days": if days.is_empty() { Value::Null } else { json!(days) },
                    }})),
                    None => return refuse(
                        "A reminder repeats by the day, the week or the month, not within a day.",
                        "A to-do repeats by the day, the week or the month, not within a day. \
                         For something every few minutes or hours use create_schedule.",
                    ),
                }
            }
            Err(RepeatUnclear::NoCadence) => {
                let Some(at) = moment(raw, now, tz) else {
                    return refuse(
                        "I could not place that time.",
                        &format!(
                            "`when` \"{raw}\" is not a time the Pond can place. Say it plainly: \
                             \"tomorrow at 6pm\", \"Friday 9am\", \"in 2 hours\", \"19 November \
                             at 10:00\". Ask them when if they did not say."
                        ),
                    );
                };
                if at < now - Duration::minutes(PAST_SLACK_MINUTES) {
                    let local = now.with_timezone(&tz).format("%A %-d %B %Y %H:%M");
                    return refuse(
                        "That time has already passed.",
                        &format!(
                            "`when` \"{raw}\" has already passed: it is now {local} there. Ask \
                             them which day they mean."
                        ),
                    );
                }
                due_at = Some(at);
                hooks.push(json!({"kind": "time", "spec": {"at": rfc3339(at)}}));
            }
            Err(unclear) => {
                return refuse(
                    "I could not tell how often it should repeat.",
                    unclear.advice(),
                )
            }
        }
    }
    if let Some(repeat) = repeat {
        match repeat_spec(repeat, when) {
            Ok(spec) => hooks.push(json!({"kind": "repeat", "spec": spec})),
            Err(why) => return refuse("I could not tell how often it should repeat.", &why),
        }
    }
    let places = match repo.places(owner).await {
        Ok(places) => places,
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            return refuse(
                "The Pond could not read your places just now.",
                "The Pond could not read their places just now.",
            );
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
            Err(why) => return refuse("I do not know that place.", &why),
        }
    }
    let kind = p
        .kind
        .as_deref()
        .filter(|k| !k.trim().is_empty())
        .unwrap_or("todo");
    if kind.eq_ignore_ascii_case("reminder") && hooks.is_empty() {
        return refuse(
            "A reminder needs a time or a place.",
            "A reminder needs a moment to remind them: `when` or `place`. Ask them when.",
        );
    }
    let body = json!({
        "kind": kind,
        "text": text,
        "due_at": due_at.map(rfc3339),
        "ask": p.ask,
        "hooks": hooks,
        "list": p.list.as_deref().map(|list| clipped(list, MAX_LIST_CHARS)),
        // A priority the model made up is no priority; it must not cost them the to-do.
        "priority": p.priority.as_deref().and_then(Priority::parse).map(Priority::as_str),
        "steps": steps_from(p.steps.as_ref()),
    });
    let catalog = HookCatalog {
        places,
        sources: vec![HookSource::presence()],
    };
    let todo = match new_todo(owner, &body, &catalog, TodoSource::Chat, None, now, tz) {
        Ok(todo) => todo,
        Err(HookError(why)) => return refuse("I could not make a reminder out of that.", &why),
    };
    let added = match add_once(repo, todo).await {
        Ok(added) => added,
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            return refuse(
                "The Pond could not save it. Ask again in a moment.",
                "The Pond could not save it just now; try again shortly.",
            );
        }
    };
    let todo = added.todo();
    let fires: Vec<&str> = todo.hooks.iter().map(|h| h.label.as_str()).collect();
    let when = match (fires.as_slice(), todo.due_at) {
        ([], Some(due)) => format!(
            "It is due: {}.",
            clock_label(&HookSpec::Time { at: due }, now, tz)
        ),
        ([], None) => "It has no time or place, so it waits on their list.".to_string(),
        _ => format!("Their phone gets it: {}.", fires.join("; ")),
    };
    let named = match extras(todo).as_slice() {
        [] => format!("\"{}\"", todo.text),
        extras => format!("\"{}\" ({})", todo.text, extras.join(", ")),
    };
    let text = match &added {
        AddedTodo::New(_) => {
            tracing::info!(target: "giap::trace", kind = "todo_added", hooks = todo.hooks.len());
            format!(
                "Added to their {}: {named}. {when} Tell them in one short sentence.",
                if todo.kind == TodoKind::Reminder {
                    "reminders"
                } else {
                    "to-dos"
                },
            )
        }
        AddedTodo::Listed { moved: false, .. } => {
            tracing::info!(target: "giap::trace", kind = "todo_already_added");
            format!(
                "That is already on their list: {named}. {when} Nothing was added. Tell them \
                 it is already there."
            )
        }
        AddedTodo::Listed { moved: true, .. } => {
            tracing::info!(target: "giap::trace", kind = "todo_set_again");
            format!(
                "That was already on their list: {named}. No second one was added; it now has \
                 the time or place they just gave. {when} Tell them in one short sentence."
            )
        }
    };
    let kept = if fires.is_empty() {
        todo.text.clone()
    } else {
        format!("{}: {}", todo.text, fires.join("; "))
    };
    Added {
        text,
        receipt: Receipt::set(todo.kind.as_str(), kept, Some(todo.id.clone())),
    }
}

/// What `add` did: its words for the model and the receipt for the person.
pub(crate) struct Added {
    pub text: String,
    pub receipt: Receipt,
}

impl Added {
    pub(crate) fn into_result(self) -> CallToolResult {
        crate::format::with_receipt(self.text, self.receipt)
    }
}

/// What a to-do carries beyond its words: its list, a priority that is not normal, its steps.
fn extras(todo: &Todo) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(list) = &todo.list {
        out.push(format!("list: {list}"));
    }
    if todo.priority != Priority::Normal {
        out.push(format!("{} priority", todo.priority.as_str()));
    }
    if !todo.steps.is_empty() {
        let done = todo.steps.iter().filter(|step| step.done).count();
        out.push(format!("{done} of {} steps done", todo.steps.len()));
    }
    out
}

fn line(todo: &Todo) -> String {
    let fires: Vec<&str> = todo.hooks.iter().map(|h| h.label.as_str()).collect();
    let mut tags = vec![todo.kind.as_str().to_string()];
    tags.extend(extras(todo));
    let mut out = format!("- [{}] {} ({})", todo.id, todo.text, tags.join(", "));
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

async fn list(meta: &RequestMetaObject, p: ListTodosParams) -> String {
    let what = "reading their to-dos";
    let Some(repo) = TODO_DEPS.get().cloned() else {
        return format_not_done(what, "To-dos are not set up on this Pond yet.");
    };
    let Some(owner) = speaker(meta).await else {
        return format_not_done(
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
            format_not_done(what, "The Pond could not read their list just now.")
        }
    }
}

async fn complete(meta: &RequestMetaObject, p: CompleteTodoParams) -> String {
    let what = "ticking that off";
    let Some(repo) = TODO_DEPS.get().cloned() else {
        return format_not_done(what, "To-dos are not set up on this Pond yet.");
    };
    let Some(owner) = speaker(meta).await else {
        return format_not_done(
            what,
            "Only a household member the Pond knows keeps a to-do list.",
        );
    };
    let open = match repo.list(&owner, StateFilter::Open).await {
        Ok(open) => open,
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            return format_not_done(what, "The Pond could not read their list just now.");
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
            return format_not_done(
                what,
                "No open to-do of theirs matches. Call list_todos and pass the `id`.",
            )
        }
        many => {
            let lines: Vec<String> = many.iter().map(|t| line(t)).collect();
            return format_not_done(
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
        return format_not_done(what, "That to-do is already closed.");
    }
    match repo.update(&done, false).await {
        Ok(()) => format!(
            "Marked done: \"{}\". Goose will not ask about it again.",
            done.text
        ),
        Err(e) => {
            tracing::warn!(target: "giap::trace", kind = "todo_tool_failed", error = %e);
            format_not_done(what, "The Pond could not save that just now.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::shared::mocks::mock_todo::MockTodoRepository;

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

    fn asks(text: &str) -> AddTodoParams {
        AddTodoParams {
            text: text.into(),
            ..Default::default()
        }
    }

    async fn open(repo: &MockTodoRepository, owner: &str) -> Vec<Todo> {
        repo.list(owner, StateFilter::Open).await.unwrap()
    }

    #[tokio::test]
    async fn a_second_ask_for_what_is_open_on_their_list_adds_nothing_and_says_so() {
        let repo = MockTodoRepository::new();
        let first = add_for(&repo, "jerry", asks("Call the garage"), now(), NAIROBI).await;
        assert!(
            first
                .text
                .starts_with("Added to their to-dos: \"Call the garage\"."),
            "{}",
            first.text
        );
        let id = open(&repo, "jerry").await[0].id.clone();

        let again = add_for(&repo, "jerry", asks(" call the GARAGE! "), now(), NAIROBI).await;
        assert!(
            again
                .text
                .starts_with("That is already on their list: \"Call the garage\"."),
            "{}",
            again.text
        );
        assert!(again.text.contains("Nothing was added"), "{}", again.text);
        assert!(!again.text.contains(&id), "no id in the reply");

        let at_home = AddTodoParams {
            kind: Some("reminder".into()),
            place: Some("home".into()),
            ..asks("Call the garage")
        };
        let moved = add_for(&repo, "jerry", at_home, now(), NAIROBI).await;
        assert!(
            moved.text.contains("No second one was added")
                && moved.text.contains("When you get home"),
            "it had no time or place, so it takes this one: {}",
            moved.text
        );

        let timed = AddTodoParams {
            when: Some("tomorrow at 6pm".into()),
            ..asks("call the garage")
        };
        let waiting = add_for(&repo, "jerry", timed, now(), NAIROBI).await;
        assert!(
            waiting.text.contains("Nothing was added")
                && waiting.text.contains("When you get home"),
            "what fires the one they have: {}",
            waiting.text
        );
        let kept = open(&repo, "jerry").await;
        assert_eq!(
            kept.len(),
            1,
            "a reminder and a to-do in the same words are one"
        );
        assert_eq!((kept[0].id.as_str(), kept[0].hooks.len()), (id.as_str(), 1));

        let theirs = add_for(&repo, "liz", asks("Call the garage"), now(), NAIROBI).await;
        assert!(theirs.text.starts_with("Added"), "{}", theirs.text);

        let mut done = kept[0].clone();
        done.state = TodoState::Done;
        repo.update(&done, false).await.unwrap();
        let afresh = add_for(&repo, "jerry", asks("Call the garage"), now(), NAIROBI).await;
        assert!(
            afresh.text.starts_with("Added"),
            "a done to-do does not block a new one: {}",
            afresh.text
        );
        assert_eq!(repo.list("jerry", StateFilter::All).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_reminder_that_already_fired_is_set_again_not_doubled() {
        let repo = MockTodoRepository::new();
        let remind = |when: &str| AddTodoParams {
            kind: Some("reminder".into()),
            when: Some(when.into()),
            ..asks("Check the oven")
        };
        add_for(&repo, "jerry", remind("18:00"), now(), NAIROBI).await;
        let first = open(&repo, "jerry").await.remove(0);
        let fired = first.due_at.unwrap();
        repo.hook_fired(&first.hooks[0].id, fired, None)
            .await
            .unwrap();

        let again = add_for(&repo, "jerry", remind("19:00"), fired, NAIROBI).await;
        assert!(
            again.text.contains("it now has the time or place"),
            "{}",
            again.text
        );
        let kept = open(&repo, "jerry").await;
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, first.id);
        assert_eq!(
            kept[0].due_at.map(rfc3339).as_deref(),
            Some("2026-11-09T16:00:00Z")
        );
    }

    #[tokio::test]
    async fn the_list_priority_and_steps_ride_the_add_and_show_in_the_list() {
        let repo = MockTodoRepository::new();
        let trip = AddTodoParams {
            list: Some(" Personal ".into()),
            priority: Some("High".into()),
            steps: Some(json!(["Book the train", {"text": " Pack "}, 7, "  "])),
            ..asks("Plan the Mombasa trip")
        };
        let added = add_for(&repo, "jerry", trip, now(), NAIROBI).await;
        assert!(
            added.text.starts_with(
                "Added to their to-dos: \"Plan the Mombasa trip\" (list: Personal, high \
                 priority, 0 of 2 steps done)."
            ),
            "{}",
            added.text
        );
        let mut kept = open(&repo, "jerry").await.remove(0);
        assert_eq!(
            (kept.list.as_deref(), kept.priority),
            (Some("Personal"), Priority::High)
        );
        assert_eq!(kept.steps[1].text, "Pack");
        kept.steps[0].done = true;
        assert_eq!(
            line(&kept),
            format!(
                "- [{}] Plan the Mombasa trip (todo, list: Personal, high priority, 1 of 2 \
                 steps done)",
                kept.id
            )
        );

        let loose = AddTodoParams {
            list: Some("x".repeat(60)),
            priority: Some("whenever".into()),
            steps: Some(json!(vec!["a step"; 20])),
            ..asks("Buy milk")
        };
        let added = add_for(&repo, "liz", loose, now(), NAIROBI).await;
        assert!(added.text.starts_with("Added"), "{}", added.text);
        let kept = open(&repo, "liz").await.remove(0);
        assert_eq!(
            kept.priority,
            Priority::Normal,
            "a made-up priority is none"
        );
        assert_eq!(
            (kept.list.unwrap().chars().count(), kept.steps.len()),
            (MAX_LIST_CHARS, MAX_STEPS)
        );

        add_for(&repo, "emmanuel", asks("Buy milk"), now(), NAIROBI).await;
        let plain = open(&repo, "emmanuel").await.remove(0);
        assert_eq!(line(&plain), format!("- [{}] Buy milk (todo)", plain.id));
    }

    #[test]
    fn add_reminder_asks_for_a_full_action_and_takes_a_list_priority_and_steps() {
        let tool = ScheduleMcpServer::tool_defs()
            .into_iter()
            .find(|t| t.name == "add_reminder")
            .unwrap();
        let fields = &tool.input_schema["properties"];
        for field in ["list", "priority", "steps"] {
            assert!(fields.get(field).is_some(), "{field} missing: {fields}");
        }
        let text = fields["text"]["description"].as_str().unwrap();
        assert!(
            text.contains("full action") && text.contains("\"Watch Avengers: Doomsday\""),
            "{text}"
        );
    }

    #[test]
    fn the_tool_block_carries_the_three_todo_tools() {
        let names: Vec<String> = ScheduleMcpServer::tool_defs()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        for tool in ["add_reminder", "list_todos", "complete_todo", "set_timer"] {
            assert!(
                names.iter().any(|n| n == tool),
                "{tool} missing from {names:?}"
            );
        }
    }
}
