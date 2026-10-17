//! Hearing a voice before choosing it (`POST /voice/sample`): one fixed sentence, so no caller's
//! text ever reaches the speaker, and at most one sample per [`VOICE_SAMPLE_SPACING`].

use std::time::{Duration, Instant};

/// Short and neutral: enough to hear a voice's character, nothing that reads as a reply.
pub const VOICE_SAMPLE_SENTENCE: &str = "Hello, I'm Goose. This is how I sound.";

/// The least time between two samples, whoever asks: a phone tapping through voices must not
/// turn the pond's speaker into a loop.
pub const VOICE_SAMPLE_SPACING: Duration = Duration::from_secs(3);

#[derive(Debug, Default)]
pub struct VoiceSampleLimiter {
    last: Option<Instant>,
}

impl VoiceSampleLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the slot at `now`, or says how long until it frees. A refusal takes nothing.
    pub fn try_acquire(&mut self, now: Instant) -> Result<(), Duration> {
        if let Some(last) = self.last {
            let wait = VOICE_SAMPLE_SPACING.saturating_sub(now.saturating_duration_since(last));
            if !wait.is_zero() {
                return Err(wait);
            }
        }
        self.last = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_sample_per_spacing_and_a_refusal_does_not_extend_the_wait() {
        let mut limiter = VoiceSampleLimiter::new();
        let t0 = Instant::now();
        assert!(limiter.try_acquire(t0).is_ok());
        assert_eq!(
            limiter.try_acquire(t0 + Duration::from_secs(1)),
            Err(Duration::from_secs(2))
        );
        assert_eq!(
            limiter.try_acquire(t0 + Duration::from_millis(2_500)),
            Err(Duration::from_millis(500))
        );
        assert!(
            limiter.try_acquire(t0 + VOICE_SAMPLE_SPACING).is_ok(),
            "the refusals above must not have pushed the slot back"
        );
        assert!(limiter.try_acquire(t0 + VOICE_SAMPLE_SPACING).is_err());
    }

    #[test]
    fn a_clock_that_runs_backwards_waits_the_full_spacing_rather_than_none() {
        let mut limiter = VoiceSampleLimiter::new();
        let t1 = Instant::now() + Duration::from_secs(10);
        assert!(limiter.try_acquire(t1).is_ok());
        assert_eq!(
            limiter.try_acquire(t1 - Duration::from_secs(5)),
            Err(VOICE_SAMPLE_SPACING)
        );
    }

    #[test]
    fn the_sentence_is_one_short_line_with_no_digits() {
        assert!(VOICE_SAMPLE_SENTENCE.split_whitespace().count() <= 12);
        assert!(!VOICE_SAMPLE_SENTENCE.contains('\n'));
        assert!(!VOICE_SAMPLE_SENTENCE.chars().any(|c| c.is_ascii_digit()));
    }
}
