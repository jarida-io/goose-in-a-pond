//! The pond's own words heard back. The canceller leaves enough of a loud reply for Whisper to
//! transcribe, so a segment holding the wake phrase is the pond's echo, not a wake, when most of
//! its words are, in order, words the pond has just said.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub use pond_voice::text::in_order_share;

/// Speech said this long ago can still come back: playback runs behind synthesis, and the
/// segment's transcription behind both.
pub const ECHO_WINDOW: Duration = Duration::from_secs(10);
/// The share of a segment's words, in order, that makes it the pond's own.
pub const ECHO_SHARE: f32 = 0.6;
/// A segment shorter than this is never taken for an echo: a person interrupting may say the
/// wake phrase and little else.
pub const ECHO_MIN_WORDS: usize = 2;
/// What a long reply may keep, oldest dropped first.
const MAX_WORDS: usize = 4_000;

/// What the pond said, unit by unit.
#[derive(Default)]
pub(crate) struct Spoken {
    units: VecDeque<(Instant, Vec<String>)>,
    words: usize,
}

impl Spoken {
    pub fn say(&mut self, text: &str, at: Instant) {
        // The transcript's own normaliser, so the two sides' words compare.
        let words: Vec<String> = pond_voice::text::normalize_transcript(text)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        if words.is_empty() {
            return;
        }
        self.words += words.len();
        self.units.push_back((at, words));
        while self.words > MAX_WORDS {
            match self.units.pop_front() {
                Some((_, dropped)) => self.words -= dropped.len(),
                None => break,
            }
        }
    }

    /// Forget what is past the window, keeping everything said since `turn_began`.
    pub fn forget_old(&mut self, now: Instant, turn_began: Option<Instant>) {
        while let Some((at, _)) = self.units.front() {
            let in_turn = turn_began.is_some_and(|began| *at >= began);
            if in_turn || now.saturating_duration_since(*at) <= ECHO_WINDOW {
                break;
            }
            if let Some((_, dropped)) = self.units.pop_front() {
                self.words -= dropped.len();
            }
        }
    }

    /// The share of `transcript`'s words the pond said, in order, when that makes it an echo.
    pub fn echo_of(&self, transcript: &str) -> Option<f32> {
        let normalized = pond_voice::text::normalize_transcript(transcript);
        let heard: Vec<&str> = normalized.split_whitespace().collect();
        if heard.len() < ECHO_MIN_WORDS || self.words == 0 {
            return None;
        }
        let said: Vec<&str> = self
            .units
            .iter()
            .flat_map(|(_, words)| words.iter().map(String::as_str))
            .collect();
        let share = in_order_share(&heard, &said);
        (share >= ECHO_SHARE).then_some(share)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reply the nano told when its own echo woke it five times.
    const STORY: [&str; 3] = [
        "Once upon a time, Goosey Goose left the pond to see the wide world.",
        "She flew past the mill and over the hill.",
        "Since the long story about Goosey Goose leaving the pond has been delivered, shall I tell another?",
    ];

    fn told(at: Instant) -> Spoken {
        let mut spoken = Spoken::default();
        for unit in STORY {
            spoken.say(unit, at);
        }
        spoken
    }

    #[test]
    fn what_the_nano_transcribed_of_its_own_reply_is_its_echo() {
        let spoken = told(Instant::now());
        for heard in [
            "Goosey Goose left a pond.",
            "since the lock story about Goosey. Goose leaving the pilot has been delivered",
        ] {
            let share = spoken.echo_of(heard);
            assert!(share.is_some_and(|s| s >= ECHO_SHARE), "{heard}: {share:?}");
        }
    }

    #[test]
    fn a_person_over_the_reply_is_not_its_echo() {
        let spoken = told(Instant::now());
        for heard in [
            "Goose, stop.",
            "Hey goose, what time is it?",
            "Goose, tell me about the weather tomorrow instead.",
        ] {
            assert_eq!(spoken.echo_of(heard), None, "{heard}");
        }
    }

    #[test]
    fn only_the_window_and_the_turn_under_way_are_remembered() {
        let then = Instant::now();
        let later = then + ECHO_WINDOW + Duration::from_secs(1);
        let mut spoken = told(then);
        spoken.forget_old(later, Some(then));
        assert!(
            spoken.echo_of("Goosey Goose left a pond.").is_some(),
            "same turn"
        );
        spoken.forget_old(later, None);
        assert_eq!(spoken.echo_of("Goosey Goose left a pond."), None);
        assert_eq!(spoken.words, 0);
    }

    #[test]
    fn the_share_counts_words_in_order_with_gaps() {
        assert_eq!(
            in_order_share(&["a", "b", "c"], &["a", "x", "b", "y", "c"]),
            1.0
        );
        assert_eq!(
            in_order_share(&["c", "b", "a"], &["a", "b", "c"]),
            1.0 / 3.0
        );
        assert_eq!(in_order_share(&[], &["a"]), 0.0);
    }

    #[test]
    fn a_long_reply_keeps_only_its_latest_words() {
        let mut spoken = Spoken::default();
        let now = Instant::now();
        for _ in 0..(MAX_WORDS / 4 + 10) {
            spoken.say("one two three four", now);
        }
        assert!(spoken.words <= MAX_WORDS);
    }
}
