use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::network::domain::claim::RetryAfter;

/// The least time between two codes read aloud.
pub const SPOKEN_CODE_SPACING: Duration = Duration::from_secs(30);
/// The most codes read aloud in any rolling [`SPOKEN_CODE_WINDOW`].
pub const SPOKEN_CODES_PER_WINDOW: usize = 10;
pub const SPOKEN_CODE_WINDOW: Duration = Duration::from_secs(60 * 60);

/// Bounds how often the pond reads a pairing code aloud, whatever asks: anything on the setup
/// channel can ask, and a pond reciting codes all evening is both a nuisance and a guessing aid.
#[derive(Debug)]
pub struct SpokenCodeLimiter {
    spoken: VecDeque<Instant>,
}

impl Default for SpokenCodeLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl SpokenCodeLimiter {
    pub fn new() -> Self {
        Self {
            spoken: VecDeque::with_capacity(SPOKEN_CODES_PER_WINDOW),
        }
    }

    /// Takes a slot at `now`, or says how long until one frees. A refusal takes nothing.
    pub fn try_acquire(&mut self, now: Instant) -> Result<(), RetryAfter> {
        while let Some(&oldest) = self.spoken.front() {
            if now.saturating_duration_since(oldest) < SPOKEN_CODE_WINDOW {
                break;
            }
            self.spoken.pop_front();
        }

        let spacing = self
            .spoken
            .back()
            .map(|&last| SPOKEN_CODE_SPACING.saturating_sub(now.saturating_duration_since(last)))
            .unwrap_or_default();
        let window = match self.spoken.front() {
            Some(&oldest) if self.spoken.len() >= SPOKEN_CODES_PER_WINDOW => {
                SPOKEN_CODE_WINDOW.saturating_sub(now.saturating_duration_since(oldest))
            }
            _ => Duration::ZERO,
        };
        let wait = spacing.max(window);
        if !wait.is_zero() {
            return Err(RetryAfter(wait));
        }
        self.spoken.push_back(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn codes_are_at_least_thirty_seconds_apart() {
        let t0 = Instant::now();
        let mut limiter = SpokenCodeLimiter::new();
        assert_eq!(limiter.try_acquire(t0), Ok(()));
        assert_eq!(limiter.try_acquire(t0), Err(RetryAfter(secs(30))));
        assert_eq!(
            limiter.try_acquire(t0 + secs(12)),
            Err(RetryAfter(secs(18)))
        );
        assert_eq!(limiter.try_acquire(t0 + secs(30)), Ok(()));
    }

    #[test]
    fn the_eleventh_code_within_an_hour_waits_for_the_oldest_to_leave_the_window() {
        let t0 = Instant::now();
        let mut limiter = SpokenCodeLimiter::new();
        for i in 0..SPOKEN_CODES_PER_WINDOW as u64 {
            assert_eq!(limiter.try_acquire(t0 + secs(30 * i)), Ok(()), "code {i}");
        }
        let eleventh = t0 + secs(300);
        assert_eq!(limiter.try_acquire(eleventh), Err(RetryAfter(secs(3_300))));
        assert_eq!(
            limiter.try_acquire(t0 + secs(3_599)),
            Err(RetryAfter(secs(1)))
        );
    }

    #[test]
    fn the_window_rolls() {
        let t0 = Instant::now();
        let mut limiter = SpokenCodeLimiter::new();
        for i in 0..SPOKEN_CODES_PER_WINDOW as u64 {
            limiter.try_acquire(t0 + secs(30 * i)).unwrap();
        }
        assert_eq!(limiter.try_acquire(t0 + secs(3_600)), Ok(()));
        assert_eq!(
            limiter.try_acquire(t0 + secs(3_615)),
            Err(RetryAfter(secs(15))),
            "the window rolled but the spacing still holds"
        );
        assert_eq!(
            limiter.try_acquire(t0 + secs(3_630)),
            Ok(()),
            "the second code has also left the window"
        );
    }

    #[test]
    fn a_refused_request_takes_no_slot() {
        let t0 = Instant::now();
        let mut limiter = SpokenCodeLimiter::new();
        limiter.try_acquire(t0).unwrap();
        for s in 1..30 {
            assert!(limiter.try_acquire(t0 + secs(s)).is_err());
        }
        assert_eq!(limiter.try_acquire(t0 + secs(30)), Ok(()));
    }
}
