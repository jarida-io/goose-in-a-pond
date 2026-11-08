//! The speaker as one resource for the whole process, used like a critical section: one stream,
//! opened on first use and kept open (CoreAudio degrades with open/close churn, ALSA `hw` refuses
//! a second open), one holder at a time, a queue that hands it on, and its state published so the
//! rest of the pond knows when it is free without asking.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use rodio::cpal::FromSample;
use rodio::{Sample, Source};
use tokio::sync::watch;

use crate::{tap, Speaker};

/// What is asking for the bus: it names the holder in [`BusState`] and picks the queue's lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// The wake ping answers the person, so it has a lane ahead of everything else waiting.
    Ping,
    /// A voice turn, from its first sound (the working tone or a sentence) to its end.
    Turn,
    /// A reminder or a schedule's answer, said outside any turn.
    Notice,
    /// The pairing code read aloud.
    Code,
    /// The startup chime, or a device's chime once it is chosen.
    Chime,
    /// A sound test of a speaker and a microphone.
    Test,
    /// A speaker probe: silence, and on Linux the card suspended in the sound server.
    Probe,
}

/// What the bus tells the rest of the process whenever it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BusState {
    pub holder: Option<Claim>,
    pub waiting: usize,
}

impl BusState {
    pub fn is_free(&self) -> bool {
        self.holder.is_none()
    }
}

/// A holder silent this long is taken to have died holding the bus, and the head of the queue
/// takes over. A turn's tone and sentences renew it as they play; only a turn's silence between
/// sentences, a long tool call, comes near it.
const LEASE: Duration = Duration::from_secs(30);
/// How often a waiter looks again, for a lapsed lease or a reason to stop waiting.
const RECHECK: Duration = Duration::from_millis(25);
/// How often a one-shot sound checks it still holds the bus.
const POLL: Duration = Duration::from_millis(25);

struct Holder {
    id: u64,
    claim: Claim,
    since: Instant,
    active_at: Instant,
}

struct Ticket {
    id: u64,
    claim: Claim,
}

#[derive(Default)]
struct State {
    holder: Option<Holder>,
    queue: VecDeque<Ticket>,
    next_id: u64,
}

impl State {
    /// Joins the queue: a ping behind the pings already waiting, anything else at the back.
    fn enqueue(&mut self, claim: Claim) -> u64 {
        self.next_id += 1;
        let ticket = Ticket {
            id: self.next_id,
            claim,
        };
        match claim {
            Claim::Ping => {
                let at = self
                    .queue
                    .iter()
                    .take_while(|t| t.claim == Claim::Ping)
                    .count();
                self.queue.insert(at, ticket);
            }
            _ => self.queue.push_back(ticket),
        }
        self.next_id
    }

    fn published(&self) -> BusState {
        BusState {
            holder: self.holder.as_ref().map(|h| h.claim),
            waiting: self.queue.len(),
        }
    }
}

pub struct AudioBus {
    state: Mutex<State>,
    turned: Condvar,
    published: watch::Sender<BusState>,
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
            published: watch::Sender::new(BusState::default()),
            speaker: Mutex::new(None),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(&self, state: &State) {
        let now = state.published();
        self.published.send_if_modified(|was| {
            let changed = *was != now;
            *was = now;
            changed
        });
    }

    /// Who holds the bus and how many wait, as it changes; the way to learn the bus is free.
    pub fn subscribe(&self) -> watch::Receiver<BusState> {
        self.published.subscribe()
    }

    /// Joins the queue and blocks until this claim holds the bus, for at most `most` (`None`: for
    /// as long as `still_wanted` says). The only way to make a sound.
    pub fn claim(
        &'static self,
        claim: Claim,
        most: Option<Duration>,
        still_wanted: impl Fn() -> bool,
    ) -> Option<Floor> {
        let asked = Instant::now();
        let deadline = most.map(|m| asked + m);
        let mut state = self.state();
        let id = state.enqueue(claim);
        self.publish(&state);
        let granted = loop {
            let now = Instant::now();
            if state.queue.front().is_some_and(|t| t.id == id) {
                let free = match &state.holder {
                    None => true,
                    Some(h) if now.duration_since(h.active_at) >= LEASE => {
                        tracing::warn!(
                            holder = ?h.claim,
                            "audio bus: the holder went silent past its lease; the queue moves on"
                        );
                        true
                    }
                    Some(_) => false,
                };
                if free {
                    state.queue.pop_front();
                    state.holder = Some(Holder {
                        id,
                        claim,
                        since: now,
                        active_at: now,
                    });
                    break true;
                }
            }
            if !still_wanted() || deadline.is_some_and(|d| now >= d) {
                state.queue.retain(|t| t.id != id);
                break false;
            }
            state = self
                .turned
                .wait_timeout(state, RECHECK)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        };
        self.publish(&state);
        let waiting = state.queue.len();
        drop(state);
        self.turned.notify_all();
        if !granted {
            tracing::debug!(claim = ?claim, "audio bus: a claim left the queue unserved");
            return None;
        }
        tracing::info!(
            target: "giap::trace",
            kind = "audio_bus",
            event = "held",
            claim = ?claim,
            waited_ms = asked.elapsed().as_millis() as u64,
            waiting,
            "the audio bus went to the head of its queue"
        );
        Some(Floor { bus: self, id })
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
        let Some(holder) = state.holder.take_if(|h| h.id == id) else {
            return;
        };
        self.publish(&state);
        drop(state);
        self.turned.notify_all();
        tracing::info!(
            target: "giap::trace",
            kind = "audio_bus",
            event = "free",
            claim = ?holder.claim,
            held_ms = holder.since.elapsed().as_millis() as u64,
            "the audio bus is free"
        );
    }
}

impl Floor {
    /// Whether this still holds the bus; renews its lease.
    pub fn keep(&self) -> bool {
        self.bus.keep(self.id)
    }

    /// Plays mono `samples` to the end, or until this no longer holds the bus.
    pub fn play(&self, samples: Vec<f32>, rate: u32, volume: f32) -> Result<()> {
        self.play_source(rodio::buffer::SamplesBuffer::new(1, rate, samples).amplify(volume))
    }

    /// Plays a WAV to the end, or until this no longer holds the bus.
    pub fn play_wav(&self, wav: Vec<u8>) -> Result<()> {
        self.play_source(rodio::Decoder::new(std::io::Cursor::new(wav))?)
    }

    fn play_source<S>(&self, source: S) -> Result<()>
    where
        S: Source + Send + 'static,
        S::Item: Sample + Send,
        f32: FromSample<S::Item>,
    {
        let speaker = self.bus.speaker()?;
        let generation = speaker.generation();
        let sink = rodio::Sink::try_new(&speaker.handle())?;
        sink.append(tap::tapped(source));
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
    /// One claim at a time, so a turn's tone and first sentence never queue behind each other.
    claiming: Arc<Mutex<()>>,
}

impl TurnFloor {
    /// Holds the bus for the turn, queueing while `still_wanted`; false once the turn gave up.
    pub fn hold(&self, still_wanted: impl Fn() -> bool) -> bool {
        let _claiming = self.claiming.lock().unwrap_or_else(|e| e.into_inner());
        if self.keep() {
            return true;
        }
        match bus().claim(Claim::Turn, None, still_wanted) {
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

    fn now(bus: &'static AudioBus, claim: Claim) -> Option<Floor> {
        bus.claim(claim, Some(Duration::ZERO), || true)
    }

    /// Blocks until `waiting` claims are queued, as the bus publishes it.
    fn until_waiting(bus: &AudioBus, waiting: usize) {
        let state = bus.subscribe();
        let deadline = Instant::now() + Duration::from_secs(5);
        while state.borrow().waiting != waiting {
            assert!(Instant::now() < deadline, "never saw {waiting} waiting");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn waiters_hold_the_bus_one_at_a_time_in_the_order_they_asked() {
        let bus = fresh();
        let first = now(bus, Claim::Turn).unwrap();
        assert!(
            now(bus, Claim::Notice).is_none(),
            "nobody plays over a holder"
        );
        let order = Arc::new(Mutex::new(Vec::new()));
        let waiter = |claim: Claim| {
            let order = order.clone();
            std::thread::spawn(move || {
                let floor = bus.claim(claim, None, || true).unwrap();
                order.lock().unwrap().push(claim);
                std::thread::sleep(Duration::from_millis(20));
                drop(floor);
            })
        };
        let notice = waiter(Claim::Notice);
        until_waiting(bus, 1);
        let chime = waiter(Claim::Chime);
        until_waiting(bus, 2);
        drop(first);
        notice.join().unwrap();
        chime.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec![Claim::Notice, Claim::Chime]);
    }

    #[test]
    fn a_ping_waiting_goes_before_the_rest_of_the_queue() {
        let bus = fresh();
        let first = now(bus, Claim::Turn).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let waiter = |claim: Claim| {
            let order = order.clone();
            std::thread::spawn(move || {
                let _floor = bus.claim(claim, None, || true).unwrap();
                order.lock().unwrap().push(claim);
            })
        };
        let notice = waiter(Claim::Notice);
        until_waiting(bus, 1);
        let ping = waiter(Claim::Ping);
        until_waiting(bus, 2);
        drop(first);
        notice.join().unwrap();
        ping.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec![Claim::Ping, Claim::Notice]);
    }

    #[test]
    fn a_claim_no_longer_wanted_leaves_the_queue() {
        let bus = fresh();
        let _held = now(bus, Claim::Turn).unwrap();
        let wanted = Arc::new(AtomicBool::new(true));
        let waiter = std::thread::spawn({
            let wanted = wanted.clone();
            move || {
                bus.claim(Claim::Notice, None, || wanted.load(Ordering::SeqCst))
                    .is_some()
            }
        });
        until_waiting(bus, 1);
        wanted.store(false, Ordering::SeqCst);
        assert!(!waiter.join().unwrap());
        assert_eq!(bus.subscribe().borrow().waiting, 0);
    }

    #[test]
    fn the_bus_tells_its_subscribers_who_holds_it_and_when_it_is_free() {
        let bus = fresh();
        let mut state = bus.subscribe();
        assert!(state.borrow_and_update().is_free());
        let floor = now(bus, Claim::Chime).unwrap();
        assert!(state.has_changed().unwrap());
        assert_eq!(state.borrow_and_update().holder, Some(Claim::Chime));
        drop(floor);
        assert!(state.has_changed().unwrap());
        assert!(state.borrow_and_update().is_free());
    }

    #[test]
    fn a_holder_that_lost_the_bus_cannot_release_the_next_ones() {
        let bus = fresh();
        let stale = now(bus, Claim::Turn).unwrap();
        bus.state().holder.as_mut().unwrap().active_at = Instant::now() - LEASE;
        let next = now(bus, Claim::Notice).unwrap();
        assert!(!stale.keep(), "the silent holder's lease lapsed");
        drop(stale);
        assert!(
            next.keep(),
            "dropping the stale floor leaves the new holder's"
        );
    }
}
