/// Whether this host can do something now, and why not when it cannot, so the phone and the
/// dashboard offer only what works and say what is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable { reason: String },
}

impl Availability {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unavailable_capability_carries_its_reason() {
        assert!(Availability::Available.is_available());
        assert_eq!(Availability::Available.reason(), None);
        let missing = Availability::unavailable("no Wi-Fi adapter");
        assert!(!missing.is_available());
        assert_eq!(missing.reason(), Some("no Wi-Fi adapter"));
    }
}
