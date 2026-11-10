//! A bus is one shared device behind one door. A claim queues in its lane, one holder uses the
//! device at a time, a stronger lane waiting asks the holder to yield, and every change is
//! published, so nothing polls to learn whether the device is free.

use std::collections::VecDeque;
use std::fmt::Debug;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::{watch, Notify};

/// What asks for a bus. A higher lane is served first and asks a holder in a lower lane to yield;
/// within a lane, first come first served.
pub trait Lane: Copy + Debug + PartialEq + Send + Sync + 'static {
    fn lane(self) -> u8;
}

/// What the bus tells the rest of the process whenever it changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BusState<C> {
    pub holder: Option<C>,
    /// The claim served next.
    pub next: Option<C>,
    pub waiting: usize,
}

impl<C> Default for BusState<C> {
    fn default() -> Self {
        Self {
            holder: None,
            next: None,
            waiting: 0,
        }
    }
}

impl<C: Lane> BusState<C> {
    pub fn is_free(&self) -> bool {
        self.holder.is_none()
    }

    /// Whether a claim matching `which` holds the bus or is served next.
    pub fn has(&self, which: impl Fn(C) -> bool) -> bool {
        self.holder.is_some_and(&which) || self.next.is_some_and(&which)
    }
}

/// How often a waiter looks again for a reason to stop waiting.
const RECHECK: Duration = Duration::from_millis(25);
/// A wait this long is worth a warning naming the holder: a run whose end never came, say.
const LONG_WAIT: Duration = Duration::from_secs(60);

struct Holder<C> {
    id: u64,
    claim: C,
    since: Instant,
    /// A claim in a higher lane waits; [`Floor::keep`] answers false from now on.
    yielding: bool,
}

struct Ticket<C> {
    id: u64,
    claim: C,
}

struct State<C> {
    holder: Option<Holder<C>>,
    /// Highest lane first, first come first served within a lane.
    queue: VecDeque<Ticket<C>>,
    next_id: u64,
}

impl<C: Lane> State<C> {
    fn enqueue(&mut self, claim: C) -> u64 {
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

    /// The holder and the claim it now yields to, when the head of the queue outranks it.
    fn preempt(&mut self) -> Option<(C, C)> {
        let head = self.queue.front().map(|t| t.claim)?;
        let holder = self.holder.as_mut()?;
        if head.lane() > holder.claim.lane() && !holder.yielding {
            holder.yielding = true;
            return Some((holder.claim, head));
        }
        None
    }

    fn published(&self) -> BusState<C> {
        BusState {
            holder: self.holder.as_ref().map(|h| h.claim),
            next: self.queue.front().map(|t| t.claim),
            waiting: self.queue.len(),
        }
    }
}

pub struct Bus<C: Lane> {
    /// The trace `kind` every line about this bus carries.
    name: &'static str,
    state: Mutex<State<C>>,
    turned: Notify,
    asked_to_yield: Notify,
    published: watch::Sender<BusState<C>>,
}

impl<C: Lane> Bus<C> {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            state: Mutex::new(State {
                holder: None,
                queue: VecDeque::new(),
                next_id: 0,
            }),
            turned: Notify::new(),
            asked_to_yield: Notify::new(),
            published: watch::channel(BusState::default()).0,
        }
    }

    /// Who holds the bus and who waits, sent on every change.
    pub fn subscribe(&self) -> watch::Receiver<BusState<C>> {
        self.published.subscribe()
    }

    pub fn state(&self) -> BusState<C> {
        *self.published.borrow()
    }

    fn lock(&self) -> MutexGuard<'_, State<C>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn publish(&self, state: &State<C>) {
        self.published.send_replace(state.published());
    }

    fn ask_to_yield(&self, state: &mut State<C>) {
        if let Some((holder, to)) = state.preempt() {
            tracing::info!(
                target: "giap::trace",
                kind = self.name,
                event = "yield",
                claim = ?holder,
                to = ?to,
                "a bus asked its holder to yield"
            );
            self.asked_to_yield.notify_waiters();
        }
    }

    /// Joins the queue in `claim`'s lane and waits until it holds the bus, for at most `most`
    /// (`None`: for as long as `still_wanted` says). The only way to use the device. Dropping
    /// the future leaves the queue.
    pub async fn claim(
        &'static self,
        claim: C,
        most: Option<Duration>,
        still_wanted: impl Fn() -> bool + Send,
    ) -> Option<Floor<C>> {
        let asked = Instant::now();
        let deadline = most.map(|m| asked + m);
        let mut queued = Queued {
            bus: self,
            id: {
                let mut state = self.lock();
                let id = state.enqueue(claim);
                self.ask_to_yield(&mut state);
                self.publish(&state);
                id
            },
            served: false,
        };
        let mut warned = false;
        loop {
            let turned = self.turned.notified();
            tokio::pin!(turned);
            turned.as_mut().enable();
            let holder = {
                let mut state = self.lock();
                if state.holder.is_none() && state.queue.front().is_some_and(|t| t.id == queued.id)
                {
                    state.queue.pop_front();
                    state.holder = Some(Holder {
                        id: queued.id,
                        claim,
                        since: Instant::now(),
                        yielding: false,
                    });
                    self.ask_to_yield(&mut state);
                    self.publish(&state);
                    queued.served = true;
                    tracing::info!(
                        target: "giap::trace",
                        kind = self.name,
                        event = "held",
                        claim = ?claim,
                        waited_ms = asked.elapsed().as_millis() as u64,
                        waiting = state.queue.len(),
                        "a bus went to the head of its queue"
                    );
                    return Some(Floor {
                        bus: self,
                        id: queued.id,
                        claim,
                    });
                }
                state.holder.as_ref().map(|h| h.claim)
            };
            // Asked outside the lock: a caller's test may read this bus's state.
            let now = Instant::now();
            if !still_wanted() || deadline.is_some_and(|d| now >= d) {
                tracing::debug!(kind = self.name, claim = ?claim, "a claim left the queue unserved");
                return None;
            }
            if !warned && now.duration_since(asked) >= LONG_WAIT {
                warned = true;
                tracing::warn!(
                    kind = self.name,
                    claim = ?claim,
                    holder = ?holder,
                    "a claim has waited a minute for its bus"
                );
            }
            let _ = tokio::time::timeout(RECHECK, turned).await;
        }
    }

    fn keep(&self, id: u64) -> bool {
        self.lock()
            .holder
            .as_ref()
            .is_some_and(|h| h.id == id && !h.yielding)
    }

    fn release(&self, id: u64) {
        let mut state = self.lock();
        let Some(holder) = state.holder.take_if(|h| h.id == id) else {
            return;
        };
        self.publish(&state);
        drop(state);
        self.turned.notify_waiters();
        tracing::info!(
            target: "giap::trace",
            kind = self.name,
            event = "free",
            claim = ?holder.claim,
            held_ms = holder.since.elapsed().as_millis() as u64,
            yielded = holder.yielding,
            "a bus is free"
        );
    }
}

/// A place in the queue, given up if the claim stops waiting for any reason.
struct Queued<C: Lane> {
    bus: &'static Bus<C>,
    id: u64,
    served: bool,
}

impl<C: Lane> Drop for Queued<C> {
    fn drop(&mut self) {
        if self.served {
            return;
        }
        let mut state = self.bus.lock();
        state.queue.retain(|t| t.id != self.id);
        self.bus.publish(&state);
        drop(state);
        self.bus.turned.notify_waiters();
    }
}

/// Holding the bus. Dropping it frees the bus for the next claim.
pub struct Floor<C: Lane> {
    bus: &'static Bus<C>,
    id: u64,
    claim: C,
}

impl<C: Lane> Floor<C> {
    pub fn claim(&self) -> C {
        self.claim
    }

    /// Whether this floor still holds the bus with nobody stronger waiting for it.
    pub fn keep(&self) -> bool {
        self.bus.keep(self.id)
    }

    /// Resolves once a stronger claim waits for the bus.
    pub async fn yield_requested(&self) {
        loop {
            let asked = self.bus.asked_to_yield.notified();
            tokio::pin!(asked);
            asked.as_mut().enable();
            if !self.keep() {
                return;
            }
            asked.await;
        }
    }
}

impl<C: Lane> Drop for Floor<C> {
    fn drop(&mut self) {
        self.bus.release(self.id);
    }
}

/// A claim held across a run that uses the device in bursts, as a task step does between its
/// model calls. Asked to yield, it gives the bus up once no burst runs, and its next burst queues
/// again. Its run keeps everything else, so the bus is all that changes hands.
pub struct HeldFloor<C: Lane> {
    bus: &'static Bus<C>,
    claim: C,
    inner: Arc<Held<C>>,
}

impl<C: Lane> Clone for HeldFloor<C> {
    fn clone(&self) -> Self {
        Self {
            bus: self.bus,
            claim: self.claim,
            inner: self.inner.clone(),
        }
    }
}

struct Held<C: Lane> {
    floor: Mutex<Option<Floor<C>>>,
    claiming: tokio::sync::Mutex<()>,
    bursts: AtomicUsize,
}

impl<C: Lane> HeldFloor<C> {
    pub fn new(bus: &'static Bus<C>, claim: C) -> Self {
        Self {
            bus,
            claim,
            inner: Arc::new(Held {
                floor: Mutex::new(None),
                claiming: tokio::sync::Mutex::new(()),
                bursts: AtomicUsize::new(0),
            }),
        }
    }

    /// Holds the bus for one burst, queueing for it first when it was given up. `None` once
    /// `still_wanted` says the run is over.
    pub async fn burst(&self, still_wanted: impl Fn() -> bool + Send) -> Option<Burst<C>> {
        let _claiming = self.inner.claiming.lock().await;
        if !self.holds() {
            // A burst still running frees a yielding floor itself when it ends.
            if self.inner.bursts.load(Ordering::SeqCst) == 0 {
                self.slot().take();
            }
            let floor = self.bus.claim(self.claim, None, still_wanted).await?;
            *self.slot() = Some(floor);
        }
        self.inner.bursts.fetch_add(1, Ordering::SeqCst);
        Some(Burst { held: self.clone() })
    }

    /// Whether the run holds the bus with nobody stronger waiting: what a burst asks between its
    /// tokens.
    pub fn holds(&self) -> bool {
        self.slot().as_ref().is_some_and(Floor::keep)
    }

    /// Gives the bus up whenever it is asked to and no burst runs. Run it beside the run.
    pub async fn give_way(&self) {
        loop {
            let asked = self.bus.asked_to_yield.notified();
            tokio::pin!(asked);
            asked.as_mut().enable();
            self.give_up_if_asked();
            asked.await;
        }
    }

    fn give_up_if_asked(&self) {
        if self.inner.bursts.load(Ordering::SeqCst) > 0 {
            return;
        }
        let mut slot = self.slot();
        if slot.as_ref().is_some_and(|f| !f.keep()) {
            let floor = slot.take();
            drop(slot);
            drop(floor);
        }
    }

    fn slot(&self) -> MutexGuard<'_, Option<Floor<C>>> {
        self.inner.floor.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One burst of a [`HeldFloor`]'s run, holding the bus until it ends.
pub struct Burst<C: Lane> {
    held: HeldFloor<C>,
}

impl<C: Lane> Burst<C> {
    /// Whether the burst may go on: the stop it asks between its tokens.
    pub fn keep(&self) -> bool {
        self.held.holds()
    }
}

impl<C: Lane> Drop for Burst<C> {
    fn drop(&mut self) {
        self.held.inner.bursts.fetch_sub(1, Ordering::SeqCst);
        self.held.give_up_if_asked();
    }
}

#[cfg(test)]
mod tests;
