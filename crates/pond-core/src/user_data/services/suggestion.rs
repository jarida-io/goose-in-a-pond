//! The suggestion engine — what the household might want to ask, and why.
//!
//! A **suggestion** is a question the household could put to the pond right
//! now, that the pond can currently answer, phrased only from facts the pond
//! actually holds. It is not a [`Proposal`](crate::user_data::domain::proposal),
//! and the difference is the whole reason this module exists rather than a
//! fourth arm inside `proactive_review`.
//!
//! | | proposal | suggestion |
//! |---|---|---|
//! | what it is | a staged action | an offer |
//! | who it is for | one named member | nobody in particular |
//! | when it acts | on approval | when tapped, by the person tapping |
//! | if ignored | expires | keeps |
//! | budget | 6 interruptions a day | none; it interrupts nothing |
//!
//! Because a suggestion performs nothing until somebody taps it, **the tap is
//! the consent**. That is what lets it skip the apparatus a proposal needs —
//! [`ProposalAudience`](crate::user_data::domain::proposal::ProposalAudience),
//! the expiry, the daily cap — and it is also why skipping that apparatus is
//! not a loophole: there is no action being staged to approve.
//!
//! # Derived on read, never stored
//!
//! Nothing here writes a row. A suggestion is a pure function of a
//! [`SuggestionSnapshot`] the caller measured a moment ago, so it cannot go
//! stale and there is no table that could acquire a writer and no reader. That
//! is deliberate: the memory-edge table, `activity_watcher`, the context
//! preamble and `TaskKind::ToolCall` are all shapes in this tree where
//! something was built and nothing ever reached it. A derived suggestion is
//! immune to that class by construction.
//!
//! # Every suggestion carries the fact that produced it
//!
//! [`Suggestion::because`] is not decoration and is never a template with no
//! number in it. DESIGN.md's rule is "never invent meaning the data lacks", so
//! a suggestor that cannot measure its own fact does not soften the sentence —
//! it emits nothing, and says so in [`Considered::silent_because`]. The set is
//! therefore small on a bare pond and grows as the household connects things,
//! which is the honest shape.
//!
//! # Nothing here spends inference
//!
//! The one model-backed producer in this tree is the proactive reviewer, and
//! its yield has been zero on three Orin runs — twice on `deny_unknown_fields`,
//! once on a 2B model omitting a required field. Every suggestor below is a
//! `match` over integers and booleans, which has none of that failure mode and
//! does not queue behind the household's next turn for the one GPU.
//!
//! # A suggestion that nothing can answer is not offered
//!
//! [`Suggestion::answered_by`] names the tool group whose tools the model would
//! reach for, and [`suggest`] drops any suggestion whose group the pond does
//! not have. This is the control that keeps the engine from producing the one
//! card everybody wants and nothing can serve: there is no music tool anywhere
//! in `pond-mcp-server` — `POST /api/v1/music/control` is an HTTP route the
//! desktop calls, not something the model can invoke — so "Play some music"
//! would reach a model that answers it cannot. Rather than ship it behind a
//! flag that is false on every pond, it is not written, and this paragraph is
//! where the next person finds out why.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ── What the household may be shown ─────────────────────────────────────────

/// Who is looking, in the only terms this module needs.
///
/// Deliberately NOT [`ProfileScope`](crate::user_data::domain::profile::ProfileScope):
/// the question here is "may this screen show personal facts", which has two
/// answers, not three.
///
/// The mapping is the subtle part and it follows
/// [`identity_resolution::resolve`](super::identity_resolution::resolve)
/// exactly. That function returns `Guest` **only** when the household has more
/// than one member, and `Household` otherwise — so `ProfileScope::Household` is
/// reachable only on a pond with at most one member, where "every member's
/// rows" and "the one member's rows" are the same rows. That is why the
/// personal tier is gated on *not-Guest* rather than on `Owner`, and it is the
/// same call commit `881da889` made for connecting a context source: "Household
/// in a single-member household now resolves to that member."
///
/// Gating on `Owner` instead would be defensible and would also make the whole
/// personal half unreachable on every desktop pond that has not paired an
/// attributed device, which is the inert shape this engine exists to avoid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Audience {
    /// One member, or a household of one. Personal facts are theirs to see.
    Personal,
    /// An unidentified speaker on a pond with more than one member. Only facts
    /// about the house itself — never about a person.
    Shared,
}

impl Audience {
    /// Whether a suggestor drawing on one person's data may run.
    pub fn may_see_personal(self) -> bool {
        matches!(self, Audience::Personal)
    }
}

/// The tool group whose tools would answer a suggestion.
///
/// A suggestion is only ever offered when the pond actually has this group, so
/// the household is never invited to ask something the model has no way to
/// serve. The strings match `TOOL_GROUPS` in
/// [`tool_group`](crate::mcp::domain::tool_group); `groups_present_on_this_pond`
/// in the caller supplies the set to check against.
pub const GROUP_CONTEXT: &str = "giap-context";
pub const GROUP_SCHEDULE: &str = "giap-schedule";
pub const GROUP_MEMORY: &str = "giap-memory";
pub const GROUP_DEVICE: &str = "giap-device";
pub const GROUP_WEATHER: &str = "giap-weather";

/// One thing the household might want to ask.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Suggestion {
    /// Stable across renders, so muting one mutes the same one tomorrow.
    ///
    /// It is the suggestor's own id and nothing else — deliberately not a hash
    /// of the fact. A suggestion is recomputed every read, so an id that moved
    /// with the number would make a mute a key that never matched again.
    pub id: String,
    /// The sentence the household reads AND the prompt that is sent when they
    /// tap it. One string, because two would let the card promise something
    /// other than what it does — which is exactly what
    /// `SuggestionQueue.tsx` refused to build for proposals.
    pub prompt: String,
    /// The fact that produced it, with the number in it. Never blank.
    pub because: String,
    /// The tool group that can answer [`Self::prompt`].
    pub answered_by: &'static str,
}

/// One suggestor's outcome, whether or not it produced anything.
///
/// This is the anti-inertness device and it is the reason the route returns it.
/// Today a failed fetch, a 403 and a genuinely quiet house are pixel-identical
/// on the Dashboard — `SuggestionQueue` swallows every error into the quiet
/// line — so "the feature is broken" and "there is nothing to say" look the
/// same to the household AND to whoever is debugging it. A silent suggestor
/// that states its reason is falsifiable; one that merely returns `None` is not.
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

/// What the pond knows about which tool groups it has.
///
/// Three states, not two, and the third is the one that matters. An extension
/// manager answers from a live agent session, so on a pond whose model provider
/// is not configured yet -- a fresh install mid-onboarding, or any pond whose
/// provider failed to start -- it returns an EMPTY list rather than an error.
///
/// Reading that empty list as "this pond has no extensions" silences every
/// suggestion at exactly the moment the column most needs to be useful. Verified
/// live while building this: a scratch pond with two devices registered and
/// weather switched on reported `giap-device is not installed` and
/// `giap-weather is not installed`, because its log said
/// `LLM: llamafile skipped (provider = )`.
///
/// So absence of evidence is [`Unknown`](Self::Unknown) and is treated as
/// permissive; only a populated answer is evidence, and then it is trusted
/// completely. The cost of being wrong in the permissive direction is a prompt
/// the model answers with "I can't do that"; the cost of being wrong in the
/// strict direction is a feature that never appears at all, which this codebase
/// has shipped several times.
#[derive(Debug, Clone, PartialEq)]
pub enum GroupsKnown {
    /// The manager answered with a real list. A group not in it is absent.
    These(BTreeSet<String>),
    /// Nobody could say. Offer everything and let the model speak for itself.
    Unknown,
}

impl GroupsKnown {
    /// Build from what an extension manager reported.
    ///
    /// An empty list is `Unknown`, not `These(empty)` -- see the type's docs.
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

impl Default for GroupsKnown {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Everything the suggestors read, gathered once by the caller.
///
/// Plain counts and small owned values rather than ports, so this module stays
/// a pure function and its tests need no database and no mocks. Each field is
/// something the caller actually measured; there is no `Option` here standing
/// in for "did not bother to look", because a suggestor cannot tell that apart
/// from "looked and found nothing" and would phrase a guess either way.
#[derive(Debug, Clone, Default)]
pub struct SuggestionSnapshot {
    pub audience: Audience,
    /// What is known about the tool groups this pond has. A suggestion whose
    /// `answered_by` is known to be absent is dropped before anybody sees it;
    /// one that is merely unconfirmed is offered.
    pub groups: GroupsKnown,
    /// Suggestor ids the household has muted.
    pub muted: BTreeSet<String>,

    /// Calendar items between now and the household's local midnight.
    /// `None` means no calendar source is connected — a different thing from
    /// `Some(0)`, which means a connected calendar with an empty day.
    pub calendar_events_today: Option<usize>,
    /// Mail items in the last seven days, `None` when no mail account is
    /// connected. Seven days rather than "today" because a mail sync that has
    /// not run since yesterday would make a today-count read zero on a pond
    /// with a full inbox.
    pub mail_items_this_week: Option<usize>,
    /// Unpaused schedules whose next run falls before local midnight.
    pub schedules_before_midnight: usize,
    /// The soonest of those, as the household's own label for it.
    pub next_schedule_label: Option<String>,
    /// Memories that are actually retrievable — lifecycle active, not archived.
    /// Counted the same way the read path counts them, because a number larger
    /// than what an answer can draw on is a number that overpromises.
    pub active_memories: usize,
    /// Of those, the ones the extractor classified as a standing habit.
    pub routine_memories: usize,
    /// Devices registered with this pond.
    pub devices_registered: usize,
    /// Weather is switched on AND a place resolved. Both, because either alone
    /// produces a card that cannot be answered.
    pub weather_ready: bool,
    /// The household's own name for where it is, when it has one.
    pub place: Option<String>,
}

impl Default for Audience {
    /// Shared, because it is the narrower of the two. A snapshot somebody
    /// forgot to fill should show less, not more.
    fn default() -> Self {
        Audience::Shared
    }
}

// ── The suggestors ──────────────────────────────────────────────────────────

/// Every suggestor, in the order they would be offered.
///
/// Order is by how specific the answer is to today: a calendar with three
/// events on it says more than a device count that is the same every day.
const SUGGESTOR_IDS: &[&str] = &[
    "calendar_today",
    "upcoming_schedule",
    "inbox_recent",
    "routine_recall",
    "memory_recall",
    "devices_online",
    "weather_today",
];

/// How many a household is shown at once.
///
/// The design's left column holds one open card, two faded peek rows and a
/// count. Beyond about that the column stops being a glance and becomes a list,
/// which is the thing the Home screen was pared back to avoid.
pub const MAX_SUGGESTIONS: usize = 4;

/// Work out what this household might want to ask.
///
/// Pure. Every refusal is recorded in [`SuggestionSet::considered`] rather than
/// dropped, so a caller can tell an empty set apart from a broken one.
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

    // The cap is applied AFTER everything has been considered, so the
    // `considered` list still names the suggestors that would have fired. A cap
    // that shortened the record as well as the output would hide the fact that
    // the pond had more to say.
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
        // Unreachable while SUGGESTOR_IDS and this match agree, and
        // `every_suggestor_id_is_reachable` is what keeps them agreeing.
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
        // Deliberately silent rather than "nothing on today". Inviting somebody
        // to ask a question whose answer is "nothing" wastes the one card on
        // the screen that is supposed to be worth reading.
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
    })
}

fn upcoming_schedule(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if s.schedules_before_midnight == 0 {
        return Err("nothing is set to run before midnight".to_string());
    }
    // The label is the household's own name for the routine, never a
    // description this module invented for it.
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
        because,
        answered_by: GROUP_SCHEDULE,
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
        // Note what this does NOT say. There is no observation count anywhere
        // in this tree -- `KnownMemory.pattern` is hardcoded false and the
        // extractor drops a candidate that matches a stored memory rather than
        // counting it -- so "you usually" and "you have mentioned this three
        // times" are both unbackable. What the pond can say is how many notes
        // it filed under that heading, which is what this says.
        because: format!(
            "{} filed as something you do regularly.",
            plural(s.routine_memories, "note", "notes")
        ),
        answered_by: GROUP_MEMORY,
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
    })
}

fn devices_online(s: &SuggestionSnapshot) -> Result<Suggestion, String> {
    if s.devices_registered == 0 {
        return Err("no devices are registered with this pond".to_string());
    }
    Ok(Suggestion {
        id: "devices_online".to_string(),
        prompt: "Which of my devices are online?".to_string(),
        // Registered, never "on". The production `DeviceControlPort` is
        // `LoggingDeviceControl`, which implements neither `state()` nor
        // `describe()`, and `GET /api/v1/devices` emits no metadata at all --
        // so the pond does not know whether a single light is lit. Asking which
        // are ONLINE is answerable (`is_online` is a real column); asserting
        // which are on is not.
        because: format!(
            "{} registered here.",
            plural(s.devices_registered, "device", "devices")
        ),
        answered_by: GROUP_DEVICE,
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
    })
}

/// "1 event" / "3 events", with the caller supplying both spellings.
///
/// A free function rather than an inline `if`, because the singular form of
/// "routine runs" is not the plural with an "s" removed and a generic helper
/// would get it wrong.
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
    //
    // Every suggestor gets a PAIR: one test that it fires, one that it is
    // silent. A fires-only suite passes just as well when the silence condition
    // is inverted, and a silent-only suite passes on a function that returns
    // `Err` unconditionally -- which is how a feature ships inert with green
    // tests. The pairs are what make each claim falsifiable in both directions.

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
        // And the house's own facts still are, or the guest rule would have
        // emptied the screen rather than narrowed it.
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
        // The distinction this type exists for, in one test.
        //
        // An extension manager that cannot answer -- a pond whose model
        // provider has not started, which is every pond mid-onboarding --
        // returns an empty list, NOT an error. Reading that as "no extensions"
        // silenced every suggestion on a live scratch pond that had devices
        // registered and weather switched on.
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
        // The default narrows the OTHER way from `Audience::default`, and that
        // is deliberate: showing a suggestion nobody can answer costs a shrug
        // from the model, while hiding every suggestion costs the feature.
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
        // `SUGGESTOR_IDS` and `run_one`'s match are two lists that must agree.
        // A new id added to one and not the other would be silent forever with
        // a reason that reads like a bug report, which is better than silence
        // but worse than not shipping it.
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
        // The pond does not know. `LoggingDeviceControl` implements neither
        // `state()` nor `describe()`, and the devices route sends no metadata,
        // so any sentence asserting a device's state would be invented.
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
        // There is no observation count in this tree. A suggestion that says
        // "you usually" or "you always" would be asserting evidence that does
        // not exist anywhere.
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
        // "never invent meaning the data lacks": a `because` with no measured
        // value in it is a template, and a template is what this engine exists
        // not to be.
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
        // One string, never two. A separate headline and prompt is how a card
        // comes to promise something other than what it does.
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
        // A caller that forgot to fill the audience gets the narrow answer.
        assert_eq!(Audience::default(), Audience::Shared);
        assert!(!Audience::default().may_see_personal());
    }
}
