//! Barge-in detection, fed by whatever owns the mic (never the TTS port). Debounced so a cough
//! or echo can't cut a reply; latched so a torn-down turn isn't torn down twice.

use std::time::{Duration, Instant};

/// Recent mic level; `None` (mic closed or disabled) must stay distinct from `Some(0.0)`.
pub trait SpeechEnergy: Send + Sync {
    fn recent_rms(&self, window_ms: u64) -> Option<f32>;

    /// Permanently no mic (unlike a momentary `None`), so a turn can skip the barge-in poll.
    fn is_inert(&self) -> bool {
        false
    }

    /// Keep the mic capturing while the hold lives, opening it if nobody else has.
    /// Barge-in needs this: with no wake-word detector running, nothing else listens mid-reply.
    fn hold_mic(&self) -> MicHold {
        MicHold::none()
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

/// A claim on the microphone, released on drop. `Sync` so a voice turn's future, which holds
/// one, can run on any runtime worker.
pub struct MicHold(Option<Box<dyn FnOnce() + Send + Sync>>);

impl MicHold {
    pub fn new(release: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self(Some(Box::new(release)))
    }

    /// A hold with nothing to release.
    pub fn none() -> Self {
        Self(None)
    }
}

impl Drop for MicHold {
    fn drop(&mut self) {
        if let Some(release) = self.0.take() {
            release();
        }
    }
}

impl std::fmt::Debug for MicHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("MicHold").field(&self.0.is_some()).finish()
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
/// Audio the follow-up capture keeps from before the first loud window: the threshold is set
/// above the reply's echo, so a softly spoken first syllable sits under it.
pub const ONSET_PRE_ROLL_MS: u64 = 200;

/// Debounced, latching barge-in detector.
#[derive(Debug, Clone)]
pub struct BargeIn {
    threshold: f32,
    required: u32,
    seen: u32,
    fired: bool,
    /// Start of the current loud run's first window.
    run_began: Option<Instant>,
}

impl BargeIn {
    pub fn new(threshold: f32, required: u32) -> Self {
        Self {
            threshold,
            required,
            seen: 0,
            fired: false,
            run_began: None,
        }
    }

    pub fn while_speaking() -> Self {
        Self::new(THRESHOLD_WHILE_SPEAKING, CONSECUTIVE_WINDOWS)
    }

    /// Feed one window's level; true exactly once, on the window that confirms the interruption.
    pub fn on_rms(&mut self, rms: f32) -> bool {
        self.on_window(rms, Instant::now()).is_some()
    }

    /// [`on_rms`](Self::on_rms) for a [`WINDOW_MS`] window ending at `end`; on confirming,
    /// returns when the confirmed run began, i.e. when the user started talking over the reply.
    pub fn on_window(&mut self, rms: f32, end: Instant) -> Option<Instant> {
        if self.fired {
            return None;
        }
        if rms > self.threshold {
            if self.seen == 0 {
                let window = Duration::from_millis(WINDOW_MS);
                self.run_began = Some(end.checked_sub(window).unwrap_or(end));
            }
            self.seen += 1;
            if self.seen >= self.required {
                self.fired = true;
                return self.run_began;
            }
        } else {
            self.reset();
        }
        None
    }

    /// Drop the loud run but keep the latch; a stale run must not finish on much later audio.
    pub fn reset(&mut self) {
        self.seen = 0;
        self.run_began = None;
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
    fn the_onset_is_the_start_of_the_confirmed_run_not_the_moment_it_confirmed() {
        let t0 = Instant::now() + Duration::from_secs(1);
        let at = |windows: u64| t0 + Duration::from_millis(windows * WINDOW_MS);
        let mut b = BargeIn::while_speaking();

        assert_eq!(b.on_window(LOUD, at(1)), None);
        assert_eq!(b.on_window(QUIET, at(2)), None, "a cough, then quiet");
        assert_eq!(b.on_window(LOUD, at(3)), None, "the real run starts here");
        assert_eq!(b.on_window(LOUD, at(4)), None);
        assert_eq!(
            b.on_window(LOUD, at(5)),
            Some(at(2)),
            "the onset is where the run's first window began, not the abandoned cough"
        );
        assert_eq!(b.on_window(LOUD, at(6)), None, "latched");
    }

    #[test]
    fn reset_forgets_where_the_run_began() {
        let t0 = Instant::now() + Duration::from_secs(1);
        let at = |windows: u64| t0 + Duration::from_millis(windows * WINDOW_MS);
        let mut b = BargeIn::while_speaking();
        b.on_window(LOUD, at(1));
        b.reset();
        b.on_window(LOUD, at(4));
        b.on_window(LOUD, at(5));
        assert_eq!(b.on_window(LOUD, at(6)), Some(at(3)));
    }

    #[test]
    fn the_pre_roll_reaches_a_soft_syllable_but_not_far_into_the_reply() {
        let debounce = POLL_MS * CONSECUTIVE_WINDOWS as u64;
        assert!(
            (WINDOW_MS..debounce).contains(&ONSET_PRE_ROLL_MS),
            "{ONSET_PRE_ROLL_MS}ms: under a window misses a soft start; as long as the \
             {debounce}ms debounce it mostly captures the reply's own echo"
        );
    }

    #[test]
    fn a_hold_releases_exactly_once_on_drop() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let released = Arc::new(AtomicUsize::new(0));
        let r = released.clone();
        let hold = MicHold::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(released.load(Ordering::SeqCst), 0);
        drop(hold);
        assert_eq!(released.load(Ordering::SeqCst), 1);

        drop(MicHold::none());
        assert!(NoEnergy.hold_mic().0.is_none(), "no mic, nothing to hold");
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
