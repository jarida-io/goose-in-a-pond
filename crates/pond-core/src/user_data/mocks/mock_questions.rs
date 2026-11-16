//! In-memory [`QuestionRepository`] with the SQLite adapter's rule: a member is asked a
//! question once, by its key.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::question::{Question, QuestionPurpose, QuestionState};
use crate::user_data::ports::questions::QuestionRepository;

#[derive(Default)]
pub struct InMemoryQuestionRepository {
    rows: Mutex<Vec<Question>>,
}

impl InMemoryQuestionRepository {
    pub fn new() -> Self {
        Self::default()
    }

    fn oldest_first(&self, keep: impl Fn(&Question) -> bool) -> Vec<Question> {
        let mut found: Vec<Question> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|q| keep(q))
            .cloned()
            .collect();
        found.sort_by_key(|q| q.created_at);
        found
    }
}

#[async_trait]
impl QuestionRepository for InMemoryQuestionRepository {
    async fn add_batch(&self, questions: &[Question]) -> Result<Vec<Question>> {
        let mut rows = self.rows.lock().unwrap();
        let mut stored = Vec::new();
        for question in questions {
            let key = question.key();
            if rows.iter().any(|q| {
                q.id == question.id || (q.profile_id == question.profile_id && q.key() == key)
            }) {
                continue;
            }
            rows.push(question.clone());
            stored.push(question.clone());
        }
        Ok(stored)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Question>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|q| q.profile_id == profile_id && q.id == id)
            .cloned())
    }

    async fn open(&self, profile_id: &str) -> Result<Vec<Question>> {
        Ok(self.oldest_first(|q| q.profile_id == profile_id && q.state == QuestionState::Open))
    }

    async fn for_anchor(&self, profile_id: &str, anchor_id: &str) -> Result<Vec<Question>> {
        Ok(self.oldest_first(|q| {
            q.profile_id == profile_id && q.anchor_id.as_deref() == Some(anchor_id)
        }))
    }

    async fn answered(&self, profile_id: &str, limit: usize) -> Result<Vec<Question>> {
        let mut found: Vec<Question> = self
            .rows
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|q| q.profile_id == profile_id && q.state == QuestionState::Answered)
            .cloned()
            .collect();
        found.sort_by(|a, b| b.answered_at.cmp(&a.answered_at));
        found.truncate(limit);
        Ok(found)
    }

    async fn save(&self, question: &Question) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        match rows
            .iter_mut()
            .find(|q| q.profile_id == question.profile_id && q.id == question.id)
        {
            Some(held) => {
                held.state = question.state;
                held.answer = question.answer.clone();
                held.asked_at = question.asked_at;
                held.answered_at = question.answered_at;
                held.expires_at = question.expires_at;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn mark_asked(&self, profile_id: &str, batch_id: &str, at: DateTime<Utc>) -> Result<()> {
        let mut rows = self.rows.lock().unwrap();
        for question in rows.iter_mut().filter(|q| {
            q.profile_id == profile_id && q.batch_id == batch_id && q.state == QuestionState::Open
        }) {
            question.asked_at = Some(at);
        }
        Ok(())
    }

    async fn last_batch_at(
        &self,
        profile_id: &str,
        purpose: QuestionPurpose,
    ) -> Result<Option<DateTime<Utc>>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|q| q.profile_id == profile_id && q.purpose == purpose)
            .map(|q| q.created_at)
            .max())
    }

    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64> {
        let mut rows = self.rows.lock().unwrap();
        let mut expired = 0;
        for question in rows.iter_mut().filter(|q| q.is_overdue(now)) {
            question.expire(now);
            expired += 1;
        }
        Ok(expired)
    }
}
