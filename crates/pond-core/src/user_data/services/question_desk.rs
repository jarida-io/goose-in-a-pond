//! Where structured questions are checked, stored, waited on and answered (sections 2, 4 and 5
//! of docs/architecture/structured-questions.md). The chat `ask` tool waits here on a per-batch
//! waiter; the answer route resolves it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::oneshot;

use crate::models::ports::embedding::EmbeddingProvider;
use crate::user_data::domain::memory::cosine_similarity;
use crate::user_data::domain::question::{
    text_key, Answer, AnswerRefused, Question, QuestionDraft, QuestionPurpose, QuestionRefused,
    QuestionState, QUESTIONS_PER_BATCH, SOFT_LIMIT_ROUNDS,
};
use crate::user_data::ports::questions::QuestionRepository;

/// Cosine at or above which two questions are the same question.
pub const ALIKE: f32 = 0.86;
/// How long the `ask` tool holds its turn before the batch waits in My Goose.
pub const ASK_WAIT: Duration = Duration::from_secs(600);
/// Batches of one chat this close together are one chain for the soft limit.
pub const CHAT_CHAIN_HOURS: i64 = 2;
/// Stored questions a new one is compared with by meaning; the newest, so the check stays cheap.
const COMPARED_BY_MEANING: usize = 40;
/// Stored questions a new one is compared with by key.
const COMPARED_BY_KEY: usize = 500;

pub const SPOKEN_REFUSAL: &str = "Ask in plain speech.";
pub const ENOUGH_REPLY: &str = "The member wants you to go ahead with what you have.";
pub const NOT_YET_REPLY: &str = "Not answered yet; the questions wait in My Goose. Say you'll \
carry on when they answer.";
const SOFT_LIMIT_REPLY: &str = "Goose has enough to start: you have asked this member five \
rounds of questions. Go ahead with what you have and say what you assumed; ask in plain words \
only if they want more questions.";

/// How a batch the `ask` tool waits on was settled.
#[derive(Debug, Clone, PartialEq)]
pub enum Settled {
    Answered(Vec<Question>),
    Enough,
}

struct Waiter {
    profile_id: String,
    session_id: String,
    tell: oneshot::Sender<Settled>,
}

/// The batches a chat turn is waiting on, by batch id. Never persisted: a waiter lives only as
/// long as the turn holding it.
#[derive(Default)]
pub struct QuestionWaiters {
    waiting: Mutex<HashMap<String, Waiter>>,
}

impl QuestionWaiters {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Waiter>> {
        self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn register(&self, batch_id: &str, profile_id: &str, session_id: &str) -> WaitTicket<'_> {
        let (tell, heard) = oneshot::channel();
        self.lock().insert(
            batch_id.to_string(),
            Waiter {
                profile_id: profile_id.to_string(),
                session_id: session_id.to_string(),
                tell,
            },
        );
        WaitTicket {
            waiters: self,
            batch_id: batch_id.to_string(),
            heard: Some(heard),
        }
    }

    /// Hands `settled` to the turn waiting on `batch_id`; `false` when no turn is.
    fn resolve(&self, batch_id: &str, settled: Settled) -> bool {
        match self.lock().remove(batch_id) {
            Some(waiter) => waiter.tell.send(settled).is_ok(),
            None => false,
        }
    }

    pub fn is_waiting(&self, batch_id: &str) -> bool {
        self.lock()
            .get(batch_id)
            .is_some_and(|waiter| !waiter.tell.is_closed())
    }

    /// The batch a live turn of `session_id` waits on, and whose it is.
    pub fn waiting_in(&self, session_id: &str) -> Option<(String, String)> {
        self.lock()
            .iter()
            .find(|(_, w)| w.session_id == session_id && !w.tell.is_closed())
            .map(|(batch, w)| (batch.clone(), w.profile_id.clone()))
    }
}

/// Removes its waiter when dropped, so a turn that was cancelled leaves none behind.
struct WaitTicket<'a> {
    waiters: &'a QuestionWaiters,
    batch_id: String,
    heard: Option<oneshot::Receiver<Settled>>,
}

impl Drop for WaitTicket<'_> {
    fn drop(&mut self) {
        self.waiters.lock().remove(&self.batch_id);
    }
}

/// Why a batch answer or "enough" was not taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeskRefused {
    NoSuchBatch,
    NoSuchQuestion(String),
    NotOpen,
    Answer(String, AnswerRefused),
}

impl std::fmt::Display for DeskRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchBatch => f.write_str("no such batch of questions"),
            Self::NoSuchQuestion(id) => write!(f, "no question {id} in this batch"),
            Self::NotOpen => f.write_str("these questions are no longer open"),
            Self::Answer(id, refused) => write!(f, "question {id}: {refused}"),
        }
    }
}

impl std::error::Error for DeskRefused {}

/// What answering a batch did.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchAnswered {
    /// The batch's questions as they now stand.
    pub questions: Vec<Question>,
    /// A chat turn that was waiting took the answers.
    pub resumed: bool,
    /// The message to post into the chat, as the member's, when no turn was waiting: the
    /// session and the words.
    pub late: Option<(String, String)>,
}

impl BatchAnswered {
    /// Whether every question of the batch is answered or passed on.
    pub fn is_settled(&self) -> bool {
        !self.questions.iter().any(|q| q.state.is_open())
    }
}

/// Per question, why it may not be asked: it repeats one asked before or a stored answer
/// settles it. By key always, and by meaning when an embedder is loaded; a failed embedder
/// never fails the check.
pub async fn screen(
    texts: &[String],
    profile_id: &str,
    repo: &dyn QuestionRepository,
    embedder: Option<&dyn EmbeddingProvider>,
) -> anyhow::Result<Vec<Option<QuestionRefused>>> {
    let stored = repo.recent(profile_id, COMPARED_BY_KEY).await?;
    let refusal_for = |held: &Question| match (&held.state, &held.answer) {
        (QuestionState::Answered, Some(answer)) => QuestionRefused::Settled {
            asked: held.text.clone(),
            answer: answer.as_words(),
        },
        _ => QuestionRefused::Repeats {
            asked: held.text.clone(),
        },
    };
    let mut out: Vec<Option<QuestionRefused>> = Vec::with_capacity(texts.len());
    for (at, text) in texts.iter().enumerate() {
        let key = text_key(text);
        let earlier = texts[..at].iter().find(|t| text_key(t) == key);
        out.push(match earlier {
            Some(earlier) => Some(QuestionRefused::Repeats {
                asked: earlier.clone(),
            }),
            None => stored.iter().find(|q| q.key() == key).map(refusal_for),
        });
    }
    let Some(embedder) = embedder.filter(|e| e.available()) else {
        return Ok(out);
    };
    let compared: Vec<&Question> = stored.iter().take(COMPARED_BY_MEANING).collect();
    let alike = by_meaning(texts, &compared, embedder).await;
    match alike {
        Ok(found) => {
            for (at, held) in found {
                if out[at].is_none() {
                    out[at] = Some(refusal_for(held));
                }
            }
        }
        Err(e) => tracing::debug!(error = %e, "questions compared by key only"),
    }
    Ok(out)
}

/// The stored question each new text is alike to, by place in `texts`.
async fn by_meaning<'a>(
    texts: &[String],
    stored: &[&'a Question],
    embedder: &dyn EmbeddingProvider,
) -> anyhow::Result<Vec<(usize, &'a Question)>> {
    if stored.is_empty() || texts.is_empty() {
        return Ok(Vec::new());
    }
    let mut all: Vec<String> = texts.to_vec();
    all.extend(stored.iter().map(|q| q.text.clone()));
    let mut vectors = Vec::with_capacity(all.len());
    for chunk in all.chunks(embedder.max_batch().max(1)) {
        vectors.extend(embedder.embed_batch(chunk).await?);
    }
    let (new, old) = vectors.split_at(texts.len());
    let mut found = Vec::new();
    for (at, vector) in new.iter().enumerate() {
        let best = old
            .iter()
            .enumerate()
            .map(|(i, held)| (i, cosine_similarity(vector, held)))
            .filter(|(_, score)| *score >= ALIKE)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((i, _)) = best {
            found.push((at, stored[i]));
        }
    }
    Ok(found)
}

/// The questions `screen` lets through, in order; the rest are logged and dropped.
pub async fn keep_new(
    questions: Vec<Question>,
    profile_id: &str,
    repo: &dyn QuestionRepository,
    embedder: Option<&dyn EmbeddingProvider>,
) -> anyhow::Result<Vec<Question>> {
    let texts: Vec<String> = questions.iter().map(|q| q.text.clone()).collect();
    let screened = screen(&texts, profile_id, repo, embedder).await?;
    Ok(questions
        .into_iter()
        .zip(screened)
        .filter_map(|(question, refused)| match refused {
            None => Some(question),
            Some(refused) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "question_refused",
                    code = refused.code(),
                    "a question was not asked: {}",
                    refused.message()
                );
                None
            }
        })
        .collect())
}

/// Asks, stores, waits on and answers structured questions.
pub struct QuestionDesk {
    repo: Arc<dyn QuestionRepository>,
    waiters: Arc<QuestionWaiters>,
    embedder: Option<Arc<dyn EmbeddingProvider>>,
    wait: Duration,
}

impl QuestionDesk {
    pub fn new(repo: Arc<dyn QuestionRepository>) -> Self {
        Self {
            repo,
            waiters: Arc::new(QuestionWaiters::new()),
            embedder: None,
            wait: ASK_WAIT,
        }
    }

    /// Compares new questions with stored ones by meaning too, while `embedder` is loaded.
    pub fn with_embedder(mut self, embedder: Option<Arc<dyn EmbeddingProvider>>) -> Self {
        self.embedder = embedder;
        self
    }

    pub fn waiting_for(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    pub fn repo(&self) -> &Arc<dyn QuestionRepository> {
        &self.repo
    }

    pub fn embedder(&self) -> Option<&dyn EmbeddingProvider> {
        self.embedder.as_deref()
    }

    pub fn waiters(&self) -> &QuestionWaiters {
        &self.waiters
    }

    /// The chat `ask` tool: checks the batch, stores it for `session_id`, and waits for the
    /// member. Returns the tool's reply in plain lines.
    pub async fn ask(
        &self,
        profile_id: &str,
        session_id: &str,
        drafts: Vec<QuestionDraft>,
        spoken: bool,
        now: DateTime<Utc>,
    ) -> anyhow::Result<String> {
        if spoken {
            return Ok(SPOKEN_REFUSAL.to_string());
        }
        if drafts.is_empty() {
            return Ok("No question was asked: give one to four questions.".to_string());
        }
        let round = self.chat_round(profile_id, session_id, now).await?;
        if round > SOFT_LIMIT_ROUNDS {
            return Ok(SOFT_LIMIT_REPLY.to_string());
        }
        let batch_id = uuid::Uuid::new_v4().to_string();
        let mut notes: Vec<String> = Vec::new();
        let mut candidates: Vec<Question> = Vec::new();
        for draft in drafts.iter().take(QUESTIONS_PER_BATCH) {
            let mut draft = draft.clone();
            draft.gap = None;
            match Question::from_draft(profile_id, &batch_id, QuestionPurpose::Chat, &draft, now) {
                Ok(mut question) => {
                    question.session_id = Some(session_id.to_string());
                    question.round = round;
                    candidates.push(question);
                }
                Err(refused) => notes.push(not_asked(&draft.text, &refused)),
            }
        }
        if drafts.len() > QUESTIONS_PER_BATCH {
            notes.push(format!(
                "Only the first {QUESTIONS_PER_BATCH} questions were asked."
            ));
        }
        let texts: Vec<String> = candidates.iter().map(|q| q.text.clone()).collect();
        let screened = screen(&texts, profile_id, self.repo.as_ref(), self.embedder()).await?;
        let mut batch = Vec::new();
        for (question, refused) in candidates.into_iter().zip(screened) {
            match refused {
                None => batch.push(question),
                Some(refused) => notes.push(not_asked(&question.text, &refused)),
            }
        }
        if batch.is_empty() {
            return Ok(format!(
                "No question was asked:\n{}\nFix them and call ask again, or carry on.",
                notes.join("\n")
            ));
        }
        let ticket = self.waiters.register(&batch_id, profile_id, session_id);
        let stored = self.repo.add_batch(&batch).await?;
        if stored.is_empty() {
            return Ok("No question was asked: each repeats one asked before.".to_string());
        }
        self.repo.mark_asked(profile_id, &batch_id, now).await?;
        tracing::info!(
            target: "giap::trace",
            kind = "questions_asked_in_chat",
            session_id,
            batch_id = %batch_id,
            questions = stored.len(),
            round,
            "the chat model asked the member"
        );
        let settled = self.wait_for(ticket).await;
        let mut reply = match settled {
            Some(Settled::Answered(answered)) => {
                let lines: Vec<String> =
                    answered.iter().filter_map(Question::answer_line).collect();
                if lines.is_empty() {
                    "The member passed on these questions.".to_string()
                } else {
                    lines.join("\n")
                }
            }
            Some(Settled::Enough) => ENOUGH_REPLY.to_string(),
            None => {
                self.repo.set_waiting(profile_id, &batch_id).await?;
                NOT_YET_REPLY.to_string()
            }
        };
        if !notes.is_empty() {
            reply.push_str(&format!("\nNot asked:\n{}", notes.join("\n")));
        }
        Ok(reply)
    }

    async fn wait_for(&self, mut ticket: WaitTicket<'_>) -> Option<Settled> {
        let heard = ticket.heard.take()?;
        match tokio::time::timeout(self.wait, heard).await {
            Ok(Ok(settled)) => Some(settled),
            _ => None,
        }
    }

    /// The round a new chat batch takes: its chat's batches in the last few hours, plus one.
    async fn chat_round(
        &self,
        profile_id: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<u32> {
        let since = now - chrono::Duration::hours(CHAT_CHAIN_HOURS);
        let mut batches: Vec<String> = self
            .repo
            .for_session(profile_id, session_id)
            .await?
            .into_iter()
            .filter(|q| q.created_at >= since)
            .map(|q| q.batch_id)
            .collect();
        batches.sort();
        batches.dedup();
        Ok(batches.len() as u32 + 1)
    }

    /// Takes the member's answers to a batch's questions. Questions left out stay open. Once the
    /// batch is settled, a turn waiting on it carries on, or, for a chat whose turn stopped
    /// waiting, the answers are handed back to post as the member's next message.
    pub async fn answer_batch(
        &self,
        profile_id: &str,
        batch_id: &str,
        answers: Vec<(String, Answer)>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Result<BatchAnswered, DeskRefused>> {
        let mut questions = self.repo.in_batch(profile_id, batch_id).await?;
        if questions.is_empty() {
            return Ok(Err(DeskRefused::NoSuchBatch));
        }
        if !questions.iter().any(|q| q.state.is_open()) {
            return Ok(Err(DeskRefused::NotOpen));
        }
        for (id, _) in &answers {
            if !questions.iter().any(|q| &q.id == id) {
                return Ok(Err(DeskRefused::NoSuchQuestion(id.clone())));
            }
        }
        let mut changed = Vec::new();
        for (id, answer) in answers {
            let question = questions.iter_mut().find(|q| q.id == id).expect("checked");
            if let Err(refused) = question.answer(answer, now) {
                return Ok(Err(DeskRefused::Answer(id, refused)));
            }
            changed.push(question.clone());
        }
        for question in &changed {
            self.repo.save(question).await?;
        }
        Ok(Ok(self.after_answers(questions)))
    }

    /// The batch as it stands after one of its questions changed outside an answer, such as a
    /// pass: a turn waiting on it carries on once it is settled.
    pub async fn settled(&self, profile_id: &str, batch_id: &str) -> anyhow::Result<BatchAnswered> {
        let questions = self.repo.in_batch(profile_id, batch_id).await?;
        Ok(self.after_answers(questions))
    }

    /// What follows a batch whose answers were just kept.
    fn after_answers(&self, questions: Vec<Question>) -> BatchAnswered {
        let mut done = BatchAnswered {
            questions,
            resumed: false,
            late: None,
        };
        let Some(first) = done.questions.first() else {
            return done;
        };
        if !done.is_settled() || first.purpose != QuestionPurpose::Chat {
            return done;
        }
        let batch_id = first.batch_id.clone();
        let answered: Vec<Question> = done
            .questions
            .iter()
            .filter(|q| q.state == QuestionState::Answered)
            .cloned()
            .collect();
        if self
            .waiters
            .resolve(&batch_id, Settled::Answered(done.questions.clone()))
        {
            done.resumed = true;
            return done;
        }
        let message = crate::user_data::domain::question::answers_message(&answered);
        done.late = first.session_id.clone().zip(message);
        done
    }

    /// "Enough": the batch's open questions are passed on, and a turn waiting on it is told to
    /// go ahead. Returns the batch as it now stands.
    pub async fn enough(
        &self,
        profile_id: &str,
        batch_id: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Result<Vec<Question>, DeskRefused>> {
        let mut questions = self.repo.in_batch(profile_id, batch_id).await?;
        if questions.is_empty() {
            return Ok(Err(DeskRefused::NoSuchBatch));
        }
        for question in questions.iter_mut() {
            if question.skip(now) {
                self.repo.save(question).await?;
            }
        }
        self.waiters.resolve(batch_id, Settled::Enough);
        Ok(Ok(questions))
    }

    /// A message typed in `session_id` while a turn there waits on a batch: it answers the
    /// batch's first open question as "Other", the rest are passed on, and the turn carries on.
    /// Returns the batch it answered, or `None` when no turn there is waiting.
    pub async fn typed_while_open(
        &self,
        session_id: &str,
        text: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<Option<String>> {
        if text.trim().is_empty() {
            return Ok(None);
        }
        let Some((batch_id, profile_id)) = self.waiters.waiting_in(session_id) else {
            return Ok(None);
        };
        let mut questions = self.repo.in_batch(&profile_id, &batch_id).await?;
        let mut answered_one = false;
        for question in questions.iter_mut().filter(|q| q.state.is_open()) {
            let changed = if answered_one {
                question.skip(now)
            } else {
                let typed = Answer {
                    choices: Vec::new(),
                    text: Some(text.to_string()),
                };
                answered_one = question.answer(typed, now).is_ok();
                answered_one
            };
            if changed {
                self.repo.save(question).await?;
            }
        }
        if !answered_one {
            return Ok(None);
        }
        self.waiters
            .resolve(&batch_id, Settled::Answered(questions));
        Ok(Some(batch_id))
    }
}

fn not_asked(text: &str, refused: &QuestionRefused) -> String {
    let named = if text.trim().is_empty() {
        "A question".to_string()
    } else {
        format!("\"{}\"", text.trim())
    };
    format!("- {named}: {}", refused.message())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::question::QuestionOption;
    use crate::user_data::mocks::mock_questions::InMemoryQuestionRepository;
    use async_trait::async_trait;

    fn draft(header: &str, text: &str) -> QuestionDraft {
        QuestionDraft {
            header: header.into(),
            text: text.into(),
            options: vec![
                QuestionOption::new("12-15 Dec", "A long weekend"),
                QuestionOption::new("19-22 Dec", "Before Christmas"),
            ],
            recommended: Some(0),
            ..QuestionDraft::default()
        }
    }

    fn desk(repo: Arc<InMemoryQuestionRepository>) -> Arc<QuestionDesk> {
        Arc::new(QuestionDesk::new(repo).waiting_for(Duration::from_secs(5)))
    }

    /// Scores two texts alike when they share their first word.
    struct FirstWord;

    #[async_trait]
    impl EmbeddingProvider for FirstWord {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            let first = text.split_whitespace().next().unwrap_or("").to_lowercase();
            Ok(match first.as_str() {
                "when" => vec![1.0, 0.0],
                _ => vec![0.0, 1.0],
            })
        }
        fn dimensions(&self) -> usize {
            2
        }
    }

    struct Broken;

    #[async_trait]
    impl EmbeddingProvider for Broken {
        async fn embed(&self, _: &str) -> anyhow::Result<Vec<f32>> {
            anyhow::bail!("no model")
        }
        fn dimensions(&self) -> usize {
            2
        }
    }

    async fn the_batch_in(desk: &QuestionDesk, session: &str) -> Vec<Question> {
        for _ in 0..200 {
            let found = desk.repo.for_session("jerry", session).await.unwrap();
            if !found.is_empty() {
                return found;
            }
            tokio::task::yield_now().await;
        }
        panic!("the batch was never stored");
    }

    #[tokio::test]
    async fn a_chat_turn_waits_for_the_answers_and_gets_them_as_plain_lines() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let desk = desk(repo.clone());
        let asking = tokio::spawn({
            let desk = desk.clone();
            async move {
                desk.ask(
                    "jerry",
                    "s1",
                    vec![
                        draft("Dates", "When do you go?"),
                        draft("Budget", "How much?"),
                    ],
                    false,
                    Utc::now(),
                )
                .await
                .unwrap()
            }
        });
        let batch = the_batch_in(&desk, "s1").await;
        assert_eq!(batch[0].purpose, QuestionPurpose::Chat);
        assert!(desk.waiters().is_waiting(&batch[0].batch_id));
        let answered = desk
            .answer_batch(
                "jerry",
                &batch[0].batch_id,
                vec![
                    (
                        batch[0].id.clone(),
                        Answer {
                            choices: vec!["12-15 Dec".into()],
                            text: None,
                        },
                    ),
                    (
                        batch[1].id.clone(),
                        Answer {
                            choices: vec![],
                            text: Some("about 30k".into()),
                        },
                    ),
                ],
                Utc::now(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(answered.resumed);
        assert_eq!(answered.late, None);
        assert_eq!(
            asking.await.unwrap(),
            "Dates: 12-15 Dec\nBudget: Other: \"about 30k\""
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_batch_waits_and_a_later_answer_goes_to_the_chat() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let desk = desk(repo.clone());
        let reply = desk
            .ask(
                "jerry",
                "s1",
                vec![draft("Dates", "When do you go?")],
                false,
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(reply, NOT_YET_REPLY);
        let batch = repo.for_session("jerry", "s1").await.unwrap();
        assert_eq!(batch[0].state, QuestionState::Waiting);
        let answered = desk
            .answer_batch(
                "jerry",
                &batch[0].batch_id,
                vec![(
                    batch[0].id.clone(),
                    Answer {
                        choices: vec!["19-22 Dec".into()],
                        text: None,
                    },
                )],
                Utc::now(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(!answered.resumed);
        assert_eq!(
            answered.late,
            Some(("s1".into(), "My answers:\nDates: 19-22 Dec".into()))
        );
    }

    #[tokio::test]
    async fn enough_tells_the_waiting_turn_to_go_ahead() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let desk = desk(repo.clone());
        let asking = tokio::spawn({
            let desk = desk.clone();
            async move {
                desk.ask(
                    "jerry",
                    "s1",
                    vec![draft("Dates", "When do you go?")],
                    false,
                    Utc::now(),
                )
                .await
                .unwrap()
            }
        });
        let batch = the_batch_in(&desk, "s1").await;
        let left = desk
            .enough("jerry", &batch[0].batch_id, Utc::now())
            .await
            .unwrap()
            .unwrap();
        assert!(left.iter().all(|q| q.state == QuestionState::Skipped));
        assert_eq!(asking.await.unwrap(), ENOUGH_REPLY);
    }

    #[tokio::test]
    async fn a_message_typed_while_a_batch_is_open_answers_its_first_question() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let desk = desk(repo.clone());
        assert_eq!(
            desk.typed_while_open("s1", "whenever", Utc::now())
                .await
                .unwrap(),
            None,
            "no turn is waiting"
        );
        let asking = tokio::spawn({
            let desk = desk.clone();
            async move {
                desk.ask(
                    "jerry",
                    "s1",
                    vec![
                        draft("Dates", "When do you go?"),
                        draft("Budget", "How much?"),
                    ],
                    false,
                    Utc::now(),
                )
                .await
                .unwrap()
            }
        });
        let batch = the_batch_in(&desk, "s1").await;
        let answered = desk
            .typed_while_open("s1", "the week after next", Utc::now())
            .await
            .unwrap();
        assert_eq!(answered.as_deref(), Some(batch[0].batch_id.as_str()));
        assert_eq!(
            asking.await.unwrap(),
            "Dates: Other: \"the week after next\""
        );
        let after = repo.in_batch("jerry", &batch[0].batch_id).await.unwrap();
        assert_eq!(after[1].state, QuestionState::Skipped);
    }

    #[tokio::test]
    async fn a_spoken_turn_and_a_sixth_round_are_refused() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let desk = Arc::new(QuestionDesk::new(repo.clone()).waiting_for(Duration::ZERO));
        let ask = |text: &'static str, spoken: bool| {
            let desk = desk.clone();
            async move {
                desk.ask("jerry", "s1", vec![draft("Q", text)], spoken, Utc::now())
                    .await
                    .unwrap()
            }
        };
        assert_eq!(ask("When do you go?", true).await, SPOKEN_REFUSAL);
        for text in ["One?", "Two?", "Three?", "Four?", "Five?"] {
            assert_eq!(ask(text, false).await, NOT_YET_REPLY);
        }
        assert_eq!(ask("Six?", false).await, SOFT_LIMIT_REPLY);
    }

    #[tokio::test]
    async fn a_refused_question_comes_back_with_its_reason_or_the_stored_answer() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let mut old = Question::from_draft(
            "jerry",
            "old",
            QuestionPurpose::Anchor,
            &draft("Dates", "When do you go?"),
            Utc::now(),
        )
        .unwrap();
        old.answer(
            Answer {
                choices: vec!["12-15 Dec".into()],
                text: None,
            },
            Utc::now(),
        )
        .unwrap();
        repo.add_batch(&[old]).await.unwrap();
        let desk = desk(repo.clone());
        let mut bad = draft("Way too long a header", "How do you get there?");
        bad.options.push(QuestionOption::new("Other", ""));
        let reply = desk
            .ask(
                "jerry",
                "s1",
                vec![draft("Dates", "when do you GO"), bad],
                false,
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(reply.starts_with("No question was asked:"), "{reply}");
        assert!(
            reply.contains("the member already answered \"When do you go?\": 12-15 Dec"),
            "{reply}"
        );
        assert!(reply.contains("header is over 12"), "{reply}");
        assert!(repo.for_session("jerry", "s1").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn questions_alike_in_meaning_repeat_and_a_failed_embedder_falls_back_to_keys() {
        let repo = InMemoryQuestionRepository::new();
        let old = Question::from_draft(
            "jerry",
            "old",
            QuestionPurpose::Anchor,
            &draft("Dates", "When do you leave?"),
            Utc::now(),
        )
        .unwrap();
        repo.add_batch(&[old]).await.unwrap();
        let texts = vec![
            "When are you travelling?".to_string(),
            "How much will you spend?".to_string(),
        ];
        let by_meaning = screen(&texts, "jerry", &repo, Some(&FirstWord))
            .await
            .unwrap();
        assert!(matches!(
            by_meaning[0],
            Some(QuestionRefused::Repeats { .. })
        ));
        assert_eq!(by_meaning[1], None);
        let by_key = screen(&texts, "jerry", &repo, Some(&Broken)).await.unwrap();
        assert_eq!(by_key, [None, None]);
        let same = vec![
            "when do you LEAVE".to_string(),
            "When do you leave".to_string(),
        ];
        let keyed = screen(&same, "jerry", &repo, None).await.unwrap();
        assert!(keyed.iter().all(Option::is_some));
    }
}
