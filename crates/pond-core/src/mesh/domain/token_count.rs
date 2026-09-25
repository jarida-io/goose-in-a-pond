//! Inference-token count for the mesh usage tally.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TokenCount(u64);

impl TokenCount {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(&self) -> u64 {
        self.0
    }

    pub fn checked_add(&self, other: TokenCount) -> Option<TokenCount> {
        self.0.checked_add(other.0).map(TokenCount)
    }

    pub fn checked_sub(&self, other: TokenCount) -> Option<TokenCount> {
        self.0.checked_sub(other.0).map(TokenCount)
    }
}

impl std::fmt::Display for TokenCount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} tokens", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_add_overflow_returns_none() {
        assert_eq!(
            TokenCount::new(u64::MAX).checked_add(TokenCount::new(1)),
            None
        );
    }

    #[test]
    fn checked_add_accumulates() {
        let mut total = TokenCount::new(0);
        for _ in 0..3 {
            total = total.checked_add(TokenCount::new(10)).unwrap();
        }
        assert_eq!(total.value(), 30);
    }
}
