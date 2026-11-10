//! Keeps other audio paused while the pond makes a sound, for Spotify's Developer Policy III.7
//! (see [`crate::models::ports::audio_focus`]).
//!
//! The pond's audio bus says when it is held, and [`Quiet::follow`] keeps a [`QuietHold`] for as
//! long as it is, so every sound the pond plays, a whole voice turn with the pauses between its
//! sentences included, keeps the music paused; a sound waits for [`Quiet::until_quiet`] before its
//! first sample. [`Quiet::linger`] covers a stretch nothing plays, like the words said after a wake
//! word or audio a browser is playing. Once nothing holds and nothing lingers, what was paused is
//! resumed after the grace, so two sounds close together do not bring it back in between.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use crate::models::ports::audio_focus::{AudioFocus, Paused};

/// How long after the last sound the music comes back.
pub const GRACE: Duration = Duration::from_secs(2);
/// How long a sound waits for other audio to pause before it plays anyway.
pub const WAIT_FOR_QUIET: Duration = Duration::from_millis(1500);

enum Event {
    Hold,
    Unhold,
    Linger(Instant),
}

/// Pauses other audio for as long as anything asks for quiet. Cheap to share; one per process.
pub struct Quiet {
    events: mpsc::UnboundedSender<Event>,
    quiet: watch::Receiver<bool>,
}

impl Quiet {
    /// Starts the task that pauses and resumes; call from inside a Tokio runtime.
    pub fn start(focus: Arc<dyn AudioFocus>, grace: Duration) -> Arc<Self> {
        let (events, received) = mpsc::unbounded_channel();
        let (said, quiet) = watch::channel(false);
        tokio::spawn(run(focus, grace, received, said));
        Arc::new(Self { events, quiet })
    }

    /// Other audio stays paused until the hold is dropped; the first hold pauses it.
    pub fn hold(&self) -> QuietHold {
        let _ = self.events.send(Event::Hold);
        QuietHold {
            events: self.events.clone(),
        }
    }

    /// Keeps other audio paused while `held` is true: the audio bus publishes it, held from the
    /// moment a sound takes the bus to the moment it is free. Call from inside a Tokio runtime.
    pub fn follow(self: &Arc<Self>, mut held: watch::Receiver<bool>) {
        let quiet = self.clone();
        tokio::spawn(async move {
            let mut hold = None;
            loop {
                let now = *held.borrow_and_update();
                if now != hold.is_some() {
                    hold = now.then(|| quiet.hold());
                }
                if held.changed().await.is_err() {
                    break;
                }
            }
        });
    }

    /// Other audio stays paused for `span` from now, with or without a hold.
    pub fn linger(&self, span: Duration) {
        if !span.is_zero() {
            let _ = self.events.send(Event::Linger(Instant::now() + span));
        }
    }

    /// Returns once other audio is paused, or was found not playing, and after `most` at the
    /// latest: a slow service delays a sound by that much and never more.
    pub async fn until_quiet(&self, most: Duration) {
        let mut quiet = self.quiet.clone();
        let _ = tokio::time::timeout(most, quiet.wait_for(|quiet| *quiet)).await;
    }

    /// [`Self::until_quiet`] for a thread outside the runtime, like the wake ping's.
    pub fn until_quiet_blocking(&self, most: Duration) {
        let started = std::time::Instant::now();
        while !*self.quiet.borrow() && started.elapsed() < most {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Other audio stays paused while this is alive.
pub struct QuietHold {
    events: mpsc::UnboundedSender<Event>,
}

impl Drop for QuietHold {
    fn drop(&mut self) {
        let _ = self.events.send(Event::Unhold);
    }
}

static INSTALLED: OnceLock<Arc<Quiet>> = OnceLock::new();

/// Makes `quiet` the process's, for sounds that are not a `VoiceOutput` (the wake ping) and for
/// audio a browser plays. The first install wins.
pub fn install(quiet: Arc<Quiet>) {
    let _ = INSTALLED.set(quiet);
}

/// The process's, once installed.
pub fn installed() -> Option<Arc<Quiet>> {
    INSTALLED.get().cloned()
}

#[derive(Default)]
struct State {
    holds: usize,
    linger_until: Option<Instant>,
    /// When to resume, once nothing holds: after the grace, or the last linger if that is later.
    release_at: Option<Instant>,
    /// `Some` while quiet: what was paused, if anything was.
    paused: Option<Option<Paused>>,
}

impl State {
    fn apply(&mut self, event: Event, grace: Duration) {
        match event {
            Event::Hold => {
                self.holds += 1;
                self.release_at = None;
            }
            Event::Unhold => {
                self.holds = self.holds.saturating_sub(1);
                if self.holds == 0 {
                    self.release_at = Some(later(Instant::now() + grace, self.linger_until));
                }
            }
            // Reached late, while this was busy asking Spotify: already over, so nothing to keep.
            Event::Linger(until) if until <= Instant::now() => {}
            Event::Linger(until) => {
                self.linger_until = Some(later(until, self.linger_until));
                if self.holds == 0 {
                    self.release_at = Some(later(until, self.release_at));
                }
            }
        }
    }

    fn wants_quiet(&self) -> bool {
        self.holds > 0 || self.release_at.is_some()
    }
}

fn later(at: Instant, other: Option<Instant>) -> Instant {
    other.map_or(at, |other| at.max(other))
}

async fn run(
    focus: Arc<dyn AudioFocus>,
    grace: Duration,
    mut events: mpsc::UnboundedReceiver<Event>,
    quiet: watch::Sender<bool>,
) {
    let mut state = State::default();
    loop {
        let event = match state.release_at {
            Some(at) => tokio::select! {
                event = events.recv() => event,
                () = tokio::time::sleep_until(at) => {
                    // Not quiet from here, so a sound starting now waits; then take in whatever was
                    // sent before this, since a hold among it means the music stays paused. That
                    // hold's sound may already be playing: it saw quiet before the line below.
                    quiet.send_replace(false);
                    while let Ok(event) = events.try_recv() {
                        state.apply(event, grace);
                    }
                    let due = state.release_at.is_some_and(|at| at <= Instant::now());
                    if state.holds == 0 && due {
                        state.release_at = None;
                        state.linger_until = None;
                        if let Some(Some(paused)) = state.paused.take() {
                            focus.resume(paused).await;
                        }
                    } else {
                        quiet.send_replace(true);
                    }
                    continue;
                }
            },
            None => events.recv().await,
        };
        let Some(event) = event else {
            break;
        };
        state.apply(event, grace);
        // A burst of holds is one pause, not one each.
        while let Ok(event) = events.try_recv() {
            state.apply(event, grace);
        }
        if state.wants_quiet() && state.paused.is_none() {
            state.paused = Some(focus.pause().await);
            quiet.send_replace(true);
        }
    }
    if let Some(Some(paused)) = state.paused.take() {
        focus.resume(paused).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// A Spotify that is playing unless told otherwise, and says what was done to it.
    struct FakeService {
        playing: AtomicBool,
        pause_takes: Duration,
        resume_takes: Duration,
        done: Mutex<Vec<&'static str>>,
    }

    impl FakeService {
        fn playing() -> Arc<Self> {
            Self::with(true, Duration::ZERO)
        }
        fn with(playing: bool, pause_takes: Duration) -> Arc<Self> {
            Arc::new(Self {
                playing: AtomicBool::new(playing),
                pause_takes,
                resume_takes: Duration::ZERO,
                done: Mutex::new(Vec::new()),
            })
        }
        fn slow_to_resume(resume_takes: Duration) -> Arc<Self> {
            Arc::new(Self {
                playing: AtomicBool::new(true),
                pause_takes: Duration::ZERO,
                resume_takes,
                done: Mutex::new(Vec::new()),
            })
        }
        fn done(&self) -> Vec<&'static str> {
            self.done.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl AudioFocus for FakeService {
        async fn pause(&self) -> Option<Paused> {
            tokio::time::sleep(self.pause_takes).await;
            self.done.lock().unwrap().push("pause");
            self.playing.swap(false, Ordering::SeqCst).then(|| Paused {
                device: Some("kitchen".into()),
                item: Some("spotify:track:1".into()),
            })
        }
        async fn resume(&self, _paused: Paused) {
            tokio::time::sleep(self.resume_takes).await;
            self.done.lock().unwrap().push("resume");
            self.playing.store(true, Ordering::SeqCst);
        }
    }

    /// Lets the controller's task take in what was sent.
    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_pauses_and_the_music_comes_back_a_grace_after_it_ends() {
        let spotify = FakeService::playing();
        let quiet = Quiet::start(spotify.clone(), GRACE);

        let hold = quiet.hold();
        quiet.until_quiet(Duration::from_secs(1)).await;
        assert_eq!(spotify.done(), ["pause"]);

        drop(hold);
        tokio::time::sleep(GRACE - Duration::from_millis(10)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause"], "back before the grace was over");

        tokio::time::sleep(Duration::from_millis(20)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn following_the_bus_pauses_while_it_is_held_and_resumes_a_grace_after() {
        let spotify = FakeService::playing();
        let quiet = Quiet::start(spotify.clone(), GRACE);
        let (bus, held) = watch::channel(false);
        quiet.follow(held);

        bus.send_replace(true);
        quiet.until_quiet(Duration::from_secs(1)).await;
        assert_eq!(spotify.done(), ["pause"]);
        tokio::time::sleep(GRACE * 3).await;
        settle().await;
        assert_eq!(
            spotify.done(),
            ["pause"],
            "back while the bus was still held"
        );

        bus.send_replace(false);
        settle().await;
        tokio::time::sleep(GRACE + Duration::from_millis(20)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn holds_that_overlap_pause_once_and_resume_once() {
        let spotify = FakeService::playing();
        let quiet = Quiet::start(spotify.clone(), GRACE);

        let turn = quiet.hold();
        let sentence = quiet.hold();
        quiet.until_quiet(Duration::from_secs(1)).await;
        drop(sentence);
        let next_sentence = quiet.hold();
        drop(next_sentence);
        drop(turn);
        tokio::time::sleep(GRACE * 2).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sound_inside_the_grace_keeps_it_paused() {
        let spotify = FakeService::playing();
        let quiet = Quiet::start(spotify.clone(), GRACE);

        drop(quiet.hold());
        settle().await;
        tokio::time::sleep(GRACE / 2).await;
        drop(quiet.hold());
        settle().await;
        tokio::time::sleep(GRACE / 2 + Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause"], "came back between two sounds");

        tokio::time::sleep(GRACE).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_playing_is_nothing_to_resume() {
        let spotify = FakeService::with(false, Duration::ZERO);
        let quiet = Quiet::start(spotify.clone(), GRACE);

        drop(quiet.hold());
        tokio::time::sleep(GRACE * 2).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_linger_keeps_it_paused_with_nothing_holding() {
        let spotify = FakeService::playing();
        let quiet = Quiet::start(spotify.clone(), GRACE);

        quiet.linger(Duration::from_secs(5));
        quiet.until_quiet(Duration::from_secs(1)).await;
        assert_eq!(spotify.done(), ["pause"]);

        tokio::time::sleep(Duration::from_millis(4900)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause"], "back before the linger was over");

        tokio::time::sleep(Duration::from_millis(200)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_after_a_wake_word_resumes_at_the_later_of_the_two() {
        let spotify = FakeService::playing();
        let quiet = Quiet::start(spotify.clone(), GRACE);

        // The wake word lingers over the words that follow it; the turn then holds.
        quiet.linger(Duration::from_secs(10));
        settle().await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        let turn = quiet.hold();
        tokio::time::sleep(Duration::from_secs(10)).await;
        drop(turn);
        settle().await;
        tokio::time::sleep(GRACE - Duration::from_millis(100)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause"]);
        tokio::time::sleep(Duration::from_millis(200)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_sound_waits_for_the_pause_but_no_longer_than_it_asks() {
        let slow = FakeService::with(true, Duration::from_secs(10));
        let quiet = Quiet::start(slow.clone(), GRACE);

        let _hold = quiet.hold();
        let started = Instant::now();
        quiet.until_quiet(Duration::from_millis(1500)).await;
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(1500) && waited < Duration::from_secs(2),
            "waited {waited:?}"
        );

        let quick = FakeService::with(true, Duration::from_millis(200));
        let quiet = Quiet::start(quick, GRACE);
        let _hold = quiet.hold();
        let started = Instant::now();
        quiet.until_quiet(Duration::from_millis(1500)).await;
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(200) && waited < Duration::from_millis(300),
            "waited {waited:?} for a pause that took 200 ms"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_sent_as_the_grace_runs_out_keeps_it_paused() {
        // On one thread the hold always reaches the task before the timer does. The other order, a
        // hold from another thread landing after the release was chosen, is what the drain in
        // `run` is for; one thread cannot produce it, so this does not test the drain.
        for _ in 0..20 {
            let spotify = FakeService::playing();
            let quiet = Quiet::start(spotify.clone(), GRACE);

            drop(quiet.hold());
            settle().await;
            tokio::time::sleep(GRACE - Duration::from_millis(1)).await;
            let hold = quiet.hold();
            tokio::time::advance(Duration::from_millis(5)).await;
            settle().await;
            quiet.until_quiet(Duration::from_millis(1)).await;
            assert_eq!(spotify.done(), ["pause"], "resumed under a new sound");

            drop(hold);
            tokio::time::sleep(GRACE * 2).await;
            settle().await;
            assert_eq!(spotify.done(), ["pause", "resume"]);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_quick_exchange_inside_the_wake_words_linger_resumes_when_the_linger_ends() {
        let spotify = FakeService::playing();
        let quiet = Quiet::start(spotify.clone(), GRACE);

        quiet.linger(Duration::from_secs(10));
        settle().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        drop(quiet.hold());
        settle().await;
        tokio::time::sleep(Duration::from_millis(8800)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause"], "back before the linger was over");
        tokio::time::sleep(Duration::from_millis(400)).await;
        settle().await;
        assert_eq!(spotify.done(), ["pause", "resume"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_linger_read_after_it_ended_pauses_nothing() {
        // A short linger is sent while the task is busy resuming, slowly; by the time it is read
        // it has run out, and pausing for it would only resume again at once.
        let slow = FakeService::slow_to_resume(Duration::from_secs(3));
        let quiet = Quiet::start(slow.clone(), GRACE);
        drop(quiet.hold());
        settle().await;
        tokio::time::sleep(GRACE + Duration::from_millis(500)).await;
        settle().await;
        quiet.linger(Duration::from_secs(1));
        tokio::time::sleep(Duration::from_secs(5)).await;
        settle().await;
        tokio::time::sleep(GRACE * 2).await;
        settle().await;
        assert_eq!(
            slow.done(),
            ["pause", "resume"],
            "paused again for a linger already over"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_thread_outside_the_runtime_can_wait_for_it() {
        let spotify = FakeService::with(true, Duration::from_millis(50));
        let quiet = Quiet::start(spotify.clone(), GRACE);

        let waiter = quiet.clone();
        let waited = std::thread::spawn(move || {
            let _hold = waiter.hold();
            let started = std::time::Instant::now();
            waiter.until_quiet_blocking(Duration::from_secs(5));
            started.elapsed()
        })
        .join()
        .unwrap();
        assert!(waited < Duration::from_secs(2), "waited {waited:?}");
        assert_eq!(spotify.done(), ["pause"]);
    }
}
