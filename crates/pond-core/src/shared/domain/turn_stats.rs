//! Per-turn inference stats, aggregated over every inference of one user turn.
//! Rates are derived here so every consumer (SSE, event log, voice console, UI) agrees.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnStats {
    /// Engine-level time to first generated token of the FIRST inference.
    pub ttft_ms: Option<u64>,
    /// Cold model-load time, when a load happened during this turn.
    pub model_load_ms: Option<u64>,
    /// Prefill time (template + tokenize + prompt decode), summed over the turn's inferences.
    pub prefill_ms: Option<u64>,
    /// Generation (decode) time, summed across the turn's inferences.
    pub decode_ms: Option<u64>,
    /// Prompt size of the FINAL inference — the turn's real context load.
    pub prompt_tokens: u32,
    /// Prompt tokens actually DECODED, summed over the turn's inferences; the prefill-rate basis.
    /// `0` with non-zero `prefill_ms` means fully cached (time spent templating and tokenizing).
    #[serde(default)]
    pub prefilled_tokens: u32,
    /// Prompt tokens served from a retained KV cache instead of decoded, summed over inferences.
    /// `None`: not reported (HTTP providers); `Some(0)` on turn 2+ means the prefix drifted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reused_prefix_tokens: Option<u32>,
    /// Generated tokens, summed across the turn's inferences.
    pub completion_tokens: u32,
    /// Reasoning tokens the user never sees, counted by GIAP since no provider reports them.
    /// Never subtracted from `completion_tokens` or used in `finalize_rates`; `None` = uncounted.
    pub reasoning_tokens: Option<u32>,

    /// Attempts beyond the first this turn needed, e.g. 1 after an `EMPTY_TURN_STEER` re-prompt.
    /// Defaulted: it rides `AgentStreamEvent::Done` as NDJSON to a voice child of any version.
    #[serde(default)]
    pub reengagements: u32,
    pub prefill_tok_per_sec: Option<f32>,
    pub decode_tok_per_sec: Option<f32>,
    /// Final inference's prompt tokens (as `prompt_tokens`), for the UI's `used/limit`.
    pub context_used_tokens: Option<u32>,
    /// The engine's actual context window (n_ctx) for this turn.
    pub context_limit_tokens: Option<u32>,
    /// Number of inferences the agentic loop ran (1 + tool round-trips).
    pub inference_count: u32,
    /// Speculative-decoding acceptance rate, when a draft model was active.
    pub draft_accept_rate: Option<f32>,
}

impl TurnStats {
    /// Derive the rate fields from the raw timing/token fields.
    pub fn finalize_rates(&mut self) {
        // Tokens actually decoded over the time spent decoding them, not `prompt_tokens`.
        if let (Some(prefill_ms), decoded) = (self.prefill_ms, self.prefilled_tokens) {
            if prefill_ms > 0 && decoded > 0 {
                self.prefill_tok_per_sec = Some(decoded as f32 * 1000.0 / prefill_ms as f32);
            }
        }
        if let Some(decode_ms) = self.decode_ms {
            if decode_ms > 0 && self.completion_tokens > 0 {
                self.decode_tok_per_sec =
                    Some(self.completion_tokens as f32 * 1000.0 / decode_ms as f32);
            }
        }
    }

    /// Percent of the context window the final prompt fills, when both sides are known.
    pub fn context_pct(&self) -> Option<f32> {
        match (self.context_used_tokens, self.context_limit_tokens) {
            (Some(used), Some(limit)) if limit > 0 => Some(used as f32 * 100.0 / limit as f32),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_rates_computes_tok_per_sec() {
        let mut s = TurnStats {
            prefill_ms: Some(2000),
            decode_ms: Some(4000),
            prompt_tokens: 1000,
            prefilled_tokens: 1000,
            completion_tokens: 88,
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, Some(500.0));
        assert_eq!(s.decode_tok_per_sec, Some(22.0));
    }

    #[test]
    fn a_reused_prefix_does_not_inflate_the_prefill_rate() {
        let mut s = TurnStats {
            prefill_ms: Some(2000),
            prompt_tokens: 7636,
            prefilled_tokens: 55,
            reused_prefix_tokens: Some(7581),
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, Some(27.5));
    }

    #[test]
    fn several_inferences_sum_what_each_actually_decoded() {
        let mut s = TurnStats {
            prefill_ms: Some(4000),
            // Final inference's prompt, which is NOT the work done.
            prompt_tokens: 7000,
            // 6,800 decoded cold, then 100 more on each of two tool rounds.
            prefilled_tokens: 7000,
            inference_count: 3,
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, Some(1750.0));
    }

    #[test]
    fn a_fully_cached_prompt_reports_no_prefill_rate() {
        let mut s = TurnStats {
            prefill_ms: Some(300),
            prompt_tokens: 7636,
            prefilled_tokens: 0,
            reused_prefix_tokens: Some(7636),
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, None);
    }

    #[test]
    fn finalize_rates_skips_zero_denominators() {
        let mut s = TurnStats {
            prefill_ms: Some(0),
            decode_ms: None,
            prompt_tokens: 100,
            completion_tokens: 5,
            ..Default::default()
        };
        s.finalize_rates();
        assert_eq!(s.prefill_tok_per_sec, None);
        assert_eq!(s.decode_tok_per_sec, None);
    }

    #[test]
    fn context_pct_needs_both_sides() {
        let s = TurnStats {
            context_used_tokens: Some(1536),
            context_limit_tokens: Some(3072),
            ..Default::default()
        };
        assert_eq!(s.context_pct(), Some(50.0));
        let empty = TurnStats::default();
        assert_eq!(empty.context_pct(), None);
    }

    /// The provider's count may already include reasoning; subtracting it would corrupt that.
    #[test]
    fn reasoning_does_not_move_the_completion_count_or_the_decode_rate() {
        let base = TurnStats {
            decode_ms: Some(4000),
            completion_tokens: 88,
            ..Default::default()
        };
        let mut without = base.clone();
        let mut with = TurnStats {
            reasoning_tokens: Some(500),
            ..base
        };
        without.finalize_rates();
        with.finalize_rates();
        assert_eq!(with.completion_tokens, without.completion_tokens);
        assert_eq!(with.decode_tok_per_sec, without.decode_tok_per_sec);
        assert_eq!(with.reasoning_tokens, Some(500));
    }

    /// `output_reserve_tokens` is derived from this, so unmeasured must not read as zero.
    #[test]
    fn unmeasured_reasoning_is_not_zero_reasoning() {
        let unmeasured: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":1,"completion_tokens":1,"inference_count":1}"#,
        )
        .unwrap();
        assert_eq!(unmeasured.reasoning_tokens, None);
        let measured: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":1,"completion_tokens":1,"inference_count":1,"reasoning_tokens":0}"#,
        )
        .unwrap();
        assert_eq!(measured.reasoning_tokens, Some(0));
    }

    #[test]
    fn a_payload_without_the_re_engagement_count_still_parses() {
        let old: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":2,"inference_count":1}"#,
        )
        .expect("an event from a binary that predates the field must still deserialize");
        assert_eq!(old.reengagements, 0);

        let ordinary: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":2,"inference_count":1,"reengagements":0}"#,
        )
        .unwrap();
        assert_eq!(ordinary.reengagements, 0);

        let steered: TurnStats = serde_json::from_str(
            r#"{"prompt_tokens":10,"completion_tokens":2,"inference_count":1,"reengagements":2}"#,
        )
        .unwrap();
        assert_eq!(
            steered.reengagements, 2,
            "the default is swallowing a value that was actually sent"
        );
    }

    #[test]
    fn deserializes_without_optional_fields() {
        let json = r#"{"prompt_tokens": 10, "completion_tokens": 2, "inference_count": 1}"#;
        let s: TurnStats = serde_json::from_str(json).unwrap();
        assert_eq!(s.prompt_tokens, 10);
        assert_eq!(s.ttft_ms, None);
    }
}
