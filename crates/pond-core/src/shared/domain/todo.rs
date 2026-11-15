//! A member's to-dos and reminders, the hooks that fire them, the places their phone fences, and
//! the events those hooks listen for. Everything here is pure: when a repeat next falls, what a
//! hook's sentence says, which event a hook matches and what Goose asks.

use chrono::{DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveTime, TimeZone, Utc};
pub use chrono_tz::Tz;
use serde::Serialize;
use serde_json::{json, Value};

mod edit;
pub use edit::{apply_patch, hook_drafts, new_todo};

/// Times Goose asks whether a to-do is done before it stops asking and leaves it open.
pub const MAX_ASKS: u32 = 3;
/// Hooks one to-do may carry.
pub const MAX_HOOKS: usize = 8;
pub const MAX_TEXT_CHARS: usize = 500;
pub const MAX_NOTES_CHARS: usize = 4000;
/// The longest snooze a not-yet may ask for: a week.
pub const MAX_SNOOZE_MINUTES: u32 = 7 * 24 * 60;
/// The local hour a follow-up without a snooze is asked at.
pub const FOLLOW_UP_HOUR: u32 = 9;
/// A follow-up without a snooze waits at least this long, so a not-yet at 08:55 is not asked
/// again at 09:00.
pub const MIN_FOLLOW_UP_HOURS: i64 = 3;
/// A hook fired by an event stays quiet this long, so a camera seeing someone for a minute is
/// one push, not twenty.
pub const EVENT_HOOK_COOLDOWN_MINUTES: i64 = 10;
/// An event older than this when it is read is past acting on: arriving home an hour ago is not
/// a reason to remind anyone now.
pub const STALE_EVENT_MINUTES: i64 = 30;
pub const MAX_PLACES_PER_MEMBER: usize = 50;
pub const DEFAULT_PLACE_RADIUS_M: f64 = 150.0;
pub const MIN_PLACE_RADIUS_M: f64 = 25.0;
pub const MAX_PLACE_RADIUS_M: f64 = 10_000.0;
pub const MAX_PLACE_LABEL_CHARS: usize = 80;

/// The timezone `name` names, else UTC: a to-do is never refused over an unreadable setting.
pub fn zone(name: &str) -> Tz {
    name.trim().parse().unwrap_or(chrono_tz::UTC)
}

/// The id of the one presence source: the household's home.
pub const PRESENCE_HOME: &str = "presence:home";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoKind {
    Todo,
    Reminder,
}

impl TodoKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Reminder => "reminder",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "todo" | "to-do" | "to_do" => Some(Self::Todo),
            "reminder" => Some(Self::Reminder),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoState {
    Open,
    Done,
    Dropped,
}

impl TodoState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Dropped => "dropped",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "done" => Some(Self::Done),
            "dropped" => Some(Self::Dropped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoSource {
    You,
    Chat,
    Goose,
    Idea,
}

impl TodoSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::You => "you",
            Self::Chat => "chat",
            Self::Goose => "goose",
            Self::Idea => "idea",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "you" => Some(Self::You),
            "chat" => Some(Self::Chat),
            "goose" => Some(Self::Goose),
            "idea" => Some(Self::Idea),
            _ => None,
        }
    }
}

/// Which to-dos a list read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateFilter {
    Open,
    Done,
    Dropped,
    All,
}

impl StateFilter {
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw.map(|r| r.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("open") => Some(Self::Open),
            Some("done") => Some(Self::Done),
            Some("dropped") => Some(Self::Dropped),
            Some("all") => Some(Self::All),
            _ => None,
        }
    }

    pub fn admits(self, state: TodoState) -> bool {
        match self {
            Self::All => true,
            Self::Open => state == TodoState::Open,
            Self::Done => state == TodoState::Done,
            Self::Dropped => state == TodoState::Dropped,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Every {
    Day,
    Weekday,
    Week,
    Month,
}

impl Every {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Weekday => "weekday",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "day" | "daily" => Some(Self::Day),
            "weekday" | "weekdays" => Some(Self::Weekday),
            "week" | "weekly" => Some(Self::Week),
            "month" | "monthly" => Some(Self::Month),
            _ => None,
        }
    }

    /// Whether `date` is one this repeat falls on. `days` are ISO weekdays (1 is Monday) for a
    /// week and days of the month for a month; a month day past the month's end falls on its
    /// last day, so "the 31st" still happens in February.
    pub fn includes(self, date: NaiveDate, days: &[u32]) -> bool {
        match self {
            Self::Day => true,
            Self::Weekday => date.weekday().number_from_monday() <= 5,
            Self::Week => days.contains(&date.weekday().number_from_monday()),
            Self::Month => {
                let last = last_day_of_month(date);
                days.iter()
                    .any(|&d| d == date.day() || (date.day() == last && d > last))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Arrive,
    Leave,
}

impl Transition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Arrive => "arrive",
            Self::Leave => "leave",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "arrive" | "arrived" | "enter" | "arrival" => Some(Self::Arrive),
            "leave" | "left" | "exit" | "departed" | "depart" => Some(Self::Leave),
            _ => None,
        }
    }
}

/// What fires a to-do, as the phone and the database spell it: a `kind` and its `spec`.
#[derive(Debug, Clone, PartialEq)]
pub enum HookSpec {
    Time {
        at: DateTime<Utc>,
    },
    Repeat {
        every: Every,
        at: NaiveTime,
        days: Vec<u32>,
    },
    Place {
        place_id: String,
        transition: Transition,
    },
    /// Something the Pond notices; `source_id` is a source's id from `GET /me/hooks/sources`.
    Event {
        source_id: String,
        event: String,
    },
    Connector {
        connector_id: String,
        event: String,
        filter: Option<String>,
    },
}

/// Why a hook was refused, worded for the member's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookError(pub String);

impl std::fmt::Display for HookError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HookError {}

fn refuse<T>(why: impl Into<String>) -> Result<T, HookError> {
    Err(HookError(why.into()))
}

fn spec_str<'a>(spec: &'a Value, key: &str) -> Option<&'a str> {
    spec.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl HookSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Time { .. } => "time",
            Self::Repeat { .. } => "repeat",
            Self::Place { .. } => "place",
            Self::Event { .. } => "event",
            Self::Connector { .. } => "connector",
        }
    }

    /// The spec as it is stored and shown.
    pub fn to_json(&self) -> Value {
        match self {
            Self::Time { at } => json!({"at": rfc3339(*at)}),
            Self::Repeat { every, at, days } => {
                let mut spec =
                    json!({"every": every.as_str(), "at": at.format("%H:%M").to_string()});
                if matches!(every, Every::Week | Every::Month) {
                    spec["days"] = json!(days);
                }
                spec
            }
            Self::Place {
                place_id,
                transition,
            } => json!({"place_id": place_id, "transition": transition.as_str()}),
            Self::Event { source_id, event } => json!({"source_id": source_id, "event": event}),
            Self::Connector {
                connector_id,
                event,
                filter,
            } => json!({"connector_id": connector_id, "event": event, "filter": filter}),
        }
    }

    /// Reads a hook's shape. Whether its place or source exists is [`resolve_hook`]'s question.
    pub fn parse(kind: &str, spec: &Value) -> Result<Self, HookError> {
        if !spec.is_object() {
            return refuse("a hook's spec must be an object");
        }
        match kind.trim().to_ascii_lowercase().as_str() {
            "time" => {
                let Some(raw) = spec_str(spec, "at") else {
                    return refuse("a time hook needs `at`, an RFC 3339 time");
                };
                match DateTime::parse_from_rfc3339(raw) {
                    Ok(at) => Ok(Self::Time {
                        at: at.with_timezone(&Utc),
                    }),
                    Err(_) => refuse(format!("`{raw}` is not an RFC 3339 time")),
                }
            }
            "repeat" => {
                let Some(every) = spec_str(spec, "every").and_then(Every::parse) else {
                    return refuse("a repeat needs `every`: day, weekday, week or month");
                };
                let Some(at) = spec_str(spec, "at").and_then(parse_clock) else {
                    return refuse("a repeat needs `at`, a time of day such as 18:00");
                };
                let days = match spec.get("days") {
                    None | Some(Value::Null) => Vec::new(),
                    Some(Value::Array(days)) => {
                        let mut out = Vec::new();
                        for day in days {
                            let Some(d) = day.as_u64() else {
                                return refuse("`days` must be numbers");
                            };
                            out.push(d as u32);
                        }
                        out
                    }
                    Some(_) => return refuse("`days` must be a list of numbers"),
                };
                let days = match every {
                    Every::Day | Every::Weekday => Vec::new(),
                    Every::Week => {
                        if days.iter().any(|d| !(1..=7).contains(d)) {
                            return refuse(
                                "a weekly repeat's days run from 1 (Monday) to 7 (Sunday)",
                            );
                        }
                        sorted_unique(days)
                    }
                    Every::Month => {
                        if days.iter().any(|d| !(1..=31).contains(d)) {
                            return refuse("a monthly repeat's days run from 1 to 31");
                        }
                        sorted_unique(days)
                    }
                };
                Ok(Self::Repeat { every, at, days })
            }
            "place" => {
                let Some(place_id) = spec_str(spec, "place_id") else {
                    return refuse("a place hook needs `place_id`");
                };
                let transition = match spec_str(spec, "transition") {
                    None => Transition::Arrive,
                    Some(raw) => match Transition::parse(raw) {
                        Some(t) => t,
                        None => return refuse("a place hook's transition is arrive or leave"),
                    },
                };
                Ok(Self::Place {
                    place_id: place_id.to_string(),
                    transition,
                })
            }
            "event" => {
                let (Some(source_id), Some(event)) =
                    (spec_str(spec, "source_id"), spec_str(spec, "event"))
                else {
                    return refuse("an event hook needs `source_id` and `event`");
                };
                Ok(Self::Event {
                    source_id: source_id.to_string(),
                    event: event.to_ascii_lowercase(),
                })
            }
            "connector" => {
                let (Some(connector_id), Some(event)) =
                    (spec_str(spec, "connector_id"), spec_str(spec, "event"))
                else {
                    return refuse("a connector hook needs `connector_id` and `event`");
                };
                Ok(Self::Connector {
                    connector_id: connector_id.to_string(),
                    event: event.to_ascii_lowercase(),
                    filter: spec_str(spec, "filter").map(str::to_string),
                })
            }
            other => refuse(format!(
                "`{other}` is not a hook kind: time, repeat, place, event or connector"
            )),
        }
    }

    /// The event this hook waits for; a time or a repeat waits for the clock instead.
    pub fn listens_for(&self) -> Option<EventKey> {
        match self {
            Self::Time { .. } | Self::Repeat { .. } => None,
            Self::Place {
                place_id,
                transition,
            } => Some(EventKey::new("place", place_id, transition.as_str())),
            Self::Event { source_id, event } => {
                let (kind, id) = split_source_id(source_id)?;
                Some(EventKey::new(kind.as_str(), &id, event))
            }
            Self::Connector {
                connector_id,
                event,
                ..
            } => Some(EventKey::new("connector", connector_id, event)),
        }
    }

    /// When a clock hook first fires, from `now`; `None` for a hook that waits for an event.
    pub fn first_fire(&self, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
        match self {
            Self::Time { at } => Some(*at),
            Self::Repeat { every, at, days } => next_repeat(*every, *at, days, now, tz),
            _ => None,
        }
    }

    /// When a clock hook fires after firing at `now`; `None` once it is spent.
    pub fn next_after_firing(&self, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
        match self {
            Self::Repeat { every, at, days } => next_repeat(*every, *at, days, now, tz),
            _ => None,
        }
    }

    /// Whether `event` is the one this hook waits for, for a to-do `owner` keeps.
    pub fn matches(&self, event: &HookEvent, owner: &str) -> bool {
        if event.profile_id.as_deref().is_some_and(|p| p != owner) {
            return false;
        }
        let Some(key) = self.listens_for() else {
            return false;
        };
        if !key.matches(&event.key()) {
            return false;
        }
        match self {
            Self::Connector {
                filter: Some(filter),
                connector_id,
                ..
            } => filter_admits(connector_id, filter, &event.payload),
            _ => true,
        }
    }
}

fn sorted_unique(mut days: Vec<u32>) -> Vec<u32> {
    days.sort_unstable();
    days.dedup();
    days
}

/// A clock time as people write it: `18:00`, `18:00:00`, `6pm`, `6:30 pm`.
pub fn parse_clock(raw: &str) -> Option<NaiveTime> {
    let lower = raw.trim().to_ascii_lowercase().replace(' ', "");
    for format in ["%H:%M", "%H:%M:%S", "%H.%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(&lower, format) {
            return Some(t);
        }
    }
    let (body, pm) = if let Some(b) = lower.strip_suffix("pm") {
        (b, true)
    } else if let Some(b) = lower.strip_suffix("am") {
        (b, false)
    } else {
        return None;
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&h) {
        return None;
    }
    let hour = match (h, pm) {
        (12, false) => 0,
        (12, true) => 12,
        (h, true) => h + 12,
        (h, false) => h,
    };
    NaiveTime::from_hms_opt(hour, m, 0)
}

/// The source family and id a source id names: `camera:front` is camera `front`. Connector ids
/// (`mail:…`, `calendar:…`, `webhook:…`) are whole, as the connector queue spells them.
pub fn split_source_id(source_id: &str) -> Option<(SourceKind, String)> {
    let (prefix, rest) = source_id.split_once(':')?;
    if rest.trim().is_empty() {
        return None;
    }
    if CONNECTOR_PREFIXES.contains(&prefix) {
        return Some((SourceKind::Connector, source_id.to_string()));
    }
    let kind = SourceKind::parse(prefix)?;
    (kind != SourceKind::Connector).then(|| (kind, rest.to_string()))
}

/// Connector families the Pond knows; a connector added later keeps its own prefix.
pub const CONNECTOR_PREFIXES: &[&str] = &["mail", "calendar", "webhook"];

/// A connector id compared by what it names: `mail:<context source id>` where the context
/// source's own id already begins `mail:` is the same mailbox with or without the repeat.
fn connector_key(id: &str) -> &str {
    for prefix in ["mail:", "calendar:"] {
        if let Some(rest) = id.strip_prefix(prefix) {
            if rest.starts_with(prefix) {
                return rest;
            }
        }
    }
    id
}

/// Whether a connector event's payload has `filter` where that connector keeps it: a mail's
/// sender, a calendar event's title, anywhere in a webhook's body.
fn filter_admits(connector_id: &str, filter: &str, payload: &Value) -> bool {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let field = |keys: &[&str]| {
        keys.iter()
            .filter_map(|k| payload.get(*k).and_then(Value::as_str))
            .any(|v| v.to_lowercase().contains(&needle))
    };
    if connector_id.starts_with("mail:") {
        return field(&["from", "sender"]);
    }
    if connector_id.starts_with("calendar:") {
        return field(&["title", "summary", "subject"]);
    }
    payload.to_string().to_lowercase().contains(&needle)
}

/// A family, an id within it and an event: what a hook waits for and what a producer raised.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventKey {
    pub kind: String,
    pub id: String,
    pub event: String,
}

impl EventKey {
    pub fn new(kind: &str, id: &str, event: &str) -> Self {
        let kind = kind.trim().to_ascii_lowercase();
        let id = if kind == "connector" {
            connector_key(id.trim()).to_string()
        } else {
            id.trim().to_string()
        };
        Self {
            kind,
            id,
            event: event.trim().to_ascii_lowercase(),
        }
    }

    pub fn matches(&self, other: &EventKey) -> bool {
        self == other
    }
}

/// One row of the `hook_events` queue.
#[derive(Debug, Clone, PartialEq)]
pub struct HookEvent {
    pub id: String,
    /// The member it is about; `None` is a household event any member's hook may match.
    pub profile_id: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    pub event: String,
    pub payload: Value,
    pub at: DateTime<Utc>,
}

impl HookEvent {
    pub fn new(
        profile_id: Option<&str>,
        source_kind: &str,
        source_id: &str,
        event: &str,
        payload: Value,
        at: DateTime<Utc>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.map(str::to_string),
            source_kind: source_kind.to_string(),
            source_id: source_id.to_string(),
            event: event.to_string(),
            payload,
            at,
        }
    }

    pub fn key(&self) -> EventKey {
        EventKey::new(&self.source_kind, &self.source_id, &self.event)
    }
}

/// One hook as kept: its spec, the sentence the phone shows, and its clock.
#[derive(Debug, Clone, PartialEq)]
pub struct Hook {
    pub id: String,
    pub spec: HookSpec,
    pub label: String,
    pub next_fire_at: Option<DateTime<Utc>>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Hook {
    /// A new hook, its clock set from `now`.
    pub fn new(spec: HookSpec, label: String, now: DateTime<Utc>, tz: Tz) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            next_fire_at: spec.first_fire(now, tz),
            spec,
            label,
            last_fired_at: None,
            created_at: now,
        }
    }

    /// Whether an event may fire it now, or it fired too recently.
    pub fn cooled_down(&self, now: DateTime<Utc>) -> bool {
        self.last_fired_at
            .is_none_or(|at| now - at >= Duration::minutes(EVENT_HOOK_COOLDOWN_MINUTES))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Todo {
    pub id: String,
    pub owner: String,
    pub kind: TodoKind,
    pub text: String,
    pub notes: Option<String>,
    pub state: TodoState,
    pub due_at: Option<DateTime<Utc>>,
    pub ask: bool,
    pub source: TodoSource,
    pub source_ref: Option<String>,
    /// When Goose last pushed about it, a reminder or a question.
    pub last_asked_at: Option<DateTime<Utc>>,
    /// Questions asked; reminders do not count.
    pub asked: u32,
    /// When Goose asks again after a not-yet.
    pub snoozed_until: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub done_at: Option<DateTime<Utc>>,
    pub hooks: Vec<Hook>,
}

/// What a push about a to-do is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushKind {
    /// "Reminder: …", for a reminder or a to-do Goose need not ask about.
    Reminder,
    /// Goose's question, answered Done or Not yet on the phone.
    CheckIn,
}

impl Todo {
    /// What a firing pushes now; `None` once it is closed, or once Goose has asked enough.
    pub fn push_kind(&self) -> Option<PushKind> {
        if self.state != TodoState::Open {
            return None;
        }
        if self.kind == TodoKind::Reminder || !self.ask {
            return Some(PushKind::Reminder);
        }
        (self.asked < MAX_ASKS).then_some(PushKind::CheckIn)
    }

    /// Whether `asked` is this same request made again: a turn re-run after a stop, or the tool
    /// called twice. One that has fired is not, so "again in ten minutes" still stands.
    pub fn is_repeated_by(&self, asked: &Todo) -> bool {
        const MADE_WITHIN_MINUTES: i64 = 60;
        const SAME_MOMENT_SECONDS: i64 = 120;
        let same_moment = match (self.due_at, asked.due_at) {
            (None, None) => true,
            (Some(kept), Some(new)) => (kept - new).num_seconds().abs() <= SAME_MOMENT_SECONDS,
            _ => false,
        };
        self.state == TodoState::Open
            && self.owner == asked.owner
            && self.kind == asked.kind
            && self.text.trim().eq_ignore_ascii_case(asked.text.trim())
            && self.hooks.len() == asked.hooks.len()
            && self.hooks.iter().all(|hook| hook.last_fired_at.is_none())
            && same_moment
            && (asked.created_at - self.created_at).num_minutes().abs() <= MADE_WITHIN_MINUTES
    }

    /// Whether its due time stands in for a time hook and has come without a push since.
    pub fn due_without_hooks(&self, now: DateTime<Utc>) -> bool {
        self.state == TodoState::Open
            && self.hooks.is_empty()
            && self
                .due_at
                .is_some_and(|due| due <= now && self.last_asked_at.is_none_or(|last| last < due))
    }

    /// The member's answer to a check-in, applied. A not-yet asks again at the snooze, else
    /// the next morning; past [`MAX_ASKS`] it stops asking and leaves the to-do open.
    pub fn answer(
        &mut self,
        answer: Answer,
        snooze_minutes: Option<u32>,
        now: DateTime<Utc>,
        tz: Tz,
    ) -> Result<(), AnswerRefused> {
        match answer {
            Answer::Done => {
                if self.state != TodoState::Done {
                    self.state = TodoState::Done;
                    self.done_at = Some(now);
                }
                self.snoozed_until = None;
            }
            Answer::Drop => {
                self.state = TodoState::Dropped;
                self.done_at = None;
                self.snoozed_until = None;
            }
            Answer::NotYet => {
                if self.state != TodoState::Open {
                    return Err(AnswerRefused::Closed(self.state));
                }
                if let Some(m) = snooze_minutes {
                    if m == 0 || m > MAX_SNOOZE_MINUTES {
                        return Err(AnswerRefused::Snooze);
                    }
                }
                let asking_ends = self.kind == TodoKind::Todo && self.ask && self.asked >= MAX_ASKS;
                self.snoozed_until = if asking_ends {
                    None
                } else {
                    Some(follow_up_at(now, tz, snooze_minutes))
                };
            }
        }
        self.updated_at = now;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Done,
    NotYet,
    Drop,
}

impl Answer {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "done" => Some(Self::Done),
            "not_yet" | "notyet" | "not-yet" | "later" => Some(Self::NotYet),
            "drop" | "dropped" => Some(Self::Drop),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnswerRefused {
    /// A not-yet for a to-do already done or dropped.
    Closed(TodoState),
    /// A snooze of zero minutes or longer than [`MAX_SNOOZE_MINUTES`].
    Snooze,
}

/// When Goose asks again after a not-yet: at the snooze, else at [`FOLLOW_UP_HOUR`] member
/// time on the first morning at least [`MIN_FOLLOW_UP_HOURS`] away.
pub fn follow_up_at(now: DateTime<Utc>, tz: Tz, snooze_minutes: Option<u32>) -> DateTime<Utc> {
    if let Some(m) = snooze_minutes {
        return now + Duration::minutes(i64::from(m));
    }
    let morning = NaiveTime::from_hms_opt(FOLLOW_UP_HOUR, 0, 0).expect("a valid hour");
    let earliest = now + Duration::hours(MIN_FOLLOW_UP_HOURS);
    let start = now.with_timezone(&tz).date_naive();
    (0..3)
        .filter_map(|d| start.checked_add_days(chrono::Days::new(d)))
        .filter_map(|date| local_instant(tz, date, morning))
        .find(|t| *t >= earliest)
        .unwrap_or(now + Duration::hours(24))
}

/// A place a member named, which their phone fences.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub id: String,
    pub owner: String,
    pub label: String,
    pub lat: f64,
    pub lng: f64,
    pub radius_m: f64,
    pub created_at: DateTime<Utc>,
}

impl Place {
    /// A new place, or why its fields are refused. A missing radius is the default.
    pub fn new(
        owner: &str,
        label: &str,
        lat: f64,
        lng: f64,
        radius_m: Option<f64>,
        now: DateTime<Utc>,
    ) -> Result<Self, HookError> {
        let label = label.trim();
        if label.is_empty() {
            return refuse("a place needs a label");
        }
        if label.chars().count() > MAX_PLACE_LABEL_CHARS {
            return refuse(format!(
                "a place's label is at most {MAX_PLACE_LABEL_CHARS} characters"
            ));
        }
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lng) {
            return refuse("lat must be between -90 and 90, lng between -180 and 180");
        }
        let radius_m = radius_m.unwrap_or(DEFAULT_PLACE_RADIUS_M);
        if !(MIN_PLACE_RADIUS_M..=MAX_PLACE_RADIUS_M).contains(&radius_m) {
            return refuse(format!(
                "radius_m must be between {MIN_PLACE_RADIUS_M} and {MAX_PLACE_RADIUS_M} metres"
            ));
        }
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            owner: owner.to_string(),
            label: label.to_string(),
            lat,
            lng,
            radius_m,
            created_at: now,
        })
    }

    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "lat": self.lat,
            "lng": self.lng,
            "radius_m": self.radius_m,
        })
    }
}

/// A family of things a hook may listen to, as `GET /me/hooks/sources` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Presence,
    Camera,
    Device,
    Sensor,
    WebWatch,
    Cart,
    Connector,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Presence => "presence",
            Self::Camera => "camera",
            Self::Device => "device",
            Self::Sensor => "sensor",
            Self::WebWatch => "web_watch",
            Self::Cart => "cart",
            Self::Connector => "connector",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "presence" => Some(Self::Presence),
            "camera" => Some(Self::Camera),
            "device" => Some(Self::Device),
            "sensor" => Some(Self::Sensor),
            "web_watch" => Some(Self::WebWatch),
            "cart" => Some(Self::Cart),
            "connector" => Some(Self::Connector),
            _ => None,
        }
    }

    /// The events a hook on this family may wait for; `None` admits any named event.
    fn events(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Presence => Some(&["arrive", "leave"]),
            Self::Camera => Some(&["person", "motion"]),
            Self::Device => Some(&["on", "off"]),
            Self::WebWatch => Some(&["match"]),
            Self::Cart => Some(&[
                "price_drop",
                "target_reached",
                "back_in_stock",
                "price_change",
            ]),
            Self::Sensor | Self::Connector => None,
        }
    }

    /// Whether only one member's rows hold it, so a hook may name only the caller's own.
    fn is_members_own(self) -> bool {
        matches!(self, Self::WebWatch | Self::Cart | Self::Connector)
    }
}

/// One event a source raises, as the hook picker lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceEvent {
    pub id: String,
    pub label: String,
}

/// Something on this Pond a hook may listen to.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HookSource {
    pub id: String,
    pub kind: SourceKind,
    pub label: String,
    pub events: Vec<SourceEvent>,
    /// The noun a hook's sentence uses for it: "the Fielder on Kilimall", "Gmail".
    #[serde(skip)]
    pub name: String,
}

fn events(pairs: &[(&str, &str)]) -> Vec<SourceEvent> {
    pairs
        .iter()
        .map(|(id, label)| SourceEvent {
            id: (*id).to_string(),
            label: (*label).to_string(),
        })
        .collect()
}

impl HookSource {
    pub fn presence() -> Self {
        Self {
            id: PRESENCE_HOME.to_string(),
            kind: SourceKind::Presence,
            label: "You, at home".to_string(),
            events: events(&[("arrive", "You get home"), ("leave", "You leave home")]),
            name: "home".to_string(),
        }
    }

    pub fn camera(camera_id: &str) -> Self {
        let name = plain_name(camera_id);
        Self {
            id: format!("camera:{camera_id}"),
            kind: SourceKind::Camera,
            label: name.clone(),
            events: events(&[("person", "Sees a person"), ("motion", "Sees movement")]),
            name,
        }
    }

    pub fn device(device_id: &str, name: &str) -> Self {
        let name = if name.trim().is_empty() {
            plain_name(device_id)
        } else {
            name.trim().to_string()
        };
        Self {
            id: format!("device:{device_id}"),
            kind: SourceKind::Device,
            label: name.clone(),
            events: events(&[("on", "Turns on"), ("off", "Turns off")]),
            name,
        }
    }

    pub fn web_watch(watch_id: &str, title: &str, address: &str) -> Self {
        let site = site_name(address);
        let title = short_title(title);
        let label = match (title.is_empty(), site.is_empty()) {
            (true, _) => site.clone(),
            (false, true) => title.clone(),
            (false, false) => format!("{title} on {site}"),
        };
        Self {
            id: format!("web_watch:{watch_id}"),
            kind: SourceKind::WebWatch,
            label,
            events: events(&[("match", "Finds a match")]),
            name: if site.is_empty() { title } else { site },
        }
    }

    pub fn cart(cart_id: &str, title: &str, site: &str, url: &str) -> Self {
        let site = if site.trim().is_empty() {
            site_name(url)
        } else {
            site_name(site)
        };
        let title = short_title(title);
        let name = match (title.is_empty(), site.is_empty()) {
            (true, true) => "a watched link".to_string(),
            (true, false) => format!("the link on {site}"),
            (false, true) => format!("the {title}"),
            (false, false) => format!("the {title} on {site}"),
        };
        let label = name
            .strip_prefix("the ")
            .map(str::to_string)
            .unwrap_or_else(|| name.clone());
        Self {
            id: format!("cart:{cart_id}"),
            kind: SourceKind::Cart,
            label: capitalised(&label),
            events: events(&[
                ("price_drop", "Drops in price"),
                ("target_reached", "Reaches your price"),
                ("back_in_stock", "Is back in stock"),
            ]),
            name,
        }
    }

    /// A mail account the member connected; `context_source_id` is its `context_sources` id.
    pub fn mail(context_source_id: &str, provider: &str) -> Self {
        let provider = provider_name(provider);
        Self {
            id: format!("mail:{context_source_id}"),
            kind: SourceKind::Connector,
            label: format!("Mail ({provider})"),
            events: events(&[("new_mail", "A new mail arrives")]),
            name: provider,
        }
    }

    pub fn calendar(context_source_id: &str, provider: &str) -> Self {
        let provider = provider_name(provider);
        Self {
            id: format!("calendar:{context_source_id}"),
            kind: SourceKind::Connector,
            label: format!("Calendar ({provider})"),
            events: events(&[
                ("event_added", "An event is added"),
                ("event_starting", "An event is about to start"),
            ]),
            name: provider,
        }
    }

    pub fn webhook(webhook_id: &str, name: &str) -> Self {
        let name = name.trim().to_string();
        Self {
            id: format!("webhook:{webhook_id}"),
            kind: SourceKind::Connector,
            label: name.clone(),
            events: events(&[("call", "Sends an event")]),
            name,
        }
    }

    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind.as_str(),
            "label": self.label,
            "events": self.events,
        })
    }
}

/// What a hook may name: the member's places and the sources this Pond has.
#[derive(Debug, Clone, Default)]
pub struct HookCatalog {
    pub places: Vec<Place>,
    pub sources: Vec<HookSource>,
}

impl HookCatalog {
    fn source(&self, id: &str) -> Option<&HookSource> {
        let key = connector_key(id);
        self.sources
            .iter()
            .find(|s| s.id == id || connector_key(&s.id) == key)
    }
}

/// A hook the phone or the model sent, checked against what this member may name, with its
/// sentence written in their timezone.
pub fn resolve_hook(
    kind: &str,
    spec: &Value,
    catalog: &HookCatalog,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<Hook, HookError> {
    let mut spec = HookSpec::parse(kind, spec)?;
    // A weekly or monthly repeat with no days falls on today's, as a person saying "every week"
    // means.
    if let HookSpec::Repeat { every, days, .. } = &mut spec {
        if days.is_empty() {
            let today = now.with_timezone(&tz).date_naive();
            match every {
                Every::Week => days.push(today.weekday().number_from_monday()),
                Every::Month => days.push(today.day()),
                Every::Day | Every::Weekday => {}
            }
        }
    }
    let label = match &spec {
        HookSpec::Time { .. } | HookSpec::Repeat { .. } => clock_label(&spec, now, tz),
        HookSpec::Place {
            place_id,
            transition,
        } => {
            let Some(place) = catalog.places.iter().find(|p| &p.id == place_id) else {
                return refuse("no such place: add it first with POST /me/places");
            };
            place_sentence(&place.label, *transition)
        }
        HookSpec::Event { source_id, event } => {
            let Some((family, id)) = split_source_id(source_id) else {
                return refuse(format!(
                    "`{source_id}` is not a source id from GET /me/hooks/sources"
                ));
            };
            if let Some(allowed) = family.events() {
                if !allowed.contains(&event.as_str()) {
                    return refuse(format!(
                        "a {} source raises {}, not `{event}`",
                        family.as_str(),
                        allowed.join(", ")
                    ));
                }
            }
            if family == SourceKind::Presence && source_id != PRESENCE_HOME {
                return refuse(format!("presence is `{PRESENCE_HOME}`, not `{source_id}`"));
            }
            let known = catalog.source(source_id);
            if known.is_none() && family.is_members_own() {
                return refuse(format!("no such source: `{source_id}`"));
            }
            let name = known.map_or_else(|| plain_name(&id), |s| s.name.clone());
            match family {
                SourceKind::Connector => connector_sentence(source_id, &name, event, None),
                _ => event_sentence(family, &name, event),
            }
        }
        HookSpec::Connector {
            connector_id,
            event,
            filter,
        } => {
            let Some(known) = catalog.source(connector_id) else {
                return refuse(format!("no such connector: `{connector_id}`"));
            };
            if !known.events.iter().any(|e| &e.id == event) {
                let ids: Vec<&str> = known.events.iter().map(|e| e.id.as_str()).collect();
                return refuse(format!(
                    "{} raises {}, not `{event}`",
                    known.label,
                    ids.join(", ")
                ));
            }
            connector_sentence(connector_id, &known.name, event, filter.as_deref())
        }
    };
    Ok(Hook::new(spec, label, now, tz))
}

/// The local instant `time` falls at on `date`. An hour skipped by a clock change starts at the
/// first minute after the gap; an hour lived twice is its first pass.
pub fn local_instant(tz: Tz, date: NaiveDate, time: NaiveTime) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        LocalResult::Ambiguous(first, _) => Some(first.with_timezone(&Utc)),
        LocalResult::None => (1..=180).find_map(|m| {
            tz.from_local_datetime(&(naive + Duration::minutes(m)))
                .earliest()
                .map(|t| t.with_timezone(&Utc))
        }),
    }
}

/// The first time a repeat falls strictly after `after`, in `tz`.
pub fn next_repeat(
    every: Every,
    at: NaiveTime,
    days: &[u32],
    after: DateTime<Utc>,
    tz: Tz,
) -> Option<DateTime<Utc>> {
    if matches!(every, Every::Week | Every::Month) && days.is_empty() {
        return None;
    }
    let start = after.with_timezone(&tz).date_naive();
    (0..=400)
        .filter_map(|d| start.checked_add_days(chrono::Days::new(d)))
        .filter(|date| every.includes(*date, days))
        .filter_map(|date| local_instant(tz, date, at))
        .find(|t| *t > after)
}

fn last_day_of_month(date: NaiveDate) -> u32 {
    let (y, m) = if date.month() == 12 {
        (date.year() + 1, 1)
    } else {
        (date.year(), date.month() + 1)
    };
    NaiveDate::from_ymd_opt(y, m, 1)
        .and_then(|d| d.pred_opt())
        .map_or(28, |d| d.day())
}

pub fn rfc3339(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

fn listed(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

/// The sentence for a time or a repeat, in the member's timezone.
pub fn clock_label(spec: &HookSpec, now: DateTime<Utc>, tz: Tz) -> String {
    match spec {
        HookSpec::Time { at } => {
            let local = at.with_timezone(&tz);
            let year = if local.year() == now.with_timezone(&tz).year() {
                String::new()
            } else {
                format!(" {}", local.year())
            };
            format!(
                "On {} {} {}{year} at {}",
                local.format("%A"),
                local.day(),
                local.format("%B"),
                local.format("%H:%M")
            )
        }
        HookSpec::Repeat { every, at, days } => {
            let at = at.format("%H:%M");
            match every {
                Every::Day => format!("Every day at {at}"),
                Every::Weekday => format!("Every weekday at {at}"),
                Every::Week if days.len() == 7 => format!("Every day at {at}"),
                Every::Week if days.as_slice() == [1, 2, 3, 4, 5] => {
                    format!("Every weekday at {at}")
                }
                Every::Week => {
                    let names: Vec<String> = days
                        .iter()
                        .filter_map(|d| WEEKDAYS.get((*d as usize).wrapping_sub(1)))
                        .map(|d| (*d).to_string())
                        .collect();
                    format!("Every {} at {at}", listed(&names))
                }
                Every::Month => {
                    let names: Vec<String> = days.iter().map(|d| ordinal(*d)).collect();
                    format!("Every month on the {} at {at}", listed(&names))
                }
            }
        }
        _ => String::new(),
    }
}

/// "When you get home", "When you get to the gym", "When you leave work".
pub fn place_sentence(label: &str, transition: Transition) -> String {
    let lower = label.trim().to_lowercase();
    let place = match lower.as_str() {
        "home" | "house" | "my house" | "my home" | "nyumbani" => "home".to_string(),
        "work" | "school" | "church" | "college" | "campus" | "university" | "uni" => lower,
        "gym" | "office" | "shop" | "market" | "garage" | "mall" | "supermarket" | "hospital"
        | "airport" | "station" | "bank" | "pharmacy" | "clinic" => format!("the {lower}"),
        _ => label.trim().to_string(),
    };
    match (transition, place.as_str()) {
        (Transition::Arrive, "home") => "When you get home".to_string(),
        (Transition::Leave, "home") => "When you leave home".to_string(),
        (Transition::Arrive, _) => format!("When you get to {place}"),
        (Transition::Leave, _) => format!("When you leave {place}"),
    }
}

fn event_sentence(family: SourceKind, name: &str, event: &str) -> String {
    match (family, event) {
        (SourceKind::Presence, "arrive") => "When you get home".to_string(),
        (SourceKind::Presence, _) => "When you leave home".to_string(),
        (SourceKind::Camera, "person") => format!("When {name} sees a person"),
        (SourceKind::Camera, "motion") => format!("When {name} sees movement"),
        (SourceKind::Device, "on") => format!("When {name} turns on"),
        (SourceKind::Device, "off") => format!("When {name} turns off"),
        (SourceKind::WebWatch, _) => format!("When your watch on {name} finds a match"),
        (SourceKind::Cart, "price_drop") => format!("When {name} drops in price"),
        (SourceKind::Cart, "target_reached") => format!("When {name} reaches your price"),
        (SourceKind::Cart, "back_in_stock") => format!("When {name} is back in stock"),
        (SourceKind::Cart, _) => format!("When the price of {name} changes"),
        (_, event) => format!("When {name} reports {}", event.replace('_', " ")),
    }
}

fn connector_sentence(id: &str, name: &str, event: &str, filter: Option<&str>) -> String {
    let filter = filter.map(str::trim).filter(|f| !f.is_empty());
    if id.starts_with("mail:") {
        return match filter {
            Some(from) => format!("When a mail from {from} arrives"),
            None => format!("When a new mail arrives in {name}"),
        };
    }
    if id.starts_with("calendar:") {
        return match (event, filter) {
            ("event_starting", Some(title)) => {
                format!("When an event called \"{title}\" is about to start")
            }
            ("event_starting", None) => {
                format!("When an event on your {name} calendar is about to start")
            }
            (_, Some(title)) => format!("When an event called \"{title}\" is added"),
            (_, None) => format!("When an event is added to your {name} calendar"),
        };
    }
    let said = if id.starts_with("webhook:") {
        format!("When {name} sends an event")
    } else {
        format!("When {name} reports {}", event.replace('_', " "))
    };
    match filter {
        Some(word) => format!("{said} mentioning \"{word}\""),
        None => said,
    }
}

/// An id as a name: `camera-1` is "Camera 1".
fn plain_name(id: &str) -> String {
    capitalised(id.replace(['-', '_'], " ").trim())
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// A site as people say it: `https://www.kilimall.co.ke/x` is "Kilimall".
pub fn site_name(address: &str) -> String {
    let trimmed = address.trim();
    let host = trimmed
        .split_once("://")
        .map_or(trimmed, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .trim_start_matches("www.")
        .trim_start_matches("m.");
    let first = host.split('.').next().unwrap_or_default();
    capitalised(first)
}

/// A product or page title cut to what names it: before any " | " or " - ", at most six words.
pub fn short_title(title: &str) -> String {
    let mut cut = title.trim();
    for separator in [" | ", " - ", " – ", " — ", ": ", ", ", " ("] {
        if let Some((head, _)) = cut.split_once(separator) {
            if !head.trim().is_empty() {
                cut = head.trim();
            }
        }
    }
    cut.split_whitespace().take(6).collect::<Vec<_>>().join(" ")
}

fn provider_name(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "gmail" => "Gmail".into(),
        "google" => "Google".into(),
        "outlook" | "office365" | "microsoft" => "Outlook".into(),
        "icloud" | "apple" => "iCloud".into(),
        "fastmail" => "Fastmail".into(),
        "yahoo" => "Yahoo".into(),
        "nextcloud" => "Nextcloud".into(),
        "proton" | "protonmail" => "Proton".into(),
        "" | "custom" => "your own server".into(),
        other => capitalised(other),
    }
}

/// Words a to-do starts with that make it an instruction, so its question is "Did you …?".
const VERBS: &[&str] = &[
    "apply", "ask", "book", "bring", "buy", "call", "cancel", "change", "charge", "check", "clean",
    "clear", "collect", "confirm", "contact", "cook", "deposit", "do", "download", "drop", "email",
    "feed", "file", "fill", "finish", "fix", "follow", "get", "give", "go", "invite", "learn",
    "look", "mail", "make", "meet", "message", "move", "order", "pack", "pay", "phone", "pick",
    "plan", "post", "practise", "practice", "prepare", "print", "put", "read", "record",
    "register", "renew", "reply", "research", "reserve", "return", "review", "ring", "schedule",
    "see", "sell", "send", "service", "set", "share", "sign", "sort", "start", "study", "submit",
    "take", "talk", "tell", "test", "text", "tidy", "top", "try", "update", "upload", "visit",
    "wash", "watch", "water", "whatsapp", "write",
];

/// The member's own words turned to face them: "my" is "your".
fn to_second_person(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let (core, tail) = word.split_at(
                word.find(|c: char| !c.is_alphanumeric() && c != '\'')
                    .unwrap_or(word.len()),
            );
            let swapped = match core.to_lowercase().as_str() {
                "my" => "your",
                "me" => "you",
                "mine" => "yours",
                "myself" => "yourself",
                "i" => "you",
                "i'm" => "you're",
                _ => return word.to_string(),
            };
            format!("{swapped}{tail}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Goose's check-in question for a to-do: "Did you call the garage about the Fielder?".
pub fn question_for(text: &str) -> String {
    let mut body = text
        .trim()
        .trim_end_matches(['.', '!', '?', ';', ','])
        .trim();
    for lead in [
        "remember to ",
        "remind me to ",
        "i need to ",
        "i have to ",
        "i must ",
        "need to ",
        "have to ",
        "to ",
    ] {
        if body.len() > lead.len() && body[..lead.len()].eq_ignore_ascii_case(lead) {
            body = body[lead.len()..].trim_start();
            break;
        }
    }
    let first = body.split_whitespace().next().unwrap_or_default();
    let verb = first
        .trim_matches(|c: char| !c.is_alphabetic())
        .to_lowercase();
    if VERBS.contains(&verb.as_str()) {
        let rest = &body[first.len()..];
        return format!(
            "Did you {}{}?",
            first.to_lowercase(),
            to_second_person(rest)
        );
    }
    format!("Did you get to this: {}?", to_second_person(body))
}

/// What the event bus raised, as a row for the hook queue; `None` for what no hook listens to.
pub fn hook_event_from_bus(
    event: &crate::shared::ports::event_bus::BusEvent,
    now: DateTime<Utc>,
) -> Option<HookEvent> {
    use crate::shared::domain::session_activity::PresenceTransition;
    use crate::shared::ports::event_bus::BusEvent;
    use crate::user_data::domain::device::{DeviceStateValue, POWER_KEY};
    match event {
        BusEvent::Presence(p) => {
            let transition = match p.transition {
                PresenceTransition::Arrived => Transition::Arrive,
                PresenceTransition::Departed => Transition::Leave,
            };
            Some(HookEvent::new(
                Some(&p.profile_id),
                "presence",
                "home",
                transition.as_str(),
                json!({"source": p.source.as_str()}),
                now,
            ))
        }
        BusEvent::Camera(c) => {
            let kind = c.event_type.trim().to_ascii_lowercase();
            matches!(kind.as_str(), "person" | "motion").then(|| {
                HookEvent::new(
                    None,
                    "camera",
                    &c.camera_id,
                    &kind,
                    json!({"confidence": c.confidence}),
                    now,
                )
            })
        }
        BusEvent::Device(d) if d.key == POWER_KEY => match d.value {
            DeviceStateValue::Bool(on) => Some(HookEvent::new(
                None,
                "device",
                d.device_id.as_str(),
                if on { "on" } else { "off" },
                json!({}),
                now,
            )),
            _ => None,
        },
        // Only a boolean reading turning true is an event; a temperature is a measurement.
        BusEvent::Sensor(r) if r.unit.eq_ignore_ascii_case("bool") && r.value >= 0.5 => {
            Some(HookEvent::new(
                None,
                "sensor",
                &r.device_id,
                &r.sensor_type.trim().to_ascii_lowercase(),
                json!({}),
                now,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
