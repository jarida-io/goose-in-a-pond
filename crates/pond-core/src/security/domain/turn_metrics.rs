//! Per-turn telemetry (tokens, latency, tools, context use) for tuning the inference loop.

use serde::{Deserialize, Serialize};

/// Snapshot of a single chat turn's performance characteristics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnMetrics {
    pub session_id: String,
    /// 1-based turn index within the session.
    pub turn_number: u32,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,

    /// Reasoning tokens; `None` means unmeasured (pre-0008 rows, no ProviderStats), never zero.
    pub reasoning_tokens: Option<u32>,

    /// Re-steers after the turn went silent; `Some(0)` is ordinary, `None` predates the column.
    pub reengagements: Option<u32>,
    /// Time to first token in milliseconds (measures model load + prefill latency).
    pub ttft_ms: u64,
    /// Total wall-clock time for the turn in milliseconds.
    pub total_latency_ms: u64,
    pub tool_name: Option<String>,
    /// Wall-clock time for tool execution in milliseconds.
    pub tool_latency_ms: Option<u64>,
    pub tool_cache_hit: Option<bool>,
    /// Estimated percentage of the model's context window consumed (0.0–100.0).
    pub context_utilization_pct: f32,
    pub model_name: String,
    /// ISO 8601 timestamp when the turn completed.
    pub timestamp: String,
    /// Prefill (template + tokenize + prompt decode) summed over the turn; engine-reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill_ms: Option<u64>,
    /// Prompt tokens actually DECODED this turn; `Some(0)` means all came from the KV cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefilled_tokens: Option<u32>,
    /// Prompt tokens served from the KV cache; `Some(0)` on turn 2+ means the prefix drifted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_prefix_tokens: Option<u32>,
    /// Cold model-load time when a load happened during this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_load_ms: Option<u64>,
    /// Generation speed in tokens/second (decode phase).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decode_tok_per_sec: Option<f32>,
    /// Prompt-processing speed in tokens/second (prefill phase).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefill_tok_per_sec: Option<f32>,
    /// Engine-reported context window (n_ctx) for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit_tokens: Option<u32>,
    /// Number of inferences the agentic loop ran (1 + tool round-trips).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_count: Option<u32>,
}

/// Aggregated telemetry summary for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetrySummary {
    /// Average time to first token across all turns (milliseconds).
    pub avg_ttft_ms: f64,
    /// Average completion tokens per turn.
    pub avg_completion_tokens: f64,
    /// Total number of turns recorded.
    pub total_turns: u32,
    /// Average context window utilization across all turns (percentage).
    pub avg_context_utilization: f64,
}
