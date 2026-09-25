//! KV-cache prefix reuse for multi-turn conversations.
//!
//! On small-context platforms (Jetson ≤4096), re-prefilling the entire prompt
//! every turn wastes 10-20 seconds processing static content (system prompt +
//! tool declarations). This module enables prefix matching: compare what's
//! already in the KV cache with the new prompt, and only decode the delta.
//!
//! ## Architecture
//!
//! ```text
//! Turn 1: [system_prompt + tools + user_msg_1]  → full prefill (cold start)
//! Turn 2: [system_prompt + tools + user_msg_1 + assistant_1 + user_msg_2]
//!          ├── prefix match: system_prompt + tools (stable) ──── SKIP
//!          └── delta: user_msg_1 + assistant_1 + user_msg_2 ── decode only this
//! ```
//!
//! ## Approach: Session File Persistence (Phase 1)
//!
//! After each generation, the context state is saved to disk via
//! `LlamaContext::state_save_file()`. On the next turn, the state is loaded
//! into a fresh context, the common prefix is identified, stale positions are
//! cleared, and only new tokens are decoded. This avoids unsafe code while
//! providing 5-15s savings on Jetson.

use llama_cpp_2::token::LlamaToken;
use std::path::{Path, PathBuf};

/// Find the length of the common prefix between two token sequences.
///
/// Returns the number of tokens from the start that are identical in both
/// sequences. Used to determine how much of the KV cache can be reused.
///
/// # Examples
///
/// ```ignore
/// let cached = vec![tok(1), tok(2), tok(3), tok(4)];
/// let new    = vec![tok(1), tok(2), tok(5), tok(6), tok(7)];
/// assert_eq!(common_prefix_len(&cached, &new), 2);
/// ```
pub fn common_prefix_len(cached_tokens: &[LlamaToken], new_tokens: &[LlamaToken]) -> usize {
    cached_tokens
        .iter()
        .zip(new_tokens.iter())
        .take_while(|(a, b)| a == b)
        .count()
}

/// Describes what action to take based on prefix matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheAction {
    /// No usable cache — full prefill required (cold start or prompt completely changed).
    FullPrefill,
    /// Trim KV entries from `trim_from`, then decode `new_tokens[decode_from..]`.
    IncrementalDecode {
        /// Position from which to clear stale KV entries (inclusive).
        trim_from: usize,
        /// Index into new_tokens from which to start decoding.
        decode_from: usize,
        /// Tokens saved (not re-decoded).
        tokens_saved: usize,
    },
    /// Cache covers the entire new prompt — nothing to decode (rare: identical prompt).
    FullHit,
}

/// Determine the cache action given cached tokens and the new prompt tokens.
pub fn plan_cache_reuse(cached_tokens: &[LlamaToken], new_tokens: &[LlamaToken]) -> CacheAction {
    if cached_tokens.is_empty() || new_tokens.is_empty() {
        return CacheAction::FullPrefill;
    }

    let prefix_len = common_prefix_len(cached_tokens, new_tokens);

    if prefix_len == 0 {
        // Total mismatch — system prompt changed, model swapped, etc.
        return CacheAction::FullPrefill;
    }

    if prefix_len >= new_tokens.len() {
        // The new prompt is entirely covered by the cache (or shorter).
        return CacheAction::FullHit;
    }

    CacheAction::IncrementalDecode {
        trim_from: prefix_len,
        decode_from: prefix_len,
        tokens_saved: prefix_len,
    }
}

/// Path where the session cache file is stored for a given model.
pub fn cache_file_path(data_dir: &Path, model_id: &str) -> PathBuf {
    data_dir
        .join("cache")
        .join(format!("{}.session.bin", sanitize_filename(model_id)))
}

/// Sanitize a model ID for use as a filename (replace path-unsafe chars).
fn sanitize_filename(model_id: &str) -> String {
    model_id
        .replace('/', "_")
        .replace('\\', "_")
        .replace(':', "_")
        .replace(' ', "_")
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(id: i32) -> LlamaToken {
        LlamaToken(id)
    }

    #[test]
    fn common_prefix_empty() {
        assert_eq!(common_prefix_len(&[], &[tok(1), tok(2)]), 0);
        assert_eq!(common_prefix_len(&[tok(1)], &[]), 0);
        assert_eq!(common_prefix_len(&[], &[]), 0);
    }

    #[test]
    fn common_prefix_no_match() {
        let cached = vec![tok(1), tok(2), tok(3)];
        let new = vec![tok(4), tok(5), tok(6)];
        assert_eq!(common_prefix_len(&cached, &new), 0);
    }

    #[test]
    fn common_prefix_partial_match() {
        let cached = vec![tok(1), tok(2), tok(3), tok(4)];
        let new = vec![tok(1), tok(2), tok(5), tok(6), tok(7)];
        assert_eq!(common_prefix_len(&cached, &new), 2);
    }

    #[test]
    fn common_prefix_full_match() {
        let cached = vec![tok(1), tok(2), tok(3)];
        let new = vec![tok(1), tok(2), tok(3), tok(4), tok(5)];
        assert_eq!(common_prefix_len(&cached, &new), 3);
    }

    #[test]
    fn common_prefix_identical() {
        let tokens = vec![tok(1), tok(2), tok(3)];
        assert_eq!(common_prefix_len(&tokens, &tokens), 3);
    }

    #[test]
    fn plan_full_prefill_on_empty_cache() {
        let new = vec![tok(1), tok(2), tok(3)];
        assert_eq!(plan_cache_reuse(&[], &new), CacheAction::FullPrefill);
    }

    #[test]
    fn plan_full_prefill_on_no_match() {
        let cached = vec![tok(1), tok(2)];
        let new = vec![tok(3), tok(4), tok(5)];
        assert_eq!(plan_cache_reuse(&cached, &new), CacheAction::FullPrefill);
    }

    #[test]
    fn plan_incremental_decode() {
        // Simulate: system prompt (tok 1,2,3) is stable, user message changed
        let cached = vec![tok(1), tok(2), tok(3), tok(10), tok(11)];
        let new = vec![tok(1), tok(2), tok(3), tok(20), tok(21), tok(22)];

        let action = plan_cache_reuse(&cached, &new);
        assert_eq!(
            action,
            CacheAction::IncrementalDecode {
                trim_from: 3,
                decode_from: 3,
                tokens_saved: 3,
            }
        );
    }

    #[test]
    fn plan_full_hit_when_prompt_unchanged() {
        let tokens = vec![tok(1), tok(2), tok(3)];
        assert_eq!(plan_cache_reuse(&tokens, &tokens), CacheAction::FullHit);
    }

    #[test]
    fn plan_full_hit_when_new_is_subset() {
        let cached = vec![tok(1), tok(2), tok(3), tok(4)];
        let new = vec![tok(1), tok(2), tok(3)];
        // new is entirely within cached prefix
        assert_eq!(plan_cache_reuse(&cached, &new), CacheAction::FullHit);
    }

    #[test]
    fn cache_file_path_sanitizes() {
        let path = cache_file_path(Path::new("/data"), "gemma-4/E2B:Q4_K_M");
        assert_eq!(
            path,
            PathBuf::from("/data/cache/gemma-4_E2B_Q4_K_M.session.bin")
        );
    }
}
