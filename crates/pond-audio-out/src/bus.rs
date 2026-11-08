//! The speaker as one resource for the whole process, used like a critical section: one stream,
//! opened early and kept open (CoreAudio degrades with open/close churn, ALSA `hw` refuses a
//! second open), one holder at a time, a queue in lanes that hands it on, and its state published
//! so the rest of the pond knows when it is held and when it is free without asking.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use pond_core::models::services::voice::quiet::{self, WAIT_FOR_QUIET};
use rodio::cpal::FromSample;
use rodio::{Sample, Source};
use tokio::sync::watch;

use crate::{tap, Speaker};

/// What is asking for the bus. It names the holder in [`BusState`] and sets the claim's lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    /// The wake ping answers the person.
    Ping,
    /// A voice turn, from `begin_utterance` to `end_utterance`, its thinking included.
    Turn,
    /// Words said whole outside a turn: a greeting, a farewell, unprompted speech.
    Speech,
    /// A reminder or a schedule's answer.
    Notice,
    /// The pairing code read aloud.
    Code,
    /// A voice sample the household asked to hear.
    Sample,
    /// The startup chime, or a device's chime once it is chosen.
    Chime,
    /// A sound test of a speaker and a microphone.
    Test,
    /// A speaker probe: silence, and on Linux the card suspended in the sound server.
    Probe,
}

impl Claim {
    /// Which lane of the queue: a waiter in a higher lane goes first and asks a holder in a lower
    /// one to yield. The person comes first, then what is said to them, then everything else.
    fn lane(self) -> u8 {
        match self {
            Claim::Ping => 2,
            Claim::Turn | Claim::Speech => 1,
            _ => 0,
        }
    }
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

/// How often a waiter looks again for a reason to stop waiting.
const RECHECK: Duration = Duration::from_millis(25);
/// How often a one-shot sound checks it still holds the bus.
const POLL: Duration = Duration::from_millis(25);
/// A wait this long is worth a warning naming the holder: a turn whose end never came, say.
const LONG_WAIT: Duration = Duration::from_secs(60);

struct Holder {
    id: u64,
    claim: Claim,
    since: Instant,
    /// A waiter in a higher lane asked for the bus; [`Floor::keep`] answers false from now on.
    yielding: bool,
}

struct Ticket {
    id: u64,
    claim: Claim,
}

#[derive(Default)]
struct State {
    holder: Option<Holder>,
    /// Highest lane first, first come first served within a lane.
    queue: VecDeque<Ticket>,
    next_id: u64,
}

impl State {
    fn enqueue(&mut self, claim: Claim) -> u64 {
        self.next_id += 1;
        let at = self
            .queue
            .iter()
            .take_while(|t| t.claim.lane() >= claim.lane())
            .count();
        self.queue.insert(
            at,
            Ticket {
                id: self.next_id,
                claim,
            },
        );
        self.next_id
    }

    /// Asks the holder to yield when the head of the queue outranks it.
    fn preempt(&mut self) {
        let head = self.queue.front().map(|t| t.claim);
        if let (Some(head), Some(holder)) = (head, self.holder.as_mut()) {
            if head.lane() > holder.claim.lane() && !holder.yielding {
                holder.yielding = true;
                tracing::info!(
                    target: "giap::trace",
                    kind = "audio_bus",
                    event = "yield",
                    claim = ?holder.claim,
                    to = ?head,
                    "the audio bus asked its holder to yield"
                );
            }
        }
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
    held: watch::Sender<bool>,
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
    settled: AtomicBool,
}

impl AudioBus {
    fn new() -> Self {
        Self {
            state: Mutex::default(),
            turned: Condvar::new(),
            published: watch::Sender::new(BusState::default()),
            held: watch::Sender::new(false),
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
        self.held.send_if_modified(|was| {
            let changed = *was == now.is_free();
            *was = !now.is_free();
            changed
        });
    }

    /// Who holds the bus and how many wait, as it changes.
    pub fn subscribe(&self) -> watch::Receiver<BusState> {
        self.published.subscribe()
    }

    /// Whether anything holds the bus, as it changes: what other audio follows to stay quiet.
    pub fn subscribe_held(&self) -> watch::Receiver<bool> {
        self.held.subscribe()
    }

    /// Opens the speaker on a thread of its own, so the first sound does not wait for the device.
    pub fn open_early(&'static self) {
        let _ = std::thread::Builder::new()
            .name("audio-bus-open".into())
            .spawn(move || {
                if let Err(e) = self.speaker() {
                    tracing::debug!(error = %e, "audio bus: no speaker yet; the first sound retries");
                }
            });
    }

    /// Joins the queue in its lane and blocks until this claim holds the bus, for at most `most`
    /// (`None`: for as long as `still_wanted` says). The only way to make a sound.
    pub fn claim(
        &'static self,
        claim: Claim,
        most: Option<Duration>,
        still_wanted: impl Fn() -> bool,
    ) -> Option<Floor> {
        let asked = Instant::now();
        let deadline = most.map(|m| asked + m);
        let mut warned = false;
        let mut state = self.state();
        let id = state.enqueue(claim);
        state.preempt();
        self.publish(&state);
        let granted = loop {
            if state.holder.is_none() && state.queue.front().is_some_and(|t| t.id == id) {
                state.queue.pop_front();
                state.holder = Some(Holder {
                    id,
                    claim,
                    since: Instant::now(),
                    yielding: false,
                });
                state.preempt();
                break true;
            }
            let now = Instant::now();
            if !still_wanted() || deadline.is_some_and(|d| now >= d) {
                state.queue.retain(|t| t.id != id);
                break false;
            }
            if !warned && now.duration_since(asked) >= LONG_WAIT {
                warned = true;
                tracing::warn!(
                    claim = ?claim,
                    holder = ?state.holder.as_ref().map(|h| h.claim),
                    "audio bus: a claim has waited a minute for the speaker"
                );
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
        Some(Floor {
            bus: self,
            id,
            settled: AtomicBool::new(false),
        })
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

    /// The stream every sound plays on; a failed open is retried by the next sound.
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
        self.state()
            .holder
            .as_ref()
            .is_some_and(|h| h.id == id && !h.yielding)
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
            yielded = holder.yielding,
            "the audio bus is free"
        );
    }
}

impl Floor {
    /// Whether this still holds the bus and nobody of a higher lane wants it back.
    pub fn keep(&self) -> bool {
        self.bus.keep(self.id)
    }

    /// Waits, once per hold and at most `most`, for other audio to pause: the bus being held is
    /// what pauses it (see [`AudioBus::subscribe_held`]).
    pub fn settle(&self, most: Duration) {
        if self.settled.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(quiet) = quiet::installed() {
            quiet.until_quiet_blocking(most);
        }
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
        self.settle(WAIT_FOR_QUIET);
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

/// The bus held for a turn: queued for as the turn begins, kept through its thinking and the
/// pauses between its sentences, and given back when it ends, is cut off, or must yield.
#[derive(Clone, Default)]
pub struct TurnFloor {
    floor: Arc<Mutex<Option<Floor>>>,
    /// One claim at a time, so a turn's own claims never queue behind each other.
    claiming: Arc<Mutex<()>>,
}

impl TurnFloor {
    /// Queues for the bus as the turn begins, on a thread of its own, and returns at once.
    pub fn begin(&self, still_wanted: impl Fn() -> bool + Send + 'static) {
        let turn = self.clone();
        let _ = std::thread::Builder::new()
            .name("turn-floor".into())
            .spawn(move || {
                turn.hold(still_wanted);
            });
    }

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

    /// Whether the turn still holds the bus. A turn asked to yield gives it up here, at once.
    pub fn keep(&self) -> bool {
        let mut slot = self.slot();
        if slot.as_ref().is_some_and(Floor::keep) {
            return true;
        }
        slot.take();
        false
    }

    /// Waits once per hold for other audio to pause; see [`Floor::settle`].
    pub fn settle(&self, most: Duration) {
        if let Some(floor) = self.slot().as_ref() {
            floor.settle(most);
        }
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

    /// Each claim, once it holds the bus, notes itself and plays for 20 ms.
    fn waiter(
        bus: &'static AudioBus,
        claim: Claim,
        order: &Arc<Mutex<Vec<Claim>>>,
    ) -> std::thread::JoinHandle<()> {
        let order = order.clone();
        std::thread::spawn(move || {
            let floor = bus.claim(claim, None, || true).unwrap();
            order.lock().unwrap().push(claim);
            std::thread::sleep(Duration::from_millis(20));
            drop(floor);
        })
    }

    #[test]
    fn a_lane_is_served_in_the_order_its_claims_asked() {
        let bus = fresh();
        let first = now(bus, Claim::Notice).unwrap();
        assert!(
            now(bus, Claim::Chime).is_none(),
            "nobody plays over a holder"
        );
        let order = Arc::new(Mutex::new(Vec::new()));
        let chime = waiter(bus, Claim::Chime, &order);
        until_waiting(bus, 1);
        let code = waiter(bus, Claim::Code, &order);
        until_waiting(bus, 2);
        drop(first);
        chime.join().unwrap();
        code.join().unwrap();
        assert_eq!(*order.lock().unwrap(), vec![Claim::Chime, Claim::Code]);
    }

    #[test]
    fn the_ping_then_a_turn_go_before_the_background() {
        let bus = fresh();
        let first = now(bus, Claim::Turn).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let notice = waiter(bus, Claim::Notice, &order);
        until_waiting(bus, 1);
        let speech = waiter(bus, Claim::Speech, &order);
        until_waiting(bus, 2);
        let ping = waiter(bus, Claim::Ping, &order);
        until_waiting(bus, 3);
        drop(first);
        for w in [notice, speech, ping] {
            w.join().unwrap();
        }
        assert_eq!(
            *order.lock().unwrap(),
            vec![Claim::Ping, Claim::Speech, Claim::Notice]
        );
    }

    #[test]
    fn a_higher_lane_waiting_asks_the_holder_to_yield_and_a_lower_one_does_not() {
        let bus = fresh();
        let notice = now(bus, Claim::Notice).unwrap();
        let chime = std::thread::spawn(move || {
            bus.claim(Claim::Chime, Some(Duration::from_millis(100)), || true)
                .is_some()
        });
        until_waiting(bus, 1);
        assert!(notice.keep(), "a chime does not cut a notice");
        assert!(!chime.join().unwrap());

        let turn = std::thread::spawn(move || bus.claim(Claim::Turn, None, || true).is_some());
        until_waiting(bus, 1);
        assert!(!notice.keep(), "a turn waiting asks the notice to yield");
        drop(notice);
        assert!(turn.join().unwrap());
    }

    #[test]
    fn a_turn_asked_to_yield_gives_the_bus_up_and_queues_again_for_its_next_sound() {
        let bus = fresh();
        let holder = now(bus, Claim::Turn).unwrap();
        let turn = TurnFloor::default();
        *turn.slot() = Some(holder);
        let ping = std::thread::spawn(move || {
            let floor = bus.claim(Claim::Ping, Some(Duration::from_secs(2)), || true);
            std::thread::sleep(Duration::from_millis(30));
            floor.is_some()
        });
        until_waiting(bus, 1);
        assert!(!turn.keep(), "the ping outranks the turn");
        assert!(ping.join().unwrap());
        assert!(bus.subscribe().borrow().is_free());
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
        let mut held = bus.subscribe_held();
        assert!(state.borrow_and_update().is_free());
        assert!(!*held.borrow_and_update());
        let floor = now(bus, Claim::Chime).unwrap();
        assert_eq!(state.borrow_and_update().holder, Some(Claim::Chime));
        assert!(held.has_changed().unwrap() && *held.borrow_and_update());
        drop(floor);
        assert!(state.borrow_and_update().is_free());
        assert!(held.has_changed().unwrap() && !*held.borrow_and_update());
    }
}
