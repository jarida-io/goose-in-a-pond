//! Agent turns that outlive their connection: every SSE body (the POST and each reattach)
//! replays the [`RunHandle`] frame ring, then tails its broadcast. Runs die with the process.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

/// A UUID, kept as a string because it is only compared, logged and put in URLs.
pub type RunId = String;

/// Replay ring length: a long answer's tokens plus tool and thinking frames, across a reload.
const MAX_FRAMES: usize = 2048;

/// Byte cap on the ring too: one tool result can carry a base64 image.
const MAX_BYTES: usize = 1024 * 1024;

/// Live fan-out depth. Must stay below [`MAX_FRAMES`] so whatever a lagging subscriber misses
/// is still in the ring: `RecvError::Lagged` means "re-read", not a transcript hole.
const BROADCAST_CAP: usize = 256;

const _: () = assert!(
    BROADCAST_CAP < MAX_FRAMES,
    "the replay ring must hold strictly more than the broadcast queue can drop, \
     or a lagging subscriber has no way back"
);

/// How long a finished run stays reattachable; a client restart with re-auth takes ~20s.
pub const DEFAULT_RETENTION: Duration = Duration::from_secs(120);

/// How many detached runs may be in flight at once.
pub const DEFAULT_MAX_RUNS: usize = 8;

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    /// Drained normally. Says nothing about whether the answer was any good.
    Finished,
    /// The agent stream errored, or the turn hit its idle timeout.
    Failed,
    /// Somebody asked it to stop, or an `Ephemeral` run was abandoned.
    Cancelled,
}

impl RunState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, RunState::Running)
    }
}

/// Who may reattach to a run, captured from the request that started it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOwner {
    /// The bearer token named a paired device.
    Device(String),
    /// No device named (loopback dev bypass, unattributed token): reattach needs any valid token.
    Unattributed,
}

/// Whether being abandoned ends a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPolicy {
    /// Last subscriber out cancels the turn. Voice needs this: `WebVoiceBackend` aborts
    /// speculative `/chat/stream` turns, which must not persist an answer to a half-sentence.
    Ephemeral,
    /// Nobody listening is not a reason to stop.
    Detached,
}

// ── Frames ────────────────────────────────────────────────────────────────────

/// One SSE frame as sent; `payload` is the `data:` line (never re-parsed), `seq` rides in `id:`.
#[derive(Clone, Debug)]
pub struct RunFrame {
    pub seq: u64,
    pub payload: Arc<str>,
    /// The run's last frame; a subscriber that sees it may close.
    pub terminal: bool,
}

/// What a subscriber gets when it attaches or recovers.
#[derive(Debug)]
pub struct Snapshot {
    pub frames: Vec<RunFrame>,
    /// `Some(first_still_held)` when the asked-for position was evicted: the caller must reload.
    pub gap: Option<u64>,
    pub state: RunState,
    pub last_seq: u64,
    /// Whether the terminal frame is among `frames`.
    pub saw_terminal: bool,
}

struct RunBuffer {
    frames: VecDeque<RunFrame>,
    bytes: usize,
    next_seq: u64,
    /// Lowest sequence still held; lets a request for an evicted one report a gap.
    first_seq: u64,
    state: RunState,
    finished_at: Option<Instant>,
}

// ── The handle ────────────────────────────────────────────────────────────────

/// One agent turn, and everything anyone needs to follow it.
pub struct RunHandle {
    pub run_id: RunId,
    pub session_id: String,
    pub owner: RunOwner,
    pub policy: RunPolicy,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Fired by an explicit cancel or the last subscriber leaving an `Ephemeral` run. The turn
    /// task selects on it; dropping the agent stream then fires the adapter's `DropGuard`.
    pub cancel: CancellationToken,
    tx: broadcast::Sender<RunFrame>,
    attached: AtomicUsize,
    buf: Mutex<RunBuffer>,
}

impl RunHandle {
    pub fn new(session_id: String, owner: RunOwner, policy: RunPolicy) -> Arc<Self> {
        let (tx, _) = broadcast::channel(BROADCAST_CAP);
        Arc::new(Self {
            run_id: uuid::Uuid::new_v4().to_string(),
            session_id,
            owner,
            policy,
            started_at: chrono::Utc::now(),
            cancel: CancellationToken::new(),
            tx,
            attached: AtomicUsize::new(0),
            buf: Mutex::new(RunBuffer {
                frames: VecDeque::new(),
                bytes: 0,
                // 1-based so `after_seq = 0` can only mean "from the beginning".
                next_seq: 1,
                first_seq: 1,
                state: RunState::Running,
                finished_at: None,
            }),
        })
    }

    /// Record a frame, broadcast it and return its sequence; nobody listening is not an error.
    pub fn push(&self, payload: impl Into<Arc<str>>, terminal: bool) -> u64 {
        let payload: Arc<str> = payload.into();
        let frame = {
            let mut buf = self.buf.lock().expect("run buffer poisoned");
            let seq = buf.next_seq;
            buf.next_seq += 1;
            let frame = RunFrame {
                seq,
                payload,
                terminal,
            };
            buf.bytes += frame.payload.len();
            buf.frames.push_back(frame.clone());
            while buf.frames.len() > MAX_FRAMES || (buf.bytes > MAX_BYTES && buf.frames.len() > 1) {
                if let Some(dropped) = buf.frames.pop_front() {
                    buf.bytes -= dropped.payload.len();
                    buf.first_seq = dropped.seq + 1;
                }
            }
            frame
        };
        let seq = frame.seq;
        let _ = self.tx.send(frame);
        seq
    }

    /// Everything after `after_seq` that is still held.
    pub fn snapshot(&self, after_seq: u64) -> Snapshot {
        let buf = self.buf.lock().expect("run buffer poisoned");
        let want_from = after_seq + 1;
        let gap = (want_from < buf.first_seq).then_some(buf.first_seq);
        let frames: Vec<RunFrame> = buf
            .frames
            .iter()
            .filter(|f| f.seq >= want_from)
            .cloned()
            .collect();
        Snapshot {
            saw_terminal: frames.iter().any(|f| f.terminal),
            gap,
            state: buf.state,
            last_seq: buf.next_seq.saturating_sub(1),
            frames,
        }
    }

    /// Mark the run over. Idempotent: the first terminal state wins.
    pub fn finish(&self, state: RunState) {
        let mut buf = self.buf.lock().expect("run buffer poisoned");
        if buf.state.is_terminal() {
            return;
        }
        buf.state = state;
        buf.finished_at = Some(Instant::now());
    }

    pub fn state(&self) -> RunState {
        self.buf.lock().expect("run buffer poisoned").state
    }

    fn finished_at(&self) -> Option<Instant> {
        self.buf.lock().expect("run buffer poisoned").finished_at
    }

    pub fn last_seq(&self) -> u64 {
        self.buf
            .lock()
            .expect("run buffer poisoned")
            .next_seq
            .saturating_sub(1)
    }

    pub fn first_seq(&self) -> u64 {
        self.buf.lock().expect("run buffer poisoned").first_seq
    }

    pub fn attached(&self) -> usize {
        self.attached.load(Ordering::SeqCst)
    }

    /// Register a subscriber; dropping the last guard cancels an `Ephemeral` run.
    pub fn attach(self: &Arc<Self>) -> AttachGuard {
        let now = self.attached.fetch_add(1, Ordering::SeqCst) + 1;
        tracing::debug!(
            target: "giap::runs",
            run_id = %self.run_id,
            attached = now,
            "client attached to run"
        );
        AttachGuard { run: self.clone() }
    }

    /// Subscribe to the live tail. Call BEFORE `snapshot`, or a frame between the two is lost.
    pub fn subscribe(&self) -> broadcast::Receiver<RunFrame> {
        self.tx.subscribe()
    }
}

/// Drops a subscriber's registration, and with it an `Ephemeral` run.
pub struct AttachGuard {
    run: Arc<RunHandle>,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        let left = self.run.attached.fetch_sub(1, Ordering::SeqCst) - 1;
        tracing::info!(
            target: "giap::runs",
            run_id = %self.run.run_id,
            attached_remaining = left,
            policy = ?self.run.policy,
            at_seq = self.run.last_seq(),
            "client detached from run"
        );
        if left == 0 && self.run.policy == RunPolicy::Ephemeral && !self.run.state().is_terminal() {
            tracing::info!(
                target: "giap::runs",
                run_id = %self.run.run_id,
                "last subscriber left an ephemeral run; cancelling"
            );
            self.run.cancel.cancel();
        }
    }
}

// ── The registry ──────────────────────────────────────────────────────────────

/// Why a run could not be registered.
#[derive(Debug, PartialEq, Eq)]
pub enum RegistryFull {
    AtCap { active: usize, max: usize },
}

struct RegistryInner {
    runs: HashMap<RunId, Arc<RunHandle>>,
    /// Most recent run per session: a restarted client knows only its session id.
    by_session: HashMap<String, RunId>,
}

/// Every run this process is driving or recently finished. A `std::sync::Mutex`: it is never
/// touched per token, and holding it across an `.await` is then a compile error.
pub struct RunRegistry {
    inner: Mutex<RegistryInner>,
    max_runs: usize,
    retention: Duration,
}

impl RunRegistry {
    pub fn new(max_runs: usize, retention: Duration) -> Self {
        Self {
            inner: Mutex::new(RegistryInner {
                runs: HashMap::new(),
                by_session: HashMap::new(),
            }),
            max_runs,
            retention,
        }
    }

    /// Register a run; errs when `max_runs` runs are still in flight.
    pub fn insert(&self, handle: Arc<RunHandle>) -> Result<(), RegistryFull> {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention);

        // A session runs one turn at a time, so a new turn supersedes its finished one.
        if let Some(previous) = inner.by_session.get(&handle.session_id).cloned() {
            if inner
                .runs
                .get(&previous)
                .is_some_and(|h| h.state().is_terminal())
            {
                inner.runs.remove(&previous);
            }
        }

        // Cap work in flight only; retained runs are history for reconnecting clients.
        let active = inner
            .runs
            .values()
            .filter(|h| !h.state().is_terminal())
            .count();
        if active >= self.max_runs {
            return Err(RegistryFull::AtCap {
                active,
                max: self.max_runs,
            });
        }
        inner
            .by_session
            .insert(handle.session_id.clone(), handle.run_id.clone());
        inner.runs.insert(handle.run_id.clone(), handle);
        Ok(())
    }

    pub fn get(&self, run_id: &str) -> Option<Arc<RunHandle>> {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention);
        inner.runs.get(run_id).cloned()
    }

    pub fn for_session(&self, session_id: &str) -> Option<Arc<RunHandle>> {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention);
        let run_id = inner.by_session.get(session_id)?.clone();
        inner.runs.get(&run_id).cloned()
    }

    /// Evict finished runs past their retention. Returns how many went.
    pub fn sweep(&self) -> usize {
        let mut inner = self.inner.lock().expect("run registry poisoned");
        Self::sweep_locked(&mut inner, self.retention)
    }

    fn sweep_locked(inner: &mut RegistryInner, retention: Duration) -> usize {
        let now = Instant::now();
        let expired: Vec<RunId> = inner
            .runs
            .iter()
            .filter(|(_, h)| {
                h.finished_at()
                    .is_some_and(|at| now.duration_since(at) >= retention)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &expired {
            if let Some(handle) = inner.runs.remove(id) {
                tracing::info!(
                    target: "giap::runs",
                    run_id = %handle.run_id,
                    state = ?handle.state(),
                    attached = handle.attached(),
                    "run evicted from the registry"
                );
                // A newer turn on the same session may already own the index.
                if inner.by_session.get(&handle.session_id) == Some(id) {
                    inner.by_session.remove(&handle.session_id);
                }
            }
        }
        expired.len()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("run registry poisoned").runs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Detached-run state, one `AppState` field because ~30 test fixtures build `AppState` literally.
pub struct RunSupervisor {
    pub registry: RunRegistry,
    /// Bounds concurrent detached runs. Separate from `sse_semaphore` so abandoned runs, which
    /// outlive their connection, cannot starve interactive chat.
    pub permits: Arc<tokio::sync::Semaphore>,
    /// Identifies this process, so a client can tell "pond restarted" from "run aged out".
    pub epoch: String,
}

impl RunSupervisor {
    pub fn new(max_runs: usize, retention: Duration) -> Self {
        Self {
            registry: RunRegistry::new(max_runs, retention),
            permits: Arc::new(tokio::sync::Semaphore::new(max_runs)),
            epoch: uuid::Uuid::new_v4().to_string(),
        }
    }
}

impl Default for RunSupervisor {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_RUNS, DEFAULT_RETENTION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(policy: RunPolicy) -> Arc<RunHandle> {
        RunHandle::new("sess-1".into(), RunOwner::Unattributed, policy)
    }

    #[test]
    fn a_frame_pushed_with_nobody_listening_is_not_an_error() {
        let run = handle(RunPolicy::Detached);
        assert_eq!(run.push("data", false), 1);
        assert_eq!(run.push("more", false), 2);
        assert_eq!(run.snapshot(0).frames.len(), 2);
    }

    #[test]
    fn a_snapshot_returns_only_what_came_after() {
        let run = handle(RunPolicy::Detached);
        for i in 0..5 {
            run.push(format!("f{i}"), false);
        }
        let snap = run.snapshot(2);
        assert_eq!(
            snap.frames.iter().map(|f| f.seq).collect::<Vec<_>>(),
            vec![3, 4, 5],
            "after_seq is exclusive"
        );
        assert_eq!(snap.gap, None);
        assert_eq!(snap.last_seq, 5);
    }

    #[test]
    fn the_ring_evicts_and_says_so_rather_than_skipping_silently() {
        let run = handle(RunPolicy::Detached);
        for i in 0..(MAX_FRAMES + 10) {
            run.push(format!("f{i}"), false);
        }
        assert!(run.first_seq() > 1, "the ring should have evicted");
        let snap = run.snapshot(0);
        assert_eq!(
            snap.gap,
            Some(run.first_seq()),
            "a caller asking for an evicted position must be told, not quietly \
             handed a later frame as though it were the next one"
        );
    }

    #[test]
    fn a_caller_already_current_gets_nothing_and_no_gap() {
        let run = handle(RunPolicy::Detached);
        run.push("only", false);
        let snap = run.snapshot(0);
        assert_eq!(snap.frames.len(), 1);
        let snap = run.snapshot(snap.last_seq);
        assert!(snap.frames.is_empty());
        assert_eq!(snap.gap, None);
    }

    #[test]
    fn the_terminal_frame_is_visible_to_a_late_subscriber() {
        let run = handle(RunPolicy::Detached);
        run.push("text", false);
        run.push("{\"done\":true}", true);
        run.finish(RunState::Finished);
        let snap = run.snapshot(0);
        assert!(snap.saw_terminal);
        assert_eq!(snap.state, RunState::Finished);
    }

    #[test]
    fn the_first_terminal_state_wins() {
        let run = handle(RunPolicy::Detached);
        run.finish(RunState::Cancelled);
        run.finish(RunState::Finished);
        assert_eq!(
            run.state(),
            RunState::Cancelled,
            "a tail that completes after a cancel must not relabel the turn as \
             an ordinary finish"
        );
    }

    #[test]
    fn abandoning_an_ephemeral_run_cancels_it() {
        let run = handle(RunPolicy::Ephemeral);
        let guard = run.attach();
        assert!(!run.cancel.is_cancelled());
        drop(guard);
        assert!(
            run.cancel.is_cancelled(),
            "the last subscriber leaving is what used to drop the response body"
        );
    }

    #[test]
    fn abandoning_a_detached_run_leaves_it_running() {
        let run = handle(RunPolicy::Detached);
        drop(run.attach());
        assert!(!run.cancel.is_cancelled());
    }

    #[test]
    fn one_of_several_subscribers_leaving_does_not_cancel() {
        let run = handle(RunPolicy::Ephemeral);
        let first = run.attach();
        let second = run.attach();
        drop(first);
        assert!(!run.cancel.is_cancelled());
        drop(second);
        assert!(run.cancel.is_cancelled());
    }

    #[test]
    fn a_finished_ephemeral_run_is_not_cancelled_on_the_way_out() {
        let run = handle(RunPolicy::Ephemeral);
        let guard = run.attach();
        run.finish(RunState::Finished);
        drop(guard);
        assert!(
            !run.cancel.is_cancelled(),
            "cancelling a turn that already finished would mislabel it in every \
             log that reads the token"
        );
    }

    #[test]
    fn the_registry_finds_a_run_by_its_session() {
        let reg = RunRegistry::new(4, DEFAULT_RETENTION);
        let run = handle(RunPolicy::Detached);
        reg.insert(run.clone()).unwrap();
        let found = reg
            .for_session("sess-1")
            .expect("a restarted client knows only the session id");
        assert_eq!(found.run_id, run.run_id);
    }

    #[test]
    fn a_finished_run_does_not_hold_a_slot_against_the_cap() {
        let reg = RunRegistry::new(2, Duration::from_secs(300));
        for i in 0..10 {
            let run = RunHandle::new(
                format!("sess-{i}"),
                RunOwner::Unattributed,
                RunPolicy::Detached,
            );
            reg.insert(run.clone())
                .unwrap_or_else(|e| panic!("refused turn {i}: {e:?}"));
            run.finish(RunState::Finished);
        }
    }

    #[test]
    fn a_new_turn_supersedes_the_finished_one_on_the_same_session() {
        let reg = RunRegistry::new(4, Duration::from_secs(300));
        let first = handle(RunPolicy::Detached);
        reg.insert(first.clone()).unwrap();
        first.finish(RunState::Finished);

        let second = handle(RunPolicy::Detached);
        reg.insert(second.clone()).unwrap();

        assert!(
            reg.get(&first.run_id).is_none(),
            "the previous turn on this session is superseded, not accumulated"
        );
        assert_eq!(
            reg.for_session("sess-1").map(|h| h.run_id.clone()),
            Some(second.run_id.clone())
        );
    }

    #[test]
    fn the_registry_refuses_past_its_cap() {
        let reg = RunRegistry::new(1, DEFAULT_RETENTION);
        let running = handle(RunPolicy::Detached);
        reg.insert(running).unwrap();
        let err = reg
            .insert(RunHandle::new(
                "sess-2".into(),
                RunOwner::Unattributed,
                RunPolicy::Detached,
            ))
            .unwrap_err();
        assert_eq!(err, RegistryFull::AtCap { active: 1, max: 1 });
    }

    #[test]
    fn a_sweep_evicts_finished_runs_and_never_a_running_one() {
        let reg = RunRegistry::new(4, Duration::ZERO);
        let running = handle(RunPolicy::Detached);
        let done = RunHandle::new("sess-2".into(), RunOwner::Unattributed, RunPolicy::Detached);
        reg.insert(running.clone()).unwrap();
        reg.insert(done.clone()).unwrap();
        done.finish(RunState::Finished);

        assert_eq!(reg.sweep(), 1);
        assert!(reg.get(&running.run_id).is_some());
        assert!(reg.get(&done.run_id).is_none());
        assert!(reg.for_session("sess-2").is_none());
    }

    #[test]
    fn a_swept_run_does_not_take_a_newer_turns_session_index_with_it() {
        let reg = RunRegistry::new(4, Duration::ZERO);
        let old = handle(RunPolicy::Detached);
        reg.insert(old.clone()).unwrap();
        old.finish(RunState::Finished);
        // A second turn on the SAME session, which now owns the index.
        let new = handle(RunPolicy::Detached);
        reg.insert(new.clone()).unwrap();

        reg.sweep();
        assert_eq!(
            reg.for_session("sess-1").map(|h| h.run_id.clone()),
            Some(new.run_id.clone()),
            "sweeping the old run must not orphan the live one"
        );
    }

    #[tokio::test]
    async fn a_subscriber_hears_frames_pushed_after_it_subscribed() {
        let run = handle(RunPolicy::Detached);
        let mut rx = run.subscribe();
        run.push("hello", false);
        let frame = rx.recv().await.unwrap();
        assert_eq!(&*frame.payload, "hello");
        assert_eq!(frame.seq, 1);
    }

    #[tokio::test]
    async fn a_lagging_subscriber_can_recover_everything_it_missed() {
        let run = handle(RunPolicy::Detached);
        let mut rx = run.subscribe();
        // Overrun the broadcast queue but not the ring (BROADCAST_CAP < MAX_FRAMES).
        for i in 0..(BROADCAST_CAP + 50) {
            run.push(format!("f{i}"), false);
        }
        let err = rx.recv().await.unwrap_err();
        assert!(
            matches!(err, broadcast::error::RecvError::Lagged(_)),
            "expected the queue to have dropped frames"
        );
        let snap = run.snapshot(0);
        assert_eq!(snap.gap, None, "the ring still holds everything");
        assert_eq!(snap.frames.len(), BROADCAST_CAP + 50);
    }
}
