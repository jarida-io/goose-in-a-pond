//! Post-inference answer review: the same model, under a critic prompt, scores an answer and may
//! revise it. See `docs/architecture/agent_pipeline.md`, "Answer review".
//!
//! The rules that keep a review from making an answer worse:
//!
//! - **Grounding.** The reviewer sees this turn's tool results, and is told the assistant also had
//!   the household's private context (members, relationships, preferences, past conversations),
//!   which it cannot see. A household fact is not "made up" because the reviewer cannot check it.
//! - **A revision must earn its place.** It is reviewed in turn and kept only when it scores
//!   strictly higher than the answer it would replace; otherwise the original stands and the
//!   client is sent nothing. A revision that refuses, or says the assistant has no access to
//!   something ([`is_non_answer`]), never replaces an answer that did not.
//! - **No verdict, no change.** A verdict that cannot be parsed keeps the answer as it is.
//! - **`auto` reviews what can be checked.** In `auto` mode only a turn that called tools is
//!   reviewed, against those results ([`should_review`]). An answer from the model's own
//!   knowledge or the household's memory gives the reviewer nothing to check it against, and it
//!   was exactly such an answer ("Your girlfriend is Arlene") that a review once replaced with
//!   "As an AI I have no access to your personal life".
//!
//! `review_max_rounds` and `review_pass_threshold` are read from settings on every review.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::models::domain::message::ChatMessage;
use crate::models::ports::answer_reviewer::{
    AnswerReviewer, ReviewOutcome, ReviewResult, ReviewVerdict,
};
use crate::models::ports::provider::LlmProvider;
use crate::models::services::thought_filter::strip_reasoning;
use crate::prompts::{REVIEW_SYSTEM_PROMPT, REVISION_SYSTEM_PROMPT};
use crate::user_data::ports::settings::SettingsRepository;

/// Most characters of one tool result the reviewer is shown.
const MAX_TOOL_RESULT_CHARS: usize = 2_000;

/// Most characters of all tool results together, so a long turn cannot crowd out the answer.
const MAX_GROUNDING_CHARS: usize = 8_000;

/// Openings of a reply that declines rather than answers. Matched on the lowercased start of the
/// reply only: these are a safety net under the prompts, not a classifier of questions.
const NON_ANSWER_OPENINGS: &[&str] = &[
    "as an ai",
    "i'm an ai",
    "i am an ai",
    "i don't have access",
    "i do not have access",
    "i have no access",
    "i can't access",
    "i cannot access",
    "i'm unable to access",
    "i am unable to access",
    "i don't have personal",
    "i do not have personal",
    "i don't have any information about your",
    "i do not have any information about your",
    "i'm sorry, but i can't",
    "i'm sorry, but i cannot",
];

/// Whether a turn is reviewed under `review_mode`: `on` always, `off` never, and `auto` only
/// when the turn called tools, whose results the reviewer is given to check the answer against.
pub fn should_review(review_mode: &str, turn_used_tools: bool) -> bool {
    match review_mode {
        "on" => true,
        "auto" => turn_used_tools,
        _ => false,
    }
}

/// Whether `reply` declines instead of answering ("As an AI I have no access to ...").
pub fn is_non_answer(reply: &str) -> bool {
    let start: String = reply.trim_start().chars().take(80).collect();
    let start = start.to_lowercase().replace('\u{2019}', "'");
    NON_ANSWER_OPENINGS.iter().any(|o| start.starts_with(o))
}

/// Whether `revision` (scored `revised_score`) may replace `current` (scored `current_score`):
/// only when strictly better, non-empty, and not a refusal standing in for an answer.
pub fn accept_revision(
    current: &str,
    current_score: u8,
    revision: &str,
    revised_score: u8,
) -> bool {
    !revision.trim().is_empty()
        && revised_score > current_score
        && (!is_non_answer(revision) || is_non_answer(current))
}

/// Parse the reviewer's JSON verdict, with the score clamped to 1..=5. `None` when there is no
/// JSON object in `text` or it does not have the verdict's shape.
pub fn parse_review_verdict(text: &str) -> Option<ReviewVerdict> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let mut verdict: ReviewVerdict = serde_json::from_str(&text[start..=end]).ok()?;
    verdict.score = verdict.score.clamp(1, 5);
    Some(verdict)
}

/// The tool results as the reviewer reads them, each and all cut to a budget.
fn grounding_block(tool_results: &[String]) -> String {
    if tool_results.is_empty() {
        return " (none: the assistant called no tools this turn)".to_string();
    }
    let mut block = String::new();
    for result in tool_results {
        if block.chars().count() >= MAX_GROUNDING_CHARS {
            block.push_str("\n[further tool results omitted]");
            break;
        }
        let cut: String = result.chars().take(MAX_TOOL_RESULT_CHARS).collect();
        block.push_str("\n- ");
        block.push_str(&cut);
        if cut.len() < result.len() {
            block.push_str(" [cut]");
        }
    }
    block
}

/// [`AnswerReviewer`] over the live chat model, with its limits read from settings per review.
pub struct LlmAnswerReviewer {
    /// Read per review, so it always uses the currently loaded model.
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
}

impl LlmAnswerReviewer {
    /// A reviewer over whichever provider `live_provider` holds when a review runs.
    pub fn new(
        live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
        settings: Arc<dyn SettingsRepository + Send + Sync>,
    ) -> Self {
        Self {
            live_provider,
            settings,
        }
    }

    async fn judge(
        &self,
        provider: &dyn LlmProvider,
        question: &str,
        answer: &str,
        grounding: &str,
    ) -> Result<Option<ReviewVerdict>> {
        let input = format!(
            "QUESTION: {question}\n\nTOOL RESULTS THIS TURN:{grounding}\n\nANSWER TO REVIEW:\n{answer}"
        );
        let reply = provider
            .complete(REVIEW_SYSTEM_PROMPT, vec![ChatMessage::user(input)])
            .await?;
        let verdict = parse_review_verdict(&strip_reasoning(&reply.content));
        if verdict.is_none() {
            let head: String = reply.content.chars().take(100).collect();
            tracing::warn!(
                reply_head = %head,
                "answer review: unparseable verdict; keeping the answer as it is"
            );
        }
        Ok(verdict)
    }

    async fn revise(
        &self,
        provider: &dyn LlmProvider,
        persona: &str,
        question: &str,
        answer: &str,
        verdict: &ReviewVerdict,
        grounding: &str,
    ) -> Result<String> {
        let input = format!(
            "WHO YOU ARE: {persona}\n\n\
             ORIGINAL QUESTION: {question}\n\n\
             TOOL RESULTS THIS TURN:{grounding}\n\n\
             YOUR PREVIOUS ANSWER:\n{answer}\n\n\
             REVIEWER CRITIQUE:\n{}\n\n\
             WHAT THE ANSWER SHOULD INCLUDE:\n- {}",
            verdict.critique,
            verdict.expectations.join("\n- ")
        );
        let reply = provider
            .complete(REVISION_SYSTEM_PROMPT, vec![ChatMessage::user(input)])
            .await?;
        // A side call bypasses the stream's ThoughtFilter, so reasoning is stripped here.
        Ok(strip_reasoning(&reply.content).trim().to_string())
    }
}

#[async_trait]
impl AnswerReviewer for LlmAnswerReviewer {
    async fn review(
        &self,
        question: &str,
        answer: &str,
        tool_results: &[String],
    ) -> Result<ReviewResult> {
        let provider = self
            .live_provider
            .read()
            .await
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no LLM provider available for review"))?;
        let settings = self.settings.get().await?;
        let threshold = settings.review_pass_threshold.clamp(1, 5);
        let max_rounds = settings.review_max_rounds;
        let persona = format!(
            "{}, the household's own assistant, running on their device. {}",
            settings.assistant_name, settings.assistant_personality
        );
        let grounding = grounding_block(tool_results);

        let Some(original) = self
            .judge(provider.as_ref(), question, answer, &grounding)
            .await?
        else {
            tracing::info!(
                target: "giap::trace",
                kind = "answer_review",
                verdict = ReviewOutcome::Unjudged.as_str(),
                revised = false,
                kept_original = false,
                tool_results = tool_results.len(),
            );
            return Ok(ReviewResult::unjudged(answer));
        };

        let mut best_answer = answer.to_string();
        let mut best = original.clone();
        let mut rounds = 0u32;
        while best.score < threshold && rounds < max_rounds {
            rounds += 1;
            let revision = self
                .revise(
                    provider.as_ref(),
                    &persona,
                    question,
                    &best_answer,
                    &best,
                    &grounding,
                )
                .await?;
            if is_non_answer(&revision) && !is_non_answer(&best_answer) {
                tracing::warn!(
                    round = rounds,
                    "answer review: the revision declined to answer; keeping the answer"
                );
                break;
            }
            let Some(revised) = self
                .judge(provider.as_ref(), question, &revision, &grounding)
                .await?
            else {
                break;
            };
            if !accept_revision(&best_answer, best.score, &revision, revised.score) {
                tracing::info!(
                    round = rounds,
                    score = best.score,
                    revised_score = revised.score,
                    "answer review: the revision scored no better; keeping the answer"
                );
                break;
            }
            best_answer = revision;
            best = revised;
        }

        let outcome = if best_answer != answer {
            ReviewOutcome::Revised
        } else if best.score >= threshold {
            ReviewOutcome::Passed
        } else {
            ReviewOutcome::KeptOriginal
        };
        tracing::info!(
            target: "giap::trace",
            kind = "answer_review",
            verdict = outcome.as_str(),
            score = best.score,
            first_score = original.score,
            threshold,
            rounds,
            revised = outcome == ReviewOutcome::Revised,
            kept_original = outcome == ReviewOutcome::KeptOriginal,
            tool_results = tool_results.len(),
        );
        Ok(ReviewResult {
            final_answer: best_answer,
            outcome,
            verdict: Some(best),
            rounds,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::user_data::mocks::mock_settings::MockSettingsRepository;

    /// Answers each call with the next canned reply, and records the system prompts it was sent.
    struct Scripted {
        replies: Mutex<Vec<String>>,
        prompts: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(replies: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.iter().rev().map(|r| r.to_string()).collect()),
                prompts: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> usize {
            self.prompts.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl LlmProvider for Scripted {
        async fn complete(&self, system: &str, _m: Vec<ChatMessage>) -> Result<ChatMessage> {
            self.prompts.lock().unwrap().push(system.to_string());
            let reply = self
                .replies
                .lock()
                .unwrap()
                .pop()
                .expect("an unscripted call");
            Ok(ChatMessage::assistant(reply))
        }

        fn model_name(&self) -> String {
            "scripted".into()
        }
    }

    fn verdict(score: u8) -> String {
        format!(
            r#"{{"pass": {}, "score": {score}, "expectations": [], "critique": "c"}}"#,
            score >= 3
        )
    }

    async fn reviewer(provider: Arc<Scripted>, threshold: u8, rounds: u32) -> LlmAnswerReviewer {
        let settings = Arc::new(MockSettingsRepository::new());
        settings
            .set_key("review_pass_threshold", threshold.to_string())
            .await
            .unwrap();
        settings
            .set_key("review_max_rounds", rounds.to_string())
            .await
            .unwrap();
        let live: Arc<dyn LlmProvider> = provider;
        LlmAnswerReviewer::new(Arc::new(RwLock::new(Some(live))), settings)
    }

    #[test]
    fn auto_reviews_only_a_turn_that_called_tools() {
        assert!(should_review("auto", true));
        assert!(!should_review("auto", false));
        assert!(should_review("on", false));
        assert!(!should_review("off", true));
    }

    #[test]
    fn a_refusal_is_a_non_answer() {
        assert!(is_non_answer(
            "As an AI, I have no access to your personal life."
        ));
        assert!(is_non_answer("I don\u{2019}t have access to that."));
        assert!(!is_non_answer("Your girlfriend is Arlene."));
    }

    #[test]
    fn a_revision_must_score_strictly_higher() {
        assert!(!accept_revision("a", 2, "b", 2));
        assert!(accept_revision("a", 2, "b", 3));
        assert!(!accept_revision("a", 2, "  ", 5));
    }

    #[test]
    fn a_refusal_never_replaces_an_answer() {
        assert!(!accept_revision(
            "Your girlfriend is Arlene.",
            1,
            "As an AI I have no access to your personal life.",
            5
        ));
    }

    #[test]
    fn verdicts_parse_with_the_score_clamped() {
        let v = parse_review_verdict(r#"noise {"pass": true, "score": 9} tail"#).unwrap();
        assert_eq!(v.score, 5);
        assert!(parse_review_verdict("no json here").is_none());
        assert!(parse_review_verdict(r#"{"verdict": "fine"}"#).is_none());
    }

    #[test]
    fn grounding_is_cut_to_budget() {
        let long = "x".repeat(MAX_TOOL_RESULT_CHARS * 10);
        let many = vec![long; 10];
        let block = grounding_block(&many);
        assert!(block.chars().count() < MAX_GROUNDING_CHARS + MAX_TOOL_RESULT_CHARS + 100);
        assert!(block.contains("[further tool results omitted]"));
    }

    /// The reported failure: a correct answer from memory, scored low, revised into a refusal.
    #[tokio::test]
    async fn a_refusing_revision_keeps_the_original() {
        let provider = Scripted::new(&[
            &verdict(1),
            "As an AI, I have no access to your personal life.",
        ]);
        let r = reviewer(provider.clone(), 3, 2).await;
        let result = r
            .review("Who is my girlfriend?", "Your girlfriend is Arlene.", &[])
            .await
            .unwrap();
        assert_eq!(result.outcome, ReviewOutcome::KeptOriginal);
        assert_eq!(result.final_answer, "Your girlfriend is Arlene.");
        assert!(!result.was_revised());
    }

    #[tokio::test]
    async fn a_revision_no_better_than_the_original_is_dropped() {
        let provider = Scripted::new(&[&verdict(2), "Another answer.", &verdict(2)]);
        let r = reviewer(provider.clone(), 3, 1).await;
        let result = r.review("q?", "First answer.", &[]).await.unwrap();
        assert_eq!(result.outcome, ReviewOutcome::KeptOriginal);
        assert_eq!(result.final_answer, "First answer.");
        assert_eq!(provider.calls(), 3, "the revision is re-reviewed");
    }

    #[tokio::test]
    async fn a_better_revision_is_kept() {
        let provider = Scripted::new(&[&verdict(2), "Better answer.", &verdict(4)]);
        let r = reviewer(provider, 3, 1).await;
        let result = r.review("q?", "First answer.", &[]).await.unwrap();
        assert_eq!(result.outcome, ReviewOutcome::Revised);
        assert_eq!(result.final_answer, "Better answer.");
        assert_eq!(result.verdict.map(|v| v.score), Some(4));
    }

    #[tokio::test]
    async fn zero_rounds_reviews_but_never_revises() {
        let provider = Scripted::new(&[&verdict(1)]);
        let r = reviewer(provider.clone(), 3, 0).await;
        let result = r.review("q?", "Answer.", &[]).await.unwrap();
        assert_eq!(result.outcome, ReviewOutcome::KeptOriginal);
        assert!(!result.was_revised());
        assert_eq!(provider.calls(), 1);
    }

    #[tokio::test]
    async fn an_unparseable_verdict_changes_nothing() {
        let provider = Scripted::new(&["I think it is fine."]);
        let r = reviewer(provider, 3, 2).await;
        let result = r.review("q?", "Answer.", &[]).await.unwrap();
        assert_eq!(result.outcome, ReviewOutcome::Unjudged);
        assert_eq!(result.final_answer, "Answer.");
    }

    #[tokio::test]
    async fn a_passing_score_is_judged_by_the_threshold_alone() {
        // `pass: true` with a score under the mark is not a pass.
        let provider = Scripted::new(&[
            r#"{"pass": true, "score": 2, "critique": "thin"}"#,
            "Better.",
            &verdict(4),
        ]);
        let r = reviewer(provider, 3, 1).await;
        let result = r.review("q?", "Thin.", &[]).await.unwrap();
        assert_eq!(result.outcome, ReviewOutcome::Revised);
    }

    #[tokio::test]
    async fn the_threshold_is_read_on_every_review() {
        let provider = Scripted::new(&[&verdict(3), &verdict(3), "Better.", &verdict(5)]);
        let settings = Arc::new(MockSettingsRepository::new());
        settings
            .set_key("review_pass_threshold", "3".into())
            .await
            .unwrap();
        settings
            .set_key("review_max_rounds", "1".into())
            .await
            .unwrap();
        let live: Arc<dyn LlmProvider> = provider;
        let r = LlmAnswerReviewer::new(Arc::new(RwLock::new(Some(live))), settings.clone());
        assert_eq!(
            r.review("q?", "A.", &[]).await.unwrap().outcome,
            ReviewOutcome::Passed
        );

        settings
            .set_key("review_pass_threshold", "4".into())
            .await
            .unwrap();
        assert_eq!(
            r.review("q?", "A.", &[]).await.unwrap().outcome,
            ReviewOutcome::Revised
        );
    }
}
