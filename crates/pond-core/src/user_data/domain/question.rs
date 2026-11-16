//! Structured questions Goose asks a member when details are missing: up to four to a batch,
//! each with options and a free-text "Other" the surfaces always offer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::interest::topic_key;

pub const QUESTIONS_PER_BATCH: usize = 4;
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;
const MAX_TEXT_CHARS: usize = 200;
const MAX_OPTION_CHARS: usize = 60;
const MAX_OTHER_CHARS: usize = 500;

/// The key two wordings of one question share.
pub fn text_key(text: &str) -> String {
    topic_key(text)
}

fn one_line(raw: &str, max: usize) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max)
        .collect()
}

/// Options as stored: one line each, deduplicated by key, bounded, and never an "Other".
pub fn clean_options<'a>(raw: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .map(|option| one_line(option, MAX_OPTION_CHARS))
        .filter(|option| {
            let key = topic_key(option);
            !key.is_empty() && key != "other" && seen.insert(key)
        })
        .take(MAX_OPTIONS)
        .collect()
}

/// What an answer feeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionPurpose {
    /// Details an anchor's deliverables need.
    Anchor,
    /// Whether a guessed interest is one.
    Interest,
    /// A gap in the fact sheet.
    Profile,
    /// Something the Budget specialist could not read from a capture.
    Ledger,
}

impl QuestionPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Interest => "interest",
            Self::Profile => "profile",
            Self::Ledger => "ledger",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "anchor" => Some(Self::Anchor),
            "interest" => Some(Self::Interest),
            "profile" => Some(Self::Profile),
            "ledger" => Some(Self::Ledger),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionState {
    Open,
    Answered,
    Skipped,
    Expired,
}

impl QuestionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Answered => "answered",
            Self::Skipped => "skipped",
            Self::Expired => "expired",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "open" => Some(Self::Open),
            "answered" => Some(Self::Answered),
            "skipped" => Some(Self::Skipped),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// What a member answered: options they chose, and their own words under "Other".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl Answer {
    /// The answer as a brief reads it: the choices, then the member's own words.
    pub fn as_words(&self) -> String {
        let mut parts = self.choices.clone();
        parts.extend(self.text.clone());
        parts.join("; ")
    }
}

/// Why an answer was not taken, worded for the member's screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnswerRefused {
    NotOpen,
    Empty,
    NotAnOption(String),
    OneChoiceOnly,
}

impl std::fmt::Display for AnswerRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotOpen => f.write_str("this question is no longer open"),
            Self::Empty => f.write_str("choose an option or write an answer"),
            Self::NotAnOption(choice) => write!(f, "`{choice}` is not one of the options"),
            Self::OneChoiceOnly => f.write_str("this question takes one choice"),
        }
    }
}

impl std::error::Error for AnswerRefused {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub batch_id: String,
    pub purpose: QuestionPurpose,
    #[serde(default)]
    pub anchor_id: Option<String>,
    /// The interest, fact gap or capture the question is about, when it is not an anchor.
    #[serde(default)]
    pub subject_ref: Option<String>,
    pub text: String,
    pub options: Vec<String>,
    /// Whether several options may be chosen.
    pub multi: bool,
    /// The question whose free-text answer this one follows up.
    #[serde(default)]
    pub parent_id: Option<String>,
    pub state: QuestionState,
    #[serde(default)]
    pub answer: Option<Answer>,
    /// When the batch was pushed to the member.
    #[serde(default)]
    pub asked_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub answered_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl Question {
    /// An open question of `batch_id`; `None` when the text has no words or fewer than
    /// [`MIN_OPTIONS`] options are left.
    pub fn new(
        profile_id: &str,
        batch_id: &str,
        purpose: QuestionPurpose,
        text: &str,
        options: &[String],
        multi: bool,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        let text = one_line(text, MAX_TEXT_CHARS);
        let options = clean_options(options.iter().map(String::as_str));
        if text_key(&text).is_empty() || options.len() < MIN_OPTIONS {
            return None;
        }
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            batch_id: batch_id.to_string(),
            purpose,
            anchor_id: None,
            subject_ref: None,
            text,
            options,
            multi,
            parent_id: None,
            state: QuestionState::Open,
            answer: None,
            asked_at: None,
            answered_at: None,
            expires_at: None,
            created_at: now,
        })
    }

    pub fn key(&self) -> String {
        text_key(&self.text)
    }

    /// Takes the member's answer: listed options only, one unless `multi`, or their own words.
    pub fn answer(&mut self, answer: Answer, now: DateTime<Utc>) -> Result<(), AnswerRefused> {
        if self.state != QuestionState::Open {
            return Err(AnswerRefused::NotOpen);
        }
        let mut choices: Vec<String> = Vec::new();
        for choice in &answer.choices {
            let key = topic_key(choice);
            let Some(option) = self.options.iter().find(|o| topic_key(o) == key) else {
                return Err(AnswerRefused::NotAnOption(choice.clone()));
            };
            if !choices.contains(option) {
                choices.push(option.clone());
            }
        }
        if choices.len() > 1 && !self.multi {
            return Err(AnswerRefused::OneChoiceOnly);
        }
        let text = answer
            .text
            .map(|t| one_line(&t, MAX_OTHER_CHARS))
            .filter(|t| !t.is_empty());
        if choices.is_empty() && text.is_none() {
            return Err(AnswerRefused::Empty);
        }
        self.answer = Some(Answer { choices, text });
        self.state = QuestionState::Answered;
        self.answered_at = Some(now);
        Ok(())
    }

    /// The member passed on it; `false` when it was no longer open.
    pub fn skip(&mut self, now: DateTime<Utc>) -> bool {
        self.close(QuestionState::Skipped, now)
    }

    /// Nobody answered in time; `false` when it was no longer open.
    pub fn expire(&mut self, now: DateTime<Utc>) -> bool {
        self.close(QuestionState::Expired, now)
    }

    fn close(&mut self, state: QuestionState, now: DateTime<Utc>) -> bool {
        if self.state != QuestionState::Open {
            return false;
        }
        self.state = state;
        self.answered_at = Some(now);
        true
    }

    /// Open, with a deadline that has passed.
    pub fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        self.state == QuestionState::Open && self.expires_at.is_some_and(|at| at <= now)
    }

    /// An answer in the member's own words, which one follow-up batch may dig into.
    pub fn has_free_text(&self) -> bool {
        self.answer.as_ref().is_some_and(|a| a.text.is_some())
    }
}

/// One question as a surface asks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuestionView {
    pub id: String,
    pub text: String,
    pub options: Vec<String>,
    pub multi: bool,
}

/// The open questions of one batch, under the title of what they are about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuestionBatch {
    pub id: String,
    pub purpose: QuestionPurpose,
    pub anchor_id: Option<String>,
    pub title: String,
    pub questions: Vec<QuestionView>,
}

/// The open questions grouped into their batches, in the order given, at most
/// [`QUESTIONS_PER_BATCH`] each; `title` names what a batch is about from its first question.
pub fn batches(questions: &[Question], title: impl Fn(&Question) -> String) -> Vec<QuestionBatch> {
    let mut out: Vec<QuestionBatch> = Vec::new();
    for question in questions.iter().filter(|q| q.state == QuestionState::Open) {
        let at = match out.iter().position(|b| b.id == question.batch_id) {
            Some(at) => at,
            None => {
                out.push(QuestionBatch {
                    id: question.batch_id.clone(),
                    purpose: question.purpose,
                    anchor_id: question.anchor_id.clone(),
                    title: title(question),
                    questions: Vec::new(),
                });
                out.len() - 1
            }
        };
        if out[at].questions.len() < QUESTIONS_PER_BATCH {
            out[at].questions.push(QuestionView {
                id: question.id.clone(),
                text: question.text.clone(),
                options: question.options.clone(),
                multi: question.multi,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question(batch: &str, text: &str, multi: bool) -> Question {
        Question::new(
            "jerry",
            batch,
            QuestionPurpose::Anchor,
            text,
            &[
                "Under KSh 50,000".into(),
                "KSh 50,000 to 100,000".into(),
                "More".into(),
            ],
            multi,
            Utc::now(),
        )
        .unwrap()
    }

    fn chose(choices: &[&str], text: Option<&str>) -> Answer {
        Answer {
            choices: choices.iter().map(|c| c.to_string()).collect(),
            text: text.map(str::to_string),
        }
    }

    #[test]
    fn a_question_needs_words_and_two_options_and_never_lists_other() {
        let options = |raw: &[&str]| raw.iter().map(|o| o.to_string()).collect::<Vec<_>>();
        let make = |text: &str, raw: &[&str]| {
            Question::new(
                "jerry",
                "b1",
                QuestionPurpose::Profile,
                text,
                &options(raw),
                false,
                Utc::now(),
            )
        };
        assert!(make("  ?? ", &["Yes", "No"]).is_none());
        assert!(make("Do you drive?", &["Yes", "yes", "Other"]).is_none());
        let asked = make("Do you drive?", &["Yes", "No", "Other", "a", "b", "c"]).unwrap();
        assert_eq!(asked.options, ["Yes", "No", "a", "b"]);
    }

    #[test]
    fn an_answer_is_a_listed_option_or_the_members_own_words() {
        let now = Utc::now();
        let mut budget = question("b1", "What is your budget?", false);
        assert_eq!(
            budget.answer(chose(&["A million"], None), now),
            Err(AnswerRefused::NotAnOption("A million".into()))
        );
        assert_eq!(
            budget.answer(chose(&[], Some("  ")), now),
            Err(AnswerRefused::Empty)
        );
        assert_eq!(
            budget.answer(chose(&["More", "under ksh 50,000"], None), now),
            Err(AnswerRefused::OneChoiceOnly)
        );
        budget
            .answer(chose(&["more"], Some("about 150k")), now)
            .unwrap();
        assert_eq!(budget.state, QuestionState::Answered);
        let answer = budget.answer.clone().unwrap();
        assert_eq!(answer.choices, ["More"], "stored as the option is spelled");
        assert_eq!(answer.as_words(), "More; about 150k");
        assert!(budget.has_free_text());
        assert_eq!(
            budget.answer(chose(&["More"], None), now),
            Err(AnswerRefused::NotOpen)
        );
        assert!(!budget.skip(now), "answered already");
    }

    #[test]
    fn several_options_are_taken_only_by_a_multi_question() {
        let mut wants = question("b1", "What matters most?", true);
        wants
            .answer(
                chose(&["More", "Under KSh 50,000", "More"], None),
                Utc::now(),
            )
            .unwrap();
        assert_eq!(wants.answer.unwrap().choices, ["More", "Under KSh 50,000"]);
    }

    #[test]
    fn open_questions_are_shown_by_batch_four_at_most() {
        let mut questions: Vec<Question> = (0..6)
            .map(|i| question("b1", &format!("Question {i}?"), false))
            .collect();
        questions.push(question("b2", "Another batch?", false));
        questions[1].skip(Utc::now());
        let shown = batches(&questions, |q| format!("About {}", q.batch_id));
        assert_eq!(shown.len(), 2);
        assert_eq!(shown[0].title, "About b1");
        assert_eq!(shown[0].questions.len(), QUESTIONS_PER_BATCH);
        assert!(shown[0].questions.iter().all(|q| q.text != "Question 1?"));
        assert_eq!(shown[1].questions[0].text, "Another batch?");
    }

    #[test]
    fn a_question_is_overdue_only_while_open() {
        let now = Utc::now();
        let mut asked = question("b1", "When do you leave?", false);
        assert!(!asked.is_overdue(now));
        asked.expires_at = Some(now - chrono::Duration::hours(1));
        assert!(asked.is_overdue(now));
        assert!(asked.expire(now));
        assert!(!asked.is_overdue(now));
        assert_eq!(asked.state, QuestionState::Expired);
    }
}
