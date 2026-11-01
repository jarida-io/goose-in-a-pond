//! The setup interview: a member answers questions about themselves, one a turn, and approves
//! the standing notes drawn from their answers before any is saved.
//!
//! The order of topics, what may be drafted, and when a draft may be saved live here, in code: a
//! small model follows the conversation well but keeps a checklist badly.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::prompt_extra::{
    check_active_set, validate_instruction, PromptExtra, PromptExtraViolation,
};

/// What the interview asks about, in the order it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Topic {
    Address,
    Identity,
    Language,
    Household,
    Routine,
    Food,
    Access,
    Style,
    Avoid,
    Other,
}

impl Topic {
    pub const ALL: [Topic; 10] = [
        Topic::Address,
        Topic::Identity,
        Topic::Language,
        Topic::Household,
        Topic::Routine,
        Topic::Food,
        Topic::Access,
        Topic::Style,
        Topic::Avoid,
        Topic::Other,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Topic::Address => "address",
            Topic::Identity => "identity",
            Topic::Language => "language",
            Topic::Household => "household",
            Topic::Routine => "routine",
            Topic::Food => "food",
            Topic::Access => "access",
            Topic::Style => "style",
            Topic::Avoid => "avoid",
            Topic::Other => "other",
        }
    }

    /// A topic by its id, or by its note's key: a small model reaches for either.
    pub fn from_id(id: &str) -> Option<Topic> {
        let id = id.trim().to_ascii_lowercase();
        Topic::ALL
            .into_iter()
            .find(|t| t.id() == id || t.note_key() == id)
    }

    /// The note this topic writes. Fixed, so a second interview updates a note, never adds one.
    pub fn note_key(self) -> &'static str {
        match self {
            Topic::Address => "me_address",
            Topic::Identity => "me_identity",
            Topic::Language => "me_language",
            Topic::Household => "me_household",
            Topic::Routine => "me_routine",
            Topic::Food => "me_food",
            Topic::Access => "me_access",
            Topic::Style => "me_style",
            Topic::Avoid => "me_avoid",
            Topic::Other => "me_other",
        }
    }

    /// The phone's name for the topic.
    pub fn label(self) -> &'static str {
        match self {
            Topic::Address => "What to call you",
            Topic::Identity => "About you",
            Topic::Language => "Languages",
            Topic::Household => "Your household",
            Topic::Routine => "Your days",
            Topic::Food => "Food",
            Topic::Access => "Hearing, sight and speech",
            Topic::Style => "How Goose talks to you",
            Topic::Avoid => "Topics to avoid",
            Topic::Other => "Anything else",
        }
    }

    /// A question that opens the topic; the model may reword it and ask one follow-up.
    pub fn question(self) -> &'static str {
        match self {
            Topic::Address => "What would you like me to call you?",
            Topic::Identity => {
                "Tell me a little about yourself: what do you do, and what fills your days?"
            }
            Topic::Language => {
                "Which languages do you speak, and which would you like me to use with you?"
            }
            Topic::Household => "Who else lives with you, and how are they related to you?",
            Topic::Routine => {
                "What does a usual week look like for you: work, school, or regular plans?"
            }
            Topic::Food => {
                "Do you have food preferences, allergies or dietary needs I should know about?"
            }
            Topic::Access => {
                "Is there anything about hearing, sight or speech I should keep in mind when we \
                 talk?"
            }
            Topic::Style => {
                "How would you like me to talk to you: short or detailed answers, casual or \
                 formal, and metric or imperial units?"
            }
            Topic::Avoid => "Are there topics you would rather I avoid, or handle with care?",
            Topic::Other => "Is there anything else you would like me to always keep in mind?",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopicStatus {
    Pending,
    Answered,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopicState {
    pub topic: Topic,
    pub status: TopicStatus,
    /// The model's summary of the answer; personal data, kept only until the interview ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterviewStatus {
    InProgress,
    AwaitingApproval,
    Done,
    Stopped,
}

impl InterviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            InterviewStatus::InProgress => "in_progress",
            InterviewStatus::AwaitingApproval => "awaiting_approval",
            InterviewStatus::Done => "done",
            InterviewStatus::Stopped => "stopped",
        }
    }

    pub fn parse(s: &str) -> Option<InterviewStatus> {
        match s {
            "in_progress" => Some(InterviewStatus::InProgress),
            "awaiting_approval" => Some(InterviewStatus::AwaitingApproval),
            "done" => Some(InterviewStatus::Done),
            "stopped" => Some(InterviewStatus::Stopped),
            _ => None,
        }
    }

    /// Questions are still being asked or a draft is waiting: the session runs as the interview.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            InterviewStatus::InProgress | InterviewStatus::AwaitingApproval
        )
    }
}

/// One note the model drafted for an answered topic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteDraft {
    pub topic: Topic,
    pub instruction: String,
}

impl NoteDraft {
    pub fn to_extra(&self, profile_id: &str) -> PromptExtra {
        PromptExtra {
            key: self.topic.note_key().to_string(),
            instruction: self.instruction.trim().to_string(),
            active: true,
            sort_order: Topic::ALL
                .iter()
                .position(|t| *t == self.topic)
                .unwrap_or_default() as i32,
            profile_id: Some(profile_id.to_string()),
        }
    }
}

/// Longest answer summary kept per topic, in characters.
pub const MAX_ANSWER_CHARS: usize = 600;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterviewRefusal {
    UnknownTopic,
    /// The interview is done or stopped; it has to be started again first.
    NotOpen,
    EmptyAnswer,
    AnswerTooLong,
    /// Two drafted notes for one topic.
    DuplicateTopic,
    NothingToDraft,
    /// A topic has not been asked or skipped yet, so a draft would miss it.
    TopicsLeft,
    NoDraft,
    /// The person has not replied since the draft was shown, so cannot have approved it.
    NotYetShown,
    Note(PromptExtraViolation),
}

impl InterviewRefusal {
    pub fn code(self) -> &'static str {
        match self {
            InterviewRefusal::UnknownTopic => "unknown_topic",
            InterviewRefusal::NotOpen => "not_open",
            InterviewRefusal::EmptyAnswer => "empty_answer",
            InterviewRefusal::AnswerTooLong => "answer_too_long",
            InterviewRefusal::DuplicateTopic => "duplicate_topic",
            InterviewRefusal::NothingToDraft => "nothing_to_draft",
            InterviewRefusal::TopicsLeft => "topics_left",
            InterviewRefusal::NoDraft => "no_draft",
            InterviewRefusal::NotYetShown => "not_yet_shown",
            InterviewRefusal::Note(v) => v.code(),
        }
    }

    /// Said to the model, so it can correct itself without guessing.
    pub fn message(self) -> String {
        match self {
            InterviewRefusal::UnknownTopic => format!(
                "Unknown topic. Use one of: {}.",
                Topic::ALL.map(Topic::id).join(", ")
            ),
            InterviewRefusal::NotOpen => "The interview is not running.".to_string(),
            InterviewRefusal::EmptyAnswer => "The answer summary is empty.".to_string(),
            InterviewRefusal::AnswerTooLong => {
                format!("Summarise the answer in at most {MAX_ANSWER_CHARS} characters.")
            }
            InterviewRefusal::DuplicateTopic => "Draft at most one note per topic.".to_string(),
            InterviewRefusal::NothingToDraft => {
                "Nothing was answered, so there is nothing to save. Stop the interview instead."
                    .to_string()
            }
            InterviewRefusal::TopicsLeft => {
                "Not every topic has been asked yet. Ask the next topic first. If they want to \
                 finish now, call skip_topic with topic `all`, then propose_notes."
                    .to_string()
            }
            InterviewRefusal::NoDraft => {
                "Not saved. There is no draft yet, so nothing was saved: tell them their notes are \
                 not saved yet. Call propose_notes, show them the notes, and ask whether to save \
                 them."
                    .to_string()
            }
            InterviewRefusal::NotYetShown => {
                "Not saved. They have not seen this draft yet, so nothing was saved: tell them \
                 their notes are not saved yet, show them the notes, and ask whether to save them."
                    .to_string()
            }
            InterviewRefusal::Note(v) => v.message(),
        }
    }
}

impl From<PromptExtraViolation> for InterviewRefusal {
    fn from(v: PromptExtraViolation) -> Self {
        InterviewRefusal::Note(v)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interview {
    pub profile_id: String,
    /// The chat session the interview runs in.
    pub session_id: String,
    pub status: InterviewStatus,
    pub topics: Vec<TopicState>,
    pub draft: Vec<NoteDraft>,
    /// What the pond said in the session before a turn ran. The engine drops a conversation's
    /// leading assistant message, so the next turn's context repeats it.
    pub pending_opening: Option<String>,
    pub drafted_at: Option<DateTime<Utc>>,
    pub last_reply_at: Option<DateTime<Utc>>,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Interview {
    pub fn new(profile_id: &str, session_id: &str, now: DateTime<Utc>) -> Self {
        Interview {
            profile_id: profile_id.to_string(),
            session_id: session_id.to_string(),
            status: InterviewStatus::InProgress,
            topics: Topic::ALL
                .into_iter()
                .map(|topic| TopicState {
                    topic,
                    status: TopicStatus::Pending,
                    answer: None,
                })
                .collect(),
            draft: Vec::new(),
            pending_opening: None,
            drafted_at: None,
            last_reply_at: None,
            started_at: now,
            updated_at: now,
        }
    }

    pub fn next_topic(&self) -> Option<Topic> {
        self.topics
            .iter()
            .find(|t| t.status == TopicStatus::Pending)
            .map(|t| t.topic)
    }

    fn state_mut(&mut self, topic: Topic) -> &mut TopicState {
        let i = self
            .topics
            .iter()
            .position(|t| t.topic == topic)
            .expect("every topic has a state");
        &mut self.topics[i]
    }

    fn require_open(&self) -> Result<(), InterviewRefusal> {
        if self.status.is_open() {
            Ok(())
        } else {
            Err(InterviewRefusal::NotOpen)
        }
    }

    /// An answer after a draft reopens the questions: the draft no longer covers everything.
    fn reopen(&mut self, now: DateTime<Utc>) {
        self.status = InterviewStatus::InProgress;
        self.draft.clear();
        self.drafted_at = None;
        self.updated_at = now;
    }

    pub fn record(
        &mut self,
        topic: Topic,
        answer: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<Topic>, InterviewRefusal> {
        self.require_open()?;
        let answer = answer.trim();
        if answer.is_empty() {
            return Err(InterviewRefusal::EmptyAnswer);
        }
        if answer.chars().count() > MAX_ANSWER_CHARS {
            return Err(InterviewRefusal::AnswerTooLong);
        }
        let state = self.state_mut(topic);
        state.status = TopicStatus::Answered;
        state.answer = Some(answer.to_string());
        self.reopen(now);
        Ok(self.next_topic())
    }

    /// Skips every topic not yet asked, when the person wants to finish now.
    pub fn skip_rest(&mut self, now: DateTime<Utc>) -> Result<(), InterviewRefusal> {
        self.require_open()?;
        for t in &mut self.topics {
            if t.status == TopicStatus::Pending {
                t.status = TopicStatus::Skipped;
            }
        }
        self.updated_at = now;
        Ok(())
    }

    pub fn skip(
        &mut self,
        topic: Topic,
        now: DateTime<Utc>,
    ) -> Result<Option<Topic>, InterviewRefusal> {
        self.require_open()?;
        let state = self.state_mut(topic);
        state.status = TopicStatus::Skipped;
        state.answer = None;
        self.draft.retain(|d| d.topic != topic);
        self.updated_at = now;
        Ok(self.next_topic())
    }

    /// Pauses with nothing written; starting again carries on from the next unasked topic.
    pub fn stop(&mut self, now: DateTime<Utc>) -> Result<(), InterviewRefusal> {
        self.require_open()?;
        self.status = InterviewStatus::Stopped;
        self.updated_at = now;
        Ok(())
    }

    /// Takes a stopped interview up again where it paused.
    pub fn resume(&mut self, now: DateTime<Utc>) {
        if self.status == InterviewStatus::Stopped {
            self.status = if self.draft.is_empty() {
                InterviewStatus::InProgress
            } else {
                InterviewStatus::AwaitingApproval
            };
            self.updated_at = now;
        }
    }

    /// Drafts one note per answered topic from what was recorded for it, for the person to
    /// approve, and returns the notes it would save. Building the draft here, not from the
    /// model's arguments, is what makes it whole: on Ollama the call arrived with none at all.
    ///
    /// `rewrites` replace a topic's recorded note first; one for a topic they skipped is
    /// dropped, since it says what they chose not to. `household` and `existing` are the
    /// household's and this member's notes as stored; the draft must fit beside them, since a
    /// turn reads all three.
    pub fn propose(
        &mut self,
        rewrites: Vec<NoteDraft>,
        household: &[PromptExtra],
        existing: &[PromptExtra],
        now: DateTime<Utc>,
    ) -> Result<Vec<PromptExtra>, InterviewRefusal> {
        self.require_open()?;
        if self.next_topic().is_some() {
            return Err(InterviewRefusal::TopicsLeft);
        }
        let mut seen = Vec::new();
        for rewrite in &rewrites {
            if seen.contains(&rewrite.topic) {
                return Err(InterviewRefusal::DuplicateTopic);
            }
            seen.push(rewrite.topic);
            validate_instruction(&rewrite.instruction)?;
        }
        for rewrite in rewrites {
            if let Some(t) = self
                .topics
                .iter_mut()
                .find(|t| t.topic == rewrite.topic && t.status == TopicStatus::Answered)
            {
                t.answer = Some(rewrite.instruction.trim().to_string());
            }
        }
        let drafts: Vec<NoteDraft> = self
            .topics
            .iter()
            .filter(|t| t.status == TopicStatus::Answered)
            .filter_map(|t| {
                t.answer.as_ref().map(|note| NoteDraft {
                    topic: t.topic,
                    instruction: note.clone(),
                })
            })
            .collect();
        if drafts.is_empty() {
            return Err(InterviewRefusal::NothingToDraft);
        }
        for draft in &drafts {
            validate_instruction(&draft.instruction)?;
        }

        let notes: Vec<PromptExtra> = drafts
            .iter()
            .map(|d| d.to_extra(&self.profile_id))
            .collect();
        let mut combined: Vec<PromptExtra> = household
            .iter()
            .filter(|e| e.profile_id.is_none())
            .chain(
                existing
                    .iter()
                    .filter(|e| e.profile_id.as_deref() == Some(self.profile_id.as_str())),
            )
            .cloned()
            .collect();
        for note in &notes {
            check_active_set(&combined, note)?;
            combined.retain(|e| !(e.key == note.key && e.profile_id == note.profile_id));
            combined.push(note.clone());
        }

        self.draft = drafts;
        self.status = InterviewStatus::AwaitingApproval;
        self.drafted_at = Some(now);
        self.updated_at = now;
        Ok(notes)
    }

    /// The person spoke in the interview's session: any draft shown before now has been seen.
    pub fn note_reply(&mut self, now: DateTime<Utc>) {
        self.last_reply_at = Some(now);
        self.updated_at = now;
    }

    /// The draft as the notes to save, if it may be saved now. A save from the conversation
    /// needs a reply after the draft was shown; a tap on the person's own phone is the approval.
    pub fn approve(
        &mut self,
        by_tap: bool,
        now: DateTime<Utc>,
    ) -> Result<Vec<PromptExtra>, InterviewRefusal> {
        if self.status != InterviewStatus::AwaitingApproval || self.draft.is_empty() {
            return Err(InterviewRefusal::NoDraft);
        }
        if !by_tap {
            let seen = match (self.drafted_at, self.last_reply_at) {
                (Some(drafted), Some(reply)) => reply > drafted,
                _ => false,
            };
            if !seen {
                return Err(InterviewRefusal::NotYetShown);
            }
        }
        let notes = self
            .draft
            .iter()
            .map(|d| d.to_extra(&self.profile_id))
            .collect();
        self.finish(now);
        Ok(notes)
    }

    /// Ends the interview and forgets the answers: the saved notes are what is kept.
    fn finish(&mut self, now: DateTime<Utc>) {
        self.status = InterviewStatus::Done;
        for t in &mut self.topics {
            t.answer = None;
        }
        self.draft.clear();
        self.pending_opening = None;
        self.updated_at = now;
    }

    /// What the pond says first, to a fresh interview or one taken up again.
    pub fn opening(&self, name: &str, resumed: bool) -> String {
        let name = name.trim();
        let hello = if name.is_empty() {
            "Hi!".to_string()
        } else {
            format!("Hi {name}!")
        };
        match (resumed, self.status, self.next_topic()) {
            (_, InterviewStatus::AwaitingApproval, _) => format!(
                "{hello} Welcome back. I drafted your notes last time. Would you like me to \
                 save them as they are, or change any first?"
            ),
            (true, _, Some(topic)) => format!(
                "{hello} Welcome back. Let's carry on where we stopped. {}",
                topic.question()
            ),
            (true, _, None) => format!(
                "{hello} Welcome back. We covered everything, so I'll draft your notes now if \
                 you're happy for me to."
            ),
            (false, _, topic) => format!(
                "{hello} I'd like to get to know you, so I can help in the way that suits you. \
                 I'll ask one question at a time. You can skip any of them, or stop whenever \
                 you like, and I'll show you what I'd remember before saving anything. {}",
                topic.unwrap_or(Topic::Address).question()
            ),
        }
    }

    /// The interview's state for this turn's context: short, since it stays in the history.
    pub fn turn_context(&self, name: &str) -> String {
        let ids = |status: TopicStatus| -> Vec<&'static str> {
            self.topics
                .iter()
                .filter(|t| t.status == status)
                .map(|t| t.topic.id())
                .collect()
        };
        let mut out = format!(
            "<setup-interview>\nYou are interviewing {}, the person in this conversation; their \
             notes are about them, never anyone else the prompt names.",
            name.trim()
        );
        if let Some(address) = self
            .topics
            .iter()
            .find(|t| t.topic == Topic::Address && t.status == TopicStatus::Answered)
            .and_then(|t| t.answer.as_deref())
        {
            let address: String = address.chars().take(80).collect();
            out.push_str(&format!(" They asked to be called: {address}."));
        }
        for (label, status) in [
            ("Answered", TopicStatus::Answered),
            ("Skipped", TopicStatus::Skipped),
        ] {
            let list = ids(status);
            if !list.is_empty() {
                out.push_str(&format!(" {label}: {}.", list.join(", ")));
            }
        }
        match (self.status, self.next_topic()) {
            (InterviewStatus::AwaitingApproval, _) => out.push_str(&format!(
                " A draft of {} notes is waiting for their approval. If they agree, call \
                 save_notes: nothing is saved until you do.",
                self.draft.len()
            )),
            (_, Some(topic)) => out.push_str(&format!(
                " Next topic: {} (for example: \"{}\").",
                topic.id(),
                topic.question()
            )),
            (_, None) => out.push_str(
                " No topic is left. Nothing is saved yet: call propose_notes now, then show \
                 them the notes.",
            ),
        }
        if let Some(opening) = &self.pending_opening {
            out.push_str(&format!(
                "\nYou opened this conversation by saying: \"{opening}\" Their message replies \
                 to it."
            ));
        }
        out.push_str(" Nothing is saved until save_notes succeeds.\n</setup-interview>");
        out
    }
}

/// The standing rules for a turn in an interview session, sent with the system prompt.
pub const INTERVIEW_RULES: &str = "<setup-interview-rules>
You are running a setup interview: learning about the person you are talking with, so you can \
write their standing notes, which they approve before anything is saved. Each turn's \
<setup-interview> says where it stands.
- Ask exactly one question per reply, on the next topic. Keep it short and warm. One follow-up \
on the same topic is fine when their answer is unclear.
- After each answer, call record_answer with the topic and the note to keep: one short standing \
instruction about them, in the third person, for example \"Call her Mama Amina.\" If they \
decline, would rather not say, or want to move on, call skip_topic and never return to it. Call \
skip_topic too when the answer gives nothing to keep, such as no needs or nothing to avoid: a note \
that says nothing still rides every turn.
- If they want to stop, call stop_interview, thank them, and say they can carry on another time. \
Nothing is saved. If instead they want to finish now with what they have said, call skip_topic \
with topic `all`.
- When the last topic is answered or skipped, their notes are drafted at once and the tool result \
lists them. Show them every note exactly as listed and ask whether to save them or change any. \
If no draft formed, call propose_notes.
- Call save_notes only after they say yes to the notes you showed. For a change, call \
record_answer with the new note for that topic, then propose_notes, and show the notes again.
- Notes are saved only by save_notes. Never say they are saved unless save_notes said so.
- Never add details they did not give. Never name the tools.
</setup-interview-rules>";

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-31T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn draft(topic: Topic, text: &str) -> NoteDraft {
        NoteDraft {
            topic,
            instruction: text.to_string(),
        }
    }

    #[test]
    fn topics_come_in_order_and_skips_count_as_covered() {
        let mut i = Interview::new("amina", "s1", t0());
        assert_eq!(i.next_topic(), Some(Topic::Address));
        assert_eq!(
            i.record(Topic::Address, "Mama Amina", t0()),
            Ok(Some(Topic::Identity))
        );
        assert_eq!(i.skip(Topic::Identity, t0()), Ok(Some(Topic::Language)));
        for topic in &Topic::ALL[2..] {
            i.skip(*topic, t0()).unwrap();
        }
        assert_eq!(i.next_topic(), None);
    }

    #[test]
    fn note_keys_are_unique_and_valid() {
        let keys: std::collections::HashSet<&str> =
            Topic::ALL.iter().map(|t| t.note_key()).collect();
        assert_eq!(keys.len(), Topic::ALL.len());
        for key in keys {
            assert!(
                super::super::prompt_extra::validate_key(key).is_ok(),
                "{key}"
            );
        }
        for topic in Topic::ALL {
            assert_eq!(Topic::from_id(topic.id()), Some(topic));
        }
    }

    #[test]
    fn a_skipped_topics_note_is_dropped_and_each_topic_drafts_once() {
        let mut i = Interview::new("amina", "s1", t0());
        i.record(Topic::Address, "Mama Amina", t0()).unwrap();
        i.skip(Topic::Food, t0()).unwrap();
        i.skip_rest(t0()).unwrap();
        let from_answers = i
            .clone()
            .propose(vec![draft(Topic::Food, "No pork.")], &[], &[], t0())
            .unwrap();
        assert_eq!(from_answers.len(), 1, "the skipped topic's note is dropped");
        assert_eq!(
            from_answers[0].instruction, "Mama Amina",
            "drafted from what was recorded"
        );
        let kept = i
            .clone()
            .propose(
                vec![
                    draft(Topic::Food, "No pork."),
                    draft(Topic::Address, "Call her Mama."),
                ],
                &[],
                &[],
                t0(),
            )
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].key, "me_address");
        assert_eq!(
            i.propose(
                vec![
                    draft(Topic::Address, "Call her Mama."),
                    draft(Topic::Address, "Again.")
                ],
                &[],
                &[],
                t0()
            ),
            Err(InterviewRefusal::DuplicateTopic)
        );
        let mut nothing = Interview::new("amina", "s2", t0());
        nothing.skip_rest(t0()).unwrap();
        assert_eq!(
            nothing.propose(vec![], &[], &[], t0()),
            Err(InterviewRefusal::NothingToDraft)
        );
        let notes = i
            .propose(
                vec![draft(Topic::Address, "Call her Mama Amina.")],
                &[],
                &[],
                t0(),
            )
            .unwrap();
        assert_eq!(notes[0].key, "me_address");
        assert_eq!(notes[0].profile_id.as_deref(), Some("amina"));
        assert_eq!(i.status, InterviewStatus::AwaitingApproval);
    }

    #[test]
    fn a_draft_must_fit_beside_the_household_and_the_members_other_notes() {
        let mut i = Interview::new("amina", "s1", t0());
        i.record(Topic::Address, "Mama Amina", t0()).unwrap();
        let long = "a".repeat(1_000);
        let household: Vec<PromptExtra> = (0..3)
            .map(|n| PromptExtra {
                key: format!("h{n}"),
                instruction: long.clone(),
                active: true,
                sort_order: 0,
                profile_id: None,
            })
            .collect();
        let mine = vec![PromptExtra {
            key: "typed_on_phone".to_string(),
            instruction: "a".repeat(990),
            active: true,
            sort_order: 0,
            profile_id: Some("amina".to_string()),
        }];
        i.skip_rest(t0()).unwrap();
        assert_eq!(
            i.propose(
                vec![draft(Topic::Address, "Call her Mama Amina.")],
                &household,
                &mine,
                t0()
            ),
            Err(InterviewRefusal::Note(PromptExtraViolation::ExtrasTooLong))
        );
        // Replacing her own earlier `me_address` frees its length rather than adding to it.
        let earlier = vec![PromptExtra {
            key: "me_address".to_string(),
            instruction: "a".repeat(990),
            active: true,
            sort_order: 0,
            profile_id: Some("amina".to_string()),
        }];
        assert!(i
            .propose(
                vec![draft(Topic::Address, "Call her Mama Amina.")],
                &household,
                &earlier,
                t0()
            )
            .is_ok());
    }

    #[test]
    fn a_conversation_save_needs_a_reply_after_the_draft_but_a_tap_does_not() {
        let mut i = Interview::new("amina", "s1", t0());
        i.record(Topic::Address, "Mama Amina", t0()).unwrap();
        i.note_reply(t0());
        i.skip_rest(t0()).unwrap();
        i.propose(
            vec![draft(Topic::Address, "Call her Mama Amina.")],
            &[],
            &[],
            t0() + Duration::seconds(5),
        )
        .unwrap();
        assert_eq!(
            i.clone().approve(false, t0()),
            Err(InterviewRefusal::NotYetShown)
        );
        assert_eq!(i.clone().approve(true, t0()).unwrap().len(), 1);

        i.note_reply(t0() + Duration::seconds(30));
        let saved = i.approve(false, t0() + Duration::seconds(31)).unwrap();
        assert_eq!(saved[0].instruction, "Call her Mama Amina.");
        assert_eq!(i.status, InterviewStatus::Done);
        assert!(
            i.topics.iter().all(|t| t.answer.is_none()),
            "answers are forgotten"
        );
        assert_eq!(
            i.record(Topic::Food, "x", t0()),
            Err(InterviewRefusal::NotOpen)
        );
    }

    #[test]
    fn a_new_answer_after_a_draft_withdraws_it() {
        let mut i = Interview::new("amina", "s1", t0());
        i.record(Topic::Address, "Mama Amina", t0()).unwrap();
        i.skip_rest(t0()).unwrap();
        i.propose(
            vec![draft(Topic::Address, "Call her Mama Amina.")],
            &[],
            &[],
            t0(),
        )
        .unwrap();
        i.record(Topic::Food, "No pork", t0()).unwrap();
        assert_eq!(i.status, InterviewStatus::InProgress);
        assert!(i.draft.is_empty());
        assert_eq!(i.approve(true, t0()), Err(InterviewRefusal::NoDraft));
    }

    #[test]
    fn stop_then_resume_carries_on_and_keeps_a_waiting_draft() {
        let mut i = Interview::new("amina", "s1", t0());
        i.record(Topic::Address, "Mama Amina", t0()).unwrap();
        i.stop(t0()).unwrap();
        assert_eq!(i.status, InterviewStatus::Stopped);
        assert_eq!(
            i.skip(Topic::Identity, t0()),
            Err(InterviewRefusal::NotOpen)
        );
        i.resume(t0());
        assert_eq!(i.status, InterviewStatus::InProgress);
        assert!(i
            .opening("Amina", true)
            .contains(Topic::Identity.question()));

        i.skip_rest(t0()).unwrap();

        i.propose(
            vec![draft(Topic::Address, "Call her Mama Amina.")],
            &[],
            &[],
            t0(),
        )
        .unwrap();
        i.stop(t0()).unwrap();
        i.resume(t0());
        assert_eq!(i.status, InterviewStatus::AwaitingApproval);
    }

    #[test]
    fn the_turn_context_names_the_next_topic_and_a_pending_opening() {
        let mut i = Interview::new("amina", "s1", t0());
        i.record(Topic::Address, "Mama Amina", t0()).unwrap();
        i.skip(Topic::Identity, t0()).unwrap();
        i.pending_opening = Some("Hi! What would you like me to call you?".to_string());
        let ctx = i.turn_context("Amina");
        assert!(ctx.contains("Answered: address."));
        assert!(ctx.contains("Skipped: identity."));
        assert!(ctx.contains("Next topic: language"));
        assert!(ctx.contains("You opened this conversation by saying"));
    }

    #[test]
    fn a_draft_waits_until_every_topic_is_asked_or_skipped() {
        let mut i = Interview::new("amina", "s1", t0());
        i.record(Topic::Address, "Mama Amina", t0()).unwrap();
        assert_eq!(
            i.propose(
                vec![draft(Topic::Address, "Call her Mama Amina.")],
                &[],
                &[],
                t0()
            ),
            Err(InterviewRefusal::TopicsLeft)
        );
        i.skip_rest(t0()).unwrap();
        assert!(i.topics[1..]
            .iter()
            .all(|t| t.status == TopicStatus::Skipped));
        assert!(i
            .propose(
                vec![draft(Topic::Address, "Call her Mama Amina.")],
                &[],
                &[],
                t0()
            )
            .is_ok());
    }

    #[test]
    fn a_topic_is_found_by_its_id_or_its_notes_key() {
        assert_eq!(Topic::from_id(" Food "), Some(Topic::Food));
        assert_eq!(Topic::from_id("me_address"), Some(Topic::Address));
        assert_eq!(Topic::from_id("hobbies"), None);
    }

    #[test]
    fn statuses_round_trip() {
        for s in [
            InterviewStatus::InProgress,
            InterviewStatus::AwaitingApproval,
            InterviewStatus::Done,
            InterviewStatus::Stopped,
        ] {
            assert_eq!(InterviewStatus::parse(s.as_str()), Some(s));
        }
    }
}
