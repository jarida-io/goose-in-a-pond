//! Suggestions: questions the household could ask now, phrased only from facts the pond holds.
//!
//! Unlike a proposal, a suggestion stages no action (the tap is the consent), so it needs no
//! audience, expiry or daily cap. Derived on read, never stored; no suggestor spends inference.

use crate::user_data::domain::suggestion::SuggestionContext;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ── What the household may be shown ─────────────────────────────────────────

/// Who is looking: may personal facts be shown? Every scope but `Guest` is `Personal`, as
/// `identity_resolution::resolve` yields `Household` only on a pond of at most one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// One member, or a household of one. Personal facts are theirs to see.
    Personal,
    /// An unidentified speaker on a multi-member pond: house facts only, never a person's.
    Shared,
}

impl Audience {
    /// Whether a suggestor drawing on one person's data may run.
    pub fn may_see_personal(self) -> bool {
        matches!(self, Audience::Personal)
    }
}

/// Tool-group names, matching `TOOL_GROUPS` in [`tool_group`](crate::mcp::domain::tool_group).
pub const GROUP_CONTEXT: &str = "giap-context";
pub const GROUP_SCHEDULE: &str = "giap-schedule";
pub const GROUP_MEMORY: &str = "giap-memory";
pub const GROUP_DEVICE: &str = "giap-device";
pub const GROUP_WEATHER: &str = "giap-weather";

/// One thing the household might want to ask.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    /// The suggestor's id, not a hash of the fact, so a mute keeps matching as the number moves.
    pub id: String,
    /// The card's text and the prompt sent on tap, as one string so the two can't disagree.
    pub prompt: String,
    /// The fact that produced it, with the number in it. Never blank.
    pub because: String,
    /// The tool group that can answer [`Self::prompt`].
    pub answered_by: &'static str,
    /// Grounding context passed to the model when the suggestion is tapped.
    pub context: Option<SuggestionContext>,
}

/// One suggestor's outcome, returned even when silent so "broken" and "nothing to say" differ.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Considered {
    pub id: String,
    /// `None` when it offered something.
    pub silent_because: Option<String>,
}

/// What [`suggest`] produced, and what it considered.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuggestionSet {
    pub offered: Vec<Suggestion>,
    pub considered: Vec<Considered>,
}

// ── What the caller must measure ────────────────────────────────────────────

/// What the pond knows about its tool groups; an empty report means unknown, not none.
/// The extension manager reports empty without a live agent session, e.g. no provider yet.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum GroupsKnown {
    /// The manager answered with a real list. A group not in it is absent.
    These(BTreeSet<String>),
    /// Nobody could say: offer everything; an unanswerable card costs less than a hidden feature.
    #[default]
    Unknown,
}

impl GroupsKnown {
    /// Build from an extension manager's report; an empty list is `Unknown`, not `These(empty)`.
    pub fn from_report(report: Option<Vec<String>>) -> Self {
        match report {
            Some(names) if !names.is_empty() => Self::These(names.into_iter().collect()),
            _ => Self::Unknown,
        }
    }

    fn has(&self, group: &str) -> bool {
        match self {
            Self::These(set) => set.contains(group),
            Self::Unknown => true,
        }
    }
}

/// Everything the suggestors read, gathered once by the caller.
/// Each field was measured; no `Option` stands in for "did not look".
#[derive(Debug, Clone, Default)]
pub struct SuggestionSnapshot {
    pub audience: Audience,
    pub groups: GroupsKnown,
    /// Suggestor ids the household has muted.
    pub muted: BTreeSet<String>,

    /// Calendar items from now to local midnight; `None` means no calendar is connected.
    pub calendar_events_today: Option<usize>,
    /// Mail items this past week (a lagging sync zeroes a daily count); `None`: no mail account.
    pub mail_items_this_week: Option<usize>,
    /// Unpaused schedules whose next run falls before local midnight.
    pub schedules_before_midnight: usize,
    /// The soonest of those, as the household's own label for it.
    pub next_schedule_label: Option<String>,
    /// Retrievable memories (active, not archived), counted as the read path counts them.
    pub active_memories: usize,
    /// Of those, the ones the extractor classified as a standing habit.
    pub routine_memories: usize,
    pub devices_registered: usize,
    /// Weather switched on AND a place resolved: either alone yields an unanswerable card.
    pub weather_ready: bool,
    /// The household's own name for where it is, when it has one.
    pub place: Option<String>,
}

impl Default for Audience {
    /// The narrower one, so an unfilled snapshot shows less, not more.
    fn default() -> Self {
        Audience::Shared
    }
}

// ── The suggestors ──────────────────────────────────────────────────────────

/// Every suggestor, in offer order: most specific to today first.
const SUGGESTOR_IDS: &[&str] = &[
    "calendar_today",
    "upcoming_schedule",
    "inbox_recent",
    "routine_recall",
    "memory_recall",
    "devices_online",
    "weather_today",
];

/// Max shown at once; more turns the Home column from a glance into a list.
pub const MAX_SUGGESTIONS: usize = 4;

/// Work out what this household might want to ask.
pub fn suggest(snapshot: &SuggestionSnapshot) -> SuggestionSet {
    let mut offered = Vec::new();
    let mut considered = Vec::new();

    for id in SUGGESTOR_IDS {
        let outcome = run_one(id, snapshot);
        match outcome {
            Ok(suggestion) => {
                considered.push(Considered {
                    id: (*id).to_string(),
                    silent_because: None,
                });
                offered.push(suggestion);
            }
            Err(reason) => considered.push(Considered {
                id: (*id).to_string(),
                silent_because: Some(reason),
            }),
        }
    }

    // Truncate after the loop so `considered` still records suggestors the cap cut.
    if offered.len() > MAX_SUGGESTIONS {
        offered.truncate(MAX_SUGGESTIONS);
    }

    SuggestionSet {
        offered,
        considered,
    }
}

/// One suggestor. `Err` carries the sentence explaining the silence.
fn run_one(id: &str, s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if s.muted.contains(id) {
        return Err("the household muted this suggestion".to_string());
    }

    let built = match id {
        "calendar_today" => calendar_today(s),
        "upcoming_schedule" => upcoming_schedule(s),
        "inbox_recent" => inbox_recent(s),
        "routine_recall" => routine_recall(s),
        "memory_recall" => memory_recall(s),
        "devices_online" => devices_online(s),
        "weather_today" => weather_today(s),
        // Unreachable while SUGGESTOR_IDS matches this list (`every_suggestor_id_is_reachable`).
        other => Err(format!("no suggestor is registered under '{other}'")),
    }?;

    if !s.groups.has(built.answered_by) {
        return Err(format!(
            "nothing on this pond could answer it: {} is not installed",
            built.answered_by
        ));
    }
    Ok(built)
}

fn calendar_today(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "a calendar belongs to one person, and more than one member lives here \
                    without saying who is asking"
                .to_string(),
        );
    }
    let Some(count) = s.calendar_events_today else {
        return Err("no calendar account is connected".to_string());
    };
    if count == 0 {
        // Silent, not "nothing on today": a question answered "nothing" wastes the one card.
        return Err("a calendar is connected and today is empty".to_string());
    }
    Ok(Suggestion {
        id: "calendar_today".to_string(),
        prompt: "What's on my calendar today?".to_string(),
        because: format!(
            "{} between now and midnight.",
            plural(count, "event", "events")
        ),
        answered_by: GROUP_CONTEXT,
        context: Some(SuggestionContext::Temporal {
            event_title: "Today's calendar schedule".to_string(),
            timing_summary: format!("{count} events between now and midnight"),
        }),
    })
}

fn upcoming_schedule(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if s.schedules_before_midnight == 0 {
        return Err("nothing is set to run before midnight".to_string());
    }
    // The household's own label, never a description invented here.
    let because = match &s.next_schedule_label {
        Some(label) => format!(
            "{} before midnight; the next is {label}.",
            plural(s.schedules_before_midnight, "routine runs", "routines run")
        ),
        None => format!(
            "{} before midnight.",
            plural(s.schedules_before_midnight, "routine runs", "routines run")
        ),
    };
    Ok(Suggestion {
        id: "upcoming_schedule".to_string(),
        prompt: "What's set to run before tonight?".to_string(),
        because: because.clone(),
        answered_by: GROUP_SCHEDULE,
        context: Some(SuggestionContext::Temporal {
            event_title: "Automated routines".to_string(),
            timing_summary: because,
        }),
    })
}

fn inbox_recent(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "a mailbox belongs to one person, and more than one member lives here \
                    without saying who is asking"
                .to_string(),
        );
    }
    let Some(count) = s.mail_items_this_week else {
        return Err("no mail account is connected".to_string());
    };
    if count == 0 {
        return Err("a mail account is connected and nothing arrived this week".to_string());
    }
    Ok(Suggestion {
        id: "inbox_recent".to_string(),
        prompt: "What should I know from my inbox this week?".to_string(),
        because: format!(
            "{} landed in the last seven days.",
            plural(count, "message", "messages")
        ),
        answered_by: GROUP_CONTEXT,
        context: Some(SuggestionContext::Temporal {
            event_title: "Recent mailbox activity".to_string(),
            timing_summary: format!("{count} messages landed in the last seven days"),
        }),
    })
}

fn routine_recall(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "what somebody does regularly is about them, and more than one member \
                    lives here without saying who is asking"
                .to_string(),
        );
    }
    if s.routine_memories == 0 {
        return Err("nothing has been remembered as a standing habit yet".to_string());
    }
    Ok(Suggestion {
        id: "routine_recall".to_string(),
        prompt: "What do you know about my routines?".to_string(),
        // Notes filed, not observations: nothing counts repeats, so "you usually" can't be backed.
        because: format!(
            "{} filed as something you do regularly.",
            plural(s.routine_memories, "note", "notes")
        ),
        answered_by: GROUP_MEMORY,
        context: Some(SuggestionContext::PersonalMemory {
            source_memory_id: "routines".to_string(),
            fact: format!(
                "{} routine habits recorded in personal profile",
                s.routine_memories
            ),
        }),
    })
}

fn memory_recall(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.audience.may_see_personal() {
        return Err(
            "what the pond remembers is about a person, and more than one member \
                    lives here without saying who is asking"
                .to_string(),
        );
    }
    if s.active_memories == 0 {
        return Err("nothing has been remembered yet".to_string());
    }
    Ok(Suggestion {
        id: "memory_recall".to_string(),
        prompt: "What do you remember about me?".to_string(),
        because: format!(
            "{} the pond can still reach.",
            plural(s.active_memories, "thing remembered", "things remembered")
        ),
        answered_by: GROUP_MEMORY,
        context: Some(SuggestionContext::PersonalMemory {
            source_memory_id: "active_memories".to_string(),
            fact: format!(
                "{} active memories recorded in personal profile",
                s.active_memories
            ),
        }),
    })
}

fn devices_online(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if s.devices_registered == 0 {
        return Err("no devices are registered with this pond".to_string());
    }
    Ok(Suggestion {
        id: "devices_online".to_string(),
        prompt: "Which of my devices are online?".to_string(),
        // Registered, never "on": `LoggingDeviceControl` reads no state; `is_online` is real.
        because: format!(
            "{} registered here.",
            plural(s.devices_registered, "device", "devices")
        ),
        answered_by: GROUP_DEVICE,
        context: Some(SuggestionContext::Environmental {
            device_ids: Vec::new(),
            summary: format!("{} devices currently registered", s.devices_registered),
        }),
    })
}

fn weather_today(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if !s.weather_ready {
        return Err("weather is off, or this pond has no coordinates".to_string());
    }
    let because = match &s.place {
        Some(place) => format!("Weather is on and this pond is set to {place}."),
        None => "Weather is on and this pond has coordinates.".to_string(),
    };
    Ok(Suggestion {
        id: "weather_today".to_string(),
        prompt: "What's the weather here today?".to_string(),
        because,
        answered_by: GROUP_WEATHER,
        context: Some(SuggestionContext::Temporal {
            event_title: "Local weather forecast".to_string(),
            timing_summary: "Today's forecast".to_string(),
        }),
    })
}

/// "1 event" / "3 events"; the caller spells both ("routine runs" / "routines run").
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pond with everything connected, so a test can take things away.
    fn full() -> SuggestionSnapshot {
        SuggestionSnapshot {
            audience: Audience::Personal,
            groups: GroupsKnown::These(
                [
                    GROUP_CONTEXT,
                    GROUP_SCHEDULE,
                    GROUP_MEMORY,
                    GROUP_DEVICE,
                    GROUP_WEATHER,
                ]
                .iter()
                .map(|g| (*g).to_string())
                .collect(),
            ),
            muted: BTreeSet::new(),
            calendar_events_today: Some(3),
            mail_items_this_week: Some(237),
            schedules_before_midnight: 2,
            next_schedule_label: Some("Porch lights".to_string()),
            active_memories: 379,
            routine_memories: 4,
            devices_registered: 19,
            weather_ready: true,
            place: Some("Nairobi".to_string()),
        }
    }

    fn ids(set: &SuggestionSet) -> Vec<String> {
        set.offered.iter().map(|s| s.id.clone()).collect()
    }

    fn silence(set: &SuggestionSet, id: &str) -> Option<String> {
        set.considered
            .iter()
            .find(|c| c.id == id)
            .and_then(|c| c.silent_because.clone())
    }

    // ── The vacuity control ─────────────────────────────────────────────────
    // Each suggestor gets a fires test and a silent test; either alone passes on a broken one.

    #[test]
    fn a_fully_connected_pond_offers_the_cap() {
        let set = suggest(&full());
        assert_eq!(set.offered.len(), MAX_SUGGESTIONS);
        // Ordered by how specific the answer is to today.
        assert_eq!(
            ids(&set),
            vec![
                "calendar_today",
                "upcoming_schedule",
                "inbox_recent",
                "routine_recall"
            ]
        );
    }

    #[test]
    fn a_bare_pond_offers_nothing_and_says_why_for_every_suggestor() {
        let set = suggest(&SuggestionSnapshot {
            audience: Audience::Personal,
            groups: GroupsKnown::These(BTreeSet::new()),
            ..Default::default()
        });
        assert!(set.offered.is_empty());
        // The whole point of `considered`: silence is enumerated, not implied.
        assert_eq!(set.considered.len(), SUGGESTOR_IDS.len());
        for c in &set.considered {
            let reason = c
                .silent_because
                .as_ref()
                .unwrap_or_else(|| panic!("{} offered nothing and gave no reason", c.id));
            assert!(
                !reason.trim().is_empty(),
                "{} gave a blank reason, which tells a reader nothing",
                c.id
            );
        }
    }

    #[test]
    fn calendar_tells_no_account_apart_from_an_empty_day() {
        let mut s = full();
        s.calendar_events_today = None;
        assert_eq!(
            silence(&suggest(&s), "calendar_today").as_deref(),
            Some("no calendar account is connected")
        );

        s.calendar_events_today = Some(0);
        assert_eq!(
            silence(&suggest(&s), "calendar_today").as_deref(),
            Some("a calendar is connected and today is empty")
        );
    }

    #[test]
    fn calendar_fires_when_today_has_something_in_it() {
        let mut s = full();
        s.calendar_events_today = Some(1);
        let set = suggest(&s);
        let c = set
            .offered
            .iter()
            .find(|x| x.id == "calendar_today")
            .unwrap();
        assert_eq!(c.prompt, "What's on my calendar today?");
        assert_eq!(c.because, "1 event between now and midnight.");
    }

    #[test]
    fn a_shared_pond_is_offered_nothing_personal() {
        let mut s = full();
        s.audience = Audience::Shared;
        let set = suggest(&s);
        for personal in [
            "calendar_today",
            "inbox_recent",
            "memory_recall",
            "routine_recall",
        ] {
            assert!(
                !ids(&set).contains(&personal.to_string()),
                "{personal} was offered to an unidentified speaker on a multi-member pond"
            );
        }
        // Control: house facts still show, so the rule narrows rather than empties.
        assert!(ids(&set).contains(&"devices_online".to_string()));
        assert!(ids(&set).contains(&"upcoming_schedule".to_string()));
    }

    #[test]
    fn a_suggestion_nothing_can_answer_is_not_offered() {
        let mut s = full();
        let GroupsKnown::These(ref mut set) = s.groups else {
            unreachable!("the fixture names its groups explicitly")
        };
        set.remove(GROUP_WEATHER);
        let set = suggest(&s);
        assert!(!ids(&set).contains(&"weather_today".to_string()));
        assert_eq!(
            silence(&set, "weather_today").as_deref(),
            Some("nothing on this pond could answer it: giap-weather is not installed")
        );
    }

    #[test]
    fn an_unanswerable_report_is_permissive_and_a_real_one_is_not() {
        let mut s = full();

        s.groups = GroupsKnown::Unknown;
        assert!(
            !suggest(&s).offered.is_empty(),
            "an unconfirmed extension list silenced the whole column"
        );

        // A populated answer IS evidence, and is trusted completely.
        s.groups = GroupsKnown::These([GROUP_MEMORY.to_string()].into_iter().collect());
        let set = suggest(&s);
        assert_eq!(
            ids(&set),
            vec!["routine_recall", "memory_recall"],
            "a real extension list was not trusted"
        );
    }

    #[test]
    fn an_empty_report_is_unknown_rather_than_nothing() {
        assert_eq!(GroupsKnown::from_report(None), GroupsKnown::Unknown);
        assert_eq!(GroupsKnown::from_report(Some(vec![])), GroupsKnown::Unknown);
        assert_eq!(
            GroupsKnown::from_report(Some(vec![GROUP_MEMORY.to_string()])),
            GroupsKnown::These([GROUP_MEMORY.to_string()].into_iter().collect())
        );
        assert_eq!(GroupsKnown::default(), GroupsKnown::Unknown);
    }

    #[test]
    fn muting_silences_one_suggestor_and_only_that_one() {
        let mut s = full();
        s.muted.insert("memory_recall".to_string());
        let set = suggest(&s);
        assert!(!ids(&set).contains(&"memory_recall".to_string()));
        assert_eq!(
            silence(&set, "memory_recall").as_deref(),
            Some("the household muted this suggestion")
        );
        assert!(silence(&set, "devices_online").is_none());
    }

    #[test]
    fn the_cap_shortens_the_offer_and_never_the_record() {
        let set = suggest(&full());
        assert_eq!(set.offered.len(), MAX_SUGGESTIONS);
        assert_eq!(
            set.considered.len(),
            SUGGESTOR_IDS.len(),
            "the cap hid a suggestor that fired, so nothing can tell that the pond had more to say"
        );
        // The ones past the cap fired; they are simply not shown.
        for past_cap in ["memory_recall", "devices_online", "weather_today"] {
            assert!(
                silence(&set, past_cap).is_none(),
                "{past_cap} was recorded as silent when it had actually produced something"
            );
        }
    }

    #[test]
    fn every_suggestor_id_is_reachable() {
        let s = full();
        for id in SUGGESTOR_IDS {
            let outcome = run_one(id, &s);
            if let Err(reason) = outcome {
                assert!(
                    !reason.starts_with("no suggestor is registered"),
                    "{id} is listed but has no arm in run_one"
                );
            }
        }
    }

    #[test]
    fn nothing_claims_a_device_is_on() {
        let set = suggest(&full());
        for s in &set.offered {
            let text = format!("{} {}", s.prompt, s.because).to_lowercase();
            for claim in [
                " is on",
                " are on",
                "switched on",
                "turned on",
                " is off",
                " are off",
            ] {
                assert!(
                    !text.contains(claim),
                    "{} asserts device state the pond cannot read: {text}",
                    s.id
                );
            }
        }
    }

    #[test]
    fn nothing_claims_a_habit_the_pond_has_not_counted() {
        let set = suggest(&full());
        for s in &set.offered {
            let text = format!("{} {}", s.prompt, s.because).to_lowercase();
            for claim in [
                "you usually",
                "you always",
                "you often",
                "every day",
                "as usual",
            ] {
                assert!(
                    !text.contains(claim),
                    "{} claims a pattern nothing counted: {text}",
                    s.id
                );
            }
        }
    }

    #[test]
    fn every_offered_suggestion_carries_a_number_or_a_name() {
        let set = suggest(&full());
        for s in &set.offered {
            assert!(
                s.because.chars().any(|c| c.is_ascii_digit()) || s.because.contains("Nairobi"),
                "{}'s reason quotes nothing the pond measured: {}",
                s.id,
                s.because
            );
            assert!(!s.because.trim().is_empty(), "{} has a blank reason", s.id);
        }
    }

    #[test]
    fn the_prompt_is_the_sentence_the_household_reads() {
        let set = suggest(&full());
        for s in &set.offered {
            assert!(s.prompt.ends_with('?'), "{} is not a question", s.id);
            assert!(
                s.prompt.len() <= 120,
                "{} would be truncated on the card",
                s.id
            );
        }
    }

    #[test]
    fn the_snapshot_defaults_to_showing_less() {
        assert_eq!(Audience::default(), Audience::Shared);
        assert!(!Audience::default().may_see_personal());
    }
}
