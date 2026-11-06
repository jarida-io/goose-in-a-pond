//! One run per `Idempotency-Key` on `POST /tools/invoke`: a phone resends a queued tap under the
//! key it first sent, and a tap that already reached the Pond is answered from here instead of
//! run a second time. Kept in memory: a restart forgets them, as it ends every tap in flight.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

pub const HEADER: &str = "idempotency-key";

/// Longer than the app holds a queued tap (10 minutes), so every resend finds its first answer.
const KEEP: Duration = Duration::from_secs(30 * 60);

/// Bounds what a caller sending fresh keys can hold.
const CAPACITY: usize = 1024;

const MAX_KEY_LEN: usize = 255;

/// The request a key was first used for, and the answer it got once it succeeded.
#[derive(Default)]
pub struct Outcome {
    pub request: Option<Value>,
    pub reply: Option<Value>,
}

/// Locked for the whole first run, so a resend that arrives meanwhile waits for its answer.
pub type Slot = Arc<tokio::sync::Mutex<Outcome>>;

#[derive(Default)]
pub struct IdempotentReplies {
    slots: Mutex<HashMap<(String, String), (Instant, Slot)>>,
}

impl IdempotentReplies {
    /// The slot for `key` from `caller`; one caller's key never answers another's.
    pub fn slot(&self, caller: &str, key: &str) -> Slot {
        self.slot_at(caller, key, Instant::now())
    }

    fn slot_at(&self, caller: &str, key: &str, now: Instant) -> Slot {
        let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
        let id = (caller.to_string(), key.to_string());
        if let Some((at, slot)) = slots.get(&id) {
            if now.duration_since(*at) < KEEP {
                return slot.clone();
            }
        }
        if slots.len() >= CAPACITY {
            slots.retain(|_, (at, _)| now.duration_since(*at) < KEEP);
        }
        if slots.len() >= CAPACITY {
            if let Some(oldest) = slots
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(id, _)| id.clone())
            {
                slots.remove(&oldest);
            }
        }
        let slot = Slot::default();
        slots.insert(id, (now, slot.clone()));
        slot
    }
}

/// A key is 1 to 255 visible ASCII characters, as the app's UUIDs are.
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_LEN && key.bytes().all(|b| b.is_ascii_graphic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_key_from_the_same_caller_shares_a_slot() {
        let replies = IdempotentReplies::default();
        let first = replies.slot("phone", "k");
        assert!(Arc::ptr_eq(&first, &replies.slot("phone", "k")));
        assert!(!Arc::ptr_eq(&first, &replies.slot("other-phone", "k")));
        assert!(!Arc::ptr_eq(&first, &replies.slot("phone", "k2")));
    }

    #[test]
    fn a_key_older_than_the_keep_window_starts_again() {
        let replies = IdempotentReplies::default();
        let start = Instant::now();
        let first = replies.slot_at("phone", "k", start);
        let later = replies.slot_at("phone", "k", start + KEEP);
        assert!(!Arc::ptr_eq(&first, &later));
    }

    #[test]
    fn a_full_table_drops_its_oldest_key() {
        let replies = IdempotentReplies::default();
        let start = Instant::now();
        let oldest = replies.slot_at("phone", "0", start);
        for i in 1..CAPACITY {
            replies.slot_at(
                "phone",
                &i.to_string(),
                start + Duration::from_millis(i as u64),
            );
        }
        let now = start + Duration::from_secs(1);
        replies.slot_at("phone", "new", now);
        assert_eq!(replies.slots.lock().unwrap().len(), CAPACITY);
        assert!(!Arc::ptr_eq(&oldest, &replies.slot_at("phone", "0", now)));
    }

    #[test]
    fn keys_are_visible_ascii_of_bounded_length() {
        assert!(valid_key("0b6f1c2e-9a4d-4f3e-8c1a-2d5e6f7a8b9c"));
        assert!(!valid_key(""));
        assert!(!valid_key("has space"));
        assert!(!valid_key("ключ"));
        assert!(!valid_key(&"k".repeat(MAX_KEY_LEN + 1)));
    }
}
