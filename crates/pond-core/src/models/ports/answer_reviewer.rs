//! Answer review port: the SAME model, under a critic prompt, scores an answer and may revise it.
//! The implementation and its rules are `models::services::answer_review`.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The outcome of a single review evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewVerdict {
    /// The reviewer's own pass flag. Advisory only: decisions use `score` against the threshold.
    #[serde(default)]
    pub pass: bool,
    /// Quality score 1-5 (1 = unusable, 5 = excellent).
    pub score: u8,
    /// What the reviewer expected the answer to contain.
    #[serde(default)]
    pub expectations: Vec<String>,
    /// What is missing, wrong or weak; empty when the answer passed.
    #[serde(default)]
    pub critique: String,
}

/// How a review ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewOutcome {
    /// The answer met the pass mark as it was.
    Passed,
    /// A revision scored strictly higher than the answer and replaced it.
    Revised,
    /// The answer fell short, but no revision did better (or none was allowed); it stands.
    KeptOriginal,
    /// The reviewer gave no usable verdict; the answer stands.
    Unjudged,
}

impl ReviewOutcome {
    /// Stable name for logs and status lines.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Revised => "revised",
            Self::KeptOriginal => "kept_original",
            Self::Unjudged => "unjudged",
        }
    }
}

/// The result of the full review process (review + optional revision).
#[derive(Debug, Clone)]
pub struct ReviewResult {
    /// The answer to deliver and persist: the original unless a revision beat it.
    pub final_answer: String,
    pub outcome: ReviewOutcome,
    /// The verdict on `final_answer`; `None` when the reviewer gave none.
    pub verdict: Option<ReviewVerdict>,
    /// Revision attempts made.
    pub rounds: u32,
}

impl ReviewResult {
    /// The answer kept as it was, because the reviewer gave no usable verdict.
    pub fn unjudged(answer: &str) -> Self {
        Self {
            final_answer: answer.to_string(),
            outcome: ReviewOutcome::Unjudged,
            verdict: None,
            rounds: 0,
        }
    }

    /// Whether `final_answer` differs from the answer reviewed, so the client must replace it.
    pub fn was_revised(&self) -> bool {
        self.outcome == ReviewOutcome::Revised
    }
}

/// Post-inference answer reviewer; callers persist and deliver `ReviewResult::final_answer`.
#[async_trait]
pub trait AnswerReviewer: Send + Sync {
    /// Review `answer` to `question`. `tool_results` are this turn's tool outputs, the facts the
    /// answer can be checked against.
    async fn review(
        &self,
        question: &str,
        answer: &str,
        tool_results: &[String],
    ) -> Result<ReviewResult>;
}
