use std::time::{Duration, Instant};

use crate::network::domain::claim::RetryAfter;

/// Failures that cost nothing, so a household fumbling the card is never made to wait.
pub const FREE_SETUP_FAILURES: u32 = 5;
/// The wait after the last free failure; each further failure doubles it.
pub const FIRST_SETUP_DELAY: Duration = Duration::from_secs(30);
/// One guess per ten minutes: at that rate ten random digits outlast the hardware.
pub const MAX_SETUP_DELAY: Duration = Duration::from_secs(10 * 60);

/// Slows guessing instead of retiring the code, which would let anyone in range destroy the card.
/// Monotonic `Instant`s, because a board without an RTC jumps its wall clock when NTP arrives.
#[derive(Debug, Default)]
pub struct SetupGuessThrottle {
    failures: u32,
    wait_until: Option<Instant>,
}

impl SetupGuessThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn check(&self, now: Instant) -> Result<(), RetryAfter> {
        match self.wait_until {
            Some(until) if now < until => Err(RetryAfter(until - now)),
            _ => Ok(()),
        }
    }

    pub fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        if let Some(delay) = delay_after(self.failures) {
            self.wait_until = Some(now + delay);
        }
    }

    pub fn record_success(&mut self) {
        *self = Self::default();
    }

    pub fn failures(&self) -> u32 {
        self.failures
    }
}

/// The wait the `failures`-th failure imposes, `None` while failures are still free.
fn delay_after(failures: u32) -> Option<Duration> {
    let doublings = failures.checked_sub(FREE_SETUP_FAILURES)?;
    // The cap is reached after five doublings; bounding the shift keeps the multiply in range.
    let factor = 1u32 << doublings.min(16);
    Some(
        FIRST_SETUP_DELAY
            .saturating_mul(factor)
            .min(MAX_SETUP_DELAY),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn five_failures_are_free_and_the_sixth_attempt_waits_thirty_seconds() {
        let t0 = Instant::now();
        let mut throttle = SetupGuessThrottle::new();
        for _ in 0..FREE_SETUP_FAILURES - 1 {
            assert_eq!(throttle.check(t0), Ok(()));
            throttle.record_failure(t0);
        }
        assert_eq!(
            throttle.check(t0),
            Ok(()),
            "the fifth attempt is still free"
        );
        throttle.record_failure(t0);

        assert_eq!(throttle.check(t0), Err(RetryAfter(secs(30))));
        assert_eq!(throttle.check(t0 + secs(10)), Err(RetryAfter(secs(20))));
        assert_eq!(throttle.check(t0 + secs(30)), Ok(()));
    }

    #[test]
    fn each_further_failure_doubles_the_wait() {
        let t0 = Instant::now();
        let mut throttle = SetupGuessThrottle::new();
        for _ in 0..FREE_SETUP_FAILURES {
            throttle.record_failure(t0);
        }
        let mut now = t0;
        for expected in [60, 120, 240, 480] {
            now += secs(3_600);
            assert_eq!(throttle.check(now), Ok(()));
            throttle.record_failure(now);
            assert_eq!(throttle.check(now), Err(RetryAfter(secs(expected))));
        }
    }

    #[test]
    fn the_wait_is_capped_at_ten_minutes() {
        let t0 = Instant::now();
        let mut throttle = SetupGuessThrottle::new();
        for _ in 0..FREE_SETUP_FAILURES + 5 {
            throttle.record_failure(t0);
        }
        assert_eq!(throttle.check(t0), Err(RetryAfter(MAX_SETUP_DELAY)));

        for _ in 0..1_000 {
            throttle.record_failure(t0);
        }
        assert_eq!(throttle.check(t0), Err(RetryAfter(MAX_SETUP_DELAY)));
        assert_eq!(delay_after(u32::MAX), Some(MAX_SETUP_DELAY));
    }

    #[test]
    fn a_success_resets_the_count_and_the_wait() {
        let t0 = Instant::now();
        let mut throttle = SetupGuessThrottle::new();
        for _ in 0..FREE_SETUP_FAILURES + 3 {
            throttle.record_failure(t0);
        }
        assert!(throttle.check(t0).is_err());
        throttle.record_success();
        assert_eq!(throttle.check(t0), Ok(()));
        assert_eq!(throttle.failures(), 0);

        for _ in 0..FREE_SETUP_FAILURES - 1 {
            throttle.record_failure(t0);
        }
        assert_eq!(
            throttle.check(t0),
            Ok(()),
            "the free allowance came back with the reset"
        );
    }
}
