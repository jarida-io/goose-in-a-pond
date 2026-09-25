//! MemoryConsolidator port — merges duplicate/contradicting memories.

use crate::user_data::domain::memory::{MemoryFragment, MemorySegment};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// An action proposed by the consolidation pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConsolidationAction {
    /// Merge multiple memories into one with new content.
    Merge {
        source_ids: Vec<String>,
        merged_content: String,
        segment: MemorySegment,
        importance: f32,
    },
    /// Remove a redundant or low-value memory.
    Prune { id: String },
    /// Split one memory into multiple distinct facts.
    Split {
        source_id: String,
        new_memories: Vec<SplitEntry>,
    },
    /// Recategorize a memory to a different segment with updated importance.
    Recategorize {
        id: String,
        new_segment: MemorySegment,
        new_importance: f32,
    },
}

/// A single entry produced by splitting a memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitEntry {
    pub content: String,
    pub segment: MemorySegment,
    pub importance: f32,
}

/// Driven port: consolidate memories by merging duplicates and pruning redundancy.
#[async_trait]
pub trait MemoryConsolidator: Send + Sync {
    /// Analyse a batch of memories and propose consolidation actions.
    async fn consolidate(&self, memories: &[MemoryFragment]) -> Result<Vec<ConsolidationAction>>;
}

// ── Adversarial consolidation types ─────────────────────────────────────────

/// Severity of an adversary's challenge to a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChallengeSeverity {
    Low,
    Medium,
    High,
}

/// A proposal from the Proposer agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationProposal {
    pub action: ConsolidationAction,
    pub rationale: String,
}

/// The Adversary's challenge to a proposal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChallengeVerdict {
    pub agreed: bool,
    pub rationale: String,
    pub severity: ChallengeSeverity,
}

/// The Judge's final decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeDecision {
    pub accepted: bool,
    pub rationale: String,
}

/// One complete Proposer -> Adversary -> Judge exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialExchange {
    pub proposal: ConsolidationProposal,
    pub challenge: ChallengeVerdict,
    pub judgment: JudgeDecision,
}

/// Result of a full consolidation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationRunResult {
    pub exchanges: Vec<TrialExchange>,
    pub accepted_count: usize,
    pub rejected_count: usize,
    pub duration_ms: u64,
}

/// Events emitted during consolidation for streaming to UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsolidationEvent {
    Started {
        memory_count: usize,
    },
    ProposerDone {
        proposals: Vec<ConsolidationProposal>,
    },
    AdversaryDone {
        challenges: Vec<ChallengeVerdict>,
    },
    JudgeDone {
        decisions: Vec<JudgeDecision>,
    },
    Applied {
        exchange: TrialExchange,
    },
    Completed {
        result: ConsolidationRunResult,
    },
    Error {
        message: String,
    },
    Cancelled,
}
