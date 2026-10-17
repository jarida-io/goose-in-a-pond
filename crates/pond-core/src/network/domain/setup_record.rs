use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::discriminator::Discriminator;
use super::setup_card::SetupCard;
use super::setup_code::SetupCode;

/// The only record version this build reads. A store refuses any other rather than guess at it.
pub const SETUP_RECORD_VERSION: u32 = 1;

/// Whether the pond still accepts its printed setup code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ClaimState {
    Unclaimed,
    Claimed { at: DateTime<Utc> },
}

impl ClaimState {
    pub fn is_claimed(&self) -> bool {
        matches!(self, Self::Claimed { .. })
    }
}

/// A pond's setup code and its claim latch, as the store persists them. `claimed_at` is a latch
/// with a door: factory reset clears it, which re-arms the same printed card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupRecord {
    pub version: u32,
    pub code: SetupCode,
    pub discriminator: Discriminator,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub claimed_at: Option<DateTime<Utc>>,
}

impl SetupRecord {
    pub fn new(code: SetupCode, discriminator: Discriminator, created_at: DateTime<Utc>) -> Self {
        Self {
            version: SETUP_RECORD_VERSION,
            code,
            discriminator,
            created_at,
            claimed_at: None,
        }
    }

    pub fn claim_state(&self) -> ClaimState {
        match self.claimed_at {
            Some(at) => ClaimState::Claimed { at },
            None => ClaimState::Unclaimed,
        }
    }

    pub fn card(&self) -> SetupCard {
        SetupCard::new(self.code.clone(), self.discriminator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> SetupRecord {
        SetupRecord::new(
            SetupCode::from_digits([7, 9, 9, 2, 7, 3, 9, 8, 7, 1]).unwrap(),
            Discriminator::new(0x7f3a),
            "2026-10-16T09:00:00Z".parse().unwrap(),
        )
    }

    #[test]
    fn a_new_record_is_current_and_unclaimed() {
        let record = record();
        assert_eq!(record.version, SETUP_RECORD_VERSION);
        assert_eq!(record.claim_state(), ClaimState::Unclaimed);
        assert!(!record.claim_state().is_claimed());
    }

    #[test]
    fn claimed_at_is_the_claim_state() {
        let mut record = record();
        let at: DateTime<Utc> = "2026-10-16T10:00:00Z".parse().unwrap();
        record.claimed_at = Some(at);
        assert_eq!(record.claim_state(), ClaimState::Claimed { at });
        assert!(record.claim_state().is_claimed());
    }

    #[test]
    fn the_json_has_the_documented_fields_and_round_trips() {
        let record = record();
        let value = serde_json::to_value(&record).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "version": 1,
                "code": "79927398713",
                "discriminator": "7f3a",
                "created_at": "2026-10-16T09:00:00Z",
                "claimed_at": null,
            })
        );
        assert_eq!(
            serde_json::from_value::<SetupRecord>(value).unwrap(),
            record
        );
    }

    #[test]
    fn a_stored_code_with_a_bad_check_digit_is_refused() {
        let json = r#"{"version":1,"code":"79927398714","discriminator":"7f3a",
                       "created_at":"2026-10-16T09:00:00Z","claimed_at":null}"#;
        assert!(serde_json::from_str::<SetupRecord>(json).is_err());
    }

    #[test]
    fn debug_never_shows_the_code() {
        let record = record();
        let debug = format!("{record:?} {:?}", record.card());
        assert!(!debug.contains(record.code.digits()), "{debug}");
        assert!(!debug.contains(&record.code.display()), "{debug}");
    }

    #[test]
    fn claim_state_serialises_with_a_state_tag() {
        assert_eq!(
            serde_json::to_value(ClaimState::Unclaimed).unwrap(),
            serde_json::json!({"state": "unclaimed"})
        );
        let at: DateTime<Utc> = "2026-10-16T10:00:00Z".parse().unwrap();
        assert_eq!(
            serde_json::to_value(ClaimState::Claimed { at }).unwrap(),
            serde_json::json!({"state": "claimed", "at": "2026-10-16T10:00:00Z"})
        );
    }
}
