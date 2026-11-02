//! The parts of a `giap_role` block beyond a role's limits: few-shot examples, when the agent
//! runs, the hooks around a run and whether paired peers may reach it. One collecting parser
//! serves both [`AgentRole::from_recipe_yaml`], which refuses at the first problem, and
//! [`validate_role_recipe`], which names every problem with the path of its field.

use crate::mcp::domain::tool_group::{find_group, groups_denied_to_subagents, TOOL_GROUPS};
use crate::shared::domain::orchestration::{AgentRole, MAX_ROLE_MAX_TURNS, ROLE_YAML_KEY};
use chrono::{DateTime, Utc};
use serde::ser::{SerializeMap, Serializer};
use serde::Serialize;
use serde_yaml::{Mapping, Value};
use std::collections::BTreeSet;
use std::time::Duration;

/// Cooldown for an activation that states none; a guess at how often a household wants to hear
/// from one agent about one kind of event, never measured.
pub const DEFAULT_ACTIVATION_COOLDOWN: Duration = Duration::from_secs(30 * 60);

/// Shortest cooldown an activation may ask for: each run takes the lane for seconds on an Orin.
pub const MIN_ACTIVATION_COOLDOWN: Duration = Duration::from_secs(60);

/// Examples are part of every run's preamble, so their count and size are capped; both guesses.
pub const MAX_ROLE_EXAMPLES: usize = 8;
pub const MAX_ROLE_EXAMPLE_CHARS: usize = 2_000;

/// An activation's brief becomes a step's instructions, which a step's window must hold whole.
pub const MAX_ACTIVATION_BRIEF_CHARS: usize = 1_200;

/// Heading over the rendered examples; fixed, so the preamble and its snapshot are stable.
pub const EXAMPLES_HEADING: &str = "Examples of a brief and the answer it wants:";

/// Every key a `giap_role` block may hold. `RoleBlock` in `orchestration.rs` must accept exactly
/// these; `role_block_fields_match_the_parser` holds the two together.
pub const ROLE_BLOCK_FIELDS: &[&str] = &[
    "tool_groups",
    "personal_data",
    "instructions",
    "max_turns",
    "context_fraction",
    "model",
    "context",
    "activations",
    "hooks",
    "a2a",
];

const ACTIVATION_KINDS: &[&str] = &["delegated", "task_step", "schedule", "event", "agent"];
const SCHEDULE_FIELDS: &[&str] = &["cron", "brief", "cooldown"];
const EVENT_FIELDS: &[&str] = &[
    "on", "brief", "cooldown", "member", "camera", "device", "above", "below", "at", "between",
];
const EVENT_KINDS: &[&str] = &[
    "presence.arrive",
    "presence.leave",
    "camera.person",
    "camera.motion",
    "sensor.<type>",
    "device.<key>",
    "time.daily",
];
const HOOK_FIELDS: &[&str] = &["before", "after"];
const BEFORE_HOOKS: &[&str] = &["today", "weather", "presence"];
const AFTER_HOOKS: &[&str] = &["notify_owner", "save_artifact", "start_task", "hand_to"];

// ── Types ───────────────────────────────────────────────────────────────────

/// One few-shot pair, rendered into the role's instructions by [`render_examples`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoleExample {
    pub brief: String,
    pub answer: String,
}

/// When an agent runs. None of these reads a user's words: two are model decisions, two are
/// explicit triggers, and `Agent` names who may hand it work.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Activation {
    /// The prime may hand it work with `delegate`.
    Delegated,
    /// A long task's planner may give it a step.
    TaskStep,
    Schedule(ScheduleActivation),
    Event(EventActivation),
    /// These agents may hand it work when their run ends.
    Agent {
        from: Vec<String>,
    },
}

/// What an absent `activations` key means.
pub fn default_activations() -> Vec<Activation> {
    vec![Activation::Delegated, Activation::TaskStep]
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScheduleActivation {
    pub cron: CronSchedule,
    pub brief: String,
    #[serde(rename = "cooldown_secs")]
    pub cooldown: Cooldown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventActivation {
    #[serde(flatten)]
    pub trigger: EventTrigger,
    pub brief: String,
    #[serde(rename = "cooldown_secs")]
    pub cooldown: Cooldown,
    /// Only events inside this part of the day run it, on the household's clock.
    #[serde(rename = "between", skip_serializing_if = "Option::is_none")]
    pub window: Option<ClockWindow>,
}

/// A part of the day, `22:00-06:00`; one whose end is before its start runs past midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockWindow {
    from: u32,
    to: u32,
}

impl ClockWindow {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let malformed =
            || format!("`{raw}` is not a part of the day; write it as \"22:00-06:00\", 24-hour");
        let (from, to) = raw.split_once('-').ok_or_else(malformed)?;
        let (from, to) = (
            parse_clock(from).ok_or_else(malformed)?,
            parse_clock(to).ok_or_else(malformed)?,
        );
        let window = ClockWindow {
            from: from.0 * 60 + from.1,
            to: to.0 * 60 + to.1,
        };
        if window.from == window.to {
            return Err(format!(
                "`{raw}` starts and ends at the same time; leave `between` out to run at any hour"
            ));
        }
        Ok(window)
    }

    /// `(start, end)` as `(hour, minute)` pairs.
    pub fn bounds(self) -> ((u32, u32), (u32, u32)) {
        (
            (self.from / 60, self.from % 60),
            (self.to / 60, self.to % 60),
        )
    }

    pub fn contains(self, hour: u32, minute: u32) -> bool {
        let at = hour * 60 + minute;
        if self.from < self.to {
            self.from <= at && at < self.to
        } else {
            at >= self.from || at < self.to
        }
    }
}

impl std::fmt::Display for ClockWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02}:{:02}-{:02}:{:02}",
            self.from / 60,
            self.from % 60,
            self.to / 60,
            self.to % 60
        )
    }
}

impl Serialize for ClockWindow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// Which bus event, or which time of day, starts a run.
#[derive(Debug, Clone, PartialEq)]
pub enum EventTrigger {
    PresenceArrive {
        member: Subject,
    },
    PresenceLeave {
        member: Subject,
    },
    CameraPerson {
        camera: Subject,
    },
    CameraMotion {
        camera: Subject,
    },
    /// Fires when a reading crosses the threshold, not on every reading past it.
    Sensor {
        sensor_type: String,
        device: Subject,
        threshold: Threshold,
    },
    DeviceChange {
        key: String,
        device: Subject,
    },
    /// Evaluated on the household's clock, not the bus: the bus ticks only on the hour.
    TimeDaily {
        hour: u32,
        minute: u32,
    },
}

impl EventTrigger {
    /// The `on:` spelling, for logs and the builder.
    pub fn on(&self) -> String {
        match self {
            EventTrigger::PresenceArrive { .. } => "presence.arrive".to_string(),
            EventTrigger::PresenceLeave { .. } => "presence.leave".to_string(),
            EventTrigger::CameraPerson { .. } => "camera.person".to_string(),
            EventTrigger::CameraMotion { .. } => "camera.motion".to_string(),
            EventTrigger::Sensor { sensor_type, .. } => format!("sensor.{sensor_type}"),
            EventTrigger::DeviceChange { key, .. } => format!("device.{key}"),
            EventTrigger::TimeDaily { .. } => "time.daily".to_string(),
        }
    }

    /// The clock a time-of-day trigger runs on; `None` for bus events.
    pub fn clock(&self) -> Option<CronSchedule> {
        match self {
            EventTrigger::TimeDaily { hour, minute } => {
                Some(CronSchedule::daily_at(*hour, *minute))
            }
            _ => None,
        }
    }
}

impl Serialize for EventTrigger {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("on", &self.on())?;
        match self {
            EventTrigger::PresenceArrive { member } | EventTrigger::PresenceLeave { member } => {
                map.serialize_entry("member", member)?;
            }
            EventTrigger::CameraPerson { camera } | EventTrigger::CameraMotion { camera } => {
                map.serialize_entry("camera", camera)?;
            }
            EventTrigger::Sensor {
                device, threshold, ..
            } => {
                map.serialize_entry("device", device)?;
                match threshold {
                    Threshold::Above(x) => map.serialize_entry("above", x)?,
                    Threshold::Below(x) => map.serialize_entry("below", x)?,
                }
            }
            EventTrigger::DeviceChange { device, .. } => {
                map.serialize_entry("device", device)?;
            }
            EventTrigger::TimeDaily { hour, minute } => {
                map.serialize_entry("at", &format!("{hour:02}:{minute:02}"))?;
            }
        }
        map.end()
    }
}

/// Who or what an event must be about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    Any,
    Named(String),
    /// The member who owns the recipe, so a shipped agent can mean "me" before it has an owner.
    Owner,
}

impl Subject {
    /// An agent with no owner admits nobody as `Owner`: the event would be about no one.
    pub fn admits(&self, id: &str, owner: Option<&str>) -> bool {
        match self {
            Subject::Any => true,
            Subject::Named(name) => name == id,
            Subject::Owner => owner == Some(id),
        }
    }
}

impl Serialize for Subject {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Subject::Any => serializer.serialize_str("any"),
            Subject::Named(name) => serializer.serialize_str(name),
            Subject::Owner => serializer.serialize_str("owner"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Threshold {
    Above(f64),
    Below(f64),
}

impl Threshold {
    /// Whether a reading is past the threshold; NaN never is.
    pub fn passes(self, value: f64) -> bool {
        match self {
            Threshold::Above(x) => value > x,
            Threshold::Below(x) => value < x,
        }
    }
}

/// Least time between two runs of one activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cooldown(Duration);

impl Default for Cooldown {
    fn default() -> Self {
        Cooldown(DEFAULT_ACTIVATION_COOLDOWN)
    }
}

impl Cooldown {
    /// `90s`, `30m`, `2h`, `1d` or a sum such as `1h30m`; refused below [`MIN_ACTIVATION_COOLDOWN`].
    pub fn parse(raw: &str) -> Result<Self, String> {
        let text = raw.trim().to_ascii_lowercase();
        if text.is_empty() {
            return Err("is empty; write a duration such as `30m` or `2h`".to_string());
        }
        let mut total: u64 = 0;
        let mut digits = String::new();
        for c in text.chars().filter(|c| !c.is_whitespace()) {
            if c.is_ascii_digit() {
                digits.push(c);
                continue;
            }
            let unit = match c {
                's' => 1,
                'm' => 60,
                'h' => 3_600,
                'd' => 86_400,
                _ => {
                    return Err(format!(
                        "`{raw}` is not a duration: use a number and s, m, h or d, such as `30m`"
                    ))
                }
            };
            let Ok(n) = digits.parse::<u64>() else {
                return Err(format!(
                    "`{raw}` is not a duration: each unit needs a number before it, such as `30m`"
                ));
            };
            total = total.saturating_add(n.saturating_mul(unit));
            digits.clear();
        }
        if !digits.is_empty() {
            return Err(format!(
                "`{raw}` has no unit after `{digits}`: write `{digits}m` for minutes or \
                 `{digits}s` for seconds"
            ));
        }
        let cooldown = Duration::from_secs(total);
        if cooldown < MIN_ACTIVATION_COOLDOWN {
            return Err(format!(
                "`{raw}` is shorter than the least cooldown, {}s; each run takes the pond's one \
                 model for a while",
                MIN_ACTIVATION_COOLDOWN.as_secs()
            ));
        }
        Ok(Cooldown(cooldown))
    }

    pub fn duration(self) -> Duration {
        self.0
    }
}

impl Serialize for Cooldown {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(self.0.as_secs())
    }
}

/// A 6-field cron (`sec min hour day-of-month month day-of-week`), read as the scheduler reads
/// one: seconds required, day-of-month AND day-of-week.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CronSchedule(String);

fn cron_parser() -> croner::parser::CronParser {
    croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Required)
        .dom_and_dow(true)
        .build()
}

impl CronSchedule {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let fields: Vec<&str> = raw.split_whitespace().collect();
        if fields.len() != 6 {
            return Err(format!(
                "has {} fields; a cron here has six: second minute hour day-of-month month \
                 day-of-week, such as \"0 0 7 * * MON-FRI\"",
                fields.len()
            ));
        }
        let expr = fields.join(" ");
        let cron = cron_parser()
            .parse(&expr)
            .map_err(|e| format!("`{expr}` is not a cron the scheduler can read: {e}"))?;
        // Any time works: six fields have no year, so a cron that ever fires fires every year.
        if cron
            .find_next_occurrence(&DateTime::<Utc>::default(), false)
            .is_err()
        {
            return Err(format!("`{expr}` names a time that never comes round"));
        }
        Ok(CronSchedule(expr))
    }

    pub fn daily_at(hour: u32, minute: u32) -> Self {
        CronSchedule(format!("0 {minute} {hour} * * *"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first occurrence strictly after `after`, on the wall clock of IANA `timezone` (UTC if
    /// unknown), as the scheduler resolves its own schedules.
    pub fn next_after(&self, after: DateTime<Utc>, timezone: &str) -> Option<DateTime<Utc>> {
        let cron = cron_parser().parse(&self.0).ok()?;
        let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
        cron.find_next_occurrence(&after.with_timezone(&tz), false)
            .ok()
            .map(|at| at.with_timezone(&Utc))
    }
}

/// Context gathered before a run, from a fixed catalog; never code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BeforeHook {
    /// The local date and today's schedule.
    Today,
    Weather,
    /// Who the pond last saw arrive or leave.
    Presence,
}

/// What the runner does with a completed run's hand-back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterHook {
    /// Tell the task's owner on their own devices.
    NotifyOwner,
    /// Keep the hand-back as the task's result artifact.
    SaveArtifact,
    /// Start a long task, when the hand-back asks for one.
    StartTask,
    /// Run this agent next with the run's artifacts.
    HandTo(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RoleHooks {
    pub before: Vec<BeforeHook>,
    pub after: Vec<AfterHook>,
}

impl RoleHooks {
    /// The agent a `hand_to` hook names; at most one, the parser refuses a second.
    pub fn hand_to(&self) -> Option<&str> {
        self.after.iter().find_map(|hook| match hook {
            AfterHook::HandTo(agent) => Some(agent.as_str()),
            _ => None,
        })
    }

    pub fn after_has(&self, hook: &AfterHook) -> bool {
        self.after.contains(hook)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RoleA2a {
    /// Offer the agent to paired peers over A2A; off unless a member turns it on.
    pub expose: bool,
}

/// The four optional parts of a block, typed.
#[derive(Debug, Clone, PartialEq)]
pub struct RoleExtras {
    pub examples: Vec<RoleExample>,
    pub activations: Vec<Activation>,
    pub hooks: RoleHooks,
    pub a2a: RoleA2a,
}

impl Default for RoleExtras {
    fn default() -> Self {
        Self {
            examples: Vec::new(),
            activations: default_activations(),
            hooks: RoleHooks::default(),
            a2a: RoleA2a::default(),
        }
    }
}

/// One problem, at the field it is at (`giap_role.activations[2].event.on`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub path: String,
    pub message: String,
}

/// The examples in the fixed format the preamble carries; empty when there are none.
pub fn render_examples(examples: &[RoleExample]) -> String {
    if examples.is_empty() {
        return String::new();
    }
    let mut out = format!("\n\n{EXAMPLES_HEADING}\n");
    for example in examples {
        out.push_str(&format!(
            "\nBrief: {}\nAnswer: {}\n",
            example.brief, example.answer
        ));
    }
    out
}

// ── The collecting parser ───────────────────────────────────────────────────

/// The four optional parts as the block holds them, before they are typed.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ExtrasInput<'a> {
    pub context: Option<&'a Value>,
    pub activations: Option<&'a Value>,
    pub hooks: Option<&'a Value>,
    pub a2a: Option<&'a Value>,
}

/// What cross-references are checked against; [`References::none`] at run time, where a named
/// agent is looked up when it is needed.
#[derive(Debug, Clone, Copy, Default)]
pub struct References<'a> {
    pub own_name: Option<&'a str>,
    pub known_agents: Option<&'a BTreeSet<String>>,
}

impl References<'_> {
    pub fn none() -> Self {
        Self::default()
    }

    fn check(&self, name: &str, path: &str, errors: &mut Vec<FieldError>) {
        if self.own_name == Some(name) {
            push(
                errors,
                path,
                format!("names this agent itself; `{name}` cannot hand work to itself"),
            );
            return;
        }
        if let Some(known) = self.known_agents {
            if !known.contains(name) {
                let mut message =
                    format!("names `{name}`, and there is no saved agent by that name");
                let candidates: Vec<&str> = known.iter().map(String::as_str).collect();
                if let Some(near) = nearest(name, &candidates) {
                    message.push_str(&format!("; did you mean `{near}`?"));
                }
                push(errors, path, message);
            }
        }
    }
}

pub(crate) fn parse_extras(
    input: ExtrasInput<'_>,
    refs: &References<'_>,
    errors: &mut Vec<FieldError>,
) -> RoleExtras {
    let base = ROLE_YAML_KEY;
    RoleExtras {
        examples: parse_context(input.context, &format!("{base}.context"), errors),
        activations: parse_activations(
            input.activations,
            &format!("{base}.activations"),
            refs,
            errors,
        ),
        hooks: parse_hooks(input.hooks, &format!("{base}.hooks"), refs, errors),
        a2a: parse_a2a(input.a2a, &format!("{base}.a2a"), errors),
    }
}

fn push(errors: &mut Vec<FieldError>, path: impl Into<String>, message: impl Into<String>) {
    errors.push(FieldError {
        path: path.into(),
        message: message.into(),
    });
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Sequence(_) => "a list",
        Value::Mapping(_) => "a map",
        Value::Tagged(_) => "a tagged value",
    }
}

/// `x` from `{x: ...}` or `!x ...`: serde_yaml writes enums with tags, people write maps.
fn single_entry(value: &Value) -> Option<(String, &Value)> {
    match value {
        Value::Mapping(map) if map.len() == 1 => {
            let (key, inner) = map.iter().next()?;
            Some((key.as_str()?.trim().to_string(), inner))
        }
        Value::Tagged(tagged) => Some((
            tagged.tag.to_string().trim_start_matches('!').to_string(),
            &tagged.value,
        )),
        _ => None,
    }
}

/// Refuses keys outside `allowed`, suggesting the nearest.
fn check_keys(map: &Mapping, allowed: &[&str], path: &str, errors: &mut Vec<FieldError>) {
    for key in map.keys() {
        let Some(name) = key.as_str() else {
            push(
                errors,
                path,
                format!("has a key that is {}, not a name", kind_of(key)),
            );
            continue;
        };
        if allowed.contains(&name) {
            continue;
        }
        let message = match nearest(name, allowed) {
            Some(near) => format!("unknown field; did you mean `{near}`?"),
            None => format!("unknown field; expected one of {}", allowed.join(", ")),
        };
        push(errors, format!("{path}.{name}"), message);
    }
}

/// Required text: trimmed, non-empty, at most `max` characters.
fn required_text(
    map: &Mapping,
    key: &str,
    path: &str,
    max: usize,
    errors: &mut Vec<FieldError>,
) -> Option<String> {
    let at = format!("{path}.{key}");
    match map.get(key) {
        None | Some(Value::Null) => {
            push(errors, at, "missing; write it as text");
            None
        }
        Some(Value::String(text)) if text.trim().is_empty() => {
            push(errors, at, "is empty; write it as text");
            None
        }
        Some(Value::String(text)) if text.trim().chars().count() > max => {
            push(
                errors,
                at,
                format!(
                    "is {} characters; the most is {max}",
                    text.trim().chars().count()
                ),
            );
            None
        }
        Some(Value::String(text)) => Some(text.trim().to_string()),
        Some(other) => {
            push(errors, at, format!("must be text, not {}", kind_of(other)));
            None
        }
    }
}

fn parse_context(
    value: Option<&Value>,
    path: &str,
    errors: &mut Vec<FieldError>,
) -> Vec<RoleExample> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Vec::new();
    };
    let Some(map) = value.as_mapping() else {
        push(
            errors,
            path,
            format!(
                "must be a map with `examples:` under it, not {}",
                kind_of(value)
            ),
        );
        return Vec::new();
    };
    check_keys(map, &["examples"], path, errors);
    match map.get("examples") {
        None | Some(Value::Null) => Vec::new(),
        Some(examples) => parse_examples(examples, &format!("{path}.examples"), errors),
    }
}

fn parse_examples(value: &Value, path: &str, errors: &mut Vec<FieldError>) -> Vec<RoleExample> {
    let Some(items) = value.as_sequence() else {
        push(
            errors,
            path,
            format!(
                "must be a list of `- brief: ... answer: ...` pairs, not {}",
                kind_of(value)
            ),
        );
        return Vec::new();
    };
    if items.len() > MAX_ROLE_EXAMPLES {
        push(
            errors,
            path,
            format!(
                "has {} examples; the most is {MAX_ROLE_EXAMPLES}, since every run's preamble \
                 carries them",
                items.len()
            ),
        );
    }
    let mut examples = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let at = format!("{path}[{i}]");
        let Some(map) = item.as_mapping() else {
            push(
                errors,
                &at,
                format!(
                    "must be a map with `brief` and `answer`, not {}",
                    kind_of(item)
                ),
            );
            continue;
        };
        check_keys(map, &["brief", "answer"], &at, errors);
        let brief = required_text(map, "brief", &at, MAX_ROLE_EXAMPLE_CHARS, errors);
        let answer = required_text(map, "answer", &at, MAX_ROLE_EXAMPLE_CHARS, errors);
        if let (Some(brief), Some(answer)) = (brief, answer) {
            examples.push(RoleExample { brief, answer });
        }
    }
    examples
}

fn parse_activations(
    value: Option<&Value>,
    path: &str,
    refs: &References<'_>,
    errors: &mut Vec<FieldError>,
) -> Vec<Activation> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return default_activations();
    };
    let Some(items) = value.as_sequence() else {
        push(
            errors,
            path,
            format!(
                "must be a list, such as [delegated, task_step], not {}",
                kind_of(value)
            ),
        );
        return Vec::new();
    };
    if items.is_empty() {
        push(
            errors,
            path,
            "is empty, so nothing could ever run this agent; leave the key out for the default \
             [delegated, task_step], or set the recipe inactive",
        );
        return Vec::new();
    }
    let mut out: Vec<Activation> = Vec::with_capacity(items.len());
    let mut handed_by: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let at = format!("{path}[{i}]");
        match parse_activation(item, &at, refs, errors) {
            Some(Activation::Agent { from }) => {
                for name in from {
                    if !handed_by.contains(&name) {
                        handed_by.push(name);
                    }
                }
            }
            Some(activation) if !out.contains(&activation) => out.push(activation),
            Some(_) | None => {}
        }
    }
    if !handed_by.is_empty() {
        out.push(Activation::Agent { from: handed_by });
    }
    out
}

fn parse_activation(
    item: &Value,
    path: &str,
    refs: &References<'_>,
    errors: &mut Vec<FieldError>,
) -> Option<Activation> {
    if let Value::String(word) = item {
        return match word.trim() {
            "delegated" => Some(Activation::Delegated),
            "task_step" => Some(Activation::TaskStep),
            kind @ ("schedule" | "event" | "agent") => {
                push(
                    errors,
                    path,
                    format!("`{kind}` needs its settings under it: write `{kind}: {{ ... }}`"),
                );
                None
            }
            other => {
                push(
                    errors,
                    path,
                    unknown_word("an activation", other, ACTIVATION_KINDS),
                );
                None
            }
        };
    }
    let Some((kind, inner)) = single_entry(item) else {
        let mut message = format!(
            "must be one of {} on its own line, not {}",
            ACTIVATION_KINDS.join(", "),
            kind_of(item)
        );
        if let Value::Mapping(map) = item {
            let keys: Vec<&str> = map.keys().filter_map(Value::as_str).collect();
            message = format!(
                "has {} keys ({}); each activation is one kind. If the settings belong to the \
                 kind, indent them under it",
                keys.len(),
                keys.join(", ")
            );
        }
        push(errors, path, message);
        return None;
    };
    let at = format!("{path}.{kind}");
    match kind.as_str() {
        "schedule" => parse_schedule(inner, &at, errors).map(Activation::Schedule),
        "event" => parse_event(inner, &at, errors).map(Activation::Event),
        "agent" => parse_handed_by(inner, &at, refs, errors),
        "delegated" | "task_step" => {
            push(
                errors,
                path,
                format!("`{kind}` takes no settings; write it alone, as `- {kind}`"),
            );
            None
        }
        other => {
            push(
                errors,
                path,
                unknown_word("an activation", other, ACTIVATION_KINDS),
            );
            None
        }
    }
}

fn unknown_word(what: &str, word: &str, allowed: &[&str]) -> String {
    match nearest(word, allowed) {
        Some(near) => format!("`{word}` is not {what}; did you mean `{near}`?"),
        None => format!("`{word}` is not {what}; use one of {}", allowed.join(", ")),
    }
}

fn cooldown_field(map: &Mapping, path: &str, errors: &mut Vec<FieldError>) -> Cooldown {
    let at = format!("{path}.cooldown");
    match map.get("cooldown") {
        None | Some(Value::Null) => Cooldown::default(),
        Some(Value::String(raw)) => Cooldown::parse(raw).unwrap_or_else(|why| {
            push(errors, at, why);
            Cooldown::default()
        }),
        Some(Value::Number(n)) => {
            push(
                errors,
                at,
                format!("`{n}` has no unit; write `{n}m` for minutes or `{n}s` for seconds"),
            );
            Cooldown::default()
        }
        Some(other) => {
            push(
                errors,
                at,
                format!("must be a duration such as `30m`, not {}", kind_of(other)),
            );
            Cooldown::default()
        }
    }
}

fn parse_schedule(
    value: &Value,
    path: &str,
    errors: &mut Vec<FieldError>,
) -> Option<ScheduleActivation> {
    let Some(map) = value.as_mapping() else {
        push(
            errors,
            path,
            format!(
                "must be a map such as {{ cron: \"0 0 7 * * *\", brief: \"...\" }}, not {}",
                kind_of(value)
            ),
        );
        return None;
    };
    check_keys(map, SCHEDULE_FIELDS, path, errors);
    let cron_at = format!("{path}.cron");
    let cron = match map.get("cron") {
        None | Some(Value::Null) => {
            push(
                errors,
                &cron_at,
                "missing; write a 6-field cron such as \"0 0 7 * * MON-FRI\"",
            );
            None
        }
        Some(Value::String(raw)) => CronSchedule::parse(raw)
            .map_err(|why| push(errors, &cron_at, why))
            .ok(),
        Some(other) => {
            push(
                errors,
                &cron_at,
                format!("must be text in quotes, not {}", kind_of(other)),
            );
            None
        }
    };
    let brief = required_text(map, "brief", path, MAX_ACTIVATION_BRIEF_CHARS, errors);
    let cooldown = cooldown_field(map, path, errors);
    Some(ScheduleActivation {
        cron: cron?,
        brief: brief?,
        cooldown,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OnKind {
    PresenceArrive,
    PresenceLeave,
    CameraPerson,
    CameraMotion,
    Sensor(String),
    Device(String),
    TimeDaily,
}

impl OnKind {
    fn family(&self) -> &'static str {
        match self {
            OnKind::PresenceArrive | OnKind::PresenceLeave => "presence",
            OnKind::CameraPerson | OnKind::CameraMotion => "camera",
            OnKind::Sensor(_) => "sensor",
            OnKind::Device(_) => "device",
            OnKind::TimeDaily => "time",
        }
    }
}

fn parse_on(raw: &str) -> Result<OnKind, String> {
    let on = raw.trim().to_ascii_lowercase();
    let named = |prefix: &str, what: &str| -> Result<String, String> {
        let name = &on[prefix.len()..];
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
        if valid {
            Ok(name.to_string())
        } else {
            Err(format!(
                "`{raw}` needs a {what} after `{prefix}`, in letters, digits, `_` or `-`, such as \
                 `{prefix}temperature`"
            ))
        }
    };
    match on.as_str() {
        "presence.arrive" => Ok(OnKind::PresenceArrive),
        "presence.leave" => Ok(OnKind::PresenceLeave),
        "camera.person" => Ok(OnKind::CameraPerson),
        "camera.motion" => Ok(OnKind::CameraMotion),
        "time.daily" => Ok(OnKind::TimeDaily),
        _ if on.starts_with("sensor.") => named("sensor.", "sensor type").map(OnKind::Sensor),
        _ if on.starts_with("device.") => named("device.", "state key").map(OnKind::Device),
        _ => Err(unknown_word("an event", raw.trim(), EVENT_KINDS)),
    }
}

/// `any`, absent, or an id; `owner` too when the subject is a member.
fn subject_field(map: &Mapping, key: &str, path: &str, errors: &mut Vec<FieldError>) -> Subject {
    match map.get(key) {
        Some(Value::String(text))
            if key == "member" && text.trim().eq_ignore_ascii_case("owner") =>
        {
            Subject::Owner
        }
        None | Some(Value::Null) => Subject::Any,
        Some(Value::String(text)) if text.trim().is_empty() => {
            push(
                errors,
                format!("{path}.{key}"),
                "is empty; write `any` or an id",
            );
            Subject::Any
        }
        Some(Value::String(text)) if text.trim().eq_ignore_ascii_case("any") => Subject::Any,
        Some(Value::String(text)) => Subject::Named(text.trim().to_string()),
        Some(other) => {
            push(
                errors,
                format!("{path}.{key}"),
                format!("must be `any` or an id, not {}", kind_of(other)),
            );
            Subject::Any
        }
    }
}

/// `HH:MM`, 24-hour.
fn parse_clock(raw: &str) -> Option<(u32, u32)> {
    let (hour, minute) = raw.trim().split_once(':')?;
    if hour.is_empty() || hour.len() > 2 || minute.len() != 2 {
        return None;
    }
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    (hour < 24 && minute < 60).then_some((hour, minute))
}

fn parse_event(value: &Value, path: &str, errors: &mut Vec<FieldError>) -> Option<EventActivation> {
    let Some(map) = value.as_mapping() else {
        push(
            errors,
            path,
            format!(
                "must be a map such as {{ on: presence.arrive, brief: \"...\" }}, not {}",
                kind_of(value)
            ),
        );
        return None;
    };
    check_keys(map, EVENT_FIELDS, path, errors);
    let on_at = format!("{path}.on");
    let on = match map.get("on") {
        None | Some(Value::Null) => {
            push(
                errors,
                &on_at,
                format!(
                    "missing; say which event, one of {}",
                    EVENT_KINDS.join(", ")
                ),
            );
            None
        }
        Some(Value::String(raw)) => parse_on(raw).map_err(|why| push(errors, &on_at, why)).ok(),
        Some(other) => {
            push(
                errors,
                &on_at,
                format!("must be an event name, not {}", kind_of(other)),
            );
            None
        }
    };
    let brief = required_text(map, "brief", path, MAX_ACTIVATION_BRIEF_CHARS, errors);
    let cooldown = cooldown_field(map, path, errors);
    let on = on?;

    // A field for another family would be ignored, so the agent would fire more often than meant.
    let family = on.family();
    for (key, belongs) in [
        ("member", &["presence"][..]),
        ("camera", &["camera"][..]),
        ("device", &["sensor", "device"][..]),
        ("above", &["sensor"][..]),
        ("below", &["sensor"][..]),
        ("at", &["time"][..]),
        ("between", &["presence", "camera", "sensor", "device"][..]),
    ] {
        if map.contains_key(key) && !belongs.contains(&family) {
            push(
                errors,
                format!("{path}.{key}"),
                format!(
                    "applies only to {} events, not `{family}` ones",
                    belongs.join(" and ")
                ),
            );
        }
    }

    let trigger = match on {
        OnKind::PresenceArrive => EventTrigger::PresenceArrive {
            member: subject_field(map, "member", path, errors),
        },
        OnKind::PresenceLeave => EventTrigger::PresenceLeave {
            member: subject_field(map, "member", path, errors),
        },
        OnKind::CameraPerson => EventTrigger::CameraPerson {
            camera: subject_field(map, "camera", path, errors),
        },
        OnKind::CameraMotion => EventTrigger::CameraMotion {
            camera: subject_field(map, "camera", path, errors),
        },
        OnKind::Sensor(sensor_type) => {
            let device = subject_field(map, "device", path, errors);
            let number = |key: &str, errors: &mut Vec<FieldError>| match map.get(key) {
                None | Some(Value::Null) => None,
                Some(v) => match v.as_f64().filter(|x| x.is_finite()) {
                    Some(x) => Some(x),
                    None => {
                        push(
                            errors,
                            format!("{path}.{key}"),
                            format!("must be a number, not {}", kind_of(v)),
                        );
                        None
                    }
                },
            };
            let above = number("above", errors);
            let below = number("below", errors);
            let threshold = match (above, below) {
                (Some(x), None) => Threshold::Above(x),
                (None, Some(x)) => Threshold::Below(x),
                (Some(_), Some(_)) => {
                    push(
                        errors,
                        path,
                        "has both `above` and `below`; give one, and a second activation for the \
                         other",
                    );
                    return None;
                }
                (None, None) => {
                    if !map.contains_key("above") && !map.contains_key("below") {
                        push(
                            errors,
                            path,
                            "a sensor event needs `above:` or `below:` a number, or it would run \
                             on every reading",
                        );
                    }
                    return None;
                }
            };
            EventTrigger::Sensor {
                sensor_type,
                device,
                threshold,
            }
        }
        OnKind::Device(key) => EventTrigger::DeviceChange {
            key,
            device: subject_field(map, "device", path, errors),
        },
        OnKind::TimeDaily => {
            let at = format!("{path}.at");
            let clock = match map.get("at") {
                None | Some(Value::Null) => {
                    push(
                        errors,
                        &at,
                        "missing; a daily event needs a time such as \"07:30\"",
                    );
                    None
                }
                Some(Value::String(raw)) => {
                    let parsed = parse_clock(raw);
                    if parsed.is_none() {
                        push(
                            errors,
                            &at,
                            format!(
                                "`{raw}` is not a time; write 24-hour HH:MM, such as \"07:30\""
                            ),
                        );
                    }
                    parsed
                }
                Some(other) => {
                    push(
                        errors,
                        &at,
                        format!(
                            "must be a time in quotes such as \"07:30\", not {}",
                            kind_of(other)
                        ),
                    );
                    None
                }
            };
            let (hour, minute) = clock?;
            EventTrigger::TimeDaily { hour, minute }
        }
    };
    let window = match map.get("between") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) => match ClockWindow::parse(raw) {
            Ok(window) => Some(window),
            Err(why) => {
                push(errors, format!("{path}.between"), why);
                return None;
            }
        },
        Some(other) => {
            push(
                errors,
                format!("{path}.between"),
                format!(
                    "must be a part of the day in quotes such as \"22:00-06:00\", not {}",
                    kind_of(other)
                ),
            );
            return None;
        }
    };
    Some(EventActivation {
        trigger,
        brief: brief?,
        cooldown,
        window,
    })
}

fn parse_handed_by(
    value: &Value,
    path: &str,
    refs: &References<'_>,
    errors: &mut Vec<FieldError>,
) -> Option<Activation> {
    let names: Vec<(String, String)> = match value {
        Value::String(name) => vec![(name.trim().to_string(), path.to_string())],
        Value::Sequence(items) => items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let at = format!("{path}[{i}]");
                match item.as_str() {
                    Some(name) => Some((name.trim().to_string(), at)),
                    None => {
                        push(
                            errors,
                            at,
                            format!("must be an agent's name, not {}", kind_of(item)),
                        );
                        None
                    }
                }
            })
            .collect(),
        other => {
            push(
                errors,
                path,
                format!(
                    "must be a list of agent names, such as [trip-planner], not {}",
                    kind_of(other)
                ),
            );
            return None;
        }
    };
    let mut from: Vec<String> = Vec::new();
    for (name, at) in names {
        if name.is_empty() {
            push(errors, at, "is empty; write an agent's name");
            continue;
        }
        refs.check(&name, &at, errors);
        if !from.contains(&name) {
            from.push(name);
        }
    }
    if from.is_empty() {
        push(
            errors,
            path,
            "names no agent; list the agents that may hand this one work",
        );
        return None;
    }
    Some(Activation::Agent { from })
}

fn parse_hooks(
    value: Option<&Value>,
    path: &str,
    refs: &References<'_>,
    errors: &mut Vec<FieldError>,
) -> RoleHooks {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return RoleHooks::default();
    };
    let Some(map) = value.as_mapping() else {
        push(
            errors,
            path,
            format!(
                "must be a map with `before:` and `after:` lists, not {}",
                kind_of(value)
            ),
        );
        return RoleHooks::default();
    };
    check_keys(map, HOOK_FIELDS, path, errors);
    let mut hooks = RoleHooks::default();
    if let Some(items) = hook_list(map, "before", path, errors) {
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}.before[{i}]");
            let hook = match item.as_str().map(str::trim) {
                Some("today") => BeforeHook::Today,
                Some("weather") => BeforeHook::Weather,
                Some("presence") => BeforeHook::Presence,
                Some(other) => {
                    push(
                        errors,
                        at,
                        unknown_word("a before hook", other, BEFORE_HOOKS),
                    );
                    continue;
                }
                None => {
                    push(
                        errors,
                        at,
                        format!(
                            "must be one of {}, not {}",
                            BEFORE_HOOKS.join(", "),
                            kind_of(item)
                        ),
                    );
                    continue;
                }
            };
            if !hooks.before.contains(&hook) {
                hooks.before.push(hook);
            }
        }
    }
    if let Some(items) = hook_list(map, "after", path, errors) {
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}.after[{i}]");
            let hook = match item {
                Value::String(word) => match word.trim() {
                    "notify_owner" => AfterHook::NotifyOwner,
                    "save_artifact" => AfterHook::SaveArtifact,
                    "start_task" => AfterHook::StartTask,
                    "hand_to" => {
                        push(
                            errors,
                            at,
                            "`hand_to` needs the agent's name: write `hand_to: <agent>`",
                        );
                        continue;
                    }
                    other => {
                        push(
                            errors,
                            at,
                            unknown_word("an after hook", other, AFTER_HOOKS),
                        );
                        continue;
                    }
                },
                _ => match single_entry(item) {
                    Some((kind, target)) if kind == "hand_to" => {
                        let at = format!("{at}.hand_to");
                        let Some(name) = target.as_str().map(str::trim).filter(|n| !n.is_empty())
                        else {
                            push(errors, at, "must be the name of the agent to run next");
                            continue;
                        };
                        refs.check(name, &at, errors);
                        if hooks.hand_to().is_some() {
                            push(
                                errors,
                                at,
                                "is a second `hand_to`; the runner runs one agent next, so keep one",
                            );
                            continue;
                        }
                        AfterHook::HandTo(name.to_string())
                    }
                    Some((kind, _)) => {
                        push(
                            errors,
                            at,
                            unknown_word("an after hook", &kind, AFTER_HOOKS),
                        );
                        continue;
                    }
                    None => {
                        push(
                            errors,
                            at,
                            format!(
                                "must be one of {}, not {}",
                                AFTER_HOOKS.join(", "),
                                kind_of(item)
                            ),
                        );
                        continue;
                    }
                },
            };
            if !hooks.after.contains(&hook) {
                hooks.after.push(hook);
            }
        }
    }
    hooks
}

fn hook_list<'a>(
    map: &'a Mapping,
    key: &str,
    path: &str,
    errors: &mut Vec<FieldError>,
) -> Option<&'a Vec<Value>> {
    match map.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::Sequence(items)) => Some(items),
        Some(other) => {
            push(
                errors,
                format!("{path}.{key}"),
                format!(
                    "must be a list, such as [{}], not {}",
                    if key == "before" {
                        "today"
                    } else {
                        "notify_owner"
                    },
                    kind_of(other)
                ),
            );
            None
        }
    }
}

fn parse_a2a(value: Option<&Value>, path: &str, errors: &mut Vec<FieldError>) -> RoleA2a {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return RoleA2a::default();
    };
    let Some(map) = value.as_mapping() else {
        push(
            errors,
            path,
            format!(
                "must be a map such as {{ expose: false }}, not {}",
                kind_of(value)
            ),
        );
        return RoleA2a::default();
    };
    check_keys(map, &["expose"], path, errors);
    match map.get("expose") {
        None | Some(Value::Null) => RoleA2a::default(),
        Some(Value::Bool(expose)) => RoleA2a { expose: *expose },
        Some(other) => {
            push(
                errors,
                format!("{path}.expose"),
                format!("must be true or false, not {}", kind_of(other)),
            );
            RoleA2a::default()
        }
    }
}

/// The closest of `candidates` to `word`, if it is a plausible typo of it.
fn nearest<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let folded = |s: &str| s.to_ascii_lowercase().replace(['-', ' '], "_");
    let word = folded(word);
    candidates
        .iter()
        .map(|c| (edit_distance(&word, &folded(c)), *c))
        .filter(|(distance, c)| {
            let limit = match c.len() {
                0..=4 => 1,
                5..=8 => 2,
                _ => 3,
            };
            *distance <= limit
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, c)| c)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(prev + usize::from(ca != *cb));
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

// ── The validator ───────────────────────────────────────────────────────────

/// Every problem in a recipe's role block, and the role when there are none.
#[derive(Debug, Clone)]
pub struct RoleValidation {
    pub role: Option<AgentRole>,
    pub errors: Vec<FieldError>,
}

/// The validate route's answer.
#[derive(Debug, Serialize)]
pub struct ValidationReport<'a> {
    pub ok: bool,
    pub role: Option<RoleSummary<'a>>,
    pub errors: &'a [FieldError],
}

/// What the builder shows of a valid role.
#[derive(Debug, Serialize)]
pub struct RoleSummary<'a> {
    pub name: &'a str,
    pub tool_groups: &'a BTreeSet<String>,
    pub activations: &'a [Activation],
    pub hooks: &'a RoleHooks,
    /// The role's own preamble text, examples included; tool schemas are not counted.
    pub preamble_chars: usize,
}

impl<'a> RoleSummary<'a> {
    pub fn of(role: &'a AgentRole) -> Self {
        RoleSummary {
            name: role.name(),
            tool_groups: role.requested_tool_groups(),
            activations: role.activations(),
            hooks: role.hooks(),
            preamble_chars: role.instructions().chars().count(),
        }
    }
}

impl RoleValidation {
    pub fn ok(&self) -> bool {
        self.errors.is_empty() && self.role.is_some()
    }

    pub fn report(&self) -> ValidationReport<'_> {
        ValidationReport {
            ok: self.ok(),
            role: self
                .role
                .as_ref()
                .filter(|_| self.errors.is_empty())
                .map(RoleSummary::of),
            errors: &self.errors,
        }
    }
}

/// Checks a recipe's role block and collects every problem with its field's path. Stricter than
/// [`AgentRole::from_recipe_yaml`] where a value would parse but quietly do nothing (a tool group
/// an agent is never given, a hand-off to nobody), and never more lenient: whatever the parser
/// refuses, this refuses too.
pub fn validate_role_recipe(
    name: &str,
    yaml: &str,
    known_agents: Option<&BTreeSet<String>>,
) -> RoleValidation {
    let mut errors = Vec::new();
    let refs = References {
        own_name: Some(name),
        known_agents,
    };
    let doc: Value = match serde_yaml::from_str(yaml) {
        Ok(doc) => doc,
        Err(e) => {
            push(&mut errors, "", format!("the YAML does not parse: {e}"));
            return RoleValidation { role: None, errors };
        }
    };
    let Some(doc) = doc.as_mapping() else {
        push(
            &mut errors,
            "",
            format!("a recipe is a map of fields, not {}", kind_of(&doc)),
        );
        return RoleValidation { role: None, errors };
    };
    let block = match doc.get(ROLE_YAML_KEY) {
        None | Some(Value::Null) => {
            push(
                &mut errors,
                ROLE_YAML_KEY,
                "missing; an agent needs a `giap_role:` block with at least `tool_groups:`",
            );
            return RoleValidation { role: None, errors };
        }
        Some(Value::Mapping(block)) => block,
        Some(other) => {
            push(
                &mut errors,
                ROLE_YAML_KEY,
                format!("must be a map of fields, not {}", kind_of(other)),
            );
            return RoleValidation { role: None, errors };
        }
    };
    check_keys(block, ROLE_BLOCK_FIELDS, ROLE_YAML_KEY, &mut errors);
    check_limits(block, &mut errors);

    let has_text = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    if !has_text(block.get("instructions"))
        && !has_text(doc.get("instructions"))
        && !has_text(doc.get("prompt"))
    {
        push(
            &mut errors,
            format!("{ROLE_YAML_KEY}.instructions"),
            "missing; say in plain words what the agent is for (or give the recipe a prompt)",
        );
    }
    let extras = parse_extras(
        ExtrasInput {
            context: block.get("context"),
            activations: block.get("activations"),
            hooks: block.get("hooks"),
            a2a: block.get("a2a"),
        },
        &refs,
        &mut errors,
    );
    let denies_personal_data = block
        .get("personal_data")
        .and_then(Value::as_str)
        .is_some_and(|p| p.trim() == "deny");
    if denies_personal_data && extras.hooks.before.contains(&BeforeHook::Presence) {
        let index = block
            .get("hooks")
            .and_then(|h| h.get("before"))
            .and_then(Value::as_sequence)
            .and_then(|items| {
                items
                    .iter()
                    .position(|i| i.as_str().map(str::trim) == Some("presence"))
            })
            .unwrap_or(0);
        push(
            &mut errors,
            format!("{ROLE_YAML_KEY}.hooks.before[{index}]"),
            "this agent has `personal_data: deny`, so it is never shown who is home; remove \
             `presence` or let it inherit personal data",
        );
    }

    if !errors.is_empty() {
        return RoleValidation { role: None, errors };
    }
    match AgentRole::from_recipe_yaml(name, yaml) {
        Ok(Some(role)) => RoleValidation {
            role: Some(role),
            errors,
        },
        Ok(None) => {
            push(&mut errors, ROLE_YAML_KEY, "missing");
            RoleValidation { role: None, errors }
        }
        // The backstop: a refusal this walk did not foresee still reaches the person.
        Err(e) => {
            push(&mut errors, ROLE_YAML_KEY, e.to_string());
            RoleValidation { role: None, errors }
        }
    }
}

/// The limits `AgentRole::new` enforces, checked per field so each gets its own path.
fn check_limits(block: &Mapping, errors: &mut Vec<FieldError>) {
    let path = |key: &str| format!("{ROLE_YAML_KEY}.{key}");
    match block.get("tool_groups") {
        None | Some(Value::Null) => push(
            errors,
            path("tool_groups"),
            "missing; list the tool groups the agent may use, such as [giap-knowledge], or [] \
             for none",
        ),
        Some(Value::Sequence(groups)) => {
            let catalog: Vec<&str> = TOOL_GROUPS.iter().map(|g| g.extension).collect();
            for (i, group) in groups.iter().enumerate() {
                let at = format!("{}[{i}]", path("tool_groups"));
                let Some(group) = group.as_str().map(str::trim) else {
                    push(
                        errors,
                        at,
                        format!("must be a tool group's name, not {}", kind_of(group)),
                    );
                    continue;
                };
                // Other names may be user-added MCP servers; a `giap-` one can only be ours.
                if group.starts_with("giap-") && find_group(group).is_none() {
                    let mut message = format!("`{group}` is not a GIAP tool group");
                    if let Some(near) = nearest(group, &catalog) {
                        message.push_str(&format!("; did you mean `{near}`?"));
                    }
                    push(errors, at, message);
                } else if groups_denied_to_subagents().contains(&group) {
                    push(
                        errors,
                        at,
                        format!(
                            "an agent is never given `{group}`: it acts outside the pond or widens \
                             an agent's reach, and an agent has no approval path yet. Remove it"
                        ),
                    );
                }
            }
        }
        Some(other) => push(
            errors,
            path("tool_groups"),
            format!(
                "must be a list, such as [giap-knowledge], not {}",
                kind_of(other)
            ),
        ),
    }
    match block.get("personal_data") {
        None | Some(Value::Null) => {}
        Some(Value::String(p)) if matches!(p.trim(), "inherit" | "deny") => {}
        Some(other) => push(
            errors,
            path("personal_data"),
            format!("must be `inherit` or `deny`, not {}", describe(other)),
        ),
    }
    match block.get("instructions") {
        None | Some(Value::Null) | Some(Value::String(_)) => {}
        Some(other) => push(
            errors,
            path("instructions"),
            format!("must be text, not {}", kind_of(other)),
        ),
    }
    match block.get("max_turns") {
        None | Some(Value::Null) => {}
        Some(v) => match v.as_u64() {
            Some(n) if (1..=u64::from(MAX_ROLE_MAX_TURNS)).contains(&n) => {}
            _ => push(
                errors,
                path("max_turns"),
                format!(
                    "must be a whole number from 1 to {MAX_ROLE_MAX_TURNS}, not {}",
                    describe(v)
                ),
            ),
        },
    }
    match block.get("context_fraction") {
        None | Some(Value::Null) => {}
        Some(v) => match v.as_f64() {
            Some(x) if x.is_finite() && x > 0.0 && x <= 1.0 => {}
            _ => push(
                errors,
                path("context_fraction"),
                format!(
                    "must be a number above 0 and at most 1, not {}",
                    describe(v)
                ),
            ),
        },
    }
    match block.get("model") {
        None | Some(Value::Null) => {}
        Some(Value::String(m)) if !m.trim().is_empty() => {}
        Some(other) => push(
            errors,
            path("model"),
            format!(
                "must be a model's name; leave the key out to use the conversation's model, not {}",
                describe(other)
            ),
        ),
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::String(s) => format!("`{s}`"),
        Value::Number(n) => format!("`{n}`"),
        Value::Bool(b) => format!("`{b}`"),
        other => kind_of(other).to_string(),
    }
}

#[cfg(test)]
mod tests;
