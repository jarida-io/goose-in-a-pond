//! Money unit for mesh metering/settlement. Arithmetic is checked: money must never silently wrap.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Millisats(u64);

impl Millisats {
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(&self) -> u64 {
        self.0
    }

    pub fn checked_add(&self, other: Millisats) -> Option<Millisats> {
        self.0.checked_add(other.0).map(Millisats)
    }

    pub fn checked_sub(&self, other: Millisats) -> Option<Millisats> {
        self.0.checked_sub(other.0).map(Millisats)
    }
}

impl std::fmt::Display for Millisats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} msat", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_add_overflow_returns_none() {
        assert_eq!(
            Millisats::new(u64::MAX).checked_add(Millisats::new(1)),
            None
        );
    }

    #[test]
    fn checked_sub_underflow_returns_none() {
        assert_eq!(Millisats::new(0).checked_sub(Millisats::new(1)), None);
    }

    #[test]
    fn checked_add_sub_roundtrip() {
        let a = Millisats::new(100);
        let b = Millisats::new(40);
        let sum = a.checked_add(b).unwrap();
        assert_eq!(sum.value(), 140);
        assert_eq!(sum.checked_sub(b).unwrap(), a);
    }
}
