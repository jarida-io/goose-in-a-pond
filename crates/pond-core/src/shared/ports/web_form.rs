//! Driven ports for POST forms a member confirms on their phone: where they wait, and how one is
//! sent.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::shared::domain::web_form::{FormSubmission, SubmissionState};

#[async_trait]
pub trait FormSubmissionStore: Send + Sync {
    async fn create(&self, submission: &FormSubmission) -> Result<()>;
    async fn get(&self, id: &str) -> Result<Option<FormSubmission>>;
    /// The owner's submissions asked at or after `since`, newest first.
    async fn recent_for(&self, owner: &str, since: DateTime<Utc>) -> Result<Vec<FormSubmission>>;
    /// Moves a submission still in state `from` to `to`; `false` when it had already moved on.
    /// `decided_at` is set the first time it leaves `waiting`.
    async fn advance(
        &self,
        id: &str,
        from: &SubmissionState,
        to: &SubmissionState,
        at: DateTime<Utc>,
    ) -> Result<bool>;
    /// Deletes the submissions asked before `before`; returns how many.
    async fn prune(&self, before: DateTime<Utc>) -> Result<u64>;
}

/// What the site answered a sent form with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentPage {
    pub status: u16,
    /// Where the answer ended, after any redirect.
    pub address: String,
    pub title: String,
}

/// Why a form was not sent, or its answer was lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendFailure {
    /// Nothing reached the site; a sentence saying why.
    NotSent(String),
    /// The form went out but no answer came back, so the site may have it.
    AnswerLost(String),
}

#[async_trait]
pub trait FormSender: Send + Sync {
    async fn send(&self, submission: &FormSubmission)
        -> std::result::Result<SentPage, SendFailure>;
}
