//! Memory consolidation as three LLM calls: Proposer, Adversary (guards against loss), Judge.

use anyhow::{anyhow, Result};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::memory::MemoryFragment;
use pond_core::user_data::ports::memory_consolidator::*;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

// ── Prompts ─────────────────────────────────────────────────────────────────

const PROPOSER_PROMPT: &str = "\
<identity>
You are the Memory Proposer. You analyze a user's memory store and propose \
changes that improve how the assistant can help the user.
</identity>

<instructions>
Think carefully about the thematic, semantic, and logical relationships between memories. \
Then output a JSON array of proposals.

<actions>
- merge: combine related or duplicate memories into one clear fact.
  {\"action\":\"merge\",\"ids\":[\"id1\",\"id2\"],\"merged\":\"combined text\",\"segment\":\"...\",\"importance\":0.0-1.0,\"reason\":\"...\"}
- split: break a compound memory into separate distinct facts, each properly categorized.
  {\"action\":\"split\",\"id\":\"SOURCE_ID\",\"new\":[{\"content\":\"fact 1\",\"segment\":\"...\",\"importance\":0.5},{\"content\":\"fact 2\",\"segment\":\"...\",\"importance\":0.6}],\"reason\":\"...\"}
- prune: remove memories that do not help the assistant serve the user.
  {\"action\":\"prune\",\"id\":\"ID\",\"reason\":\"...\"}
- supersede: a newer memory replaces an older conflicting one.
  {\"action\":\"supersede\",\"newer\":\"ID\",\"older\":\"ID\",\"reason\":\"...\"}
- recategorize: move a memory to the correct segment category.
  {\"action\":\"recategorize\",\"id\":\"ID\",\"segment\":\"identity|preference|correction|relationship|project|knowledge|context\",\"importance\":0.0-1.0,\"reason\":\"...\"}
</actions>

<rules>
- Evaluate each memory by asking: does this help the assistant serve the user better?
- MERGE memories that express the same fact in different words.
- SPLIT memories that contain multiple unrelated facts crammed into one entry.
- PRUNE: empty/vague content, general knowledge the assistant already has, \
info duplicated in the system prompt (assistant name, timezone, personality), \
speculative claims, conversational filler, assistant-provided facts (weather, search results).
- KEEP: identity facts, corrections, personal preferences, relationships, active projects.
- Organize by segment: ensure each memory is in its correct thematic category.
- Corrections must never be pruned or superseded by non-corrections.
</rules>
</instructions>

If nothing to change, return [].
Output ONLY the JSON array.";

const ADVERSARY_PROMPT: &str = "\
<identity>
You are the Memory Adversary. You protect user memories from harmful changes. \
Your role is to challenge proposals that risk losing valuable information.
</identity>

<instructions>
Think carefully about whether each proposal preserves the user's knowledge. \
Then output a JSON array with one challenge per proposal.

[{\"index\":0,\"agreed\":true|false,\"reason\":\"...\",\"severity\":\"low\"|\"medium\"|\"high\"}]

<rules>
- DENY (high severity) if a proposal loses unique personal information.
- DENY (high severity) if a correction is superseded by a non-correction.
- DENY (medium severity) if a merge combines thematically unrelated facts.
- DENY (medium severity) if a split loses important context from the original.
- AGREE (low severity) for pruning genuinely useless content (empty, filler, system-prompt duplicates).
- AGREE for merging true duplicates that say the same thing differently.
- You MUST include an entry for EVERY proposal, even if you agree.
- When unsure, err on the side of DENY — memory is cheap, information loss is not.
</rules>
</instructions>

Output ONLY the JSON array.";

const JUDGE_PROMPT: &str = "\
<identity>
You are the Memory Judge. You make final decisions on disputed memory proposals \
by weighing the Proposer's rationale against the Adversary's objections.
</identity>

<instructions>
Think carefully about the balance between keeping the memory store clean and \
preserving valuable user information. Then output a JSON array of decisions.

[{\"index\":0,\"accepted\":true|false,\"reason\":\"...\"}]

<rules>
- High severity objections: usually reject unless the benefit clearly outweighs the risk.
- Medium severity objections: weigh the specific tradeoff — accept if information is truly preserved.
- Low severity objections: usually accept the proposal.
- Never accept a proposal that would delete correction metadata.
- For splits: accept if the resulting parts are cleaner and better categorized.
- For merges: accept only if no distinct information is lost.
- The goal is a memory store that maximally helps the assistant serve the user.
</rules>
</instructions>

Output ONLY the JSON array.";

// ── ThreeStageConsolidator ───────────────────────────────────────────────────

pub struct ThreeStageConsolidator {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
}

impl ThreeStageConsolidator {
    pub fn new(live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>) -> Self {
        Self { live_provider }
    }

    /// Run all three stages, checking `cancel` between them; progress goes to `event_tx` if set.
    pub async fn run(
        &self,
        memories: &[MemoryFragment],
        cancel: CancellationToken,
        event_tx: Option<tokio::sync::mpsc::Sender<ConsolidationEvent>>,
    ) -> Result<ConsolidationRunResult> {
        let start = std::time::Instant::now();

        if cancel.is_cancelled() {
            send_event(&event_tx, ConsolidationEvent::Cancelled).await;
            return Ok(empty_result(start));
        }

        let formatted = format_memories(memories);

        send_event(
            &event_tx,
            ConsolidationEvent::Started {
                memory_count: memories.len(),
            },
        )
        .await;

        // ── Stage 1: Proposer ────────────────────────────────────────────────
        let proposals = self.run_proposer(&formatted, &cancel).await?;
        send_event(
            &event_tx,
            ConsolidationEvent::ProposerDone {
                proposals: proposals.clone(),
            },
        )
        .await;

        if proposals.is_empty() {
            let result = ConsolidationRunResult {
                exchanges: vec![],
                accepted_count: 0,
                rejected_count: 0,
                duration_ms: start.elapsed().as_millis() as u64,
            };
            send_event(
                &event_tx,
                ConsolidationEvent::Completed {
                    result: result.clone(),
                },
            )
            .await;
            return Ok(result);
        }

        if cancel.is_cancelled() {
            send_event(&event_tx, ConsolidationEvent::Cancelled).await;
            return Ok(empty_result(start));
        }

        // ── Stage 2: Adversary ───────────────────────────────────────────────
        let challenges = self.run_adversary(&formatted, &proposals, &cancel).await?;
        send_event(
            &event_tx,
            ConsolidationEvent::AdversaryDone {
                challenges: challenges.clone(),
            },
        )
        .await;

        if cancel.is_cancelled() {
            send_event(&event_tx, ConsolidationEvent::Cancelled).await;
            return Ok(empty_result(start));
        }

        // ── Stage 3: Judge ───────────────────────────────────────────────────
        let decisions = self.run_judge(&proposals, &challenges, &cancel).await?;
        send_event(
            &event_tx,
            ConsolidationEvent::JudgeDone {
                decisions: decisions.clone(),
            },
        )
        .await;

        // ── Build result ─────────────────────────────────────────────────────
        let exchanges = build_exchanges(proposals, challenges, decisions);
        let accepted_count = exchanges.iter().filter(|e| e.judgment.accepted).count();
        let rejected_count = exchanges.len() - accepted_count;

        let result = ConsolidationRunResult {
            exchanges,
            accepted_count,
            rejected_count,
            duration_ms: start.elapsed().as_millis() as u64,
        };

        send_event(
            &event_tx,
            ConsolidationEvent::Completed {
                result: result.clone(),
            },
        )
        .await;

        Ok(result)
    }

    async fn get_provider(&self) -> Result<Arc<dyn LlmProvider>> {
        let guard = self.live_provider.read().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow!("no LLM provider available for consolidation"))
    }

    async fn run_proposer(
        &self,
        formatted_memories: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<ConsolidationProposal>> {
        if cancel.is_cancelled() {
            return Ok(vec![]);
        }
        let provider = self.get_provider().await?;
        let messages = vec![ChatMessage::user(format!(
            "Memories:\n{formatted_memories}"
        ))];
        let response = provider.complete(PROPOSER_PROMPT, messages).await?;
        let cleaned = crate::llm_memory_extractor::strip_thinking(&response.content);
        Ok(parse_proposals(&cleaned))
    }

    async fn run_adversary(
        &self,
        formatted_memories: &str,
        proposals: &[ConsolidationProposal],
        cancel: &CancellationToken,
    ) -> Result<Vec<ChallengeVerdict>> {
        if cancel.is_cancelled() {
            return Ok(vec![]);
        }
        let provider = self.get_provider().await?;

        let proposals_json = serde_json::to_string_pretty(proposals).unwrap_or_default();
        let messages = vec![ChatMessage::user(format!(
            "Memories:\n{formatted_memories}\n\nProposals:\n{proposals_json}"
        ))];
        let response = provider.complete(ADVERSARY_PROMPT, messages).await?;
        let cleaned = crate::llm_memory_extractor::strip_thinking(&response.content);
        Ok(parse_challenges(&cleaned))
    }

    async fn run_judge(
        &self,
        proposals: &[ConsolidationProposal],
        challenges: &[ChallengeVerdict],
        cancel: &CancellationToken,
    ) -> Result<Vec<JudgeDecision>> {
        if cancel.is_cancelled() {
            return Ok(vec![]);
        }
        let provider = self.get_provider().await?;

        let proposals_json = serde_json::to_string_pretty(proposals).unwrap_or_default();
        let challenges_json = serde_json::to_string_pretty(challenges).unwrap_or_default();
        let messages = vec![ChatMessage::user(format!(
            "Proposals:\n{proposals_json}\n\nChallenges:\n{challenges_json}"
        ))];
        let response = provider.complete(JUDGE_PROMPT, messages).await?;
        let cleaned = crate::llm_memory_extractor::strip_thinking(&response.content);
        Ok(parse_decisions(&cleaned))
    }
}

// ── Formatting ───────────────────────────────────────────────────────────────

fn format_memories(memories: &[MemoryFragment]) -> String {
    memories
        .iter()
        .map(|m| {
            let seg = m
                .segment
                .as_ref()
                .map(|s| format!("{:?}", s).to_lowercase())
                .unwrap_or_else(|| "unknown".to_string());
            let imp = m.importance.unwrap_or(0.5);
            let mut line = format!("[{}] ({}, {:.1}) {}", m.id, seg, imp, m.content);
            if let Some(ref corrects) = m.corrects {
                line.push_str(&format!(" [corrects: {corrects}]"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// ── JSON parsing helpers ─────────────────────────────────────────────────────

/// Extract a JSON array from potentially noisy LLM output.
fn extract_json_array(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.starts_with('[') {
        if let Some(end) = trimmed.rfind(']') {
            return Some(trimmed[..=end].to_string());
        }
    }
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            if end > start {
                return Some(trimmed[start..=end].to_string());
            }
        }
    }
    None
}

fn parse_proposals(raw: &str) -> Vec<ConsolidationProposal> {
    let json_str = match extract_json_array(raw) {
        Some(s) => s,
        None => return vec![],
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
    let mut proposals = Vec::new();

    for v in &arr {
        let action_str = v.get("action").and_then(|a| a.as_str()).unwrap_or("");
        let reason = v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();

        let action = match action_str {
            "merge" | "supersede" => {
                let ids: Vec<String> = if action_str == "supersede" {
                    // supersede format: {newer, older}
                    let newer = v.get("newer").and_then(|n| n.as_str()).unwrap_or("");
                    let older = v.get("older").and_then(|o| o.as_str()).unwrap_or("");
                    if newer.is_empty() || older.is_empty() {
                        continue;
                    }
                    vec![newer.to_string(), older.to_string()]
                } else {
                    v.get("ids")
                        .and_then(|a| a.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|i| i.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default()
                };

                let merged = v
                    .get("merged")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string();
                let segment = v
                    .get("segment")
                    .and_then(|s| s.as_str())
                    .and_then(|s| crate::llm_memory_extractor::parse_segment_str(s))
                    .unwrap_or(pond_core::user_data::domain::memory::MemorySegment::Knowledge);
                let importance = v
                    .get("importance")
                    .and_then(|i| i.as_f64())
                    .map(|i| (i as f32).clamp(0.0, 1.0))
                    .unwrap_or(0.5);

                if ids.len() < 2 {
                    continue;
                }

                ConsolidationAction::Merge {
                    source_ids: ids,
                    merged_content: merged,
                    segment,
                    importance,
                }
            }
            "prune" => {
                let id = v
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                ConsolidationAction::Prune { id }
            }
            "recategorize" => {
                let id = v
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if id.is_empty() {
                    continue;
                }
                let new_segment = v
                    .get("segment")
                    .and_then(|s| s.as_str())
                    .and_then(|s| crate::llm_memory_extractor::parse_segment_str(s))
                    .unwrap_or(pond_core::user_data::domain::memory::MemorySegment::Knowledge);
                let new_importance = v
                    .get("importance")
                    .and_then(|i| i.as_f64())
                    .map(|i| (i as f32).clamp(0.0, 1.0))
                    .unwrap_or(0.5);
                ConsolidationAction::Recategorize {
                    id,
                    new_segment,
                    new_importance,
                }
            }
            "split" => {
                let source_id = v
                    .get("id")
                    .and_then(|i| i.as_str())
                    .unwrap_or("")
                    .to_string();
                if source_id.is_empty() {
                    continue;
                }
                let new_entries = v
                    .get("new")
                    .and_then(|a| a.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|entry| {
                                let content = entry.get("content")?.as_str()?.to_string();
                                if content.trim().is_empty() {
                                    return None;
                                }
                                let segment = entry
                                    .get("segment")
                                    .and_then(|s| s.as_str())
                                    .and_then(|s| crate::llm_memory_extractor::parse_segment_str(s))
                                    .unwrap_or(pond_core::user_data::domain::memory::MemorySegment::Knowledge);
                                let importance = entry
                                    .get("importance")
                                    .and_then(|i| i.as_f64())
                                    .map(|i| (i as f32).clamp(0.0, 1.0))
                                    .unwrap_or(0.5);
                                Some(pond_core::user_data::ports::memory_consolidator::SplitEntry {
                                    content,
                                    segment,
                                    importance,
                                })
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if new_entries.len() < 2 {
                    continue; // a split must produce at least 2 entries
                }
                ConsolidationAction::Split {
                    source_id,
                    new_memories: new_entries,
                }
            }
            _ => continue,
        };

        proposals.push(ConsolidationProposal {
            action,
            rationale: reason,
        });
    }

    proposals
}

fn parse_challenges(raw: &str) -> Vec<ChallengeVerdict> {
    let json_str = match extract_json_array(raw) {
        Some(s) => s,
        None => return vec![],
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
    let mut challenges = Vec::new();

    for v in &arr {
        let agreed = v.get("agreed").and_then(|a| a.as_bool()).unwrap_or(false);
        let reason = v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();
        let severity = match v
            .get("severity")
            .and_then(|s| s.as_str())
            .unwrap_or("medium")
        {
            "low" => ChallengeSeverity::Low,
            "high" => ChallengeSeverity::High,
            _ => ChallengeSeverity::Medium,
        };

        challenges.push(ChallengeVerdict {
            agreed,
            rationale: reason,
            severity,
        });
    }

    challenges
}

fn parse_decisions(raw: &str) -> Vec<JudgeDecision> {
    let json_str = match extract_json_array(raw) {
        Some(s) => s,
        None => return vec![],
    };

    let arr: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap_or_default();
    let mut decisions = Vec::new();

    for v in &arr {
        let accepted = v.get("accepted").and_then(|a| a.as_bool()).unwrap_or(false);
        let reason = v
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();

        decisions.push(JudgeDecision {
            accepted,
            rationale: reason,
        });
    }

    decisions
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Send a consolidation event to the channel, ignoring send failures.
async fn send_event(
    tx: &Option<tokio::sync::mpsc::Sender<ConsolidationEvent>>,
    event: ConsolidationEvent,
) {
    if let Some(tx) = tx {
        let _ = tx.send(event).await;
    }
}

/// Zip stages by index. Missing entries fail safe: a High challenge, a rejected decision.
fn build_exchanges(
    proposals: Vec<ConsolidationProposal>,
    challenges: Vec<ChallengeVerdict>,
    decisions: Vec<JudgeDecision>,
) -> Vec<TrialExchange> {
    proposals
        .into_iter()
        .enumerate()
        .map(|(i, proposal)| {
            let challenge = challenges.get(i).cloned().unwrap_or(ChallengeVerdict {
                agreed: false,
                rationale: "no adversary response — defaulting to reject".to_string(),
                severity: ChallengeSeverity::High,
            });
            let judgment = decisions.get(i).cloned().unwrap_or(JudgeDecision {
                accepted: false,
                rationale: "no judge response — defaulting to reject".to_string(),
            });
            TrialExchange {
                proposal,
                challenge,
                judgment,
            }
        })
        .collect()
}

/// Return an empty result (used when cancelled or nothing to do).
fn empty_result(start: std::time::Instant) -> ConsolidationRunResult {
    ConsolidationRunResult {
        exchanges: vec![],
        accepted_count: 0,
        rejected_count: 0,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::memory::{MemorySegment, MemoryTier};

    fn make_memory(id: &str, content: &str, segment: MemorySegment) -> MemoryFragment {
        MemoryFragment {
            id: id.to_string(),
            profile_id: None,
            session_id: None,
            content: content.to_string(),
            embedding: None,
            source: "test".to_string(),
            tags: vec![],
            created_at: chrono::Utc::now(),
            segment: Some(segment),
            importance: Some(0.7),
            tier: Some(MemoryTier::Long),
            decay_rate: None,
            access_count: 0,
            last_accessed_at: None,
            lifecycle: None,
            superseded_by: None,
            corrects: None,
        }
    }

    fn make_correction_memory(id: &str, content: &str, corrects: &str) -> MemoryFragment {
        let mut m = make_memory(id, content, MemorySegment::Correction);
        m.corrects = Some(corrects.to_string());
        m
    }

    #[test]
    fn format_memories_includes_corrects_metadata() {
        let m = make_correction_memory("c1", "User's name is Jerry, not John", "name was John");
        let formatted = format_memories(&[m]);
        assert!(formatted.contains("[corrects: name was John]"));
        assert!(formatted.contains("(correction, 0.7)"));
    }

    #[test]
    fn parse_proposals_merge() {
        let json = r#"[
            {"action":"merge","ids":["a1","a2"],"merged":"combined fact","segment":"knowledge","importance":0.8,"reason":"duplicates"}
        ]"#;
        let proposals = parse_proposals(json);
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].rationale, "duplicates");
        match &proposals[0].action {
            ConsolidationAction::Merge {
                source_ids,
                merged_content,
                ..
            } => {
                assert_eq!(source_ids, &["a1", "a2"]);
                assert_eq!(merged_content, "combined fact");
            }
            other => panic!("expected Merge, got {other:?}"),
        }
    }

    #[test]
    fn parse_proposals_prune() {
        let json = r#"[{"action":"prune","id":"x1","reason":"expired"}]"#;
        let proposals = parse_proposals(json);
        assert_eq!(proposals.len(), 1);
        match &proposals[0].action {
            ConsolidationAction::Prune { id } => assert_eq!(id, "x1"),
            other => panic!("expected Prune, got {other:?}"),
        }
    }

    #[test]
    fn parse_proposals_supersede() {
        let json = r#"[{"action":"supersede","newer":"n1","older":"o1","reason":"updated info"}]"#;
        let proposals = parse_proposals(json);
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].rationale, "updated info");
        match &proposals[0].action {
            ConsolidationAction::Merge { source_ids, .. } => {
                assert_eq!(source_ids, &["n1", "o1"]);
            }
            other => panic!("expected Merge from supersede, got {other:?}"),
        }
    }

    #[test]
    fn parse_proposals_empty_array() {
        assert!(parse_proposals("[]").is_empty());
    }

    #[test]
    fn parse_proposals_with_thinking_tags() {
        let raw = "<think>let me think about this</think>[{\"action\":\"prune\",\"id\":\"x\",\"reason\":\"old\"}]";
        let cleaned = crate::llm_memory_extractor::strip_thinking(raw);
        let proposals = parse_proposals(&cleaned);
        assert_eq!(proposals.len(), 1);
    }

    #[test]
    fn parse_challenges_basic() {
        let json = r#"[
            {"index":0,"agreed":true,"reason":"safe merge","severity":"low"},
            {"index":1,"agreed":false,"reason":"loses identity info","severity":"high"}
        ]"#;
        let challenges = parse_challenges(json);
        assert_eq!(challenges.len(), 2);
        assert!(challenges[0].agreed);
        assert!(!challenges[1].agreed);
        assert!(matches!(challenges[1].severity, ChallengeSeverity::High));
    }

    #[test]
    fn parse_decisions_basic() {
        let json = r#"[
            {"index":0,"accepted":true,"reason":"low risk"},
            {"index":1,"accepted":false,"reason":"information loss"}
        ]"#;
        let decisions = parse_decisions(json);
        assert_eq!(decisions.len(), 2);
        assert!(decisions[0].accepted);
        assert!(!decisions[1].accepted);
    }

    #[test]
    fn build_exchanges_zips_correctly() {
        let proposals = vec![
            ConsolidationProposal {
                action: ConsolidationAction::Prune {
                    id: "a".to_string(),
                },
                rationale: "old".to_string(),
            },
            ConsolidationProposal {
                action: ConsolidationAction::Prune {
                    id: "b".to_string(),
                },
                rationale: "stale".to_string(),
            },
        ];
        let challenges = vec![ChallengeVerdict {
            agreed: true,
            rationale: "ok".to_string(),
            severity: ChallengeSeverity::Low,
        }];
        // Only 1 challenge for 2 proposals — second should get default rejection
        let decisions = vec![
            JudgeDecision {
                accepted: true,
                rationale: "fine".to_string(),
            },
            JudgeDecision {
                accepted: false,
                rationale: "no adversary input".to_string(),
            },
        ];

        let exchanges = build_exchanges(proposals, challenges, decisions);
        assert_eq!(exchanges.len(), 2);
        assert!(exchanges[0].judgment.accepted);
        assert!(!exchanges[1].judgment.accepted);
        assert!(matches!(
            exchanges[1].challenge.severity,
            ChallengeSeverity::High
        ));
    }

    #[test]
    fn extract_json_array_from_noisy_output() {
        let raw = "Here is the result:\n[{\"x\":1}]\nDone.";
        let arr = extract_json_array(raw);
        assert!(arr.is_some());
        assert_eq!(arr.unwrap(), "[{\"x\":1}]");
    }

    #[test]
    fn extract_json_array_clean_input() {
        let raw = "[{\"a\":1},{\"b\":2}]";
        assert_eq!(extract_json_array(raw).unwrap(), raw);
    }

    #[test]
    fn extract_json_array_no_array() {
        assert!(extract_json_array("no json here").is_none());
    }
}
