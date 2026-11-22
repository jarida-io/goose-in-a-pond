//! The questions Goose asks a member about a plan: code picks the open gaps (`gaps.rs`), the
//! maker words one question for each, and the critic drops what the shown facts already settle
//! or whose options do not fit its gap.

use chrono::{DateTime, Utc};

use super::brief::anchor_block;
use super::gaps::Gap;
use super::{answer_lines, section};
use crate::models::services::thought_filter::strip_reasoning;
use crate::user_data::domain::anchor::Anchor;
use crate::user_data::domain::interest::topic_key;
use crate::user_data::domain::question::{
    Question, QuestionDraft, QuestionPurpose, MAX_HEADER_CHARS, QUESTIONS_PER_BATCH,
};
use crate::user_data::services::model_json::{object_in, text_of};

/// Fixed so it stays in the KV prefix. It shows no worked question: a small model copies one.
pub const QUESTIONS_SYSTEM: &str = "\
You word the questions Goose asks a member of this household before it \
prepares something for them. Code has already chosen what to ask: the \
details listed under \"Details to ask about\". You are given what the work \
is for, the facts about the member that bear on those details, what they \
already answered, and what the sources say.

For each detail, write one question with options the member can tap.

Reply with JSON and nothing else:

{\"questions\": [{\"gap\": \"<the detail's name>\", \"header\": \"<1 to 2 words>\", \
\"text\": \"<the question>\", \"options\": [{\"label\": \"<short answer>\", \"description\": \
\"<what choosing it means>\"}], \"recommended\": \"<the label you would pick, or empty>\", \
\"multi\": false}]}

- One question for each detail listed, and none for anything else.
- \"text\": one short question to the member, as \"you\", under 120 characters.
- \"options\": 2 to 4 answers, each different and each under 40 characters. Take them \
from the facts and the sources, or use plain ranges such as \"Under KSh 20,000\". Never \
invent places, names or prices. Never \"Other\": it is always offered beside them.
- \"description\": one short line on what choosing the option means for the work.
- \"multi\": true only when several options can be chosen together.
- No explanation, no preamble.";

/// Fixed so it stays in the KV prefix.
pub const QUESTIONS_CRITIC_SYSTEM: &str = "\
You check questions before Goose asks them of a member of this household. You \
are given what the work is for, the facts about the member that bear on it, what \
they already answered, and the numbered questions, each with the detail it asks about.

Drop a question when any of these is true:
- the facts or an earlier answer already settle it;
- its options do not fit the detail it asks about, or are unrelated to the work.

Reply with JSON and nothing else:

{\"drop\": [<the numbers of the questions to drop>]}

- Use only the numbers you were given. An empty list when every question is worth asking.
- No explanation, no preamble.";

const ANCHOR_HEADING: &str = "What the work is for:";
const SHEET_HEADING: &str = "Facts about the member that bear on these details:";
const ANSWERED_HEADING: &str = "What they already answered:";
const SOURCES_HEADING: &str = "What the sources say:";
const GAPS_HEADING: &str = "Details to ask about:";
const QUESTIONS_HEADING: &str = "Questions to check:";
const REPLY_CUE: &str = "JSON:";

/// What a maker and its critic are both shown.
#[derive(Debug, Clone, Copy)]
pub struct Shown<'a> {
    pub anchor: &'a Anchor,
    /// The fact-sheet lines in the open gaps' categories, as a brief reads them.
    pub sheet: &'a str,
    pub answered: &'a [(String, String)],
    /// Passages the plan's work already found, one per line.
    pub sources: &'a str,
    pub gaps: &'a [&'a Gap],
    pub timezone: &'a str,
}

fn shared_parts(shown: &Shown<'_>) -> String {
    let mut out = String::new();
    section(
        &mut out,
        ANCHOR_HEADING,
        &anchor_block(shown.anchor, shown.timezone),
    );
    section(&mut out, SHEET_HEADING, shown.sheet);
    section(&mut out, ANSWERED_HEADING, &answer_lines(shown.answered));
    section(&mut out, SOURCES_HEADING, shown.sources);
    out
}

/// The maker's user message: the plan, the gap-relevant facts, the stored answers, what the
/// sources say, and the open gaps by name.
pub fn build_questions_message(shown: &Shown<'_>) -> String {
    let mut out = shared_parts(shown);
    let gaps: Vec<String> = shown
        .gaps
        .iter()
        .map(|gap| format!("- {}: {}", gap.id, gap.means))
        .collect();
    section(&mut out, GAPS_HEADING, &gaps.join("\n"));
    out.push_str(REPLY_CUE);
    out
}

/// The list a reply holds under `key`, or the reply itself when it is a bare list.
fn list_under(raw: &str, key: &str) -> Option<Vec<serde_json::Value>> {
    if let Some(map) = object_in(raw) {
        return map.get(key)?.as_array().cloned();
    }
    let cleaned = strip_reasoning(raw);
    let (start, end) = (cleaned.find('[')?, cleaned.rfind(']')?);
    serde_json::from_str(cleaned.get(start..=end)?).ok()
}

/// A maker's question as a draft for `gap`: a header that is missing or too long becomes the
/// gap's.
fn draft_of(item: &serde_json::Value, gap: &Gap) -> QuestionDraft {
    let mut draft = QuestionDraft::read(item).unwrap_or_default();
    if draft.header.is_empty() || draft.header.chars().count() > MAX_HEADER_CHARS {
        draft.header = gap.header.to_string();
    }
    draft.gap = Some(gap.id.to_string());
    draft
}

/// The questions a maker wrote, as one open batch for a plan: one per open gap, each passing
/// the shape code holds every question to. A question with no gap, or for a gap that is not
/// open, is ignored.
pub fn parse_questions(
    raw: &str,
    profile_id: &str,
    batch_id: &str,
    anchor_id: &str,
    gaps: &[&Gap],
    now: DateTime<Utc>,
) -> Vec<Question> {
    let items = list_under(raw, "questions")
        .or_else(|| object_in(raw).map(|one| vec![serde_json::Value::Object(one)]))
        .unwrap_or_default();
    let mut batch: Vec<Question> = Vec::new();
    for value in &items {
        let Some(item) = value.as_object() else {
            continue;
        };
        if batch.len() == QUESTIONS_PER_BATCH {
            break;
        }
        let named = text_of(item, "gap").map(|g| topic_key(&g).replace(' ', "_"));
        let Some(gap) = named.and_then(|g| gaps.iter().find(|gap| gap.id == g)) else {
            continue;
        };
        if batch.iter().any(|held| held.gap.as_deref() == Some(gap.id)) {
            continue;
        }
        let draft = draft_of(value, gap);
        let Ok(mut question) =
            Question::from_draft(profile_id, batch_id, QuestionPurpose::Anchor, &draft, now)
        else {
            continue;
        };
        if batch.iter().any(|held| held.key() == question.key()) {
            continue;
        }
        question.anchor_id = Some(anchor_id.to_string());
        batch.push(question);
    }
    batch
}

/// The critic's user message: what the maker saw, then the questions numbered from 1.
pub fn build_questions_critic_message(shown: &Shown<'_>, questions: &[Question]) -> String {
    let numbered: Vec<String> = questions
        .iter()
        .enumerate()
        .map(|(at, question)| {
            format!(
                "{}. ({}) {}\n   Options: {}",
                at + 1,
                question.gap.as_deref().unwrap_or("detail"),
                question.text,
                question.labels().join(" | ")
            )
        })
        .collect();
    let mut out = shared_parts(shown);
    section(&mut out, QUESTIONS_HEADING, &numbered.join("\n"));
    out.push_str(REPLY_CUE);
    out
}

/// Which of `count` questions a critic dropped, as places from 0. A reply that cannot be read
/// drops none: the batch is kept and code's own checks still run on it.
pub fn parse_question_drops(raw: &str, count: usize) -> Vec<usize> {
    let Some(items) = list_under(raw, "drop") else {
        return Vec::new();
    };
    let mut drops: Vec<usize> = items
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::Number(number) => number.as_u64(),
            serde_json::Value::String(number) => number.trim().parse().ok(),
            _ => None,
        })
        .filter_map(|number| usize::try_from(number).ok())
        .filter(|number| (1..=count).contains(number))
        .map(|number| number - 1)
        .collect();
    drops.sort_unstable();
    drops.dedup();
    drops
}

/// The questions a critic left standing.
pub fn without_dropped(questions: Vec<Question>, drops: &[usize]) -> Vec<Question> {
    questions
        .into_iter()
        .enumerate()
        .filter(|(at, _)| !drops.contains(at))
        .map(|(_, question)| question)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::{AnchorKind, AnchorOrigin};
    use crate::user_data::domain::question::{QuestionState, MAX_OPTIONS, MIN_OPTIONS};
    use crate::user_data::services::engine::gaps::{gap_named, gaps_of};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn trip() -> Anchor {
        let mut trip = Anchor::new(
            "jerry",
            AnchorKind::Trip,
            "Kisumu in December",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        trip.starts_at = Some(Utc.with_ymd_and_hms(2026, 12, 11, 21, 0, 0).unwrap());
        trip
    }

    fn trip_gaps() -> Vec<&'static Gap> {
        ["budget", "stay", "who"]
            .iter()
            .map(|id| gap_named(AnchorKind::Trip, id).unwrap())
            .collect()
    }

    fn answered() -> Vec<(String, String)> {
        vec![("Who is coming?".into(), "Two adults".into())]
    }

    fn read(raw: &str) -> Vec<Question> {
        parse_questions(raw, "jerry", "batch-1", "anchor-1", &trip_gaps(), now())
    }

    const TWO: &str = r#"{"questions": [
        {"gap": "budget", "header": "Budget", "text": "What is your budget for the trip?", "options": [{"label": "Under KSh 50,000", "description": "Buses and guest houses"}, {"label": "KSh 50,000 to 100,000", "description": "A mid-range hotel"}, {"label": "More", "description": "Flights and a resort"}], "recommended": "KSh 50,000 to 100,000", "multi": false},
        {"gap": "stay", "text": "Where do you want to stay?", "options": ["By the lake", "In town"], "multi": true}]}"#;

    #[test]
    fn the_maker_is_shown_the_plan_the_relevant_facts_and_the_gaps_to_ask_about() {
        let gaps = trip_gaps();
        let anchor = trip();
        let message = build_questions_message(&Shown {
            anchor: &anchor,
            sheet: "People: lives with Amina",
            answered: &answered(),
            sources: "- Lake Victoria hotels from KSh 6,000 a night",
            gaps: &gaps,
            timezone: "Africa/Nairobi",
        });
        assert_eq!(
            message,
            "What the work is for:\nKind: trip\nTitle: Kisumu in December\n\
             When: on Saturday 12 December 2026\n\n\
             Facts about the member that bear on these details:\nPeople: lives with Amina\n\n\
             What they already answered:\n- Who is coming? Answer: Two adults\n\n\
             What the sources say:\n- Lake Victoria hotels from KSh 6,000 a night\n\n\
             Details to ask about:\n- budget: how much they want to spend in all\n\
             - stay: where to stay, or the base to work from\n- who: who is going\n\n\
             JSON:"
        );
        let bare = build_questions_message(&Shown {
            anchor: &anchor,
            sheet: "",
            answered: &[],
            sources: "",
            gaps: &gaps,
            timezone: "Africa/Nairobi",
        });
        assert!(!bare.contains("Facts about"), "{bare}");
        assert!(!bare.contains("sources say"), "{bare}");
    }

    #[test]
    fn a_batch_reads_as_structured_questions_one_per_gap() {
        let batch = read(TWO);
        assert_eq!(batch.len(), 2);
        let budget = &batch[0];
        assert_eq!(budget.header, "Budget");
        assert_eq!(budget.gap.as_deref(), Some("budget"));
        assert_eq!(budget.options[1].description, "A mid-range hotel");
        assert_eq!(budget.recommended, Some(1), "the pick read from its label");
        assert_eq!(budget.anchor_id.as_deref(), Some("anchor-1"));
        assert_eq!(budget.purpose, QuestionPurpose::Anchor);
        assert_eq!(budget.state, QuestionState::Open);
        let stay = &batch[1];
        assert_eq!(
            stay.header, "Stay",
            "the gap's header when the maker gave none"
        );
        assert!(stay.multi);
    }

    #[test]
    fn an_option_meaning_other_refuses_its_question() {
        let raw = r#"{"questions": [{"gap": "stay", "header": "Stay", "text": "Where?", "options": ["Lake", "Town", "Other"]}]}"#;
        assert!(read(raw).is_empty());
        let fine = r#"{"questions": [{"gap": "stay", "header": "Stay", "text": "Where?", "options": ["Lake", "Town"]}]}"#;
        assert_eq!(read(fine).len(), 1);
    }

    #[test]
    fn a_question_with_no_open_gap_is_ignored() {
        let raw = r#"{"questions": [
            {"text": "Do you like physics?", "header": "Physics", "options": ["Yes", "No"]},
            {"gap": "dates", "header": "Dates", "text": "When do you leave?", "options": ["Friday", "Saturday"]},
            {"gap": "who", "header": "Who", "text": "Who is going?", "options": ["Just me", "Two of us"]},
            {"gap": "who", "header": "Who", "text": "Who else comes?", "options": ["Kids", "Friends"]}]}"#;
        let batch = read(raw);
        assert_eq!(batch.len(), 1, "{batch:?}");
        assert_eq!(batch[0].gap.as_deref(), Some("who"));
    }

    #[test]
    fn the_shapes_a_small_model_writes_all_read() {
        let one = r#"{"gap": "budget", "header": "Budget", "text": "How much will you spend?", "options": ["Under 50k", "Over 50k"]}"#;
        for raw in [
            TWO.to_string(),
            format!("```json\n{TWO}\n```"),
            format!("Here are the questions:\n{TWO}\nI hope these help."),
            format!("<think>what is missing?</think>{TWO}"),
            format!(
                "[{one}, {}]",
                one.replace("budget", "stay").replace("How much", "Where")
            ),
        ] {
            assert_eq!(read(&raw).len(), 2, "{raw}");
        }
        assert_eq!(read(one).len(), 1, "one question with no list round it");
        for nothing in ["", "No questions are needed.", r#"{"questions": []}"#] {
            assert!(read(nothing).is_empty(), "{nothing}");
        }
    }

    #[test]
    fn the_critic_is_shown_what_the_maker_saw_and_each_questions_gap() {
        let gaps = trip_gaps();
        let anchor = trip();
        let shown = Shown {
            anchor: &anchor,
            sheet: "People: lives with Amina",
            answered: &answered(),
            sources: "",
            gaps: &gaps,
            timezone: "Africa/Nairobi",
        };
        let message = build_questions_critic_message(&shown, &read(TWO));
        assert!(message.starts_with("What the work is for:"));
        assert!(!message.contains("Details to ask about"));
        assert!(
            message.ends_with(
                "Questions to check:\n\
             1. (budget) What is your budget for the trip?\n   \
             Options: Under KSh 50,000 | KSh 50,000 to 100,000 | More\n\
             2. (stay) Where do you want to stay?\n   \
             Options: By the lake | In town\n\nJSON:"
            ),
            "{message}"
        );
    }

    #[test]
    fn the_critics_drops_read_as_places_and_only_the_numbers_it_was_given() {
        assert_eq!(parse_question_drops(r#"{"drop": [2]}"#, 3), [1]);
        assert_eq!(parse_question_drops(r#"{"drop": []}"#, 3), [0usize; 0]);
        assert_eq!(
            parse_question_drops(
                "```json\n{\"drop\": [3, 1, 3, 0, 9, \"2\", 1.5, null]}\n```",
                3
            ),
            [0, 1, 2]
        );
        assert_eq!(parse_question_drops("Drop these: [1, 3]", 3), [0, 2]);
    }

    #[test]
    fn a_critic_that_cannot_be_read_keeps_the_batch() {
        for raw in [
            "",
            "They all look fine to me.",
            r#"{"keep": [1, 2]}"#,
            r#"{"drop": "none"}"#,
            r#"{"drop": [1"#,
        ] {
            assert_eq!(parse_question_drops(raw, 3), [0usize; 0], "{raw}");
        }
    }

    #[test]
    fn the_questions_a_critic_dropped_are_not_asked() {
        let batch = read(TWO);
        let kept = without_dropped(batch.clone(), &[0]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "Where do you want to stay?");
        assert_eq!(without_dropped(batch, &[]).len(), 2);
    }

    #[test]
    fn the_prompts_state_the_caps_the_code_holds_them_to() {
        assert!(QUESTIONS_SYSTEM.contains(&format!("{MIN_OPTIONS} to {MAX_OPTIONS} answers")));
        let all: Vec<&Gap> = gaps_of(AnchorKind::Trip).iter().collect();
        assert!(
            parse_questions(QUESTIONS_SYSTEM, "jerry", "b", "a", &all, now()).is_empty(),
            "the schema line is not a batch"
        );
        assert_eq!(
            parse_question_drops(QUESTIONS_CRITIC_SYSTEM, 2),
            [0usize; 0],
            "the schema line is not a ruling"
        );
    }
}
