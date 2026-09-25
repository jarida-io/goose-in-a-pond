//! Wall-clock ticks on the reactive event spine, so the pond can notice that nothing happened.
//! Clockless: the `pond-server` publisher owns the timer and passes in the readings.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which wall-clock boundary a [`TimeTick`] marks.
/// Hour only: `Settings` has no reliable coordinates for dawn/dusk and no quiet hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeBoundary {
    /// The local wall clock passed the top of an hour.
    Hour,
}

impl TimeBoundary {
    /// Short, stable label for structured logs and the event log.
    pub fn as_str(self) -> &'static str {
        match self {
            TimeBoundary::Hour => "hour",
        }
    }
}

/// The pond crossed a wall-clock boundary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeTick {
    pub boundary: TimeBoundary,
    /// When the boundary was observed, in UTC.
    pub at: DateTime<Utc>,
    /// Local hour `0..=23` in the host's zone, the same clock the rules engine uses.
    /// `Settings::timezone` is deliberately ignored so the two clocks cannot disagree.
    pub local_hour: u8,
}

/// Seconds from `now` to the top of the next hour; takes the clock so the fields can't swap.
pub fn secs_to_next_hour_from<T: chrono::Timelike>(now: &T) -> u64 {
    secs_to_next_hour(now.minute(), now.second())
}

/// Seconds from `minute`:`second` past the hour until the top of the next hour.
/// Never zero (even for a leap second): the publisher sleeps this long, and zero would spin.
fn secs_to_next_hour(minute: u32, second: u32) -> u64 {
    const HOUR: u64 = 3600;
    HOUR.saturating_sub(u64::from(minute) * 60 + u64::from(second))
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_of_the_hour_waits_a_full_hour() {
        assert_eq!(secs_to_next_hour(0, 0), 3600);
    }

    #[test]
    fn half_past_waits_half_an_hour() {
        assert_eq!(secs_to_next_hour(30, 0), 1800);
        assert_eq!(secs_to_next_hour(30, 30), 1770);
    }

    #[test]
    fn one_second_before_the_hour_waits_one_second() {
        assert_eq!(secs_to_next_hour(59, 59), 1);
    }

    #[test]
    fn a_leap_second_never_yields_a_zero_wait() {
        for (minute, second) in [(59, 60), (60, 60), (u32::MAX, u32::MAX)] {
            let wait = secs_to_next_hour(minute, second);
            assert_ne!(
                wait, 0,
                "{minute}:{second} is past the end of the hour and produced a zero-length wait; \
                 the publisher sleeps on this, so zero is not one tick early, it is a spin that \
                 publishes an unbounded burst of ticks"
            );
            assert_eq!(wait, 1, "an out-of-range clock waits the one-second floor");
        }
    }

    /// A sweep, not sample points: a regression here is off-by-one at one end of the range.
    #[test]
    fn every_position_in_the_hour_waits_between_one_second_and_an_hour() {
        for minute in 0..60 {
            for second in 0..60 {
                let wait = secs_to_next_hour(minute, second);
                assert!(
                    (1..=3600).contains(&wait),
                    "wait for {minute}:{second} was {wait}, outside 1..=3600"
                );
                assert_eq!(
                    wait == 3600,
                    minute == 0 && second == 0,
                    "only the top of the hour waits a full hour; {minute}:{second} waited {wait}"
                );
            }
        }
    }

    /// 1504 is 12:34:56 read correctly; 206 is the same clock read as `(second, minute)`.
    #[test]
    fn the_wait_is_read_off_the_clock_the_right_way_round() {
        let at = chrono::NaiveTime::from_hms_opt(12, 34, 56).expect("valid time");
        let wait = secs_to_next_hour_from(&at);
        assert_ne!(
            wait, 206,
            "the clock was read as (second, minute): 12:34:56 waits 1504s, not 206s"
        );
        assert_eq!(wait, 1504);
    }

    /// Vacuity control: proves the `assert_ne!(wait, 206)` above can fail.
    #[test]
    fn the_two_readings_of_that_clock_are_different_numbers() {
        assert_eq!(secs_to_next_hour(34, 56), 1504);
        assert_eq!(secs_to_next_hour(56, 34), 206);
    }

    #[test]
    fn boundary_label_is_stable() {
        assert_eq!(TimeBoundary::Hour.as_str(), "hour");
    }
}
