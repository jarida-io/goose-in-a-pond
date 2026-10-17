use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::network::domain::claim::RetryAfter;

/// Device runs one `client_id` may start in a rolling [`DEVICE_RESUME_WINDOW`] without a success.
pub const DEVICE_RESUME_ATTEMPTS: usize = 5;
pub const DEVICE_RESUME_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Devices tracked at once. `client_id` is the phone's own claim, so the map is bounded by this
/// rather than by the household.
pub const MAX_TRACKED_DEVICES: usize = 256;

/// Bounds load, not guessing: the password is a 256-bit hash. Keyed by the claimed `client_id`,
/// paired or not, so the limit reveals nothing; `Instant`s survive an RTC-less board's clock jump.
#[derive(Debug, Default)]
pub struct DeviceResumeThrottle {
    charges: HashMap<String, VecDeque<Instant>>,
}

impl DeviceResumeThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Charged when a run starts, as the setup throttle is, so runs started together cannot all
    /// slip in; a refusal charges nothing, and [`Self::record_success`] gives the allowance back.
    pub fn admit(&mut self, device: &str, now: Instant) -> Result<(), RetryAfter> {
        if let Some(charges) = self.charges.get_mut(device) {
            forget_expired(charges, now);
            if charges.len() >= DEVICE_RESUME_ATTEMPTS {
                let oldest = charges.front().copied().unwrap_or(now);
                return Err(RetryAfter(
                    DEVICE_RESUME_WINDOW.saturating_sub(now.saturating_duration_since(oldest)),
                ));
            }
            charges.push_back(now);
            return Ok(());
        }
        self.make_room(now);
        let mut charges = VecDeque::with_capacity(DEVICE_RESUME_ATTEMPTS);
        charges.push_back(now);
        self.charges.insert(device.to_string(), charges);
        Ok(())
    }

    /// A confirmed run: the device's count starts again.
    pub fn record_success(&mut self, device: &str) {
        self.charges.remove(device);
    }

    /// Attempts `device` has charged inside the window ending at `now`.
    pub fn charged(&self, device: &str, now: Instant) -> usize {
        self.charges.get(device).map_or(0, |charges| {
            charges
                .iter()
                .filter(|&&at| now.saturating_duration_since(at) < DEVICE_RESUME_WINDOW)
                .count()
        })
    }

    pub fn tracked(&self) -> usize {
        self.charges.len()
    }

    /// Evicting is safe because the limit bounds load, not guessing: whoever floods the map with
    /// ids could as easily have used a fresh id per attempt.
    fn make_room(&mut self, now: Instant) {
        if self.charges.len() < MAX_TRACKED_DEVICES {
            return;
        }
        self.charges.retain(|_, charges| {
            forget_expired(charges, now);
            !charges.is_empty()
        });
        if self.charges.len() < MAX_TRACKED_DEVICES {
            return;
        }
        let stalest = self
            .charges
            .iter()
            .min_by_key(|(_, charges)| charges.back().copied())
            .map(|(device, _)| device.clone());
        if let Some(device) = stalest {
            self.charges.remove(&device);
        }
    }
}

fn forget_expired(charges: &mut VecDeque<Instant>, now: Instant) {
    while let Some(&oldest) = charges.front() {
        if now.saturating_duration_since(oldest) < DEVICE_RESUME_WINDOW {
            break;
        }
        charges.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn five_attempts_then_a_wait_until_the_oldest_leaves_the_window() {
        let t0 = Instant::now();
        let mut throttle = DeviceResumeThrottle::new();
        for minute in 0..DEVICE_RESUME_ATTEMPTS as u64 {
            assert_eq!(throttle.admit("phone", t0 + secs(minute * 60)), Ok(()));
        }
        let at = t0 + secs(5 * 60);
        assert_eq!(throttle.admit("phone", at), Err(RetryAfter(secs(5 * 60))));
        assert_eq!(throttle.charged("phone", at), DEVICE_RESUME_ATTEMPTS);
        assert_eq!(
            throttle.admit("phone", t0 + secs(9 * 60 + 30)),
            Err(RetryAfter(secs(30)))
        );
        assert_eq!(
            throttle.admit("phone", t0 + DEVICE_RESUME_WINDOW),
            Ok(()),
            "the first attempt has left the window"
        );
        assert_eq!(
            throttle.admit("phone", t0 + DEVICE_RESUME_WINDOW),
            Err(RetryAfter(secs(60))),
            "and only that one"
        );
    }

    #[test]
    fn a_refusal_charges_nothing() {
        let t0 = Instant::now();
        let mut throttle = DeviceResumeThrottle::new();
        for _ in 0..DEVICE_RESUME_ATTEMPTS {
            throttle.admit("phone", t0).unwrap();
        }
        for _ in 0..50 {
            assert!(throttle.admit("phone", t0 + secs(1)).is_err());
        }
        assert_eq!(
            throttle.charged("phone", t0 + secs(1)),
            DEVICE_RESUME_ATTEMPTS
        );
        assert_eq!(throttle.admit("phone", t0 + DEVICE_RESUME_WINDOW), Ok(()));
    }

    #[test]
    fn a_success_resets_the_count() {
        let t0 = Instant::now();
        let mut throttle = DeviceResumeThrottle::new();
        for _ in 0..DEVICE_RESUME_ATTEMPTS {
            throttle.admit("phone", t0).unwrap();
        }
        throttle.record_success("phone");
        assert_eq!(throttle.charged("phone", t0), 0);
        for _ in 0..DEVICE_RESUME_ATTEMPTS {
            assert_eq!(throttle.admit("phone", t0), Ok(()));
        }
    }

    #[test]
    fn each_device_has_its_own_count() {
        let t0 = Instant::now();
        let mut throttle = DeviceResumeThrottle::new();
        for _ in 0..DEVICE_RESUME_ATTEMPTS {
            throttle.admit("liz", t0).unwrap();
        }
        assert!(throttle.admit("liz", t0).is_err());
        assert_eq!(throttle.admit("jerry", t0), Ok(()));
    }

    #[test]
    fn the_map_stays_bounded_and_drops_the_stalest_device_first() {
        let t0 = Instant::now();
        let mut throttle = DeviceResumeThrottle::new();
        throttle.admit("first", t0).unwrap();
        for n in 1..MAX_TRACKED_DEVICES {
            throttle.admit(&format!("flood-{n}"), t0 + secs(1)).unwrap();
        }
        assert_eq!(throttle.tracked(), MAX_TRACKED_DEVICES);
        throttle.admit("one-more", t0 + secs(2)).unwrap();
        assert_eq!(throttle.tracked(), MAX_TRACKED_DEVICES);
        assert_eq!(throttle.charged("first", t0 + secs(2)), 0);
        assert_eq!(throttle.charged("one-more", t0 + secs(2)), 1);

        let later = t0 + DEVICE_RESUME_WINDOW + secs(5);
        throttle.admit("after-the-window", later).unwrap();
        assert_eq!(throttle.tracked(), 1, "expired devices are forgotten first");
    }
}
