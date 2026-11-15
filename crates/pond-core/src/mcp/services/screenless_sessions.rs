//! Which engine sessions' current turn has nobody looking at a screen (a spoken turn), keyed by
//! the engine session id a tool server reads from Goose's `_meta`. Set at each turn; never
//! persisted.

use std::collections::VecDeque;
use std::sync::Mutex;

/// No session-end hook, so the oldest is forgotten past this; a forgotten one reads as a screen.
const MAX_SESSIONS: usize = 128;

pub struct ScreenlessSessions {
    sessions: Mutex<VecDeque<String>>,
}

impl ScreenlessSessions {
    pub const fn new() -> Self {
        Self {
            sessions: Mutex::new(VecDeque::new()),
        }
    }

    /// Records whether `session`'s turn, starting now, has a screen.
    pub fn set(&self, session: &str, screenless: bool) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|s| s != session);
        if screenless {
            sessions.push_back(session.to_string());
            while sessions.len() > MAX_SESSIONS {
                sessions.pop_front();
            }
        }
    }

    /// Whether `session`'s current turn has no screen; `false` for a session never set.
    pub fn is_screenless(&self, session: &str) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|s| s == session)
    }
}

impl Default for ScreenlessSessions {
    fn default() -> Self {
        Self::new()
    }
}

/// The one the chat's turns set and the tool servers read.
pub fn global() -> &'static ScreenlessSessions {
    static SESSIONS: ScreenlessSessions = ScreenlessSessions::new();
    &SESSIONS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spoken_turn_is_screenless_until_a_turn_with_a_screen() {
        let sessions = ScreenlessSessions::new();
        assert!(!sessions.is_screenless("s1"), "unknown reads as a screen");
        sessions.set("s1", true);
        sessions.set("s1", true);
        assert!(sessions.is_screenless("s1"));
        assert!(!sessions.is_screenless("s2"));
        sessions.set("s1", false);
        assert!(!sessions.is_screenless("s1"));
    }

    #[test]
    fn the_oldest_is_forgotten_past_the_cap() {
        let sessions = ScreenlessSessions::new();
        for i in 0..=MAX_SESSIONS {
            sessions.set(&format!("s{i}"), true);
        }
        assert!(!sessions.is_screenless("s0"));
        assert!(sessions.is_screenless(&format!("s{MAX_SESSIONS}")));
    }
}
