//! The speaker as one resource for the whole process: one stream, opened on first use and kept
//! open (CoreAudio degrades with open/close churn, ALSA `hw` refuses a second open), and one sound
//! on it at a time. Whatever plays holds the [`Floor`]; everything else waits its turn.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use rodio::Source;

use crate::{tap, Speaker};

/// Who is asking, which decides who goes first once the bus is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// The wake ping answers the person, so it is served before anything else waiting.
    Ping,
    /// A turn, a notice, a spoken code, a chime or a sound test.
    Sound,
}

/// A holder silent this long gives the bus up to a waiter, so a turn whose end never came cannot
/// keep it; a turn's tone and sentences renew it as they play.
const LEASE: Duration = Duration::from_secs(5);
/// How often a waiter looks again, for a lapsed lease or a reason to stop waiting.
const RECHECK: Duration = Duration::from_millis(25);
/// How often a one-shot sound checks it still holds the bus.
const POLL: Duration = Duration::from_millis(25);

struct Holder {
    id: u64,
    active_at: Instant,
}

#[derive(Default)]
struct State {
    holder: Option<Holder>,
    next_id: u64,
    pings_waiting: usize,
}

pub struct AudioBus {
    state: Mutex<State>,
    turned: Condvar,
    speaker: Mutex<Option<Speaker>>,
}

static BUS: OnceLock<AudioBus> = OnceLock::new();

/// The process's audio bus.
pub fn bus() -> &'static AudioBus {
    BUS.get_or_init(AudioBus::new)
}

/// The bus, held: only its holder plays, until this drops.
pub struct Floor {
    bus: &'static AudioBus,
    id: u64,
}

impl AudioBus {
    fn new() -> Self {
        Self {
            state: Mutex::default(),
            turned: Condvar::new(),
            speaker: Mutex::new(None),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits for the bus, at most `most` (`None`: for as long as `still_wanted` says).
    pub fn claim(
        &'static self,
        claim: Claim,
        most: Option<Duration>,
        still_wanted: impl Fn() -> bool,
    ) -> Option<Floor> {
        let deadline = most.map(|m| Instant::now() + m);
        let ping = claim == Claim::Ping;
        let mut state = self.state();
        if ping {
            state.pings_waiting += 1;
        }
        let granted = loop {
            let now = Instant::now();
            let lapsed = state
                .holder
                .as_ref()
                .map(|h| now.duration_since(h.active_at) >= LEASE);
            if lapsed != Some(false) && (ping || state.pings_waiting == 0) {
                if lapsed == Some(true) {
                    tracing::warn!("audio bus: a holder silent past its lease gave the speaker up");
                }
                state.next_id += 1;
                let id = state.next_id;
                state.holder = Some(Holder { id, active_at: now });
                break Some(id);
            }
            if !still_wanted() || deadline.is_some_and(|d| now >= d) {
                break None;
            }
            state = self
                .turned
                .wait_timeout(state, RECHECK)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        };
        if ping {
            state.pings_waiting -= 1;
        }
        drop(state);
        self.turned.notify_all();
        granted.map(|id| Floor { bus: self, id })
    }

    /// [`Self::claim`] from async code.
    pub async fn claim_async(&'static self, claim: Claim, most: Option<Duration>) -> Option<Floor> {
        tokio::task::spawn_blocking(move || self.claim(claim, most, || true))
            .await
            .ok()
            .flatten()
    }

    /// Whether something holds the bus within its lease.
    pub fn busy(&self) -> bool {
        self.state()
            .holder
            .as_ref()
            .is_some_and(|h| h.active_at.elapsed() < LEASE)
    }

    /// True once nothing holds the bus; false if something still did after `most`.
    pub async fn until_free(&self, most: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + most;
        while self.busy() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        true
    }

    /// Whether the bus's stream is open on `device` (`None`: the system default).
    pub(crate) fn plays_on(&self, device: Option<&str>) -> bool {
        let open = self.speaker.lock().unwrap_or_else(|e| e.into_inner());
        let Some(speaker) = open.as_ref() else {
            return false;
        };
        let want = device
            .map(str::to_string)
            .or_else(crate::route::default_output_name);
        want.is_some_and(|want| speaker.device() == want)
    }

    /// The stream every sound plays on, opened by the first; a failed open is retried by the next.
    pub(crate) fn speaker(&self) -> Result<Speaker> {
        let mut open = self.speaker.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(speaker) = open.as_ref() {
            return Ok(speaker.clone());
        }
        let speaker = Speaker::try_new("audio-bus")?;
        *open = Some(speaker.clone());
        Ok(speaker)
    }

    fn keep(&self, id: u64) -> bool {
        match self.state().holder.as_mut() {
            Some(h) if h.id == id => {
                h.active_at = Instant::now();
                true
            }
            _ => false,
        }
    }

    fn release(&self, id: u64) {
        let mut state = self.state();
        if state.holder.as_ref().is_some_and(|h| h.id == id) {
            state.holder = None;
            drop(state);
            self.turned.notify_all();
        }
    }
}

impl Floor {
    /// Whether this still holds the bus; renews its lease.
    pub fn keep(&self) -> bool {
        self.bus.keep(self.id)
    }

    /// Plays mono `samples` to the end, or until this no longer holds the bus.
    pub fn play(&self, samples: Vec<f32>, rate: u32, volume: f32) -> Result<()> {
        let speaker = self.bus.speaker()?;
        let generation = speaker.generation();
        let sink = rodio::Sink::try_new(&speaker.handle())?;
        let sound = rodio::buffer::SamplesBuffer::new(1, rate, samples).amplify(volume);
        sink.append(tap::tapped(sound));
        while !sink.empty() {
            if !self.keep() || speaker.generation() != generation {
                sink.stop();
                break;
            }
            std::thread::sleep(POLL);
        }
        Ok(())
    }
}

impl Drop for Floor {
    fn drop(&mut self) {
        self.bus.release(self.id);
    }
}

/// The bus held for a turn: taken by its first sound, kept through the pauses between its
/// sentences, and given back when the turn ends or is cut off.
#[derive(Clone, Default)]
pub struct TurnFloor {
    floor: Arc<Mutex<Option<Floor>>>,
    /// One claim at a time, so a turn's tone and first sentence never wait on each other.
    claiming: Arc<Mutex<()>>,
}

impl TurnFloor {
    /// Holds the bus for the turn, waiting while `still_wanted`; false once the turn gave up.
    pub fn hold(&self, still_wanted: impl Fn() -> bool) -> bool {
        let _claiming = self.claiming.lock().unwrap_or_else(|e| e.into_inner());
        if self.keep() {
            return true;
        }
        match bus().claim(Claim::Sound, None, still_wanted) {
            Some(floor) => {
                *self.slot() = Some(floor);
                true
            }
            None => false,
        }
    }

    /// Whether the turn still holds the bus; renews its lease.
    pub fn keep(&self) -> bool {
        self.slot().as_ref().is_some_and(Floor::keep)
    }

    pub fn release(&self) {
        self.slot().take();
    }

    fn slot(&self) -> MutexGuard<'_, Option<Floor>> {
        self.floor.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn fresh() -> &'static AudioBus {
        Box::leak(Box::new(AudioBus::new()))
    }

    #[test]
    fn one_holder_at_a_time_and_the_next_gets_it_on_release() {
        let bus = fresh();
        let first = bus
            .claim(Claim::Sound, Some(Duration::ZERO), || true)
            .unwrap();
        assert!(bus.busy());
        assert!(
            bus.claim(Claim::Sound, Some(Duration::from_millis(60)), || true)
                .is_none(),
            "a second sound waits while the first holds the bus"
        );
        let waiter = std::thread::spawn(move || bus.claim(Claim::Sound, None, || true).is_some());
        std::thread::sleep(Duration::from_millis(50));
        drop(first);
        assert!(
            waiter.join().unwrap(),
            "the waiter gets the bus once it is free"
        );
    }

    #[test]
    fn a_waiter_stops_waiting_once_it_is_no_longer_wanted() {
        let bus = fresh();
        let _held = bus
            .claim(Claim::Sound, Some(Duration::ZERO), || true)
            .unwrap();
        let wanted = Arc::new(AtomicBool::new(true));
        let waiter = std::thread::spawn({
            let wanted = wanted.clone();
            move || {
                bus.claim(Claim::Sound, None, || wanted.load(Ordering::SeqCst))
                    .is_some()
            }
        });
        wanted.store(false, Ordering::SeqCst);
        assert!(!waiter.join().unwrap());
    }

    #[test]
    fn a_ping_waiting_goes_before_any_other_sound() {
        let bus = fresh();
        let held = bus
            .claim(Claim::Sound, Some(Duration::ZERO), || true)
            .unwrap();
        let ping = std::thread::spawn(move || {
            let floor = bus.claim(Claim::Ping, Some(Duration::from_secs(2)), || true);
            std::thread::sleep(Duration::from_millis(100));
            floor.is_some()
        });
        std::thread::sleep(Duration::from_millis(50));
        drop(held);
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            bus.claim(Claim::Sound, Some(Duration::ZERO), || true)
                .is_none(),
            "the ping took the free bus first"
        );
        assert!(ping.join().unwrap());
    }

    #[test]
    fn a_holder_that_lost_the_bus_cannot_release_the_next_ones() {
        let bus = fresh();
        let stale = bus
            .claim(Claim::Sound, Some(Duration::ZERO), || true)
            .unwrap();
        bus.state().holder.as_mut().unwrap().active_at = Instant::now() - LEASE;
        let next = bus
            .claim(Claim::Sound, Some(Duration::ZERO), || true)
            .unwrap();
        assert!(!stale.keep(), "the silent holder's lease lapsed");
        drop(stale);
        assert!(
            next.keep(),
            "dropping the stale floor leaves the new holder's"
        );
    }
}
