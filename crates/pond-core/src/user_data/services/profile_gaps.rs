//! The fact-sheet gap question (docs/architecture/structured-questions.md, section 6): code picks
//! one part of the member's fact sheet that holds nothing yet, the model words one question for
//! it, and it is asked as a batch of purpose `profile`, at most one a day and never pushed.

use chrono::{DateTime, Duration, Utc};

use crate::user_data::domain::fact_sheet::{FactCategory, FactLine, FactState};
use crate::user_data::domain::question::{
    Question, QuestionDraft, QuestionPurpose, QuestionState, MAX_HEADER_CHARS,
};
use crate::user_data::services::model_json::object_in;

/// One profile batch a member gets in this long at most.
pub const ONE_BATCH_EVERY: Duration = Duration::days(1);

/// A part of the fact sheet a question may fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileGap {
    pub id: &'static str,
    pub header: &'static str,
    pub means: &'static str,
    pub category: FactCategory,
}

/// In the order asked. Health is left out: it is the member's to offer, not Goose's to probe.
pub const PROFILE_GAPS: &[ProfileGap] = &[
    ProfileGap {
        id: "home",
        header: "Home",
        means: "where they live: the town or area",
        category: FactCategory::Home,
    },
    ProfileGap {
        id: "work",
        header: "Work",
        means: "what fills their weekdays: work, study or home",
        category: FactCategory::Work,
    },
    ProfileGap {
        id: "people",
        header: "People",
        means: "who they live with or spend most time with",
        category: FactCategory::People,
    },
    ProfileGap {
        id: "routine",
        header: "Your week",
        means: "how a usual week goes",
        category: FactCategory::Routine,
    },
    ProfileGap {
        id: "food",
        header: "Food",
        means: "what they like to eat, and anything they avoid",
        category: FactCategory::Food,
    },
    ProfileGap {
        id: "style",
        header: "Free time",
        means: "how they like to spend free time",
        category: FactCategory::Style,
    },
];

/// Whether a profile batch may be asked now, given when the last one was made.
pub fn is_due(last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last.is_none_or(|at| now - at >= ONE_BATCH_EVERY)
}

/// The first gap the sheet holds no active line for and no profile question was asked about,
/// whatever came of it; `None` when every one is filled or asked.
pub fn next_gap(sheet: &[FactLine], asked: &[Question]) -> Option<&'static ProfileGap> {
    PROFILE_GAPS.iter().find(|gap| {
        let filled = sheet
            .iter()
            .any(|line| line.state == FactState::Active && line.category == gap.category);
        let tried = asked.iter().any(|q| {
            q.purpose == QuestionPurpose::Profile
                && q.gap.as_deref() == Some(gap.id)
                && q.state != QuestionState::Expired
        });
        !filled && !tried
    })
}

/// Fixed so it stays in the KV prefix. It shows no worked question: a small model copies one.
pub const PROFILE_QUESTION_SYSTEM: &str = "\
You word one question Goose asks a member of this household, to fill a gap \
in what it knows about them. You are given the detail to ask about and what \
is already known.

Reply with JSON and nothing else:

{\"header\": \"<1 to 2 words>\", \"text\": \"<the question>\", \"options\": \
[{\"label\": \"<short answer>\", \"description\": \"<what choosing it means>\"}], \
\"multi\": false}

- \"text\": one short, friendly question to the member, as \"you\", under 120 characters.
- \"options\": 2 to 4 common answers, each different and under 40 characters. Never \
names or places you were not given, and never \"Other\": it is always offered beside them.
- \"description\": one short line on what Goose does with that answer.
- \"multi\": true only when several options can be chosen together.
- No explanation, no preamble.";

/// The model's user message: the gap, then the sheet as a brief reads it.
pub fn build_profile_question_message(gap: &ProfileGap, sheet: &str) -> String {
    let known = if sheet.trim().is_empty() {
        "(nothing yet)".to_string()
    } else {
        sheet.trim().to_string()
    };
    format!(
        "Detail to ask about: {}\n\nWhat is already known about the member:\n{known}\n\nJSON:",
        gap.means
    )
}

/// The question the model wrote, held to section 2's shape; a header that is missing or too
/// long becomes the gap's. `None` when the reply is not a question code would ask.
pub fn parse_profile_question(
    raw: &str,
    profile_id: &str,
    batch_id: &str,
    gap: &ProfileGap,
    now: DateTime<Utc>,
) -> Option<Question> {
    let reply = serde_json::Value::Object(object_in(raw)?);
    let mut draft = QuestionDraft::read(&reply)?;
    if draft.header.is_empty() || draft.header.chars().count() > MAX_HEADER_CHARS {
        draft.header = gap.header.to_string();
    }
    draft.gap = Some(gap.id.to_string());
    Question::from_draft(profile_id, batch_id, QuestionPurpose::Profile, &draft, now).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::fact_sheet::{Confidence, FactSource};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, 9, 0, 0).unwrap()
    }

    fn line(category: FactCategory, fact: &str) -> FactLine {
        FactLine::new(
            "jerry",
            category,
            fact,
            Confidence::Sure,
            FactSource::you(),
            now(),
        )
        .unwrap()
    }

    fn asked(gap: &str, state: QuestionState) -> Question {
        let mut q = Question::new(
            "jerry",
            "b",
            QuestionPurpose::Profile,
            &format!("About {gap}?"),
            &["A", "B"],
            false,
            now(),
        )
        .unwrap();
        q.gap = Some(gap.into());
        q.state = state;
        q
    }

    #[test]
    fn the_gap_is_the_first_part_of_the_sheet_neither_filled_nor_asked_about() {
        assert_eq!(next_gap(&[], &[]).unwrap().id, "home");
        let sheet = [line(FactCategory::Home, "Lives in Kilimani")];
        assert_eq!(next_gap(&sheet, &[]).unwrap().id, "work");
        let tried = [
            asked("work", QuestionState::Skipped),
            asked("people", QuestionState::Expired),
        ];
        assert_eq!(
            next_gap(&sheet, &tried).unwrap().id,
            "people",
            "an expired question may be asked again; a skipped one is not"
        );
        let full: Vec<FactLine> = PROFILE_GAPS
            .iter()
            .map(|g| line(g.category, &format!("Something about {}", g.id)))
            .collect();
        assert_eq!(next_gap(&full, &[]), None);
        assert!(PROFILE_GAPS
            .iter()
            .all(|g| g.header.chars().count() <= MAX_HEADER_CHARS));
        assert!(PROFILE_GAPS
            .iter()
            .all(|g| g.category != FactCategory::Health));
    }

    #[test]
    fn a_batch_is_due_once_a_day() {
        assert!(is_due(None, now()));
        assert!(!is_due(Some(now() - Duration::hours(23)), now()));
        assert!(is_due(Some(now() - Duration::hours(24)), now()));
    }

    #[test]
    fn the_models_question_is_held_to_the_shape_and_carries_its_gap() {
        let gap = &PROFILE_GAPS[1];
        let message = build_profile_question_message(gap, "Home: lives in Kilimani");
        assert!(message.starts_with("Detail to ask about: what fills their weekdays"));
        assert!(message.contains("Home: lives in Kilimani"));
        let raw = r#"```json
{"text": "What fills most of your weekdays?", "options": [{"label": "Office work", "description": "Goose plans around office hours"}, {"label": "Studying", "description": "Goose keeps term dates in mind"}], "recommended": "Office work"}
```"#;
        let q = parse_profile_question(raw, "jerry", "b1", gap, now()).unwrap();
        assert_eq!(q.purpose, QuestionPurpose::Profile);
        assert_eq!(
            q.header, "Work",
            "the gap's header when the model gave none"
        );
        assert_eq!(q.gap.as_deref(), Some("work"));
        assert_eq!(q.recommended, Some(0));
        let other =
            r#"{"header": "Work", "text": "What do you do?", "options": ["Work", "Other"]}"#;
        assert_eq!(
            parse_profile_question(other, "jerry", "b1", gap, now()),
            None
        );
        assert_eq!(
            parse_profile_question("no idea", "jerry", "b1", gap, now()),
            None
        );
    }
}
