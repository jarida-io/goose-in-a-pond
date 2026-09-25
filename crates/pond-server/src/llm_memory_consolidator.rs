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
Review these memories and find problems. Output a JSON array of actions:
- To merge duplicates: {\"action\":\"merge\",\"ids\":[\"id1\",\"id2\"],\"merged\":\"combined fact\",\"segment\":\"...\",\"importance\":0.0-1.0}
- To prune: {\"action\":\"prune\",\"id\":\"id1\"}
- If nothing to change: []
MERGE truly duplicate facts. Keep distinct facts separate.
PRUNE: empty content, general knowledge (weather, Wikipedia facts), info already available in the system prompt (assistant name, timezone, personality), speculative/unverified claims, vague statements, conversational filler, anything the assistant said rather than a user fact.
Only keep memories personally relevant to the user that would be lost if forgotten.
Output ONLY the JSON array.";

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
        .consolidate(memories)
        .await?;

    // Cancelled mid-call: drop the proposals rather than apply interrupted work.
    if cancel.is_cancelled() {
        emit(&event_tx, ConsolidationEvent::Cancelled).await;
        return Ok(empty(start));
    }

    let exchanges: Vec<TrialExchange> = actions
        .into_iter()
        .map(|action| TrialExchange {
            proposal: ConsolidationProposal {
                action,
                rationale: "single-pass proposal".to_string(),
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
    let text = raw.trim();

    let cleaned = crate::llm_memory_extractor::strip_thinking(text);

    let json_str = if cleaned.starts_with('[') {
        cleaned.to_string()
    } else if let Some(start) = cleaned.find('[') {
        if let Some(end) = cleaned.rfind(']') {
            cleaned[start..=end].to_string()
        } else {
            return Ok(vec![]);
        }
    } else {
        return Ok(vec![]);
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
    let mut actions = Vec::new();

    for v in &arr {
        let action = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
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
                    .and_then(|s| crate::llm_memory_extractor::parse_segment_str(s))
                    .unwrap_or(MemorySegment::Knowledge);
                let importance = v
                    .get("importance")
                    .and_then(|i| i.as_f64())
                    .map(|i| (i as f32).clamp(0.0, 1.0))
                    .unwrap_or(0.5);

                if ids.len() >= 2 && !merged.is_empty() {
                    actions.push(ConsolidationAction::Merge {
                        source_ids: ids,
                        merged_content: merged,
                        segment,
                        importance,
                    });
                }
            }
            "prune" => {
                if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                    actions.push(ConsolidationAction::Prune { id: id.to_string() });
                }
            }
            _ => {}
        }
    }

    Ok(actions)
}
