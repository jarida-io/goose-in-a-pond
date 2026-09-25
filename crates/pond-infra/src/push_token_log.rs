//! Panic-free push-token prefixes for logging. Tokens are client-supplied, so never assume
//! ASCII: a byte slice can panic, and the relays' caller catches errors but not panics.

const TOKEN_LOG_PREFIX_CHARS: usize = 8;

/// Leading [`TOKEN_LOG_PREFIX_CHARS`] chars, to correlate logs without logging the credential.
pub(crate) fn token_log_prefix(token: &str) -> &str {
    match token.char_indices().nth(TOKEN_LOG_PREFIX_CHARS) {
        Some((byte_idx, _)) => &token[..byte_idx],
        None => token,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_a_long_ascii_token() {
        assert_eq!(token_log_prefix("abcdefghijklmnop"), "abcdefgh");
    }

    #[test]
    fn returns_short_tokens_whole() {
        assert_eq!(token_log_prefix("abc"), "abc");
        assert_eq!(token_log_prefix(""), "");
    }

    #[test]
    fn handles_the_exact_boundary() {
        assert_eq!(token_log_prefix("abcdefgh"), "abcdefgh");
    }

    #[test]
    fn does_not_panic_on_multi_byte_tokens() {
        // 2-byte chars: byte 8 is a boundary, but 8 chars are 16 bytes.
        assert_eq!(token_log_prefix("éééééééééé"), "éééééééé");
        // Byte 8 lands mid-character here (3-byte chars).
        assert_eq!(token_log_prefix("日本語のトークンです"), "日本語のトークン");
        // Mixed widths, and an emoji spanning 4 bytes.
        assert_eq!(token_log_prefix("ab\u{1F600}cdefghij"), "ab\u{1F600}cdefg");
    }
}
