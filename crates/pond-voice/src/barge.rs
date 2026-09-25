//! Barge-in detection, fed by whatever owns the mic (never the TTS port). Debounced so a cough
//! or echo can't cut a reply; latched so a torn-down turn isn't torn down twice.

/// Recent mic level; `None` (mic closed or disabled) must stay distinct from `Some(0.0)`.
pub trait SpeechEnergy: Send + Sync {
    fn recent_rms(&self, window_ms: u64) -> Option<f32>;

    /// Permanently no mic (unlike a momentary `None`), so a turn can skip the barge-in poll.
    fn is_inert(&self) -> bool {
        false
    }
}

/// No microphone. Used by text-only paths and tests.
pub struct NoEnergy;

impl SpeechEnergy for NoEnergy {
    fn recent_rms(&self, _window_ms: u64) -> Option<f32> {
        None
    }

    fn is_inert(&self) -> bool {
        true
    }
}

/// How often the turn samples the level while speaking.
pub const POLL_MS: u64 = 100;
/// How much recent audio each sample covers.
pub const WINDOW_MS: u64 = 100;
/// Speech threshold while the assistant talks; high because its output bleeds into the mic.
pub const THRESHOLD_WHILE_SPEAKING: f32 = 0.15;
/// Loud windows needed in a row: ~300 ms at [`POLL_MS`], enough to reject a cough.
pub const CONSECUTIVE_WINDOWS: u32 = 3;

/// Debounced, latching barge-in detector.
#[derive(Debug, Clone)]
pub struct BargeIn {
    threshold: f32,
    required: u32,
    seen: u32,
    fired: bool,
}

impl BargeIn {
    pub fn new(threshold: f32, required: u32) -> Self {
        Self {
            threshold,
            required,
            seen: 0,
            fired: false,
        }
    }

    pub fn while_speaking() -> Self {
        Self::new(THRESHOLD_WHILE_SPEAKING, CONSECUTIVE_WINDOWS)
    }

    /// Feed one window's level; true exactly once, on the window that confirms the interruption.
    pub fn on_rms(&mut self, rms: f32) -> bool {
        if self.fired {
            return false;
        }
        if rms > self.threshold {
            self.seen += 1;
            if self.seen >= self.required {
                self.fired = true;
                return true;
            }
        } else {
            self.seen = 0;
        }
        false
    }

    /// Drop the loud run but keep the latch; a stale run must not finish on much later audio.
    pub fn reset(&mut self) {
        self.seen = 0;
    }

    pub fn has_fired(&self) -> bool {
        self.fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOUD: f32 = 0.5;
    const QUIET: f32 = 0.01;

    #[test]
    fn it_takes_sustained_speech_not_one_loud_window() {
        let mut b = BargeIn::while_speaking();
        assert!(
            !b.on_rms(LOUD),
            "one window is a cough, not an interruption"
        );
        assert!(!b.on_rms(LOUD));
        assert!(b.on_rms(LOUD), "the third confirms");
    }

    #[test]
    fn a_quiet_window_resets_the_run() {
        let mut b = BargeIn::while_speaking();
        b.on_rms(LOUD);
        b.on_rms(LOUD);
        assert!(!b.on_rms(QUIET), "reset");
        assert!(!b.on_rms(LOUD), "counting starts over");
        assert!(!b.on_rms(LOUD));
        assert!(b.on_rms(LOUD));
    }

    #[test]
    fn it_fires_exactly_once() {
        let mut b = BargeIn::while_speaking();
        for _ in 0..CONSECUTIVE_WINDOWS {
            b.on_rms(LOUD);
        }
        assert!(b.has_fired());
        for _ in 0..10 {
            assert!(!b.on_rms(LOUD), "must not re-fire");
        }
    }

    #[test]
    fn the_threshold_is_exclusive() {
        let mut b = BargeIn::new(0.15, 1);
        assert!(!b.on_rms(0.15), "exactly at threshold is not over it");
        assert!(b.on_rms(0.1500001));
    }

    #[test]
    fn reset_clears_the_run_but_not_the_latch() {
        let mut b = BargeIn::while_speaking();
        b.on_rms(LOUD);
        b.reset();
        assert!(!b.on_rms(LOUD));
        assert!(!b.on_rms(LOUD));
        assert!(b.on_rms(LOUD), "needed a full run after reset");

        assert!(b.has_fired());
        b.reset();
        assert!(b.has_fired(), "reset must not resurrect a torn-down turn");
        assert!(!b.on_rms(LOUD));
    }

    #[test]
    fn a_single_window_configuration_fires_immediately() {
        let mut b = BargeIn::new(0.1, 1);
        assert!(b.on_rms(0.2));
    }

    #[test]
    fn silence_never_fires() {
        let mut b = BargeIn::while_speaking();
        for _ in 0..100 {
            assert!(!b.on_rms(QUIET));
        }
        assert!(!b.has_fired());
    }

    #[test]
    fn no_energy_reports_absence_rather_than_silence() {
        assert_eq!(NoEnergy.recent_rms(WINDOW_MS), None);
    }

    #[test]
    fn no_energy_declares_itself_permanently_inert() {
        assert!(NoEnergy.is_inert());

        struct ClosedMic;
        impl SpeechEnergy for ClosedMic {
            fn recent_rms(&self, _: u64) -> Option<f32> {
                None
            }
        }
        assert!(
            !ClosedMic.is_inert(),
            "a closed microphone is momentarily silent, not permanently absent"
        );
    }

    #[test]
    fn the_debounce_window_is_responsive_but_not_twitchy() {
        let ms = POLL_MS * CONSECUTIVE_WINDOWS as u64;
        assert!(
            (200..=500).contains(&ms),
            "{ms}ms to confirm: under 200 is twitchy, over 500 feels unresponsive"
        );
    }
}
