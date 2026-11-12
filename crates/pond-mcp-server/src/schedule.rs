//! Schedule MCP server: repeating tasks, one-shot timers and world clock. Sensor rules live in
//! `giap-sensors`; `sensor_rule_summary` stays here because `list_schedules` renders them too.

use pond_core::shared::domain::when_repeats::{
    cron_in_words, looks_like_cron, repeat_from_words, six_field_cron, RepeatUnclear,
};
use pond_core::user_data::domain::schedule::{
    routine_summary, Schedule, SensorTriggerSpec, TaskKind, TriggerSourceKind, CRON_ONCE,
};
use pond_core::user_data::ports::scheduler::{
    CreateScheduleRequest, SchedulerPort, UpdateScheduleRequest,
};
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::location::zone_from_place;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────

/// Parameters for a one-shot timer.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SetTimerParams {
    /// How long from now, in natural language: "10 minutes", "1h30m", "90s".
    #[serde(default)]
    pub duration: String,
    /// What to say or do when it fires. Defaults to announcing the timer.
    #[serde(default)]
    pub prompt: String,
    /// Label shown in the schedule list. Defaults to the duration.
    #[serde(default)]
    pub name: String,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Parse a human duration ("10 min", "1h30m", "90 seconds"). Anything unreadable is `None`:
/// a timer set for the wrong moment is worse than a refused one.
pub fn parse_duration(text: &str) -> Option<chrono::Duration> {
    let lower = text.trim().to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let mut total = chrono::Duration::zero();
    let mut found = false;
    let bytes: Vec<char> = lower.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        // Other non-digits are skipped, but a skipped minus would turn "-5m" into five minutes.
        if i > 0 && bytes[i - 1] == '-' {
            return None;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let n: i64 = bytes[start..i].iter().collect::<String>().parse().ok()?;
        while i < bytes.len() && (bytes[i] == ' ' || bytes[i] == '-') {
            i += 1;
        }
        let unit_start = i;
        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let unit: String = bytes[unit_start..i].iter().collect();
        let seconds = match unit.as_str() {
            u if u.starts_with('h') => n * 3600,
            // "m" alone is minutes; "mo"/"month" is refused, not guessed.
            u if u.starts_with("min") || u == "m" => n * 60,
            u if u.starts_with('s') || u.is_empty() => n,
            _ => return None,
        };
        total = total + chrono::Duration::seconds(seconds);
        found = true;
    }
    if !found || total <= chrono::Duration::zero() {
        return None;
    }
    Some(total)
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScheduleIdParam {
    #[serde(default)]
    pub id: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScheduleActionParams {
    /// The schedule's name or id, from list_schedules.
    #[serde(default)]
    pub id: String,
    /// delete | pause | resume | run_now.
    #[serde(default)]
    pub action: String,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetScheduleRunsParams {
    #[serde(default)]
    pub id: String,
    /// Max runs to return (default 10).
    pub limit: Option<u32>,
}

/// Create-or-change. `id` decides which.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ScheduleUpsertParams {
    /// To change an existing schedule: its name or id. Omit to create a new one.
    #[serde(default)]
    pub id: String,
    pub name: Option<String>,
    /// How often and at what time, as a person says it: "every weekday at 7am", "every Monday
    /// and Thursday at 18:00", "every 2 hours", "on the 1st of every month at 9am".
    pub when: Option<String>,
    /// What Goose should do each time, written as a request to it.
    pub prompt: Option<String>,
    /// Only when they asked for another place's time: its city or zone. Default: their own.
    pub timezone: Option<String>,
    /// Catch-all for unexpected fields the model sends; an old `cron` arrives here.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WorldClockParams {
    /// Cities, countries or zones: "Tokyo", "New York", "UK". Default: the person's own.
    #[serde(alias = "timezones")]
    #[schemars(with = "Option<Vec<String>>")]
    pub places: Option<serde_json::Value>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// A schedule's time as a person reads it: the repeat in words, a one-shot or a rule as itself.
fn when_in_words(cron: &str) -> String {
    match cron {
        CRON_ONCE => "once".to_string(),
        "@event" => "when its rule matches".to_string(),
        cron => cron_in_words(cron),
    }
}

fn clipped(text: &str, max: usize) -> String {
    if text.chars().count() > max {
        let head: String = text.chars().take(max.saturating_sub(3)).collect();
        format!("{head}...")
    } else {
        text.to_string()
    }
}

/// The cron and the words for a repeat as it was sent: a person's words, or the cron
/// expression an older caller still sends.
fn repeat_said(said: &str, today: chrono::NaiveDate) -> Result<(String, String), RepeatUnclear> {
    if looks_like_cron(said) {
        let cron = six_field_cron(said);
        let words = cron_in_words(&cron);
        return Ok((cron, words));
    }
    repeat_from_words(said, today).map(|r| (r.to_cron(), r.in_words()))
}

fn kind_label(kind: &TaskKind) -> &'static str {
    match kind {
        TaskKind::AgentPrompt { .. } => "agent",
        TaskKind::Webhook { .. } => "webhook",
        TaskKind::SensorTrigger(_) => "sensor-rule",
        TaskKind::Routine { .. } => "routine",
    }
}

fn status_label(schedule: &Schedule) -> &'static str {
    if schedule.currently_running {
        "running"
    } else if schedule.paused {
        "paused"
    } else {
        "active"
    }
}

fn said(text: impl Into<String>) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![ContentBlock::text(
        text.into(),
    )]))
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ScheduleMcpServer {
    scheduler: Arc<dyn SchedulerPort>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

impl ScheduleMcpServer {
    async fn home_zone(&self) -> String {
        self.settings_repo
            .get()
            .await
            .map(|s| s.timezone.clone())
            .unwrap_or_else(|_| "UTC".to_string())
    }

    /// The schedule a caller means, by its id or by its name as the person would say it.
    async fn find(&self, said_as: &str) -> Result<Schedule, String> {
        let wanted = said_as.trim();
        let tasks = self
            .scheduler
            .list_tasks()
            .await
            .map_err(|e| format!("Could not read the schedules: {e}"))?;
        if let Some(task) = tasks.iter().find(|t| t.id == wanted) {
            return Ok(task.clone());
        }
        let lower = wanted.to_lowercase();
        let exact: Vec<&Schedule> = tasks
            .iter()
            .filter(|t| t.label.to_lowercase() == lower)
            .collect();
        let matches = if exact.is_empty() && !lower.is_empty() {
            tasks
                .iter()
                .filter(|t| t.label.to_lowercase().contains(&lower))
                .collect()
        } else {
            exact
        };
        let names = |from: &[&Schedule]| {
            from.iter()
                .map(|t| format!("\"{}\"", t.label))
                .collect::<Vec<_>>()
                .join(", ")
        };
        match matches.as_slice() {
            [one] => Ok((*one).clone()),
            [] if tasks.is_empty() => Err("There are no schedules yet.".to_string()),
            [] => Err(format!(
                "No schedule is called \"{wanted}\". The schedules: {}.",
                names(&tasks.iter().collect::<Vec<_>>())
            )),
            many => Err(format!(
                "\"{wanted}\" fits more than one schedule: {}. Ask which they mean.",
                names(many)
            )),
        }
    }
}

#[tool_router]
impl ScheduleMcpServer {
    /// All tools, without constructing the server; the generated `tool_router()` is private.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        let mut tools = Self::tool_router().list_all();
        tools.extend(Self::todo_tool_router().list_all());
        tools
    }

    pub fn new(
        scheduler: Arc<dyn SchedulerPort>,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    ) -> Self {
        Self {
            scheduler,
            settings_repo,
            // `todo_tools.rs` holds the second router.
            tool_router: Self::tool_router() + Self::todo_tool_router(),
        }
    }

    #[tool(
        description = "List the schedules: what each one does, when it runs, and whether it is paused."
    )]
    async fn list_schedules(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let tasks = self.scheduler.list_tasks().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing schedules: {e}"),
                None,
            )
        })?;
        let text = if tasks.is_empty() {
            "No scheduled tasks.".to_string()
        } else {
            tasks
                .iter()
                .map(|t| {
                    format!(
                        "- \"{}\": {}, {} time ({}, {}) [id {}]",
                        t.label,
                        when_in_words(&t.cron),
                        t.timezone,
                        kind_label(&t.kind),
                        status_label(t),
                        t.id
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        let ui_schedules: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
                let prompt_preview = match &t.kind {
                    TaskKind::AgentPrompt { prompt } => prompt.clone(),
                    TaskKind::Webhook { webhook_url } => webhook_url.clone(),
                    TaskKind::SensorTrigger(spec) => sensor_rule_summary(spec),
                    TaskKind::Routine { actions } => routine_summary(actions),
                };
                serde_json::json!({
                    "id": t.id,
                    "name": t.label,
                    "cron": t.cron,
                    "when": when_in_words(&t.cron),
                    "timezone": t.timezone,
                    "kind": kind_label(&t.kind),
                    "status": status_label(t),
                    "prompt": prompt_preview,
                })
            })
            .collect();
        let ui_data = serde_json::json!({ "schedules": ui_schedules });
        said(format!("[[[mcp-ui:schedule:{ui_data}]]]\n{text}"))
    }

    #[tool(description = "\
Countdown timer: fires ONCE after a delay, then deletes itself (\"set a timer for 10 \
minutes\"). Anything repeating: create_schedule. Reminding the person of a task: add_todo.")]
    async fn set_timer(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SetTimerParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("set_timer");
        let p = params.0;

        let raw = if p.duration.trim().is_empty() {
            ["in", "delay", "after", "for", "time"]
                .iter()
                .find_map(|k| p.extra.get(*k).and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_string()
        } else {
            p.duration.clone()
        };

        let Some(delta) = parse_duration(&raw) else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "How long? Give a duration like \"10 minutes\", \"1h30m\" or \"90 seconds\" \
                 as 'duration'. A timer set for the wrong moment is worse than one not set, \
                 so this is not guessed.",
            )]));
        };

        let fire_at = chrono::Utc::now() + delta;
        let timezone = self
            .settings_repo
            .get()
            .await
            .map(|s| s.timezone)
            .unwrap_or_else(|_| "UTC".to_string());

        let label = if p.name.trim().is_empty() {
            format!("Timer: {}", raw.trim())
        } else {
            p.name.trim().to_string()
        };
        let prompt = if p.prompt.trim().is_empty() {
            format!("The timer \"{}\" has finished. Tell the user.", label)
        } else {
            p.prompt.trim().to_string()
        };

        let id = format!("timer-{}", uuid::Uuid::new_v4());
        let req = pond_core::user_data::ports::scheduler::CreateScheduleRequest {
            ask_if_missed: false,
            id: id.clone(),
            label: label.clone(),
            // Display-only sentinel: a 6-field cron has no year, so it can't express "once".
            cron: pond_core::user_data::domain::schedule::CRON_ONCE.to_string(),
            fire_at: Some(fire_at),
            once: false,
            timezone,
            kind: TaskKind::AgentPrompt { prompt },
        };

        match self.scheduler.create_task(req).await {
            Ok(_) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Timer set: \"{label}\" [{id}] — fires once at {}.",
                fire_at.format("%H:%M:%S UTC")
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Could not set the timer: {e}."
            ))])),
        }
    }

    #[tool(description = "\
Have Goose do something again and again: \"every weekday at 7am give me the weather and my \
day\". `when` is how often, in plain words; `prompt` is what to do each time. To change an \
existing schedule, pass its name or id as `id` with only the fields that change.")]
    async fn create_schedule(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ScheduleUpsertParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let p = params.0;
        let given = |text: &Option<String>| {
            text.as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(str::to_string)
        };
        let when = given(&p.when).or_else(|| {
            p.extra
                .get("cron")
                .and_then(|v| v.as_str())
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
        });
        let zone = match given(&p.timezone) {
            Some(place) => match zone_from_place(&place) {
                Some(tz) => Some(tz.name().to_string()),
                None => {
                    return said(format!(
                        "Nothing was changed: \"{place}\" is not a place or time zone the Pond \
                         knows. Name a city, or leave `timezone` out for their own time."
                    ))
                }
            },
            None => None,
        };

        if p.id.trim().is_empty() {
            let timezone = match zone {
                Some(zone) => zone,
                None => self.home_zone().await,
            };
            let today = zone_from_place(&timezone)
                .map(|tz| chrono::Utc::now().with_timezone(&tz).date_naive())
                .unwrap_or_else(|| chrono::Utc::now().date_naive());
            let user_msg = crate::last_user_message();

            // The model's words first; the person's own message only when it sent none that
            // name a repeat.
            let from_model = when.as_deref().map(|w| repeat_said(w, today));
            let repeat = match from_model {
                Some(Ok(found)) => Ok(found),
                Some(Err(unclear)) if unclear != RepeatUnclear::NoCadence => Err(unclear),
                _ => repeat_said(&user_msg, today),
            };
            let (cron, words) = match repeat {
                Ok(found) => found,
                Err(unclear) => {
                    return said(format!("Nothing was scheduled. {}", unclear.advice()));
                }
            };

            let prompt = given(&p.prompt).unwrap_or_else(|| {
                extract_prompt_from_message(&user_msg.to_lowercase(), &user_msg)
            });
            if prompt.trim().is_empty() {
                return said(
                    "Nothing was scheduled. Send `prompt`: what Goose should do each time.",
                );
            }
            let label = given(&p.name).unwrap_or_else(|| clipped(&prompt, 40));

            let req = CreateScheduleRequest {
                ask_if_missed: false,
                fire_at: None,
                once: false,
                id: uuid::Uuid::new_v4().to_string(),
                label,
                cron,
                timezone,
                kind: TaskKind::AgentPrompt {
                    prompt: prompt.clone(),
                },
            };
            return match self.scheduler.create_task(req).await {
                Ok(schedule) => said(format!(
                    "Scheduled \"{}\": {words}, {} time. Each time Goose is asked: \"{prompt}\" \
                     [id {}]",
                    schedule.label, schedule.timezone, schedule.id,
                )),
                Err(e) => said(format!("Nothing was scheduled: {e}")),
            };
        }

        let existing = match self.find(&p.id).await {
            Ok(schedule) => schedule,
            Err(why) => return said(format!("Nothing was changed. {why}")),
        };
        let cron = match when.as_deref() {
            Some(w) => {
                let zone_name = zone.clone().unwrap_or_else(|| existing.timezone.clone());
                let today = zone_from_place(&zone_name)
                    .map(|tz| chrono::Utc::now().with_timezone(&tz).date_naive())
                    .unwrap_or_else(|| chrono::Utc::now().date_naive());
                match repeat_said(w, today) {
                    Ok((cron, _)) => Some(cron),
                    Err(unclear) => {
                        return said(format!("Nothing was changed. {}", unclear.advice()));
                    }
                }
            }
            None => None,
        };
        let req = UpdateScheduleRequest {
            ask_if_missed: None,
            fire_at: None,
            once: false,
            label: given(&p.name),
            cron,
            timezone: zone,
            kind: given(&p.prompt).map(|prompt| TaskKind::AgentPrompt { prompt }),
        };
        match self.scheduler.update_task(&existing.id, req).await {
            Ok(schedule) => {
                let does = match &schedule.kind {
                    TaskKind::AgentPrompt { prompt } => clipped(prompt, 60),
                    TaskKind::Webhook { webhook_url } => format!("webhook: {webhook_url}"),
                    TaskKind::SensorTrigger(spec) => sensor_rule_summary(spec),
                    TaskKind::Routine { actions } => routine_summary(actions),
                };
                said(format!(
                    "Changed \"{}\": {}, {} time ({does}) [id {}]",
                    schedule.label,
                    when_in_words(&schedule.cron),
                    schedule.timezone,
                    schedule.id,
                ))
            }
            Err(e) => said(format!(
                "Nothing was changed: \"{}\" could not be updated: {e}",
                existing.label
            )),
        }
    }

    // One tool with an action verb: fewer near-identical tools for the model to tell apart.
    #[tool(
        description = "Delete, pause, resume or run now an existing schedule, named by its name or id."
    )]
    async fn schedule_action(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ScheduleActionParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let action = params
            .0
            .action
            .trim()
            .to_ascii_lowercase()
            .replace(' ', "_");
        let past = match action.as_str() {
            "delete" | "remove" | "cancel" => "deleted",
            "pause" | "stop" => "paused",
            "resume" | "unpause" | "start" => "resumed",
            "run_now" | "run" => "started now",
            other => {
                return Err(ErrorData::new(
                    ErrorCode::INVALID_PARAMS,
                    format!("unknown action '{other}' -- use delete, pause, resume or run_now"),
                    None,
                ))
            }
        };
        let schedule = match self.find(&params.0.id).await {
            Ok(schedule) => schedule,
            Err(why) => return said(format!("Nothing was {past}. {why}")),
        };
        let id = schedule.id.as_str();
        let result = match past {
            "deleted" => self.scheduler.delete_task(id).await,
            "paused" => self.scheduler.pause_task(id).await,
            "resumed" => self.scheduler.resume_task(id).await,
            _ => self.scheduler.run_now(id).await,
        };
        match result {
            Ok(()) => said(format!("\"{}\" was {past}.", schedule.label)),
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to {action} schedule: {e}"),
                None,
            )),
        }
    }

    #[tool(
        description = "The time now in other places: cities, countries or time zones. Default: the person's own."
    )]
    async fn world_clock(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WorldClockParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let mut places: Vec<String> = match params.0.places {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect(),
            Some(serde_json::Value::String(text)) => text
                .split([',', ';'])
                .flat_map(|part| part.split(" and "))
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        places.retain(|p| !p.trim().is_empty());
        if places.is_empty() {
            places.push(self.home_zone().await);
        }

        let now = chrono::Utc::now();
        let lines: Vec<String> = places
            .iter()
            .map(|place| {
                let place = place.trim();
                match zone_from_place(place) {
                    Some(tz) => {
                        let local = now.with_timezone(&tz);
                        let offset = pond_core::user_data::services::location::format_offset(
                            chrono::Offset::fix(local.offset()),
                            pond_core::user_data::services::location::OffsetStyle::Prose,
                        );
                        format!(
                            "- {place}: {} ({}, {offset})",
                            local.format("%A, %d %B %Y %I:%M %p"),
                            local.format("%Z"),
                        )
                    }
                    None => format!(
                        "- {place}: not a place the Pond has a time zone for. Name the \
                         nearest large city."
                    ),
                }
            })
            .collect();
        said(lines.join("\n"))
    }
}

#[tool_handler]
impl ServerHandler for ScheduleMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-schedule",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Schedule MCP server — the person's to-dos and reminders, one-shot timers, repeating schedules, and event rules.\n\n\
                 Tools: add_todo, list_todos, complete_todo (the person's own to-dos and reminders, \
                 at a time, a repeat or a place), set_timer (a countdown, e.g. '10m', '1h30m'), \
                 create_schedule (something Goose does again and again; `when` in plain words, \
                 e.g. 'every weekday at 7am'; also changes one), \
                 list_schedules, schedule_action (delete | pause | resume | run_now), world_clock.",
            )
    }
}

// ── Message helpers ────────────────────────────────────────────────────────

/// The task part of a scheduling message, with scheduling and time words stripped.
pub fn extract_prompt_from_message(lower: &str, original: &str) -> String {
    let strip_patterns = [
        "schedule to ",
        "schedule ",
        "set up a ",
        "set up ",
        "every morning ",
        "every day ",
        "every evening ",
        "every night ",
        "every hour ",
        "every week ",
        "every minute ",
        "daily ",
        "hourly ",
        "weekly ",
        "remind me to ",
        "remind me ",
        "at ",
        "by ",
    ];

    let mut work = lower.to_string();

    for pat in &strip_patterns {
        if let Some(rest) = work.strip_prefix(pat) {
            work = rest.to_string();
        }
    }

    let time_re_patterns = [
        "at ",
        "every morning",
        "every day",
        "every evening",
        "every night",
        "every hour",
        "every week",
        "every minute",
        "daily",
        "hourly",
        "weekly",
    ];
    for pat in &time_re_patterns {
        work = work.replace(pat, " ");
    }

    let cleaned: String = work
        .split_whitespace()
        .filter(|w| {
            !w.chars().all(|c| c.is_ascii_digit() || c == ':')
                && *w != "am"
                && *w != "pm"
                && *w != "a.m."
                && *w != "p.m."
        })
        .collect::<Vec<_>>()
        .join(" ");

    let trimmed = cleaned.trim().to_string();

    if trimmed.len() < 5 {
        return original.trim().to_string();
    }

    let mut chars = trimmed.chars();
    match chars.next() {
        Some(c) => format!("{}{}", c.to_uppercase(), chars.collect::<String>()),
        None => original.trim().to_string(),
    }
}

/// Upcoming tasks as a system-prompt block (`## Upcoming Scheduled Tasks`), or `None` if none.
pub async fn try_upcoming_schedules_context(scheduler: &dyn SchedulerPort) -> Option<String> {
    let tasks = scheduler.list_upcoming(5).await.ok()?;
    if tasks.is_empty() {
        return None;
    }
    let lines: Vec<String> = tasks
        .iter()
        .map(|t| {
            let kind_label = match &t.kind {
                TaskKind::AgentPrompt { prompt } => {
                    format!("agent: \"{}\"", clipped(prompt, 60))
                }
                TaskKind::Webhook { webhook_url } => {
                    format!("webhook: {webhook_url}")
                }
                TaskKind::SensorTrigger(spec) => {
                    format!("rule: {}", sensor_rule_summary(spec))
                }
                TaskKind::Routine { actions } => format!("routine: {}", routine_summary(actions)),
            };
            format!(
                "- \"{}\" — {}, {} time ({})",
                t.label,
                when_in_words(&t.cron),
                t.timezone,
                kind_label
            )
        })
        .collect();
    Some(format!("## Upcoming Scheduled Tasks\n{}", lines.join("\n")))
}

/// One-line human summary of a sensor rule, shared by every display site.
pub(crate) fn sensor_rule_summary(spec: &SensorTriggerSpec) -> String {
    let src = match spec.source.kind {
        TriggerSourceKind::Sensor => "sensor",
        TriggerSourceKind::Camera => "camera",
        TriggerSourceKind::Device => "device",
    };
    format!(
        "on {src} {}/{} → {} action(s)",
        spec.source.device_id.as_deref().unwrap_or("any"),
        spec.source.signal.as_deref().unwrap_or("any"),
        spec.actions.len(),
    )
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct ScheduleDeps {
    scheduler: Arc<dyn SchedulerPort>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
}

static SCHEDULE_DEPS: OnceLock<ScheduleDeps> = OnceLock::new();

/// Initialize schedule server dependencies. Call once at startup.
pub fn init_schedule_deps(
    scheduler: Arc<dyn SchedulerPort>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
) {
    let _ = SCHEDULE_DEPS.set(ScheduleDeps {
        scheduler,
        settings_repo,
    });
}

pub(crate) fn scheduler_and_settings() -> Option<(
    Arc<dyn SchedulerPort>,
    Arc<dyn SettingsRepository + Send + Sync>,
)> {
    SCHEDULE_DEPS
        .get()
        .map(|d| (d.scheduler.clone(), d.settings_repo.clone()))
}

pub async fn upcoming_schedules_context() -> Option<String> {
    let (scheduler, _) = scheduler_and_settings()?;
    try_upcoming_schedules_context(scheduler.as_ref()).await
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_schedule_server(reader: DuplexStream, writer: DuplexStream) {
    // No deps = this binary didn't install this family: skip, as a panic kills every builtin.
    let Some(deps) = SCHEDULE_DEPS.get() else {
        tracing::error!(
            "spawn_schedule_server called before init_schedule_deps — extension will not start"
        );
        return;
    };
    let server = ScheduleMcpServer::new(deps.scheduler.clone(), deps.settings_repo.clone());
    crate::serve_builtin("giap-schedule", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_prompt_strips_prefixes() {
        let lower = "schedule to get weather at 10 am every morning";
        let original = "Schedule to get weather at 10 am every morning";
        let prompt = extract_prompt_from_message(lower, original);
        assert!(!prompt.to_lowercase().contains("schedule to"));
    }

    #[test]
    fn a_repeat_is_taken_from_words_or_from_a_cron_an_older_caller_sends() {
        let today = chrono::NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        assert_eq!(
            repeat_said("every weekday at 7am", today),
            Ok((
                "0 0 7 * * 1-5".to_string(),
                "every weekday at 07:00".to_string()
            ))
        );
        assert_eq!(
            repeat_said("0 30 8 * * *", today),
            Ok(("0 30 8 * * *".to_string(), "every day at 08:30".to_string()))
        );
        assert_eq!(
            repeat_said("30 8 * * *", today),
            Ok(("0 30 8 * * *".to_string(), "every day at 08:30".to_string()))
        );
        assert_eq!(repeat_said("at 3 pm", today), Err(RepeatUnclear::NoCadence));
    }

    #[test]
    fn a_schedule_s_time_is_shown_in_words() {
        assert_eq!(
            when_in_words("0 0 8 * * 1,4"),
            "every Monday and Thursday at 08:00"
        );
        assert_eq!(when_in_words(CRON_ONCE), "once");
        assert_eq!(when_in_words("@event"), "when its rule matches");
    }

    #[test]
    fn a_long_prompt_is_clipped_on_a_character_boundary() {
        assert_eq!(clipped("habari", 40), "habari");
        let long = "é".repeat(50);
        assert_eq!(clipped(&long, 40).chars().count(), 40);
    }

    // ── One-shot timers ───────────────────────────────────────────────────

    /// A 6-field cron has no year field, so a timer written as cron is an annual alarm.
    #[test]
    fn a_timer_is_not_expressible_as_cron() {
        use pond_core::user_data::domain::schedule::CRON_ONCE;
        // Deliberately unparseable, so nothing is tempted to evaluate it.
        assert!(CRON_ONCE.starts_with('@'));
        assert_eq!(CRON_ONCE.split_whitespace().count(), 1);
        // Distinct from the event sentinel, so a list can tell a timer from a sensor rule.
        assert_ne!(CRON_ONCE, "@event");
    }

    #[test]
    fn durations_parse_in_the_forms_people_say_them() {
        let cases = [
            ("10 minutes", 600),
            ("10 min", 600),
            ("10m", 600),
            ("1 hour", 3600),
            ("2h", 7200),
            ("90 seconds", 90),
            ("90s", 90),
            ("1h30m", 5400),
            ("1 h 30 m", 5400),
        ];
        for (text, secs) in cases {
            assert_eq!(
                parse_duration(text).map(|d| d.num_seconds()),
                Some(secs),
                "failed on {text:?}"
            );
        }
    }

    #[test]
    fn an_unreadable_duration_is_refused_rather_than_guessed() {
        for text in ["", "   ", "soon", "later", "tomorrow", "0 minutes", "-5m"] {
            assert_eq!(parse_duration(text), None, "guessed at {text:?}");
        }
    }

    #[test]
    fn month_is_not_silently_read_as_minutes() {
        assert_eq!(parse_duration("3 months"), None);
        assert_eq!(parse_duration("3 mo"), None);
        assert_eq!(parse_duration("3 min").map(|d| d.num_seconds()), Some(180));
    }
}
