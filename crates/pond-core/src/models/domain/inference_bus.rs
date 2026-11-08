//! The on-device model's bus. The model has one context, so one claim uses it at a time: a
//! person's turn first, then the work a member asked for, then the pond's own chores. A holder in
//! a lower lane is asked to yield when a higher one waits, and stops at its next token.

use std::sync::{Arc, OnceLock, Weak};

use pond_bus::{Burst, Bus, Floor, HeldFloor, Lane};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InferenceClaim {
    /// A person's turn, from its start to its end, its own helpers included.
    Turn,
    /// Work a member asked for: a long task's step.
    Task,
    /// The pond's own background work: titles, summaries, memory, suggestions, compiles.
    Chore,
}

impl Lane for InferenceClaim {
    fn lane(self) -> u8 {
        match self {
            InferenceClaim::Turn => 2,
            InferenceClaim::Task => 1,
            InferenceClaim::Chore => 0,
        }
    }
}

pub type InferenceBus = Bus<InferenceClaim>;
pub type InferenceFloor = Floor<InferenceClaim>;
pub type InferenceHeldFloor = HeldFloor<InferenceClaim>;

pub fn inference_bus() -> &'static InferenceBus {
    static BUS: OnceLock<InferenceBus> = OnceLock::new();
    BUS.get_or_init(|| Bus::new("inference_bus"))
}

/// Whether a person's turn holds the model or is served next.
pub fn turn_has_the_model() -> bool {
    inference_bus()
        .state()
        .has(|claim| claim == InferenceClaim::Turn)
}

/// A turn claimed at the wake, before its words are heard, so a holder drains while the person is
/// still speaking. The turn that follows takes it over ([`take_early_turn`]).
struct EarlyTurn {
    floor: Option<Arc<InferenceFloor>>,
    /// Bumped by every wake and every hand-over, so a claim still queued for an older wake leaves.
    generation: u64,
}

fn early_turn() -> std::sync::MutexGuard<'static, EarlyTurn> {
    static EARLY: std::sync::Mutex<EarlyTurn> = std::sync::Mutex::new(EarlyTurn {
        floor: None,
        generation: 0,
    });
    EARLY.lock().unwrap_or_else(|e| e.into_inner())
}

/// Claims the model for the turn a wake starts, held for at most `unused_for` if no turn takes it.
pub fn claim_turn_early(unused_for: std::time::Duration) {
    let generation = {
        let mut early = early_turn();
        early.generation += 1;
        early.floor = None;
        early.generation
    };
    let current = move || early_turn().generation == generation;
    tokio::spawn(async move {
        let Some(floor) = inference_bus()
            .claim(InferenceClaim::Turn, Some(unused_for), current)
            .await
        else {
            return;
        };
        {
            let mut early = early_turn();
            if early.generation != generation {
                return;
            }
            early.floor = Some(Arc::new(floor));
        }
        tokio::time::sleep(unused_for).await;
        let mut early = early_turn();
        if early.generation == generation {
            early.floor = None;
        }
    });
}

/// The floor a wake claimed for this turn, if it is held; any claim still queued for it leaves.
pub fn take_early_turn() -> Option<Arc<InferenceFloor>> {
    let mut early = early_turn();
    early.generation += 1;
    early.floor.take()
}

/// What an engine call runs under, carried from whoever holds the bus to the door the call
/// passes. Without one, the call claims the bus for itself in its door's lane.
#[derive(Clone)]
pub enum InferenceHold {
    /// A turn's floor, owned by the turn: weak, so it frees the moment the turn ends.
    Floor(Weak<InferenceFloor>),
    /// A run that holds the bus between its calls and gives way when asked.
    Held(InferenceHeldFloor),
    /// Each call claims the bus for itself in this lane.
    Lane(InferenceClaim),
}

tokio::task_local! {
    static RUNNING_UNDER: InferenceHold;
}

/// Runs `f` with every engine call it makes running under `hold`.
pub async fn running_under<F: std::future::Future>(hold: InferenceHold, f: F) -> F::Output {
    RUNNING_UNDER.scope(hold, f).await
}

/// The hold the current task runs under; `None` outside [`running_under`]. A task-local does not
/// follow `tokio::spawn`: read it before handing work to another task.
pub fn current_hold() -> Option<InferenceHold> {
    RUNNING_UNDER.try_with(Clone::clone).ok()
}

/// Asked between a call's tokens: true once the call should stop and let the bus go.
pub type StopWhen = Arc<dyn Fn() -> bool + Send + Sync>;

/// What one engine call holds while it runs.
pub enum CallHold {
    Turn(Arc<InferenceFloor>),
    Burst(Burst<InferenceClaim>, InferenceHeldFloor),
    Own(Arc<InferenceFloor>),
}

impl CallHold {
    /// The stop the engine asks between tokens.
    pub fn stop_when(&self) -> StopWhen {
        match self {
            CallHold::Turn(floor) | CallHold::Own(floor) => {
                let floor = floor.clone();
                Arc::new(move || !floor.keep())
            }
            CallHold::Burst(_, held) => {
                let held = held.clone();
                Arc::new(move || !held.holds())
            }
        }
    }

    /// Whether the call was asked to stop: its answer, if it ended now, is cut.
    pub fn was_cut(&self) -> bool {
        match self {
            CallHold::Turn(floor) | CallHold::Own(floor) => !floor.keep(),
            CallHold::Burst(burst, _) => !burst.keep(),
        }
    }

    /// Whether the call can be asked to stop: anything below a person's turn.
    pub fn can_be_cut(&self) -> bool {
        match self {
            CallHold::Turn(_) => false,
            CallHold::Burst(..) => true,
            CallHold::Own(floor) => floor.claim() != InferenceClaim::Turn,
        }
    }
}

/// Takes the bus for one engine call: under `hold` when one is given, else claiming it in
/// `fallback`'s lane. Waits in the queue for as long as the caller does; dropping the future
/// leaves the queue.
pub async fn hold_for_call(hold: Option<InferenceHold>, fallback: InferenceClaim) -> CallHold {
    let own = |lane| async move {
        let floor = inference_bus()
            .claim(lane, None, || true)
            .await
            .expect("a claim with no deadline that is always wanted is served");
        CallHold::Own(Arc::new(floor))
    };
    match hold {
        Some(InferenceHold::Floor(turn)) => match turn.upgrade() {
            Some(floor) => CallHold::Turn(floor),
            None => own(fallback).await,
        },
        Some(InferenceHold::Held(held)) => match held.burst(|| true).await {
            Some(burst) => CallHold::Burst(burst, held),
            None => own(fallback).await,
        },
        Some(InferenceHold::Lane(lane)) => own(lane).await,
        None => own(fallback).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_turn_comes_before_a_task_and_a_task_before_a_chore() {
        assert!(InferenceClaim::Turn.lane() > InferenceClaim::Task.lane());
        assert!(InferenceClaim::Task.lane() > InferenceClaim::Chore.lane());
    }

    #[tokio::test]
    async fn a_call_under_a_turn_uses_the_turn_floor_and_claims_nothing_more() {
        let bus = inference_bus();
        let turn = Arc::new(
            bus.claim(InferenceClaim::Turn, Some(Duration::from_secs(5)), || true)
                .await
                .unwrap(),
        );
        let hold = InferenceHold::Floor(Arc::downgrade(&turn));
        let call = tokio::time::timeout(
            Duration::from_secs(1),
            hold_for_call(Some(hold), InferenceClaim::Turn),
        )
        .await
        .expect("a call under its own turn never waits for that turn");
        assert!(matches!(call, CallHold::Turn(_)));
        assert!(!call.stop_when()(), "nothing outranks a turn");
        assert!(!call.can_be_cut());
        drop(call);
        drop(turn);
        assert!(bus.state().is_free(), "the turn's end frees the bus");
    }
}
