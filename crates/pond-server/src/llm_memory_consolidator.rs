//! LLM memory consolidator: one compact merge/prune prompt, small enough for 3B models.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::memory::{MemoryFragment, MemorySegment};
use pond_core::user_data::ports::memory_consolidator::{
    ChallengeSeverity, ChallengeVerdict, ConsolidationAction, ConsolidationEvent,
    ConsolidationProposal, ConsolidationRunResult, JudgeDecision, MemoryConsolidator,
    TrialExchange,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

const CONSOLIDATION_PROMPT: &str = "\
Using the memories below, which the Pond keeps about one household member, \
decide what to change so the store holds only facts that help the assistant \
serve that member.

Reply with JSON only: an array of action objects, most important first. Each \
object is exactly one of these:
{\"action\":\"merge\",\"ids\":[\"id1\",\"id2\"],\"merged\":\"...\",\"segment\":\"...\",\"importance\":0.0,\"reason\":\"...\"}
{\"action\":\"prune\",\"id\":\"id1\",\"reason\":\"...\"}
- action: \"merge\" folds memories that say the same thing into one; \"prune\" \
archives a memory that does not help.
- ids, id: memory ids copied exactly from the square brackets.
- merged: the one short sentence that replaces them, at most 40 words, never \
empty. Two memories about the same person or subject that say different \
things are not duplicates: leave them as they are.
- segment: identity, preference, correction, relationship, routine, project, \
knowledge or context.
- importance: 0 to 1, how much the fact matters to serving the member.
- reason: one line naming what is duplicated or why it does not help.

PRUNE: empty content, general knowledge (weather, Wikipedia facts), info already \
available in the system prompt (assistant name, timezone, personality), \
speculative or unverified claims, vague statements, conversational filler, \
anything the assistant said rather than a fact about the member.
PRUNE a trait built from a single question: asking about something once is \
curiosity, not who the member is. Any memory that says the member inquires \
about, tracks, researches or is working through a film, show, book, game or \
public figure came from one question. For example \"<member> inquires about the \
new thriller Primetime\", \"<member> tracks the biographies of public figures \
such as Willem Dafoe\", \"<member> is working through an assignment named \
Avengers Doomsday\" (a film they asked about). PRUNE nonsense one exchange \
produced, such as \"<member> repeatedly offers apologies to <member>\".
Go through every memory in turn and decide: keep, merge or prune. Keep \
distinct facts separate: never join them into a summary or a paragraph. \
Never prune a correction.
If nothing should change, reply [].";

/// Parse a model-written segment label; `None` keeps the row's current segment.
/// Accepts every stored segment, unlike the batch extractor's closed five-value parser.
pub fn parse_segment_str(s: &str) -> Option<MemorySegment> {
    match s.to_lowercase().as_str() {
        "identity" => Some(MemorySegment::Identity),
        "preference" => Some(MemorySegment::Preference),
        "correction" => Some(MemorySegment::Correction),
        "relationship" => Some(MemorySegment::Relationship),
        "routine" => Some(MemorySegment::Routine),
        "project" => Some(MemorySegment::Project),
        "knowledge" => Some(MemorySegment::Knowledge),
        "context" => Some(MemorySegment::Context),
        _ => None,
    }
}

pub struct LlmMemoryConsolidator {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
}

impl LlmMemoryConsolidator {
    pub fn new(live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>) -> Self {
        Self { live_provider }
    }
}

#[async_trait]
impl MemoryConsolidator for LlmMemoryConsolidator {
    async fn consolidate(&self, memories: &[MemoryFragment]) -> Result<Vec<ConsolidationAction>> {
        let provider = {
            let guard = self.live_provider.read().await;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| anyhow!("no LLM provider"))?
        };

        // Max 20 memories, to stay within context.
        let batch: Vec<_> = memories.iter().take(20).collect();
        let formatted = batch
            .iter()
            .map(|m| {
                let seg = m
                    .segment
                    .as_ref()
                    .map(|s| format!("{:?}", s).to_lowercase())
                    .unwrap_or_else(|| "unknown".to_string());
                let imp = m.importance.unwrap_or(0.5);
                format!("[{}] ({}, {:.1}) {}", m.id, seg, imp, m.content)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let messages = vec![ChatMessage::user(format!("Memories:\n{formatted}"))];
        let response = provider.complete(CONSOLIDATION_PROMPT, messages).await?;

        parse_consolidation_response(&response.content)
    }
}

impl LlmMemoryConsolidator {
    /// The actions with the model's reason for each, which the run's details record.
    async fn propose(
        &self,
        memories: &[MemoryFragment],
    ) -> Result<Vec<(ConsolidationAction, String)>> {
        let provider = {
            let guard = self.live_provider.read().await;
            guard
                .as_ref()
                .cloned()
                .ok_or_else(|| anyhow!("no LLM provider"))?
        };
        let formatted = memories
            .iter()
            .take(20)
            .map(|m| {
                let seg = m
                    .segment
                    .as_ref()
                    .map(|s| format!("{:?}", s).to_lowercase())
                    .unwrap_or_else(|| "unknown".to_string());
                format!(
                    "[{}] ({}, {:.1}) {}",
                    m.id,
                    seg,
                    m.importance.unwrap_or(0.5),
                    m.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let messages = vec![ChatMessage::user(format!("Memories:\n{formatted}"))];
        let response = provider.complete(CONSOLIDATION_PROMPT, messages).await?;
        Ok(parse_reasoned_actions(&response.content))
    }
}

/// The `"single"` `memory_consolidation_mode`: one LLM call, three-stage result shape.
/// Every proposal is recorded as accepted, with a rationale saying no review happened.
pub async fn run_single_pass(
    provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
    memories: &[MemoryFragment],
    cancel: CancellationToken,
    event_tx: Option<tokio::sync::mpsc::Sender<ConsolidationEvent>>,
) -> Result<ConsolidationRunResult> {
    let start = std::time::Instant::now();

    async fn emit(
        tx: &Option<tokio::sync::mpsc::Sender<ConsolidationEvent>>,
        e: ConsolidationEvent,
    ) {
        if let Some(tx) = tx {
            let _ = tx.send(e).await;
        }
    }

    let empty = |start: std::time::Instant| ConsolidationRunResult {
        exchanges: vec![],
        accepted_count: 0,
        rejected_count: 0,
        duration_ms: start.elapsed().as_millis() as u64,
    };

    if cancel.is_cancelled() {
        emit(&event_tx, ConsolidationEvent::Cancelled).await;
        return Ok(empty(start));
    }

    emit(
        &event_tx,
        ConsolidationEvent::Started {
            memory_count: memories.len(),
        },
    )
    .await;

    let actions = LlmMemoryConsolidator::new(provider)
        .propose(memories)
        .await?;

    // Cancelled mid-call: drop the proposals rather than apply interrupted work.
    if cancel.is_cancelled() {
        emit(&event_tx, ConsolidationEvent::Cancelled).await;
        return Ok(empty(start));
    }

    let exchanges: Vec<TrialExchange> = actions
        .into_iter()
        .map(|(action, reason)| TrialExchange {
            proposal: ConsolidationProposal {
                action,
                rationale: if reason.is_empty() {
                    "single-pass proposal".to_string()
                } else {
                    reason
                },
            },
            challenge: ChallengeVerdict {
                agreed: true,
                rationale: "single-pass mode: no adversarial review was run".to_string(),
                severity: ChallengeSeverity::Low,
            },
            judgment: JudgeDecision {
                accepted: true,
                rationale: "accepted without review (single-pass mode)".to_string(),
            },
        })
        .collect();

    emit(
        &event_tx,
        ConsolidationEvent::ProposerDone {
            proposals: exchanges.iter().map(|e| e.proposal.clone()).collect(),
        },
    )
    .await;

    let accepted_count = exchanges.len();
    let result = ConsolidationRunResult {
        exchanges,
        accepted_count,
        rejected_count: 0,
        duration_ms: start.elapsed().as_millis() as u64,
    };

    emit(
        &event_tx,
        ConsolidationEvent::Completed {
            result: result.clone(),
        },
    )
    .await;

    Ok(result)
}

fn parse_consolidation_response(raw: &str) -> Result<Vec<ConsolidationAction>> {
    Ok(parse_reasoned_actions(raw)
        .into_iter()
        .map(|(action, _)| action)
        .collect())
}

/// Each action the reply proposes, with its `reason` (empty when the model gave none).
fn parse_reasoned_actions(raw: &str) -> Vec<(ConsolidationAction, String)> {
    let text = raw.trim();

    let cleaned = crate::conversation_extractor::strip_thinking(text);

    let json_str = if cleaned.starts_with('[') {
        cleaned.to_string()
    } else if let Some(start) = cleaned.find('[') {
        if let Some(end) = cleaned.rfind(']') {
            cleaned[start..=end].to_string()
        } else {
            return vec![];
        }
    } else {
        return vec![];
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
    let mut actions = Vec::new();

    for v in &arr {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let reason = v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        match action {
            "merge" => {
                let ids: Vec<String> = v
                    .get("ids")
                    .and_then(|a| a.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|i| i.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                let merged = v
                    .get("merged")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let segment = v
                    .get("segment")
                    .and_then(|s| s.as_str())
                    .and_then(|s| parse_segment_str(s))
                    .unwrap_or(MemorySegment::Knowledge);
                let importance = v
                    .get("importance")
                    .and_then(|i| i.as_f64())
                    .map(|i| (i as f32).clamp(0.0, 1.0))
                    .unwrap_or(0.5);

                if ids.len() >= 2 && !merged.trim().is_empty() {
                    actions.push((
                        ConsolidationAction::Merge {
                            source_ids: ids,
                            merged_content: merged,
                            segment,
                            importance,
                        },
                        reason,
                    ));
                }
            }
            "prune" => {
                if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                    actions.push((ConsolidationAction::Prune { id: id.to_string() }, reason));
                }
            }
            _ => {}
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_action_keeps_its_reason_and_a_blank_merge_is_dropped() {
        let raw = r#"[
            {"action":"prune","id":"m1","reason":"one question, not a trait"},
            {"action":"merge","ids":["m2","m3"],"merged":"","segment":"preference","reason":"dupes"},
            {"action":"merge","ids":["m4","m5"],"merged":"Jerry likes teal.","segment":"preference","importance":0.6,"reason":"same colour twice"}
        ]"#;
        let actions = parse_reasoned_actions(raw);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].1, "one question, not a trait");
        assert!(matches!(actions[1].0, ConsolidationAction::Merge { .. }));
        assert_eq!(actions[1].1, "same colour twice");
    }

    #[test]
    fn the_prompt_defines_every_field_and_prunes_single_question_traits() {
        for field in [
            "- action:",
            "- ids, id:",
            "- merged:",
            "- segment:",
            "- importance:",
            "- reason:",
        ] {
            assert!(CONSOLIDATION_PROMPT.contains(field), "{field}");
        }
        assert!(CONSOLIDATION_PROMPT.contains("PRUNE a trait built from a single question"));
        assert!(CONSOLIDATION_PROMPT.contains("routine"));
    }
}
