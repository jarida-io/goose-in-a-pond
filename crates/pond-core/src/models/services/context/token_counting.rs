//! Token-count fallback for every path but the live Goose adapter; same math as the trimmer.

use crate::models::ports::token_counter::TokenCounter;

/// Right for English prose; JSON, code and non-Latin scripts pack more tokens per char.
const CHARS_PER_TOKEN: usize = 4;

/// Chat-template role marker and delimiters per message; added by the trimmer, not the counter.
pub const PER_MESSAGE_TOKEN_OVERHEAD: usize = 4;

/// `text.len() / 4`. Never exact, and says so.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeuristicTokenCounter;

impl TokenCounter for HeuristicTokenCounter {
    fn count(&self, text: &str) -> usize {
        text.len() / CHARS_PER_TOKEN
    }

    fn is_exact(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "chars/4"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_by_dividing_characters() {
        let c = HeuristicTokenCounter;
        assert_eq!(c.count(""), 0);
        assert_eq!(c.count("abcd"), 1);
        assert_eq!(c.count("abcdefgh"), 2);
    }

    #[test]
    fn is_never_exact() {
        assert!(!HeuristicTokenCounter.is_exact());
        assert_eq!(HeuristicTokenCounter.name(), "chars/4");
    }

    /// Counts bytes: don't "fix" to `chars().count()`, the Jetson relies on the over-count margin.
    #[test]
    fn multibyte_text_is_over_counted_which_is_the_safe_direction() {
        let ascii = HeuristicTokenCounter.count("aaaaaaaa");
        let cyrillic = HeuristicTokenCounter.count("аааааааа");
        assert!(
            cyrillic > ascii,
            "byte-based counting must over-estimate multi-byte text"
        );
    }
}
