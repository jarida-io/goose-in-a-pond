//! What a conversation window yields beside memories: the plans a member stated, and the gate
//! every candidate passes before it is saved.

use chrono::{DateTime, Datelike, Utc};
use chrono_tz::Tz;
use serde_json::{Map, Value};

use crate::shared::domain::when_said::due_from_words;
use crate::user_data::domain::anchor::{
    clean_title, title_key, Anchor, AnchorFact, AnchorKind, AnchorOrigin,
};
use crate::user_data::services::model_json::{list_in, object_in, text_of};

/// Plans kept from one window.
pub const MAX_PLANS: usize = 3;
const MAX_WHEN_CHARS: usize = 80;
const MAX_PLACE_CHARS: usize = 120;
const MAX_DETAILS_CHARS: usize = 300;
const MAX_REASON_CHARS: usize = 120;

/// The kinds a stated plan may be; an interest or a standing anchor is never read from one.
const PLAN_KINDS: [AnchorKind; 4] = [
    AnchorKind::Event,
    AnchorKind::Trip,
    AnchorKind::Purchase,
    AnchorKind::Project,
];

/// Added to the extraction prompt; `{subject}` and `{assistant}` are filled as the prompt's are.
pub const PLANS_SECTION: &str = "\n\
plans: concrete things {subject} said they themselves plan to attend, go on,\n\
buy or build.\n\
- kind: event, trip, purchase or project.\n\
- title: its own name, in a few words: the event, the destination, the\n\
thing, the project.\n\
- when, where: {subject}'s own words about the date and the place, copied\n\
from the conversation; \"\" when they gave none. Do not work out a date.\n\
- details: what else they said about it, in one line; may be \"\".\n\
- Never a plan: a passing question or wish, something they looked up or\n\
wanted explained, anything {assistant} proposed.\n\
Most conversations hold no plan, and \"plans\" is then []. At most 3 plans.\n";

/// The `plans` member of the reply skeleton the extraction prompt shows.
pub const PLANS_SKELETON: &str = "\"plans\":[{\"kind\":\"...\",\"title\":\"...\",\
     \"when\":\"...\",\"where\":\"...\",\"details\":\"...\"}]";

/// A plan a member stated, as the model read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCandidate {
    pub kind: AnchorKind,
    pub title: String,
    /// The member's own words about the date; empty when they gave none.
    pub when: String,
    pub place: String,
    pub details: String,
}

impl PlanCandidate {
    /// The plan as one line a critic reads.
    pub fn as_line(&self) -> String {
        let mut line = format!("{}: {}", self.kind.as_str(), self.title);
        for (label, value) in [
            ("when", &self.when),
            ("where", &self.place),
            ("details", &self.details),
        ] {
            if !value.is_empty() {
                line.push_str(&format!("; {label}: {value}"));
            }
        }
        line
    }
}

/// One plan as a model wrote it; `None` when it has no title or is no kind a plan may be.
pub fn plan_in(value: &Value) -> Option<PlanCandidate> {
    let map = value.as_object()?;
    let kind = AnchorKind::parse(map.get("kind")?.as_str()?).filter(|k| PLAN_KINDS.contains(k))?;
    let title = clean_title(&text_of(map, "title")?)?;
    let field = |key: &str, max: usize| -> String {
        text_of(map, key)
            .map(|text| text.chars().take(max).collect())
            .unwrap_or_default()
    };
    let place = match field("where", MAX_PLACE_CHARS) {
        place if place.is_empty() => field("place", MAX_PLACE_CHARS),
        place => place,
    };
    Some(PlanCandidate {
        kind,
        title,
        when: field("when", MAX_WHEN_CHARS),
        place,
        details: field("details", MAX_DETAILS_CHARS),
    })
}

/// The plans a window's reply holds under `plans`, or `value` itself when it is the list: at
/// most [`MAX_PLANS`], each title once.
pub fn parse_plans(value: &Value) -> Vec<PlanCandidate> {
    let items = value.get("plans").unwrap_or(value).as_array();
    let mut plans: Vec<PlanCandidate> = Vec::new();
    for plan in items.into_iter().flatten().filter_map(plan_in) {
        if plans.len() == MAX_PLANS {
            break;
        }
        let key = title_key(&plan.title);
        if !plans.iter().any(|held| title_key(&held.title) == key) {
            plans.push(plan);
        }
    }
    plans
}

/// The four-digit years `words` name.
pub(super) fn years_in(words: &str) -> impl Iterator<Item = i32> + '_ {
    words
        .split(|c: char| !c.is_ascii_digit())
        .filter(|run| run.len() == 4)
        .filter_map(|run| run.parse::<i32>().ok())
        .filter(|year| (1900..=2200).contains(year))
}

/// When `words` mean, said at `said_at`; `None` when they name no one time still to come.
pub(super) fn moment_said(words: &str, said_at: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    let at = due_from_words(words, said_at, tz)?;
    // `due_from_words` reads no year: "3 March 2028" would land on the next 3 March.
    let read_as = at.with_timezone(&tz).year();
    years_in(words).all(|year| year == read_as).then_some(at)
}

/// A candidate anchor for a plan; `None` when its title has no words or its date has passed.
/// A date the words do not place leaves it undated, with the words kept as a fact.
pub fn anchor_from_plan(
    plan: &PlanCandidate,
    profile_id: &str,
    origin: AnchorOrigin,
    origin_ref: Option<&str>,
    said_at: DateTime<Utc>,
    tz: Tz,
    now: DateTime<Utc>,
) -> Option<Anchor> {
    let mut anchor = Anchor::new(profile_id, plan.kind, &plan.title, origin, now)?;
    let starts_at = moment_said(&plan.when, said_at, tz);
    if starts_at.is_some_and(|at| at <= now) {
        return None;
    }
    let (source, came) = match origin {
        AnchorOrigin::Capture => ("your picture", "It was in a picture you sent on"),
        _ => ("you said", "You mentioned it on"),
    };
    anchor.starts_at = starts_at;
    anchor.place = Some(plan.place.clone()).filter(|place| !place.is_empty());
    if starts_at.is_none() && !plan.when.is_empty() {
        anchor.set_fact(AnchorFact::new("When", &plan.when, source), now);
    }
    if !plan.details.is_empty() {
        anchor.set_fact(AnchorFact::new("Details", &plan.details, source), now);
    }
    anchor.origin_ref = origin_ref.map(str::to_string);
    anchor.why = format!("{came} {}", said_at.with_timezone(&tz).format("%-d %B"));
    Some(anchor)
}

/// Fixed so it stays in the KV prefix. It shows no worked ruling: a small model copies one.
pub const GATE_SYSTEM: &str = "\
You check what Goose is about to keep from one conversation with a member of \
this household, before any of it is saved. You are the last check, so look \
for what should not be kept.

You are given the conversation, with what is already remembered above it \
when there is any, the member's don't-list, and the numbered candidates. \
Each candidate is a memory about the member, an interest of theirs, a \
reminder or a plan.

Drop a candidate when any of these is true:
- the member did not say it themselves: the assistant said it, or it is a guess;
- it comes from a one-off lookup: something they asked once or wanted \
explained, which says nothing about them;
- it is about the pond, the assistant or how either works;
- it is already known: \"Already remembered\" says it, or another candidate does;
- it breaks a rule on the don't-list.

Keep every other candidate. Wording you would have chosen differently is not \
a reason to drop one.

Reply with JSON and nothing else:

{\"drop\": [{\"n\": <the candidate's number>, \"why\": \"<a few words>\"}]}

- Use only the numbers under \"Candidates to check\". An empty list when \
every candidate is worth keeping.
- \"why\": one line, under 100 characters.
- No explanation, no preamble.";

const DONT_HEADING: &str = "The member's don't-list:";
const CANDIDATES_HEADING: &str = "Candidates to check:";
pub(super) const REPLY_CUE: &str = "JSON:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateKind {
    Memory,
    Interest,
    Reminder,
    Plan,
}

impl GateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Interest => "interest",
            Self::Reminder => "reminder",
            Self::Plan => "plan",
        }
    }
}

/// One thing a window's extraction would save, as the gate is shown it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateCandidate {
    pub kind: GateKind,
    pub text: String,
}

impl GateCandidate {
    pub fn new(kind: GateKind, text: &str) -> Self {
        Self {
            kind,
            text: text.split_whitespace().collect::<Vec<_>>().join(" "),
        }
    }

    pub fn plan(plan: &PlanCandidate) -> Self {
        Self::new(GateKind::Plan, &plan.as_line())
    }
}

/// Appends one headed part of a prompt; a part with nothing in it is left out.
pub(super) fn section(out: &mut String, heading: &str, body: &str) {
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    out.push_str(heading);
    out.push('\n');
    out.push_str(body);
    out.push_str("\n\n");
}

/// The gate's user message: the window as the extraction saw it, the don't-list, then the
/// candidates numbered from 1.
pub fn build_gate_message(window: &str, candidates: &[GateCandidate], dont_list: &str) -> String {
    let numbered: Vec<String> = candidates
        .iter()
        .enumerate()
        .map(|(at, candidate)| {
            format!(
                "{}. {}: {}",
                at + 1,
                candidate.kind.as_str(),
                candidate.text
            )
        })
        .collect();
    let mut out = String::new();
    if !window.trim().is_empty() {
        out.push_str(window.trim());
        out.push_str("\n\n");
    }
    section(&mut out, DONT_HEADING, dont_list);
    section(&mut out, CANDIDATES_HEADING, &numbered.join("\n"));
    out.push_str(REPLY_CUE);
    out
}

const NUMBER_KEYS: [&str; 4] = ["n", "number", "item", "id"];

fn number_in(map: &Map<String, Value>) -> Option<usize> {
    NUMBER_KEYS.iter().find_map(|key| number_of(map.get(*key)?))
}

fn number_of(value: &Value) -> Option<usize> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|n| usize::try_from(n).ok()),
        Value::String(number) => number.trim().trim_end_matches('.').parse().ok(),
        Value::Object(map) => number_in(map),
        _ => None,
    }
}

/// Which of `count` numbered items a critic dropped, as places from 0 with its reason. `None`
/// when the reply cannot be read, or names only numbers it was never given.
pub(super) fn numbered_drops(raw: &str, count: usize) -> Option<Vec<(usize, String)>> {
    let items: Vec<Value> = match object_in(raw) {
        Some(map) => match map.get("drop") {
            Some(Value::Array(items)) => items.clone(),
            Some(one @ (Value::Number(_) | Value::Object(_))) => vec![one.clone()],
            Some(_) => return None,
            None if number_in(&map).is_some() => vec![Value::Object(map)],
            None => return None,
        },
        None => list_in(raw, "drop")?,
    };
    let mut drops: Vec<(usize, String)> = Vec::new();
    for item in &items {
        let Some(place) = number_of(item)
            .filter(|number| (1..=count).contains(number))
            .map(|number| number - 1)
        else {
            continue;
        };
        if drops.iter().any(|(held, _)| *held == place) {
            continue;
        }
        let why = item
            .as_object()
            .and_then(|map| text_of(map, "why").or_else(|| text_of(map, "reason")))
            .map(|why| why.chars().take(MAX_REASON_CHARS).collect())
            .unwrap_or_default();
        drops.push((place, why));
    }
    if drops.is_empty() && !items.is_empty() {
        return None;
    }
    drops.sort_by_key(|(place, _)| *place);
    Some(drops)
}

/// Which of `count` candidates the gate dropped, as places from 0 with its reason; `None` when
/// the reply cannot be read.
pub fn parse_gate_drops(raw: &str, count: usize) -> Option<Vec<(usize, String)>> {
    numbered_drops(raw, count)
}

/// Which candidates to keep. With an unreadable ruling, memories and reminders are kept, as
/// they were before there was a gate, and interests and plans are not.
pub fn apply_gate(kinds: &[GateKind], drops: Option<&[(usize, String)]>) -> Vec<bool> {
    kinds
        .iter()
        .enumerate()
        .map(|(at, kind)| match drops {
            Some(drops) => !drops.iter().any(|(place, _)| *place == at),
            None => matches!(kind, GateKind::Memory | GateKind::Reminder),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::AnchorState;
    use chrono::TimeZone;
    use serde_json::json;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn summit() -> PlanCandidate {
        PlanCandidate {
            kind: AnchorKind::Event,
            title: "AI Summit".into(),
            when: "3 December at 8am".into(),
            place: "KICC, Nairobi".into(),
            details: "Going with Amina; wants the robotics track".into(),
        }
    }

    #[test]
    fn the_plans_paragraph_names_the_kinds_the_fields_and_the_cap() {
        for kind in PLAN_KINDS {
            assert!(PLANS_SECTION.contains(kind.as_str()), "{kind:?}");
        }
        for field in ["kind", "title", "when", "where", "details"] {
            assert!(PLANS_SECTION.contains(field), "{field}");
            assert!(
                PLANS_SKELETON.contains(&format!("\"{field}\":\"...\"")),
                "{field}"
            );
        }
        assert!(PLANS_SECTION.contains(&format!("At most {MAX_PLANS} plans")));
        assert!(PLANS_SECTION.contains("{subject}") && PLANS_SECTION.contains("{assistant}"));
        assert!(PLANS_SECTION.starts_with('\n') && PLANS_SECTION.ends_with('\n'));
        let skeleton: Value = serde_json::from_str(&format!("{{{PLANS_SKELETON}}}")).unwrap();
        assert!(
            parse_plans(&skeleton).is_empty(),
            "the skeleton is not a plan"
        );
    }

    #[test]
    fn plans_read_from_the_reply_beside_its_other_lists() {
        let reply = json!({
            "memories": [],
            "plans": [
                {"kind": "Event", "title": " AI  Summit ", "when": "3 December at 8am",
                 "where": "KICC, Nairobi", "details": "Going with Amina"},
                {"kind": "trip", "title": "Kisumu in December", "when": "", "place": "Kisumu"},
            ]
        });
        let plans = parse_plans(&reply);
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].kind, AnchorKind::Event);
        assert_eq!(plans[0].title, "AI Summit");
        assert_eq!(plans[0].when, "3 December at 8am");
        assert_eq!(plans[0].place, "KICC, Nairobi");
        assert_eq!(plans[0].details, "Going with Amina");
        assert_eq!(plans[1].place, "Kisumu");
        assert_eq!(plans[1].when, "");
        assert_eq!(parse_plans(&reply["plans"]).len(), 2, "the list itself");
        assert!(parse_plans(&json!({"memories": []})).is_empty());
        assert!(parse_plans(&json!({"plans": "none"})).is_empty());
    }

    #[test]
    fn a_plan_needs_a_title_and_a_kind_a_plan_may_be() {
        let reply = json!({"plans": [
            {"kind": "interest", "title": "Formula 1"},
            {"kind": "standing", "title": "Everyday"},
            {"kind": "holiday", "title": "Diani"},
            {"kind": "trip", "title": "..."},
            {"kind": "trip", "title": " . "},
            {"title": "No kind"},
            "a trip to Diani",
            {"kind": "purchase", "title": "Toyota Fielder"},
        ]});
        let plans = parse_plans(&reply);
        assert_eq!(plans.len(), 1, "{plans:?}");
        assert_eq!(plans[0].title, "Toyota Fielder");
    }

    #[test]
    fn a_window_yields_three_plans_at_most_each_title_once() {
        let reply = json!({"plans": [
            {"kind": "trip", "title": "Kisumu"},
            {"kind": "event", "title": "KISUMU!"},
            {"kind": "trip", "title": "Diani"},
            {"kind": "project", "title": "Pond"},
            {"kind": "purchase", "title": "Fielder"},
        ]});
        let titles: Vec<String> = parse_plans(&reply).into_iter().map(|p| p.title).collect();
        assert_eq!(titles, ["Kisumu", "Diani", "Pond"]);
    }

    #[test]
    fn a_long_field_is_cut_to_its_cap() {
        let reply = json!({"plans": [{"kind": "trip", "title": "t".repeat(400),
            "when": "w".repeat(400), "where": "p".repeat(400), "details": "d".repeat(900)}]});
        let plan = &parse_plans(&reply)[0];
        assert_eq!(plan.title.chars().count(), 120);
        assert_eq!(plan.when.chars().count(), MAX_WHEN_CHARS);
        assert_eq!(plan.place.chars().count(), MAX_PLACE_CHARS);
        assert_eq!(plan.details.chars().count(), MAX_DETAILS_CHARS);
    }

    #[test]
    fn a_plan_becomes_a_candidate_anchor_with_its_date_place_and_facts() {
        let said_at = now() - chrono::Duration::hours(2);
        let anchor = anchor_from_plan(
            &summit(),
            "jerry",
            AnchorOrigin::Chat,
            Some("session-1"),
            said_at,
            NAIROBI,
            now(),
        )
        .unwrap();
        assert_eq!(anchor.state, AnchorState::Candidate);
        assert_eq!(anchor.kind, AnchorKind::Event);
        assert_eq!(anchor.title, "AI Summit");
        assert_eq!(
            anchor.starts_at,
            Some(Utc.with_ymd_and_hms(2026, 12, 3, 5, 0, 0).unwrap())
        );
        assert_eq!(anchor.ends_at, None);
        assert_eq!(anchor.place.as_deref(), Some("KICC, Nairobi"));
        assert_eq!(
            anchor.facts,
            [AnchorFact::new(
                "Details",
                "Going with Amina; wants the robotics track",
                "you said"
            )]
        );
        assert_eq!(anchor.origin, AnchorOrigin::Chat);
        assert_eq!(anchor.origin_ref.as_deref(), Some("session-1"));
        assert_eq!(anchor.why, "You mentioned it on 16 November");
        assert_eq!(anchor.created_at, now());
    }

    #[test]
    fn a_plan_from_a_picture_says_so() {
        let anchor = anchor_from_plan(
            &summit(),
            "jerry",
            AnchorOrigin::Capture,
            Some("capture-1"),
            now(),
            NAIROBI,
            now(),
        )
        .unwrap();
        assert_eq!(anchor.state, AnchorState::Candidate);
        assert_eq!(anchor.why, "It was in a picture you sent on 16 November");
        assert_eq!(anchor.facts[0].source, "your picture");
    }

    #[test]
    fn a_date_the_words_do_not_place_leaves_the_anchor_undated_and_keeps_the_words() {
        for when in ["sometime in March", "after the rains", "3 March 2028"] {
            let plan = PlanCandidate {
                when: when.into(),
                details: String::new(),
                ..summit()
            };
            let anchor = anchor_from_plan(
                &plan,
                "jerry",
                AnchorOrigin::Chat,
                None,
                now(),
                NAIROBI,
                now(),
            )
            .unwrap();
            assert_eq!(anchor.starts_at, None, "{when}");
            assert_eq!(
                anchor.facts,
                [AnchorFact::new("When", when, "you said")],
                "{when}"
            );
        }
        let bare = PlanCandidate {
            when: String::new(),
            place: String::new(),
            details: String::new(),
            ..summit()
        };
        let anchor = anchor_from_plan(
            &bare,
            "jerry",
            AnchorOrigin::Chat,
            None,
            now(),
            NAIROBI,
            now(),
        )
        .unwrap();
        assert!(anchor.facts.is_empty() && anchor.place.is_none() && anchor.starts_at.is_none());
    }

    #[test]
    fn a_year_the_words_name_must_be_the_year_they_were_read_as() {
        let read = |words: &str| moment_said(words, now(), NAIROBI);
        assert!(read("3 December 2026 at 8am").is_some());
        assert!(read("2 January 2027").is_some());
        assert_eq!(read("3 December 2027"), None);
        assert_eq!(read("no time at all"), None);
    }

    #[test]
    fn a_plan_whose_day_has_passed_by_the_time_it_is_read_is_no_anchor() {
        let said_at = now() - chrono::Duration::days(3);
        let plan = PlanCandidate {
            when: "tomorrow at 10am".into(),
            ..summit()
        };
        let stale = anchor_from_plan(
            &plan,
            "jerry",
            AnchorOrigin::Chat,
            None,
            said_at,
            NAIROBI,
            now(),
        );
        assert_eq!(stale, None);
        let untitled = PlanCandidate {
            title: " . ".into(),
            ..summit()
        };
        let none = anchor_from_plan(
            &untitled,
            "jerry",
            AnchorOrigin::Chat,
            None,
            now(),
            NAIROBI,
            now(),
        );
        assert_eq!(none, None);
    }

    fn candidates() -> Vec<GateCandidate> {
        vec![
            GateCandidate::new(GateKind::Memory, "Jerry lives in\n Nairobi."),
            GateCandidate::new(GateKind::Interest, "Formula 1"),
            GateCandidate::new(GateKind::Reminder, "Call the dentist (on Friday)"),
            GateCandidate::plan(&summit()),
        ]
    }

    #[test]
    fn the_gate_is_shown_the_window_the_dont_list_and_the_candidates_by_number() {
        let message = build_gate_message(
            "Conversation:\nJerry: I am going to the AI Summit on 3 December.\n",
            &candidates(),
            "Never: recipes built on a colour preference",
        );
        assert_eq!(
            message,
            "Conversation:\nJerry: I am going to the AI Summit on 3 December.\n\n\
             The member's don't-list:\nNever: recipes built on a colour preference\n\n\
             Candidates to check:\n\
             1. memory: Jerry lives in Nairobi.\n\
             2. interest: Formula 1\n\
             3. reminder: Call the dentist (on Friday)\n\
             4. plan: event: AI Summit; when: 3 December at 8am; where: KICC, Nairobi; \
             details: Going with Amina; wants the robotics track\n\n\
             JSON:"
        );
        let bare = build_gate_message("", &candidates()[..1], " ");
        assert_eq!(
            bare,
            "Candidates to check:\n1. memory: Jerry lives in Nairobi.\n\nJSON:"
        );
    }

    #[test]
    fn the_gates_drops_read_as_places_with_their_reasons() {
        let drops = parse_gate_drops(
            r#"{"drop": [{"n": 2, "why": "asked  once"}, {"n": 4, "why": "the assistant proposed it"}]}"#,
            4,
        );
        assert_eq!(
            drops,
            Some(vec![
                (1, "asked once".to_string()),
                (3, "the assistant proposed it".to_string())
            ])
        );
        assert_eq!(parse_gate_drops(r#"{"drop": []}"#, 4), Some(vec![]));
    }

    #[test]
    fn the_shapes_a_small_model_writes_all_read() {
        let places = |raw: &str| -> Vec<usize> {
            parse_gate_drops(raw, 4)
                .unwrap_or_else(|| panic!("unread: {raw}"))
                .into_iter()
                .map(|(place, _)| place)
                .collect()
        };
        assert_eq!(places("```json\n{\"drop\": [3, 1]}\n```"), [0, 2]);
        assert_eq!(places("Drop these: [1, 3]"), [0, 2]);
        assert_eq!(places("<think>2 is a lookup</think>{\"drop\":[2]}"), [1]);
        assert_eq!(
            places(r#"{"drop": [{"number": "2.", "reason": "x"}]}"#),
            [1]
        );
        assert_eq!(places(r#"{"drop": 4}"#), [3]);
        assert_eq!(places(r#"{"n": 2, "why": "a lookup"}"#), [1]);
        assert_eq!(
            places(r#"[{"n": 1, "why": "a"}, {"n": 3, "why": "b"}]"#),
            [0, 2]
        );
        assert_eq!(places(r#"{"drop": [2, 2, 9, 0, null, "x", 4]}"#), [1, 3]);
        let long = format!(r#"{{"drop": [{{"n": 1, "why": "{}"}}]}}"#, "w".repeat(400));
        let read = parse_gate_drops(&long, 4).unwrap();
        assert_eq!(read[0].1.chars().count(), MAX_REASON_CHARS);
    }

    #[test]
    fn a_ruling_that_cannot_be_read_is_none() {
        for raw in [
            "",
            "They all look fine to me.",
            r#"{"keep": [1, 2]}"#,
            r#"{"drop": "none"}"#,
            r#"{"drop": [1"#,
            r#"{"drop": [9]}"#,
            r#"{"drop": ["the first one"]}"#,
            GATE_SYSTEM,
        ] {
            assert_eq!(parse_gate_drops(raw, 4), None, "{raw}");
        }
    }

    #[test]
    fn a_readable_ruling_keeps_everything_it_did_not_drop() {
        let kinds: Vec<GateKind> = candidates().iter().map(|c| c.kind).collect();
        let drops = vec![(1, "a lookup".to_string()), (3, String::new())];
        assert_eq!(apply_gate(&kinds, Some(&drops)), [true, false, true, false]);
        assert_eq!(apply_gate(&kinds, Some(&[])), [true; 4]);
        let memory_dropped = vec![(0, "the assistant said it".to_string())];
        assert_eq!(
            apply_gate(&kinds, Some(&memory_dropped)),
            [false, true, true, true]
        );
    }

    #[test]
    fn an_unreadable_ruling_keeps_memories_and_reminders_and_drops_interests_and_plans() {
        let kinds = [
            GateKind::Memory,
            GateKind::Interest,
            GateKind::Reminder,
            GateKind::Plan,
        ];
        assert_eq!(apply_gate(&kinds, None), [true, false, true, false]);
        assert!(apply_gate(&[], None).is_empty());
    }

    #[test]
    fn the_gate_prompt_names_every_reason_to_drop() {
        for reason in [
            "did not say it themselves",
            "one-off lookup",
            "about the pond, the assistant",
            "already known",
            "don't-list",
        ] {
            assert!(GATE_SYSTEM.contains(reason), "{reason}");
        }
        assert!(GATE_SYSTEM.contains(CANDIDATES_HEADING.trim_end_matches(':')));
    }
}
