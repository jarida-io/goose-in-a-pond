//! The interests interview: a chat with tap answers that builds a draft of what a member is into,
//! which the member reviews before any of it is saved.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::user_data::domain::interest::{
    category_of, clean_facets, clean_topic, merge_facets, topic_key, Depth, Interest,
    InterestPatch, NewsMode, Origin,
};

/// Questions a round plans for; the model may stop sooner once it has enough.
pub const PLANNED_QUESTIONS: u32 = 10;
/// Questions a round never goes past, whatever the model says.
pub const MAX_QUESTIONS: u32 = 12;
/// How long a turn may sit in `thinking` before a read treats its writer as gone.
pub const THINKING_TIMEOUT_SECS: i64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InterviewStatus {
    #[default]
    NotStarted,
    Asking,
    Thinking,
    Review,
    Done,
}

impl InterviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InterviewStatus::NotStarted => "not_started",
            InterviewStatus::Asking => "asking",
            InterviewStatus::Thinking => "thinking",
            InterviewStatus::Review => "review",
            InterviewStatus::Done => "done",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "asking" => InterviewStatus::Asking,
            "thinking" => InterviewStatus::Thinking,
            "review" => InterviewStatus::Review,
            "done" => InterviewStatus::Done,
            _ => InterviewStatus::NotStarted,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speaker {
    Goose,
    You,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterviewTurn {
    pub id: String,
    pub role: Speaker,
    pub text: String,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub allow_free: bool,
    pub at: DateTime<Utc>,
}

/// One question as the model (or the fallback) wrote it.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub text: String,
    pub choices: Vec<String>,
    pub multi: bool,
    pub allow_free: bool,
}

/// What one answer established, as the model reads it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InterviewNote {
    pub topic: String,
    pub category: Option<String>,
    pub facets: Vec<String>,
    pub depth: Option<Depth>,
    pub news: Option<NewsMode>,
    pub avoid: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterestInterview {
    pub status: InterviewStatus,
    pub turns: Vec<InterviewTurn>,
    /// Interest-shaped so the phone reviews it with the same card; ids are `d1`, `d2`, ...
    pub draft: Vec<Interest>,
    pub asked: u32,
    pub planned: u32,
    #[serde(skip)]
    pub round: u32,
    #[serde(skip)]
    pub updated_at: Option<DateTime<Utc>>,
}

impl Default for InterestInterview {
    fn default() -> Self {
        Self {
            status: InterviewStatus::NotStarted,
            turns: Vec::new(),
            draft: Vec::new(),
            asked: 0,
            planned: PLANNED_QUESTIONS,
            round: 1,
            updated_at: None,
        }
    }
}

impl InterestInterview {
    /// A new round after one finished: the draft and turns start empty, the round number moves on.
    pub fn next_round(&self) -> Self {
        Self {
            round: self.round + 1,
            ..Self::default()
        }
    }

    /// Whether a `thinking` turn has outlived any writer that could still finish it.
    pub fn thinking_is_stale(&self, now: DateTime<Utc>) -> bool {
        self.status == InterviewStatus::Thinking
            && self
                .updated_at
                .is_none_or(|at| (now - at).num_seconds() > THINKING_TIMEOUT_SECS)
    }

    /// The question waiting for an answer, if the last turn is Goose's.
    pub fn open_question(&self) -> Option<&InterviewTurn> {
        self.turns.last().filter(|t| t.role == Speaker::Goose)
    }

    pub fn push_question(&mut self, question: Question, now: DateTime<Utc>) {
        let id = format!("t{}", self.turns.len() + 1);
        self.turns.push(InterviewTurn {
            id,
            role: Speaker::Goose,
            text: question.text,
            choices: question.choices,
            multi: question.multi,
            allow_free: question.allow_free,
            at: now,
        });
        self.asked += 1;
    }

    pub fn push_answer(&mut self, text: String, now: DateTime<Utc>) {
        let id = format!("t{}", self.turns.len() + 1);
        self.turns.push(InterviewTurn {
            id,
            role: Speaker::You,
            text,
            choices: Vec::new(),
            multi: false,
            allow_free: false,
            at: now,
        });
    }

    /// Whether this round has asked enough to stop.
    pub fn has_asked_enough(&self) -> bool {
        self.asked >= self.planned.min(MAX_QUESTIONS)
    }

    /// Fold what one answer established into the draft, merging by topic.
    pub fn apply_notes(&mut self, notes: &[InterviewNote], now: DateTime<Utc>) {
        for note in notes {
            let Some(topic) = clean_topic(&note.topic) else {
                continue;
            };
            let key = topic_key(&topic);
            let facets = clean_facets(note.facets.iter().map(String::as_str));
            match self.draft.iter_mut().find(|d| topic_key(&d.topic) == key) {
                Some(existing) => {
                    existing.facets = merge_facets(&existing.facets, &facets);
                    if let Some(category) = &note.category {
                        let category = category_of(category);
                        if category != "other" {
                            existing.category = category;
                        }
                    }
                    if let Some(depth) = note.depth {
                        existing.depth = depth;
                    }
                    if let Some(news) = note.news {
                        existing.news = news;
                    }
                    if let Some(avoid) = note.avoid {
                        existing.avoid = avoid;
                    }
                    existing.updated_at = now;
                }
                None => {
                    let id = format!("d{}", self.draft.len() + 1);
                    let avoid = note.avoid.unwrap_or(false);
                    self.draft.push(Interest {
                        id,
                        profile_id: String::new(),
                        topic,
                        category: note
                            .category
                            .as_deref()
                            .map(category_of)
                            .unwrap_or_else(|| "other".to_string()),
                        facets,
                        depth: note.depth.unwrap_or_default(),
                        news: note.news.unwrap_or(if avoid {
                            NewsMode::Off
                        } else {
                            NewsMode::Both
                        }),
                        avoid,
                        origin: Origin::Interview,
                        evidence: 0,
                        confirmed: false,
                        picture: None,
                        updated_at: now,
                    });
                }
            }
        }
    }

    /// The draft rows the member kept, with their edits applied.
    pub fn approved(
        &self,
        keep: &[String],
        edits: &std::collections::HashMap<String, InterestPatch>,
    ) -> Vec<Interest> {
        self.draft
            .iter()
            .filter(|d| keep.contains(&d.id))
            .cloned()
            .map(|mut d| {
                if let Some(edit) = edits.get(&d.id) {
                    if let Some(topic) = edit.topic.as_deref().and_then(clean_topic) {
                        d.topic = topic;
                    }
                    if let Some(category) = &edit.category {
                        d.category = category_of(category);
                    }
                    if let Some(facets) = &edit.facets {
                        d.facets = clean_facets(facets.iter().map(String::as_str));
                    }
                    if let Some(depth) = edit.depth {
                        d.depth = depth;
                    }
                    if let Some(news) = edit.news {
                        d.news = news;
                    }
                    if let Some(avoid) = edit.avoid {
                        d.avoid = avoid;
                    }
                }
                d
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(topic: &str, facets: &[&str]) -> InterviewNote {
        InterviewNote {
            topic: topic.to_string(),
            category: Some("film_tv".to_string()),
            facets: facets.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn notes_about_one_topic_build_one_draft_row() {
        let mut interview = InterestInterview::default();
        let now = Utc::now();
        interview.apply_notes(&[note("The Boys", &["Homelander"])], now);
        interview.apply_notes(
            &[InterviewNote {
                depth: Some(Depth::Devoted),
                ..note("the boys", &["Gen V"])
            }],
            now,
        );
        assert_eq!(interview.draft.len(), 1);
        let row = &interview.draft[0];
        assert_eq!(row.id, "d1");
        assert_eq!(row.facets, vec!["Homelander", "Gen V"]);
        assert_eq!(row.depth, Depth::Devoted);
    }

    #[test]
    fn something_to_keep_away_is_drafted_with_no_news() {
        let mut interview = InterestInterview::default();
        interview.apply_notes(
            &[InterviewNote {
                avoid: Some(true),
                ..note("Celebrity gossip", &[])
            }],
            Utc::now(),
        );
        assert!(interview.draft[0].avoid);
        assert_eq!(interview.draft[0].news, NewsMode::Off);
    }

    #[test]
    fn approval_keeps_only_the_chosen_rows_with_their_edits() {
        let mut interview = InterestInterview::default();
        let now = Utc::now();
        interview.apply_notes(&[note("The Boys", &[]), note("GTA V", &[])], now);
        let mut edits = std::collections::HashMap::new();
        edits.insert(
            "d2".to_string(),
            InterestPatch {
                category: Some("games".into()),
                ..Default::default()
            },
        );
        let kept = interview.approved(&["d2".to_string()], &edits);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].topic, "GTA V");
        assert_eq!(kept[0].category, "games");
    }

    #[test]
    fn a_thinking_turn_with_no_writer_left_is_stale() {
        let now = Utc::now();
        let mut interview = InterestInterview {
            status: InterviewStatus::Thinking,
            updated_at: Some(now - chrono::Duration::seconds(THINKING_TIMEOUT_SECS + 5)),
            ..Default::default()
        };
        assert!(interview.thinking_is_stale(now));
        interview.updated_at = Some(now);
        assert!(!interview.thinking_is_stale(now));
    }
}
