//! What the last real Brave answer said about the stored key, held in memory: the tool writes
//! it, `GET /api/v1/web-search` reads it. Process-local, so a restart reports a stored key `ok`
//! until a search says otherwise.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;

use crate::mcp::domain::web_search::WebSearchStatus;

/// One observation, pinned to the key it was made with.
#[derive(Debug, Clone, Copy)]
struct Observation {
    key: u64,
    status: WebSearchStatus,
}

/// The latest observation about the web search key.
#[derive(Debug, Default)]
pub struct WebSearchStatusBoard {
    last: RwLock<Option<Observation>>,
}

impl WebSearchStatusBoard {
    pub const fn new() -> Self {
        Self {
            last: RwLock::new(None),
        }
    }

    /// Record what an answer said about `key`, replacing any earlier observation.
    pub fn record(&self, key: &str, status: WebSearchStatus) {
        let observation = Observation {
            key: fingerprint(key),
            status,
        };
        if let Ok(mut guard) = self.last.write() {
            *guard = Some(observation);
        }
    }

    /// Forget every observation, as when the key is removed.
    pub fn clear(&self) {
        if let Ok(mut guard) = self.last.write() {
            *guard = None;
        }
    }

    /// The status for the key stored now. An observation made with a different key says nothing
    /// about this one, so a key replaced by any route never inherits a stale `rejected`.
    pub fn status_for(&self, stored: Option<&str>) -> WebSearchStatus {
        let Some(key) = stored.filter(|k| !k.trim().is_empty()) else {
            return WebSearchStatus::NotConfigured;
        };
        let current = fingerprint(key);
        self.last
            .read()
            .ok()
            .and_then(|guard| *guard)
            .filter(|o| o.key == current)
            .map_or(WebSearchStatus::Ok, |o| o.status)
    }
}

/// The board the tool and the routes share in a running pond.
pub fn global() -> &'static WebSearchStatusBoard {
    static BOARD: WebSearchStatusBoard = WebSearchStatusBoard::new();
    &BOARD
}

/// Identifies a key without keeping it. In memory only, never shown or stored.
fn fingerprint(key: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.trim().hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_is_not_configured_whatever_was_observed() {
        let board = WebSearchStatusBoard::new();
        board.record("k1", WebSearchStatus::Rejected);
        assert_eq!(board.status_for(None), WebSearchStatus::NotConfigured);
        assert_eq!(board.status_for(Some("  ")), WebSearchStatus::NotConfigured);
    }

    #[test]
    fn a_stored_key_nothing_has_judged_is_ok() {
        let board = WebSearchStatusBoard::new();
        assert_eq!(board.status_for(Some("k1")), WebSearchStatus::Ok);
    }

    #[test]
    fn the_last_observation_about_this_key_is_its_status() {
        let board = WebSearchStatusBoard::new();
        board.record("k1", WebSearchStatus::Quota);
        assert_eq!(board.status_for(Some("k1")), WebSearchStatus::Quota);
        assert_eq!(
            board.status_for(Some(" k1 ")),
            WebSearchStatus::Quota,
            "the tool trims the key it sends, so the board trims too"
        );
        board.record("k1", WebSearchStatus::Ok);
        assert_eq!(board.status_for(Some("k1")), WebSearchStatus::Ok);
    }

    #[test]
    fn a_replaced_key_does_not_inherit_the_old_ones_status() {
        let board = WebSearchStatusBoard::new();
        board.record("old", WebSearchStatus::Rejected);
        assert_eq!(board.status_for(Some("new")), WebSearchStatus::Ok);
    }

    #[test]
    fn clearing_forgets() {
        let board = WebSearchStatusBoard::new();
        board.record("k1", WebSearchStatus::Unverified);
        board.clear();
        assert_eq!(board.status_for(Some("k1")), WebSearchStatus::Ok);
    }
}
