use std::time::Duration;

/// How long until a limited action may be tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryAfter(pub Duration);

impl RetryAfter {
    pub fn duration(self) -> Duration {
        self.0
    }

    /// Whole seconds rounded up, for a wire field: a phone that waits this long is not refused.
    pub fn as_secs_ceil(self) -> u64 {
        self.0.as_secs() + u64::from(self.0.subsec_nanos() > 0)
    }
}

/// Why a setup-code attempt was refused before any guess was spent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ClaimRefusal {
    /// No code was ever made, or the pond is claimed. Both read the same to a phone on purpose.
    #[error("this pond does not accept a setup code")]
    SetupCodeUnavailable,
    #[error("too many wrong setup codes; try again in {} s", .retry_after.as_secs_ceil())]
    SetupCodeCooling { retry_after: RetryAfter },
}

impl ClaimRefusal {
    /// The wire `code` the setup channel answers with.
    pub fn code(&self) -> &'static str {
        match self {
            Self::SetupCodeUnavailable => "setup_code_unavailable",
            Self::SetupCodeCooling { .. } => "setup_code_cooling",
        }
    }

    pub fn retry_after(&self) -> Option<RetryAfter> {
        match self {
            Self::SetupCodeUnavailable => None,
            Self::SetupCodeCooling { retry_after } => Some(*retry_after),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_rounds_up_to_whole_seconds() {
        assert_eq!(RetryAfter(Duration::from_secs(30)).as_secs_ceil(), 30);
        assert_eq!(RetryAfter(Duration::from_millis(29_001)).as_secs_ceil(), 30);
        assert_eq!(RetryAfter(Duration::from_nanos(1)).as_secs_ceil(), 1);
        assert_eq!(RetryAfter(Duration::ZERO).as_secs_ceil(), 0);
    }

    #[test]
    fn refusals_carry_their_wire_codes() {
        assert_eq!(
            ClaimRefusal::SetupCodeUnavailable.code(),
            "setup_code_unavailable"
        );
        let cooling = ClaimRefusal::SetupCodeCooling {
            retry_after: RetryAfter(Duration::from_secs(60)),
        };
        assert_eq!(cooling.code(), "setup_code_cooling");
        assert_eq!(
            cooling.retry_after(),
            Some(RetryAfter(Duration::from_secs(60)))
        );
        assert_eq!(ClaimRefusal::SetupCodeUnavailable.retry_after(), None);
        assert_eq!(
            cooling.to_string(),
            "too many wrong setup codes; try again in 60 s"
        );
    }
}
