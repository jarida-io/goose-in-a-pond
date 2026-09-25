//! Token counting via Goose's tiktoken (`o200k_base`): inexact for GIAP's GGUFs, but a real BPE
//! beats `chars/4` on JSON and multi-byte text.

use pond_core::models::ports::token_counter::TokenCounter;

/// Goose's tiktoken counter; its own LRU cache makes re-counting unchanged history cheap.
pub struct TiktokenCounter {
    inner: goose::token_counter::TokenCounter,
}

impl TiktokenCounter {
    /// Offline-safe: `o200k_base` is embedded in `tiktoken_rs`.
    pub async fn new() -> Result<Self, String> {
        Ok(Self {
            inner: goose::token_counter::TokenCounter::new().await?,
        })
    }
}

impl TokenCounter for TiktokenCounter {
    fn count(&self, text: &str) -> usize {
        self.inner.count_tokens(text)
    }

    /// False: the vocabulary is not the model's, and budget code sizes its margin on this.
    fn is_exact(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "tiktoken-o200k"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::services::context::token_counting::HeuristicTokenCounter;

    #[tokio::test]
    async fn counts_plain_text_in_the_same_ballpark_as_the_heuristic() {
        let counter = TiktokenCounter::new()
            .await
            .expect("o200k_base is embedded in tiktoken_rs and must load offline");
        let text = "The quick brown fox jumps over the lazy dog, repeatedly and at length.";
        let tk = counter.count(text);
        let heuristic = HeuristicTokenCounter.count(text);
        assert!(tk > 0);
        // On English prose chars/4 is at its best, so the two must roughly agree.
        assert!(
            tk * 4 > heuristic && tk < heuristic * 4,
            "tiktoken={tk} heuristic={heuristic}"
        );
    }

    #[tokio::test]
    async fn multibyte_text_is_where_the_heuristic_is_worst() {
        let counter = TiktokenCounter::new()
            .await
            .expect("o200k_base is embedded in tiktoken_rs and must load offline");
        // `len()` is bytes, so the heuristic inflates this; a real BPE does not.
        let text = "これは日本語のテキストです。";
        assert!(
            counter.count(text) < HeuristicTokenCounter.count(text) * 3,
            "a real tokenizer should not inflate multi-byte text the way len()/4 does"
        );
    }

    #[tokio::test]
    async fn never_claims_to_be_exact() {
        let counter = TiktokenCounter::new()
            .await
            .expect("o200k_base is embedded in tiktoken_rs and must load offline");
        assert!(!counter.is_exact());
        assert_eq!(counter.name(), "tiktoken-o200k");
    }
}
