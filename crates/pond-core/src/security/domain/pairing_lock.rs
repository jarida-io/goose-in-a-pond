//! The pairing lock: once a household has a main member, a phone that proves it holds a pairing
//! code is not let in. It leaves a request, which the main member or whoever unlocked My Pond
//! accepts. A recovery PIN stands in for the main member's own phone when that phone is gone.

use chrono::Duration;
use serde::{Deserialize, Serialize};

/// The fewest digits a recovery PIN may have.
pub const MIN_PIN_DIGITS: usize = 6;
/// The most, so a pasted document is not mistaken for one.
pub const MAX_PIN_DIGITS: usize = 12;
/// How long a request waits for an answer.
pub const REQUEST_TTL_MINUTES: i64 = 10;
/// Requests that may wait at once; a new one past this is refused, not queued.
pub const MAX_PENDING_REQUESTS: usize = 5;
/// Wrong PINs allowed before each further one costs a wait.
pub const FREE_PIN_TRIES: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingLockState {
    pub locked: bool,
    /// The main member: who set the pond up, until they hand the role on.
    pub owner_profile_id: Option<String>,
    pub has_recovery_pin: bool,
    /// Whether the main member has a paired phone to accept a request on.
    pub owner_has_a_phone: bool,
}

impl PairingLockState {
    /// The lock holds only while somebody can answer: a pond nobody owns has nobody to ask,
    /// and a main member with no phone and no recovery PIN could never let a phone in again.
    pub fn enforced(&self) -> bool {
        self.locked
            && self.owner_profile_id.is_some()
            && (self.owner_has_a_phone || self.has_recovery_pin)
    }
}

/// A phone waiting to be let in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingRequest {
    pub id: String,
    pub client_id: String,
    pub client_type: String,
    pub device_name: String,
    /// The member the code it used was bound to, if any.
    pub profile_id: Option<String>,
    pub created_at: String,
    pub expires_at: String,
}

/// What the asking phone is handed, once: the request and the secret that proves it is the
/// phone that asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPairing {
    pub request_id: String,
    pub claim_secret: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedTokens {
    pub session_token: String,
    pub refresh_token: String,
    pub expires_at: String,
    /// The name the phone gave, for the household's notice.
    pub device_name: String,
}

/// What a phone finds when it comes back for its request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingClaim {
    Waiting,
    Declined,
    /// Unknown, expired, already used, or not this phone's.
    Gone,
    Paired(PairedTokens),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinCheck {
    Accepted,
    Wrong {
        tries_left: u32,
    },
    LockedFor(Duration),
    /// No PIN was ever set, so there is nothing to recover with.
    NoPin,
}

/// Digits only, within the lengths a person can keep and type.
pub fn pin_is_acceptable(pin: &str) -> bool {
    (MIN_PIN_DIGITS..=MAX_PIN_DIGITS).contains(&pin.len())
        && pin.bytes().all(|b| b.is_ascii_digit())
}

/// The wait `failures` wrong PINs in a row cost: none for the first few, then 30 seconds
/// doubling to an hour. Kept with the PIN, so a restart does not forgive it.
pub fn lockout_after(failures: u32) -> Option<Duration> {
    let past = failures.checked_sub(FREE_PIN_TRIES)?;
    let seconds = 30u64.saturating_mul(1u64 << past.min(10));
    Some(Duration::seconds(seconds.min(3600) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_six_to_twelve_digits() {
        for pin in ["123456", "000000000000"] {
            assert!(pin_is_acceptable(pin), "{pin}");
        }
        for pin in ["12345", "1234567890123", "12345a", "123 456", ""] {
            assert!(!pin_is_acceptable(pin), "{pin}");
        }
    }

    #[test]
    fn wrong_pins_are_free_at_first_and_then_cost_a_growing_wait() {
        assert_eq!(lockout_after(4), None);
        assert_eq!(lockout_after(5), Some(Duration::seconds(30)));
        assert_eq!(lockout_after(6), Some(Duration::seconds(60)));
        assert_eq!(lockout_after(9), Some(Duration::seconds(480)));
        assert_eq!(lockout_after(40), Some(Duration::seconds(3600)));
    }

    #[test]
    fn a_lock_with_no_main_member_holds_nothing() {
        let mut state = PairingLockState {
            locked: true,
            owner_profile_id: None,
            has_recovery_pin: false,
            owner_has_a_phone: true,
        };
        assert!(!state.enforced());
        state.owner_profile_id = Some("jerry".into());
        assert!(state.enforced());
        state.owner_has_a_phone = false;
        assert!(!state.enforced(), "nobody could accept, so nothing is held");
        state.has_recovery_pin = true;
        assert!(state.enforced(), "the PIN is a way in");
        state.locked = false;
        assert!(!state.enforced());
    }
}
