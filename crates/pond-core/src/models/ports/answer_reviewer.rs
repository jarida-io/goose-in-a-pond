//! Answer review port: the SAME model, under a critic prompt, scores an answer and may revise it.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// The outcome of a single review evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewVerdict {
    pub pass: bool,
    /// Quality score 1-5 (1 = unusable, 5 = excellent).
    pub score: u8,
    /// What the reviewer expected the answer to contain.
    #[serde(default)]
    pub expectations: Vec<String>,
    /// What is missing, wrong or weak; empty when `pass` is true.
    #[serde(default)]
    pub critique: String,
}

/// The result of the full review process (review + optional revision).
#[derive(Debug, Clone)]
pub struct ReviewResult {
    /// The final answer text (original if passed, revised if not).
    pub final_answer: String,
    pub was_revised: bool,
    /// The verdict from the last review round.
    pub verdict: ReviewVerdict,
    pub rounds: u32,
}

/// Post-inference answer reviewer; callers persist and deliver `ReviewResult::final_answer`.
#[async_trait]
pub trait AnswerReviewer: Send + Sync {
    /// Review `answer` against `question`; `tool_context` is any tool data the answer was given.
    async fn review(
        &self,
        question: &str,
        answer: &str,
        tool_context: Option<&str>,
    ) -> Result<ReviewResult>;
}
