//! The questions Goose asks a member about an anchor: the maker's prompt and parse, and the
//! critic that drops what the facts or their earlier answers already settle.

use chrono::{DateTime, Utc};

use super::brief::anchor_block;
use super::{answer_lines, section};
use crate::models::services::thought_filter::strip_reasoning;
use crate::user_data::domain::anchor::Anchor;
use crate::user_data::domain::interest::topic_key;
use crate::user_data::domain::question::{Question, QuestionPurpose, QUESTIONS_PER_BATCH};
use crate::user_data::services::model_json::{object_in, text_of, texts_of};

/// Fixed so it stays in the KV prefix. It shows no worked question: a small model copies one.
pub const QUESTIONS_SYSTEM: &str = "\
You write the questions Goose asks a member of this household before it \
prepares something for them, so the work fits them. You are given what the \
work is for, what is already known about the member, and what they already \
answered.

Ask only what would change the work: a budget, a date, who is coming, what \
matters most, a choice between real options. Never ask:
- what the facts or their earlier answers already say;
- what Goose can look up itself;
- a question with one sensible answer.

Reply with JSON and nothing else:

{\"questions\": [{\"text\": \"<the question>\", \"options\": [\"<option>\", \"<option>\"], \"multi\": false}]}

- At most 4 questions. Fewer is better, and an empty list when nothing is missing.
- \"text\": one short question to the member, as \"you\", under 120 characters.
- \"options\": 2 to 4 short answers they could tap, each different, each under \
40 characters. Never \"Other\": it is always offered beside them.
- \"multi\": true only when several options can be chosen together.
- No explanation, no preamble.";

/// Fixed so it stays in the KV prefix.
pub const QUESTIONS_CRITIC_SYSTEM: &str = "\
You check questions before Goose asks them of a member of this household. You \
are given what the work is for, what is already known about the member, what \
they already answered, and the numbered questions.

Drop a question when any of these is true:
- the facts or an earlier answer already settle it;
- it repeats another question in the list;
- Goose could look the answer up itself;
- no answer to it would change what Goose prepares.

Reply with JSON and nothing else:

{\"drop\": [<the numbers of the questions to drop>]}

- Use only the numbers you were given. An empty list when every question is worth asking.
- No explanation, no preamble.";

const ANCHOR_HEADING: &str = "What the work is for:";
const SHEET_HEADING: &str = "What is known about the member:";
const ANSWERED_HEADING: &str = "What they already answered:";
const QUESTIONS_HEADING: &str = "Questions to check:";
const REPLY_CUE: &str = "JSON:";

/// What a surface offers beside every question's options, so no option may mean it.
const OTHER_KEYS: [&str; 3] = ["other", "others", "something else"];

fn shared_parts(
    anchor: &Anchor,
    sheet: &str,
    answered: &[(String, String)],
    timezone: &str,
) -> String {
    let mut out = String::new();
    section(&mut out, ANCHOR_HEADING, &anchor_block(anchor, timezone));
    section(&mut out, SHEET_HEADING, sheet);
    section(&mut out, ANSWERED_HEADING, &answer_lines(answered));
    out
}

/// The maker's user message: the anchor on the household's clock, the fact sheet and the
/// member's stored answers as question and answer.
pub fn build_questions_message(
    anchor: &Anchor,
    sheet: &str,
    answered: &[(String, String)],
    timezone: &str,
) -> String {
    let mut out = shared_parts(anchor, sheet, answered, timezone);
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

fn means_other(option: &str) -> bool {
    let named = option.split('(').next().unwrap_or(option);
    OTHER_KEYS.contains(&topic_key(named).as_str())
}

/// The questions a maker wrote, as one open batch for an anchor: at most four, each with two
/// to four options, none of them "Other". A question that does not fit is left out.
pub fn parse_questions(
    raw: &str,
    profile_id: &str,
    batch_id: &str,
    anchor_id: &str,
    now: DateTime<Utc>,
) -> Vec<Question> {
    let items = list_under(raw, "questions")
        .or_else(|| object_in(raw).map(|one| vec![serde_json::Value::Object(one)]))
        .unwrap_or_default();
    let mut batch: Vec<Question> = Vec::new();
    for item in items.iter().filter_map(serde_json::Value::as_object) {
        if batch.len() == QUESTIONS_PER_BATCH {
            break;
        }
        let Some(text) = text_of(item, "text").or_else(|| text_of(item, "question")) else {
            continue;
        };
        let options: Vec<String> = texts_of(item, "options")
            .into_iter()
            .filter(|option| !means_other(option))
            .collect();
        let multi = item
            .get("multi")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let asked = Question::new(
            profile_id,
            batch_id,
            QuestionPurpose::Anchor,
            &text,
            &options,
            multi,
            now,
        );
        let Some(mut question) = asked else {
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
pub fn build_questions_critic_message(
    anchor: &Anchor,
    sheet: &str,
    answered: &[(String, String)],
    questions: &[Question],
    timezone: &str,
) -> String {
    let numbered: Vec<String> = questions
        .iter()
        .enumerate()
        .map(|(at, question)| {
            format!(
                "{}. {}\n   Options: {}",
                at + 1,
                question.text,
                question.options.join(" | ")
            )
        })
        .collect();
    let mut out = shared_parts(anchor, sheet, answered, timezone);
    section(&mut out, QUESTIONS_HEADING, &numbered.join("\n"));
    out.push_str(REPLY_CUE);
    out
}

/// Which of `count` questions a critic dropped, as places from 0. A reply that cannot be read
/// drops them all: an unchecked question is not asked.
pub fn parse_question_drops(raw: &str, count: usize) -> Vec<usize> {
    let Some(items) = list_under(raw, "drop") else {
        return (0..count).collect();
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

    fn answered() -> Vec<(String, String)> {
        vec![("Who is coming?".into(), "Two adults".into())]
    }

    fn read(raw: &str) -> Vec<Question> {
        parse_questions(raw, "jerry", "batch-1", "anchor-1", now())
    }

    const TWO: &str = r#"{"questions": [
        {"text": "What is your budget for the trip?", "options": ["Under KSh 50,000", "KSh 50,000 to 100,000", "More"], "multi": false},
        {"text": "What do you want most from it?", "options": ["Rest", "Food", "The lake", "Other"], "multi": true}]}"#;

    #[test]
    fn the_maker_is_shown_the_anchor_the_sheet_and_the_stored_answers() {
        let message = build_questions_message(
            &trip(),
            "home: lives in Nairobi\nfood: does not eat pork",
            &answered(),
            "Africa/Nairobi",
        );
        assert_eq!(
            message,
            "What the work is for:\nKind: trip\nTitle: Kisumu in December\n\
             When: on Saturday 12 December 2026\n\n\
             What is known about the member:\nhome: lives in Nairobi\nfood: does not eat pork\n\n\
             What they already answered:\n- Who is coming? Answer: Two adults\n\n\
             JSON:"
        );
        let bare = build_questions_message(&trip(), "", &[], "Africa/Nairobi");
        assert!(!bare.contains("What is known"), "{bare}");
        assert!(!bare.contains("already answered"), "{bare}");
    }

    #[test]
    fn a_batch_reads_as_open_questions_of_its_anchor() {
        let batch = read(TWO);
        assert_eq!(batch.len(), 2);
        let budget = &batch[0];
        assert_eq!(budget.text, "What is your budget for the trip?");
        assert_eq!(
            budget.options,
            ["Under KSh 50,000", "KSh 50,000 to 100,000", "More"]
        );
        assert!(!budget.multi);
        assert_eq!(budget.profile_id, "jerry");
        assert_eq!(budget.batch_id, "batch-1");
        assert_eq!(budget.anchor_id.as_deref(), Some("anchor-1"));
        assert_eq!(budget.purpose, QuestionPurpose::Anchor);
        assert_eq!(budget.state, QuestionState::Open);
        assert_eq!(budget.created_at, now());
        assert!(batch[1].multi);
        assert_eq!(
            batch[1].options,
            ["Rest", "Food", "The lake"],
            "never Other"
        );
    }

    #[test]
    fn the_shapes_a_small_model_writes_all_read() {
        let one = r#"{"text": "Do you want to drive there?", "options": ["Yes", "No"]}"#;
        for raw in [
            TWO.to_string(),
            format!("```json\n{TWO}\n```"),
            format!("Here are the questions:\n{TWO}\nI hope these help."),
            format!("<think>what is missing?</think>{TWO}"),
            format!("[{one}, {}]", one.replace("drive", "fly")),
        ] {
            assert_eq!(read(&raw).len(), 2, "{raw}");
        }
        assert_eq!(read(one).len(), 1, "one question with no list round it");
        assert_eq!(
            read(r#"{"questions": [{"question": "Do you want to drive there?", "options": ["Yes", "No"]}]}"#).len(),
            1
        );
        for nothing in [
            "",
            "No questions are needed.",
            r#"{"questions": []}"#,
            r#"{"questions": "none"}"#,
        ] {
            assert!(read(nothing).is_empty(), "{nothing}");
        }
    }

    #[test]
    fn a_batch_is_four_questions_at_most_each_asked_once() {
        let many: Vec<String> = (1..=6)
            .map(|n| format!(r#"{{"text": "Question number {n}?", "options": ["Yes", "No"]}}"#))
            .collect();
        let repeat = r#"{"text": "question NUMBER 1", "options": ["Maybe", "Never"]}"#;
        let raw = format!(
            r#"{{"questions": [{}, {repeat}, {}]}}"#,
            many[0],
            many[1..].join(",")
        );
        let batch = read(&raw);
        assert_eq!(batch.len(), QUESTIONS_PER_BATCH);
        let texts: Vec<&str> = batch.iter().map(|q| q.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Question number 1?",
                "Question number 2?",
                "Question number 3?",
                "Question number 4?"
            ]
        );
    }

    #[test]
    fn a_question_needs_words_and_two_real_options() {
        let raw = r#"{"questions": [
            {"text": "", "options": ["Yes", "No"]},
            {"text": "<the question>", "options": ["Yes", "No"]},
            {"text": "Which airline do you prefer?", "options": ["Any", "Other (please say)"]},
            {"text": "Which seat do you prefer?", "options": ["Window", "window", "Something else"]},
            {"text": "Which day do you leave?", "options": "Friday"},
            "When do you leave?",
            {"text": "How long will you stay?", "options": ["2 nights", "3 nights", "4 nights", "5 nights", "A week", "Others"]}]}"#;
        let batch = read(raw);
        assert_eq!(batch.len(), 1, "{batch:?}");
        assert_eq!(batch[0].text, "How long will you stay?");
        assert_eq!(
            batch[0].options,
            ["2 nights", "3 nights", "4 nights", "5 nights"]
        );
        assert!((MIN_OPTIONS..=MAX_OPTIONS).contains(&batch[0].options.len()));
    }

    #[test]
    fn the_critic_is_shown_what_the_maker_saw_and_the_questions_by_number() {
        let batch = read(TWO);
        let message = build_questions_critic_message(
            &trip(),
            "home: lives in Nairobi",
            &answered(),
            &batch,
            "Africa/Nairobi",
        );
        let maker = build_questions_message(
            &trip(),
            "home: lives in Nairobi",
            &answered(),
            "Africa/Nairobi",
        );
        assert!(message.starts_with(maker.trim_end_matches(REPLY_CUE)));
        assert!(
            message.ends_with(
                "Questions to check:\n\
             1. What is your budget for the trip?\n   \
             Options: Under KSh 50,000 | KSh 50,000 to 100,000 | More\n\
             2. What do you want most from it?\n   \
             Options: Rest | Food | The lake\n\nJSON:"
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
        assert_eq!(
            parse_question_drops("<think>2 is settled</think>{\"drop\":[2]}", 2),
            [1]
        );
    }

    #[test]
    fn a_critic_that_cannot_be_read_drops_every_question() {
        for raw in [
            "",
            "They all look fine to me.",
            r#"{"keep": [1, 2]}"#,
            r#"{"drop": "none"}"#,
            r#"{"drop": [1"#,
        ] {
            assert_eq!(parse_question_drops(raw, 3), [0, 1, 2], "{raw}");
        }
        assert_eq!(parse_question_drops("nonsense", 0), [0usize; 0]);
    }

    #[test]
    fn the_questions_a_critic_dropped_are_not_asked() {
        let batch = read(TWO);
        let kept = without_dropped(batch.clone(), &[0]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "What do you want most from it?");
        assert_eq!(without_dropped(batch.clone(), &[]).len(), 2);
        assert!(without_dropped(batch, &[0, 1]).is_empty());
    }

    #[test]
    fn the_prompts_state_the_caps_the_code_holds_them_to() {
        assert!(QUESTIONS_SYSTEM.contains(&format!("At most {QUESTIONS_PER_BATCH} questions")));
        assert!(QUESTIONS_SYSTEM.contains(&format!("{MIN_OPTIONS} to {MAX_OPTIONS} short answers")));
        assert!(
            read(QUESTIONS_SYSTEM).is_empty(),
            "the schema line is not a batch"
        );
        assert_eq!(
            parse_question_drops(QUESTIONS_CRITIC_SYSTEM, 2),
            [0, 1],
            "the schema line is not a ruling"
        );
    }
}
