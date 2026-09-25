//! `GooseOrchestrator`: runs `pond-core`'s authorised [`TaskSpec`]s as Goose child agents.
//! It makes no authorisation decision; beyond the narrowing it is handed, it can only refuse.
//!
//! It owns the child loop because Goose's `run_subagent_task` is crate-private and, even if a
//! fork patch exposed it, renders Goose's own `subagent_system.md` with no seam to replace it.
//! The child's prompt is built by `build_prompt_partition`, like the parent's.
//!
//! Invariants:
//! - 1, never wider: [`child_extensions`] intersects three sets and lists every tool explicitly.
//! - 2, stripped builtins stay stripped: a spec naming one of [`GOOSE_STRIPPED_BUILTINS`] is
//!   refused, and every run is audited afterwards ([`stripped_builtins_present`]).
//! - 3, concurrency 1 on this device: children and on-device parent turns
//!   ([`claim_device_for_turn`]) share one process-wide [`Semaphore`]; a sync child inherits its
//!   parent's claim through a one-permit semaphore, so siblings still run one at a time.
//! - 3b, a child is a second claim on one window: [`DeviceLedger`] holds its `context_fraction`
//!   and `turn_profile` shrinks the parent's `history_token_budget` by it.
//! - 4, child turns never reach the parent's history: only the last turn's text returns
//!   ([`ChildTurns`]) and [`ChildRunner::release`] deletes the child's engine session.
//! - 5, cancellable: a sync run's [`CancellationToken`] derives from the parent turn's; a
//!   background run owns its token and is stopped by [`Orchestrator::cancel_children_of`].
//! - 6, depth capped structurally: this tree has no delegation tool; Goose's `summon` is stripped.
//!
//! The result does not stream: [`ProgressBus`] carries only lifecycle and tool-name frames,
//! outside the port. Background runs and per-role models are refused on on-device providers
//! ([`BackgroundAvailability`], [`child_model_config`]).

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pond_core::mcp::domain::tool_group::TOOL_NAME_SEPARATOR;
use pond_core::shared::domain::agent::{AgentStreamEvent, SubagentStatus};
use pond_core::shared::domain::orchestration::{
    max_concurrent_subagents, BackgroundAvailability, ChildModel, TaskRun, TaskSpec, TaskStatus,
    REMOTE_SUBAGENT_CONCURRENCY,
};
use pond_core::shared::ports::orchestrator::Orchestrator;
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

use goose::agents::ExtensionConfig;

/// The ten Goose builtins GIAP strips from every session; the only copy of this list.
/// Excludes `default`/`suggestions`: tool-less Goose plumbing the strip loop must not remove.
pub const GOOSE_STRIPPED_BUILTINS: [&str; 10] = [
    "developer",
    "computercontroller",
    "extensionmanager",
    "todo",
    "apps",
    "analyze",
    "summon",
    "summarize",
    "orchestrator",
    "tom",
];

/// Mirror of Goose's private `MAX_TURNS_MESSAGE`: the text of an `Ok` run that hit `max_turns`.
pub const GOOSE_MAX_TURNS_MESSAGE: &str = "I've reached the maximum number of actions I can do without user input. Would you like me to continue?";

/// Sized to the widest provider concurrency, so each limit is a per-child permit count.
pub const SUBAGENT_PERMITS: usize = REMOTE_SUBAGENT_CONCURRENCY;

/// Cap on tracked runs; eviction only removes finished ones, so a live run stays cancellable.
pub const MAX_TRACKED_TASKS: usize = 64;

/// How many of [`SUBAGENT_PERMITS`] one child must hold; `div_ceil` errs toward less concurrency.
/// On-device includes ollama/llamafile (HTTP to `127.0.0.1`) and any provider nothing can place.
pub fn subagent_permits(provider: &str) -> u32 {
    let concurrent = max_concurrent_subagents(provider).max(1);
    SUBAGENT_PERMITS.div_ceil(concurrent) as u32
}

/// The one subagent semaphore on this pond. Process-wide, never a constructor argument:
/// invariant 3 is per device, and a per-instance semaphore would silently lift the limit.
pub fn process_subagent_permits() -> Arc<Semaphore> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(SUBAGENT_PERMITS)))
        .clone()
}

/// Permits a parent turn holds before touching the engine: all on-device, none elsewhere. A child
/// replying mid-turn would overwrite the model's one retained KV prefix and force a re-prefill.
pub fn parent_turn_permits(provider: &str) -> u32 {
    if max_concurrent_subagents(provider) <= 1 {
        SUBAGENT_PERMITS as u32
    } else {
        0
    }
}

// ── The live-delegation ledger ──────────────────────────────────────────────

/// What live delegations claim from their parents: a share of the history budget, and the device.
/// Process-wide ([`process_device_ledger`]): per-instance would miss other instances' children.
#[derive(Default)]
pub struct DeviceLedger {
    /// GIAP session id -> each live child's `context_fraction`. A `Vec`, not a running f32 sum,
    /// so releasing returns exactly to zero.
    reservations: Mutex<HashMap<String, Vec<f32>>>,
    /// GIAP session id -> its live turns' hold on the device.
    device_holders: Mutex<HashMap<String, DeviceHoldState>>,
}

/// One session's device hold, and the semaphore its children share.
struct DeviceHoldState {
    /// Live turns of this session holding the device; a count, as one session can have two
    /// streams open (voice and chat).
    holders: usize,
    /// One permit, taken by each child inheriting this hold: a child must not queue behind its
    /// parent (blocked in a tool call) but must still queue behind its siblings.
    children: Arc<Semaphore>,
}

/// The one ledger on this pond; see [`DeviceLedger`].
pub fn process_device_ledger() -> Arc<DeviceLedger> {
    static LEDGER: OnceLock<Arc<DeviceLedger>> = OnceLock::new();
    LEDGER
        .get_or_init(|| Arc::new(DeviceLedger::default()))
        .clone()
}

impl DeviceLedger {
    fn lock_reservations(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<f32>>> {
        self.reservations.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_holders(&self) -> std::sync::MutexGuard<'_, HashMap<String, DeviceHoldState>> {
        self.device_holders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Claim `fraction` of `parent_session_id`'s history budget until the guard drops.
    pub fn reserve(self: &Arc<Self>, parent_session_id: &str, fraction: f32) -> HistoryReservation {
        self.lock_reservations()
            .entry(parent_session_id.to_string())
            .or_default()
            .push(fraction);
        HistoryReservation {
            ledger: Arc::clone(self),
            session_id: parent_session_id.to_string(),
            fraction,
        }
    }

    /// Share of this session's history budget its live children hold (at most 1.0).
    pub fn reserved_fraction(&self, giap_session_id: &str) -> f32 {
        self.lock_reservations()
            .get(giap_session_id)
            .map(|fractions| fractions.iter().sum::<f32>().clamp(0.0, 1.0))
            .unwrap_or(0.0)
    }

    /// Record that a turn of `giap_session_id` holds the device permit.
    fn hold_device(self: &Arc<Self>, giap_session_id: &str) -> DeviceHold {
        let mut holders = self.lock_holders();
        let state = holders
            .entry(giap_session_id.to_string())
            .or_insert_with(|| DeviceHoldState {
                holders: 0,
                children: Arc::new(Semaphore::new(1)),
            });
        state.holders += 1;
        drop(holders);
        DeviceHold {
            ledger: Arc::clone(self),
            session_id: giap_session_id.to_string(),
        }
    }

    /// Whether a live turn of this session holds the device. Diagnostics and tests only: `spawn`
    /// uses [`inherited_child_permits`](Self::inherited_child_permits) so siblings still queue.
    pub fn session_holds_device(&self, giap_session_id: &str) -> bool {
        self.lock_holders()
            .get(giap_session_id)
            .is_some_and(|state| state.holders > 0)
    }

    /// The one-permit semaphore an inheriting child must take, or `None` when no live turn of
    /// this session holds the device.
    pub fn inherited_child_permits(&self, giap_session_id: &str) -> Option<Arc<Semaphore>> {
        self.lock_holders()
            .get(giap_session_id)
            .filter(|state| state.holders > 0)
            .map(|state| Arc::clone(&state.children))
    }

    /// Live children of this session. Diagnostics and tests only.
    pub fn live_children(&self, giap_session_id: &str) -> usize {
        self.lock_reservations()
            .get(giap_session_id)
            .map(Vec::len)
            .unwrap_or(0)
    }
}

/// One live child's claim on its parent's history budget; only `Drop` releases it, on every exit.
pub struct HistoryReservation {
    ledger: Arc<DeviceLedger>,
    session_id: String,
    fraction: f32,
}

impl Drop for HistoryReservation {
    fn drop(&mut self) {
        let mut reservations = self.ledger.lock_reservations();
        if let Some(fractions) = reservations.get_mut(&self.session_id) {
            if let Some(at) = fractions.iter().position(|f| *f == self.fraction) {
                fractions.remove(at);
            }
            if fractions.is_empty() {
                reservations.remove(&self.session_id);
            }
        }
    }
}

/// Records that a session's turn holds the device; dropping it clears the record.
struct DeviceHold {
    ledger: Arc<DeviceLedger>,
    session_id: String,
}

impl Drop for DeviceHold {
    fn drop(&mut self) {
        let mut holders = self.ledger.lock_holders();
        let empty = match holders.get_mut(&self.session_id) {
            Some(state) => {
                state.holders = state.holders.saturating_sub(1);
                state.holders == 0
            }
            None => false,
        };
        if empty {
            // A running inheritor keeps its own `Arc` of the sub-semaphore; removing the entry
            // only makes later children acquire from the process semaphore.
            holders.remove(&self.session_id);
        }
    }
}

/// A parent turn's exclusive claim on this device, held for the whole turn. Field order is drop
/// order: the ledger record clears before the permit frees, so no child inherits a free permit.
pub struct TurnDeviceClaim {
    _hold: DeviceHold,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// Take the device for a parent turn. `None` (off-device provider, or cancelled while queueing)
/// is safe to proceed on: a cancelled turn's reply exits on the same token.
pub async fn claim_device_for_turn(
    giap_session_id: &str,
    provider: &str,
    cancel: &CancellationToken,
) -> Option<TurnDeviceClaim> {
    let needed = parent_turn_permits(provider);
    if needed == 0 {
        return None;
    }
    let permits = process_subagent_permits();
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => return None,
        acquired = permits.acquire_many_owned(needed) => acquired.ok()?,
    };
    // Recorded only once the permit is held, or a queueing turn's child would inherit it early.
    let hold = process_device_ledger().hold_device(giap_session_id);
    Some(TurnDeviceClaim {
        _hold: hold,
        _permit: permit,
    })
}

// ── The progress channel ────────────────────────────────────────────────────

/// One thing a live delegation did, bound for its parent's chat stream. Separate from the stream
/// event because it carries the routing key, which a client must never see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildProgress {
    /// GIAP session id of the authorising parent turn (`TaskSpec::parent_session_id`).
    pub parent_session_id: String,
    pub task_id: String,
    pub role: String,
    pub status: SubagentStatus,
    /// A tool name or a reason GIAP wrote, never the child's own words ([`child_tool_names`]).
    pub detail: Option<String>,
}

impl From<ChildProgress> for AgentStreamEvent {
    fn from(progress: ChildProgress) -> Self {
        AgentStreamEvent::SubagentProgress {
            task_id: progress.task_id,
            role: progress.role,
            status: progress.status,
            detail: progress.detail,
        }
    }
}

/// Side channel for child progress frames: a parent's stream is silent while a sync child runs
/// inside its `delegate` call. Unbounded, since the child sends from under the parent's own poll
/// and a full channel would deadlock it. Routed by session key, never a filtered broadcast, so a
/// frame cannot reach another member's chat; a session's newest turn wins the key.
#[derive(Default)]
pub struct ProgressBus {
    subscribers: Mutex<HashMap<String, Subscriber>>,
    next_id: std::sync::atomic::AtomicU64,
}

/// One live parent turn's end of the channel.
struct Subscriber {
    /// Lets a stale stream's `Drop` remove only its own entry, not a newer turn's.
    id: u64,
    tx: tokio::sync::mpsc::UnboundedSender<ChildProgress>,
}

/// The one bus on this pond. Process-wide, or a per-request orchestrator publishes to nobody.
pub fn process_progress_bus() -> Arc<ProgressBus> {
    static BUS: OnceLock<Arc<ProgressBus>> = OnceLock::new();
    BUS.get_or_init(|| Arc::new(ProgressBus::default())).clone()
}

impl ProgressBus {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Subscriber>> {
        self.subscribers.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Listen for `giap_session_id`'s delegations until the returned stream drops.
    pub fn subscribe(self: &Arc<Self>, giap_session_id: &str) -> ProgressStream {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.lock().insert(
            giap_session_id.to_string(),
            Subscriber { id, tx: tx.clone() },
        );
        ProgressStream {
            rx,
            _keepalive: tx,
            bus: Arc::downgrade(self),
            session_id: giap_session_id.to_string(),
            id,
        }
    }

    /// Hand a frame to its live turn, or silently drop it (the normal case for headless runs).
    pub fn publish(&self, frame: ChildProgress) {
        if let Some(subscriber) = self.lock().get(&frame.parent_session_id) {
            let _ = subscriber.tx.send(frame);
        }
    }

    /// Live subscribers. Diagnostics and tests only.
    pub fn subscribers(&self) -> usize {
        self.lock().len()
    }
}

/// A parent turn's end of the progress channel. `_keepalive` stops the channel closing when the
/// bus entry is replaced: a closed `recv()` is always ready and would starve the biased select.
pub struct ProgressStream {
    rx: tokio::sync::mpsc::UnboundedReceiver<ChildProgress>,
    _keepalive: tokio::sync::mpsc::UnboundedSender<ChildProgress>,
    bus: std::sync::Weak<ProgressBus>,
    session_id: String,
    id: u64,
}

impl ProgressStream {
    /// The next frame, pending forever if none comes; cancel-safe, for use inside `select!`.
    pub async fn next(&mut self) -> ChildProgress {
        match self.rx.recv().await {
            Some(frame) => frame,
            // Unreachable while `_keepalive` lives; pend, or the biased select starves the engine.
            None => std::future::pending().await,
        }
    }
}

impl Drop for ProgressStream {
    fn drop(&mut self) {
        if let Some(bus) = self.bus.upgrade() {
            let mut subscribers = bus.lock();
            if subscribers
                .get(&self.session_id)
                .is_some_and(|current| current.id == self.id)
            {
                subscribers.remove(&self.session_id);
            }
        }
    }
}

/// What a parent's drain loop got when it asked for the next thing to happen.
#[derive(Debug)]
pub enum ParentStep<T> {
    /// A child's frame: yield it, but never fold it into the turn's own output.
    Progress(ChildProgress),
    Engine(T),
    EngineEnded,
}

/// Await a progress frame or the engine's next event, progress first; Goose yields nothing while
/// a sync child runs, so no engine event waits. Relies on both branches being cancel-safe.
pub async fn next_parent_step<S>(
    engine: &mut S,
    progress: &mut ProgressStream,
) -> ParentStep<S::Item>
where
    S: futures::Stream + Unpin,
{
    tokio::select! {
        biased;
        frame = progress.next() => ParentStep::Progress(frame),
        item = futures::StreamExt::next(engine) => match item {
            Some(item) => ParentStep::Engine(item),
            None => ParentStep::EngineEnded,
        },
    }
}

/// Publish a frame about a live delegation, if anyone listens. Every producer goes through here.
pub fn report_child_progress(
    parent_session_id: &str,
    task_id: &str,
    role: &str,
    status: SubagentStatus,
    detail: Option<String>,
) {
    process_progress_bus().publish(ChildProgress {
        parent_session_id: parent_session_id.to_string(),
        task_id: task_id.to_string(),
        role: role.to_string(),
        status,
        detail,
    });
}

/// The tool names in a child's message and nothing else: a child bypasses the adapter's reasoning
/// gate, so no reasoning, text or arguments may leave here. Unparsed (`Err`) calls are skipped.
pub fn child_tool_names(msg: &goose::conversation::message::Message) -> Vec<String> {
    use goose::conversation::message::MessageContent;
    msg.content
        .iter()
        .filter_map(|content| match content {
            MessageContent::ToolRequest(request) => request
                .tool_call
                .as_ref()
                .ok()
                .map(|call| call.name.to_string()),
            _ => None,
        })
        .collect()
}

/// Split a Goose tool name into `(extension, bare tool)`. A bare name (Goose's `unprefixed_tools`
/// extensions, e.g. `developer`'s `shell`) gets `None`: guessing its owner would invent one.
pub fn split_extension_tool(tool_name: &str) -> Option<(&str, &str)> {
    let at = tool_name.find(TOOL_NAME_SEPARATOR)?;
    let (extension, rest) = tool_name.split_at(at);
    if extension.is_empty() {
        return None;
    }
    Some((extension, &rest[TOOL_NAME_SEPARATOR.len()..]))
}

// ── What the engine offers, and what we decide to run ───────────────────────

/// The parent's live engine surface. Plans draw tools only from here: a subset by construction.
#[derive(Debug, Clone, Default)]
pub struct ChildEnvironment {
    /// `settings.chat_provider`, for the concurrency permit.
    pub provider_name: String,
    /// The parent's static prefix from `build_prompt_partition`; the child's prompt extends it.
    pub base_system_prefix: String,
    /// Extension -> unprefixed tool names loaded on the parent; unprefixed because Goose's
    /// `is_tool_available` matches `actual_tool_name`, not the `ext__tool` the model sees.
    pub parent_tools: BTreeMap<String, BTreeSet<String>>,
}

/// Everything needed to run one child, with every decision made; the runner only executes it.
#[derive(Debug, Clone)]
pub struct ChildPlan {
    pub task_id: String,
    pub role: String,
    pub parent_session_id: String,
    pub child_session_id: String,
    pub system_prompt: String,
    pub user_message: String,
    pub extensions: Vec<ExtensionConfig>,
    /// Every `extension__tool` name the child may call, derived from `extensions`. Must reach the
    /// child's `ShimControls` before its first call: unset, the shim silently lists every tool.
    pub allowed_tool_names: Vec<String>,
    pub max_turns: u32,
    /// Which model this child runs on, already decided. [`ChildModel::assigned`] is the only
    /// predicate the runner may use.
    pub model: ChildModel,
}

/// What came back from one child run, before it is classified.
#[derive(Debug, Clone, Default)]
pub struct ChildOutcome {
    /// Whole text of the child's last completed assistant turn ([`ChildTurns`]), not its last
    /// streamed fragment. Text only, so the child's reasoning cannot reach the parent.
    pub last_text: Option<String>,
    /// Assistant turns as counted by [`ChildTurns`]. Not Goose's (invisible) `turns_taken`: this
    /// can only under-count, so [`classify_outcome`]'s sentinel check is the primary budget check.
    pub assistant_turns: u32,
    /// Extensions the child had loaded after it ran; audited against [`GOOSE_STRIPPED_BUILTINS`]
    /// since `add_extension` is not the only way one arrives.
    pub loaded_extensions: BTreeSet<String>,
}

/// Assembles a child's messages into turns: Goose yields one message per provider chunk, so a turn
/// is a run of assistant messages closed by any other role; the answer is the last non-blank one.
#[derive(Debug, Default)]
pub struct ChildTurns {
    /// The run currently being accumulated, if a run is open.
    open: Option<String>,
    turns: u32,
    last_completed: Option<String>,
}

impl ChildTurns {
    /// One assistant message's `as_concat_text()`. Empty text still opens a run: a
    /// thinking-only turn is still a turn.
    pub fn assistant_message(&mut self, text: &str) {
        match self.open.as_mut() {
            Some(open) => open.push_str(text),
            None => {
                self.turns = self.turns.saturating_add(1);
                self.open = Some(text.to_string());
            }
        }
    }

    /// A non-assistant message (in practice a tool response); ends the open run.
    pub fn other_role_message(&mut self) {
        self.close_run();
    }

    fn close_run(&mut self) {
        if let Some(text) = self.open.take() {
            if !text.trim().is_empty() {
                self.last_completed = Some(text);
            }
        }
    }

    /// End of stream: flush the open run; returns `(last completed answer, assistant turns)`.
    pub fn finish(mut self) -> (Option<String>, u32) {
        self.close_run();
        (self.last_completed, self.turns)
    }
}

/// The drain loop's per-event decision, out here so tests can run it without an engine.
pub fn child_stream_step(turns: &mut ChildTurns, is_assistant: bool, text: &str) {
    if is_assistant {
        turns.assistant_message(text);
    } else {
        // Goose returns a tool response as a `User` message, closing the turn.
        turns.other_role_message();
    }
}

/// Why a plan could not be built. Every variant refuses the run; none substitutes a default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRefused {
    ForbiddenExtension {
        task_id: String,
        role: String,
        extension: String,
    },
}

impl std::fmt::Display for PlanRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanRefused::ForbiddenExtension {
                task_id,
                role,
                extension,
            } => write!(
                f,
                "task {task_id} for role `{role}` was authorised for `{extension}`, which is one \
                 of the Goose builtins GIAP strips from every session - refusing to hand it to a \
                 subagent"
            ),
        }
    }
}

impl std::error::Error for PlanRefused {}

/// Build the child's extension list: the parent's loaded tools narrowed per tool by
/// `spec.grants_tool`. A spec naming a stripped builtin is refused outright, not filtered.
/// Never reuse `GooseAdapter::builtin_extension_config`: Goose reads its `vec![]` as all tools.
pub fn child_extensions(
    spec: &TaskSpec,
    parent_tools: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<ExtensionConfig>, PlanRefused> {
    for extension in spec.tool_groups() {
        if GOOSE_STRIPPED_BUILTINS.contains(&extension.as_str()) {
            return Err(PlanRefused::ForbiddenExtension {
                task_id: spec.id().to_string(),
                role: spec.role().to_string(),
                extension: extension.clone(),
            });
        }
    }

    let mut configs = Vec::new();
    for (extension, tools) in parent_tools {
        // Per-tool `grants_tool` also covers the extension level, and drops builtins a parent's
        // best-effort strip failed to remove.
        let granted: Vec<String> = tools
            .iter()
            .filter(|tool| spec.grants_tool(&format!("{extension}{TOOL_NAME_SEPARATOR}{tool}")))
            .cloned()
            .collect();
        // Never `vec![]`: an empty allowlist is Goose's "all tools".
        if granted.is_empty() {
            continue;
        }
        configs.push(ExtensionConfig::Builtin {
            name: extension.clone(),
            description: String::new(),
            display_name: None,
            timeout: Some(600),
            bundled: Some(false),
            available_tools: granted,
        });
    }
    Ok(configs)
}

/// Which stripped builtins a child ended up with; anything but empty fails the run.
pub fn stripped_builtins_present(loaded: &BTreeSet<String>) -> Vec<String> {
    GOOSE_STRIPPED_BUILTINS
        .iter()
        .filter(|name| loaded.contains(**name))
        .map(|name| (*name).to_string())
        .collect()
}

/// The subagent framing appended to the parent's static prefix, in GIAP's words, not Goose's.
/// `persona` is the role's stored `instructions`; the task itself goes in the user message.
fn subagent_envelope(spec: &TaskSpec, persona: Option<&str>, tools: &[String]) -> String {
    let mut envelope = String::with_capacity(640);
    envelope.push_str("\n\n# Delegated task\n\n");
    envelope.push_str(
        "You are running as a helper for the assistant above, on ONE narrow task. \
         You are not talking to the user and they will not see this conversation - \
         only your final answer is passed back.\n\n",
    );
    envelope.push_str("Rules for this run:\n");
    envelope.push_str(
        "- Answer the task and stop. Do not ask follow-up questions; there is nobody to answer them.\n",
    );
    envelope.push_str(
        "- Your last message IS the answer. Make it complete on its own, in a few sentences.\n",
    );
    // The chat path's per-turn answer rule; a child has no `<system-context>`, so it goes here.
    envelope.push_str("- ");
    envelope.push_str(pond_core::models::services::answer_contract::ANSWER_RULE);
    envelope.push('\n');
    envelope.push_str(&format!(
        "- You have at most {} turns. Spend them on the task.\n",
        spec.max_turns()
    ));
    envelope.push_str("- You cannot delegate. There is no one below you.\n");
    if tools.is_empty() {
        envelope.push_str("- You have no tools on this run. Answer from what you are given.\n");
    } else {
        envelope.push_str(&format!(
            "- Your only tools are: {}. Nothing else is available to you.\n",
            tools.join(", ")
        ));
    }
    envelope.push_str("\n## Your role\n\n");
    envelope.push_str(&format!("You are the `{}` helper.\n", spec.role()));
    if let Some(persona) = persona.map(str::trim).filter(|p| !p.is_empty()) {
        envelope.push('\n');
        envelope.push_str(persona);
        envelope.push('\n');
    }
    envelope
}

/// The child's opening user message: the task, plus any structured inputs.
fn child_user_message(spec: &TaskSpec) -> String {
    let mut message = spec.instructions().to_string();
    if !spec.inputs().is_null() {
        message.push_str("\n\nInputs:\n");
        message.push_str(
            &serde_json::to_string_pretty(spec.inputs())
                .unwrap_or_else(|_| spec.inputs().to_string()),
        );
    }
    message
}

/// Turn an authorised [`TaskSpec`] into an executable [`ChildPlan`]; pure, so testable without an
/// engine. `role_persona` is `None` in production until `TaskSpec` carries the role's instructions.
pub fn build_child_plan(
    spec: &TaskSpec,
    child_session_id: &str,
    env: &ChildEnvironment,
    role_persona: Option<&str>,
) -> Result<ChildPlan, PlanRefused> {
    let extensions = child_extensions(spec, &env.parent_tools)?;
    let tool_names: Vec<String> = extensions
        .iter()
        .flat_map(|config| match config {
            ExtensionConfig::Builtin {
                name,
                available_tools,
                ..
            } => available_tools
                .iter()
                .map(|tool| format!("{name}{TOOL_NAME_SEPARATOR}{tool}"))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();

    let mut system_prompt = env.base_system_prefix.clone();
    system_prompt.push_str(&subagent_envelope(spec, role_persona, &tool_names));

    Ok(ChildPlan {
        task_id: spec.id().to_string(),
        role: spec.role().to_string(),
        parent_session_id: spec.parent_session_id().to_string(),
        child_session_id: child_session_id.to_string(),
        system_prompt,
        user_message: child_user_message(spec),
        extensions,
        allowed_tool_names: tool_names,
        max_turns: spec.max_turns(),
        // Decided here, the only place with both the role's request and the parent's provider.
        model: ChildModel::resolve(spec.requested_model(), &env.provider_name),
    })
}

/// Stamp a plan's model decision onto the parent's `ModelConfig` (same provider; Goose reads the
/// model per call). Clears `context_limit` so `update_provider` backfills the new model's window.
pub fn child_model_config(
    model: &ChildModel,
    parent: goose_providers::model::ModelConfig,
) -> goose_providers::model::ModelConfig {
    let Some(assigned) = model.assigned() else {
        return parent;
    };
    let mut cfg = parent;
    cfg.model_name = assigned.to_string();
    cfg.context_limit = None;
    cfg
}

/// Decide what a finished child run was: Goose returns `Ok` whether it answered, was cancelled or
/// ran out of turns. Cancellation is checked first, as a cancelled run may also exceed the count.
pub fn classify_outcome(
    cancelled: bool,
    assistant_turns: u32,
    max_turns: u32,
    last_text: Option<&str>,
) -> (TaskStatus, Option<String>) {
    if cancelled {
        return (TaskStatus::Cancelled, None);
    }
    let text = last_text.map(str::trim).unwrap_or("");
    if text == GOOSE_MAX_TURNS_MESSAGE || assistant_turns > max_turns {
        return (TaskStatus::TurnBudgetExhausted, None);
    }
    if text.is_empty() {
        return (TaskStatus::Failed, None);
    }
    (TaskStatus::Completed, Some(text.to_string()))
}

// ── The engine seam ─────────────────────────────────────────────────────────

/// How a child agent is driven; a seam so the orchestrator can be tested against a fake engine.
/// No default method bodies: deleting a real override must fail to compile.
#[async_trait]
pub trait ChildRunner: Send + Sync {
    /// What the parent's engine session currently offers.
    async fn environment(&self, parent_session_id: &str) -> Result<ChildEnvironment>;

    /// Create the child's engine session and return its id. Must precede the plan: Goose mints
    /// the id, and without its row `update_provider` fails with a misleading provider error.
    async fn open_child_session(&self, plan_role: &str) -> Result<String>;

    /// Run the plan to completion, or until `cancel` trips.
    async fn run(&self, plan: ChildPlan, cancel: CancellationToken) -> Result<ChildOutcome>;

    /// Drop the child's engine session: Goose persists every child message to its own
    /// `sessions.db`, which nothing in GIAP otherwise cleans up.
    async fn release(&self, child_session_id: &str);
}

// ── The registry ────────────────────────────────────────────────────────────

struct TaskEntry {
    run: TaskRun,
    cancel: CancellationToken,
}

#[derive(Default)]
struct TaskRegistry {
    tasks: HashMap<String, TaskEntry>,
}

/// The registry lock for callers holding an `Arc` rather than `&self` (a background run).
fn with_registry<T>(tasks: &Mutex<TaskRegistry>, f: impl FnOnce(&mut TaskRegistry) -> T) -> T {
    let mut guard = tasks.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut guard)
}

impl TaskRegistry {
    fn insert(&mut self, run: TaskRun, cancel: CancellationToken) {
        if self.tasks.len() >= MAX_TRACKED_TASKS {
            let oldest_terminal = self
                .tasks
                .values()
                .filter(|entry| entry.run.status.is_terminal())
                .min_by_key(|entry| entry.run.started_at)
                .map(|entry| entry.run.id.clone());
            if let Some(id) = oldest_terminal {
                self.tasks.remove(&id);
            }
        }
        self.tasks.insert(run.id.clone(), TaskEntry { run, cancel });
    }

    /// The run holds its concurrency permit and is now actually running.
    fn start(&mut self, task_id: &str) {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.run.status = TaskStatus::Running;
        }
    }

    fn finish(
        &mut self,
        task_id: &str,
        status: TaskStatus,
        result: Option<String>,
        error: Option<String>,
    ) {
        if let Some(entry) = self.tasks.get_mut(task_id) {
            entry.run.status = status;
            entry.run.result = result;
            entry.run.error = error;
            entry.run.finished_at = Some(chrono::Utc::now());
        }
    }
}

/// Run one authorised child to completion and record the outcome. Takes owned handles so the
/// future is `'static` for background runs; `_reservation` lives exactly as long as the child.
#[allow(clippy::too_many_arguments)]
async fn drive_run(
    runner: Arc<dyn ChildRunner>,
    tasks: Arc<Mutex<TaskRegistry>>,
    ledger: Arc<DeviceLedger>,
    permits: Arc<Semaphore>,
    provider_name: String,
    plan: ChildPlan,
    mut run: TaskRun,
    cancel: CancellationToken,
    _reservation: HistoryReservation,
) -> Result<TaskRun> {
    let parent_session_id = run.parent_session_id.clone();
    let plan_child_session_id = plan.child_session_id.clone();
    // Invariant 3. A child of a turn holding the device takes the hold's one permit: acquiring the
    // process permits deadlocks on its own parent, and `needed = 0` lets siblings overlap.
    let (device, needed) = match ledger.inherited_child_permits(&parent_session_id) {
        Some(siblings) => (siblings, 1),
        None => (permits, subagent_permits(&provider_name)),
    };
    let permit = tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        acquired = device.acquire_many_owned(needed) => Some(acquired?),
    };
    let Some(_permit) = permit else {
        // Cancelled while queued for the permit.
        runner.release(&plan_child_session_id).await;
        with_registry(&tasks, |registry| {
            registry.finish(&run.id, TaskStatus::Cancelled, None, None)
        });
        run.status = TaskStatus::Cancelled;
        run.finished_at = Some(chrono::Utc::now());
        report_child_progress(
            &parent_session_id,
            &run.id,
            &run.role,
            SubagentStatus::Cancelled,
            None,
        );
        return Ok(run);
    };

    run.status = TaskStatus::Running;
    with_registry(&tasks, |registry| registry.start(&run.id));
    report_child_progress(
        &parent_session_id,
        &run.id,
        &run.role,
        SubagentStatus::Running,
        None,
    );

    let max_turns = plan.max_turns;
    let outcome = runner.run(plan, cancel.clone()).await;
    runner.release(&plan_child_session_id).await;

    let (status, result, error) = match outcome {
        Ok(outcome) => {
            // Invariant 2, audited after the run: Goose can re-arm a `default_enabled` platform
            // extension the plan never asked for.
            let smuggled = stripped_builtins_present(&outcome.loaded_extensions);
            if !smuggled.is_empty() {
                tracing::error!(
                task_id = %run.id,
                role = %run.role,
                extensions = %smuggled.join(", "),
                "subagent loaded Goose builtins GIAP strips - discarding its result"
                );
                (
                    TaskStatus::Failed,
                    None,
                    Some(format!(
                        "subagent loaded stripped Goose builtins: {}",
                        smuggled.join(", ")
                    )),
                )
            } else {
                // Re-check the token after the await: Goose returns `Ok` when cancelled.
                let (status, result) = classify_outcome(
                    cancel.is_cancelled(),
                    outcome.assistant_turns,
                    max_turns,
                    outcome.last_text.as_deref(),
                );
                let error = match status {
                    TaskStatus::Failed => Some("subagent produced no answer".to_string()),
                    _ => None,
                };
                (status, result, error)
            }
        }
        Err(e) => (TaskStatus::Failed, None, Some(e.to_string())),
    };

    with_registry(&tasks, |registry| {
        registry.finish(&run.id, status, result.clone(), error.clone())
    });
    // Terminal frame carries `error` (GIAP's words), never `result` (the child's answer).
    report_child_progress(
        &parent_session_id,
        &run.id,
        &run.role,
        SubagentStatus::from(status),
        error.clone(),
    );
    run.status = status;
    run.result = result;
    run.error = error;
    run.finished_at = Some(chrono::Utc::now());
    Ok(run)
}

// ── The orchestrator ────────────────────────────────────────────────────────

/// Adapter: runs [`TaskSpec`]s as Goose child agents.
pub struct GooseOrchestrator {
    runner: Arc<dyn ChildRunner>,
    /// Always [`process_subagent_permits`]: the limit is per device, not per instance.
    permits: Arc<Semaphore>,
    /// An `Arc` because a background run outlives the `&self` that started it.
    tasks: Arc<Mutex<TaskRegistry>>,
    /// Where `GooseAdapter` publishes each live turn's authority; specs of ended turns are refused.
    authorities: Arc<TurnAuthorityRegistry>,
    /// Always [`process_device_ledger`]; written here, read by `GooseAdapter::turn_profile`.
    ledger: Arc<DeviceLedger>,
}

impl GooseOrchestrator {
    pub fn new(runner: Arc<dyn ChildRunner>, authorities: Arc<TurnAuthorityRegistry>) -> Self {
        Self {
            runner,
            permits: process_subagent_permits(),
            tasks: Arc::new(Mutex::new(TaskRegistry::default())),
            authorities,
            ledger: process_device_ledger(),
        }
    }

    fn with_registry<T>(&self, f: impl FnOnce(&mut TaskRegistry) -> T) -> T {
        with_registry(&self.tasks, f)
    }
}

#[async_trait]
impl Orchestrator for GooseOrchestrator {
    async fn spawn(&self, spec: TaskSpec) -> Result<TaskRun> {
        // Checked first: a spec whose parent turn has ended carries stale authority.
        let parent_turn = self
            .authorities
            .parent_turn_token(spec.parent_session_id())
            .ok_or_else(|| {
                anyhow!(
                    "no live turn holds the authority for session `{}` - refusing to run a \
                     delegation whose parent has already ended",
                    spec.parent_session_id()
                )
            })?;

        // Reserve at authorisation, not at first reply, or a concurrent parent turn trims as if it
        // owned the whole window. `Drop` releases it on every exit below.
        let reservation = self
            .ledger
            .reserve(spec.parent_session_id(), spec.context_fraction());

        let env = self.runner.environment(spec.parent_session_id()).await?;

        // Checked before the engine is touched, so a refusal leaves no child session behind.
        if spec.background() {
            if let Some(refusal) =
                BackgroundAvailability::for_provider(&env.provider_name).refusal()
            {
                tracing::info!(
                    target: "giap::trace",
                    kind = "background_delegation_refused",
                    role = %spec.role(),
                    provider = %env.provider_name,
                    "a background delegation was refused"
                );
                return Err(anyhow!(refusal));
            }
        }

        // A sync child's token derives from the turn's; a background run outlives the turn.
        let cancel = if spec.background() {
            CancellationToken::new()
        } else {
            parent_turn.child_token()
        };

        let child_session_id = self.runner.open_child_session(spec.role()).await?;
        // The role's persona is not on `TaskSpec` yet.
        let role_persona: Option<&str> = None;
        let plan = match build_child_plan(&spec, &child_session_id, &env, role_persona) {
            Ok(plan) => plan,
            Err(refused) => {
                self.runner.release(&child_session_id).await;
                return Err(anyhow!(refused));
            }
        };

        // A refused role model is only a WARN: the delegation still runs on the parent's model.
        match (plan.model.assigned(), plan.model.refusal()) {
            (Some(model), _) => tracing::info!(
                target: "giap::trace",
                kind = "role_model_assigned",
                task_id = %plan.task_id,
                role = %plan.role,
                model = %model,
                "subagent runs on the model its role asked for"
            ),
            (None, Some(refusal)) => tracing::warn!(
                target: "giap::trace",
                kind = "role_model_refused",
                task_id = %plan.task_id,
                role = %plan.role,
                "{refusal}"
            ),
            (None, None) => {}
        }

        // `TaskRun::started` stamps `Running`; the run is Queued until `drive_run` gets a permit.
        let mut run = TaskRun::started(&spec, chrono::Utc::now());
        run.status = TaskStatus::Queued;
        self.with_registry(|registry| registry.insert(run.clone(), cancel.clone()));

        // Emitted before the permit is acquired: on-device, a second delegation spends its time
        // queueing. Every registry status change needs a matching `report_child_progress`.
        report_child_progress(
            spec.parent_session_id(),
            &run.id,
            &run.role,
            SubagentStatus::Queued,
            None,
        );

        // The fork is last, so a background call that cannot proceed fails to the caller's face.
        let drive = drive_run(
            self.runner.clone(),
            self.tasks.clone(),
            self.ledger.clone(),
            self.permits.clone(),
            env.provider_name.clone(),
            plan,
            run.clone(),
            cancel,
            reservation,
        );
        if spec.background() {
            tokio::spawn(async move {
                if let Err(e) = drive.await {
                    // `drive_run` only fails before recording anything, so just log it.
                    tracing::error!(
                        target: "giap::trace",
                        kind = "background_delegation_failed",
                        "a background delegation could not be driven: {e}"
                    );
                }
            });
            // Still `Queued`: the port's contract for a background run.
            return Ok(run);
        }
        drive.await
    }

    async fn poll(&self, task_id: &str) -> Result<Option<TaskRun>> {
        Ok(self
            .with_registry(|registry| registry.tasks.get(task_id).map(|entry| entry.run.clone())))
    }

    async fn cancel(&self, task_id: &str) -> Result<()> {
        self.with_registry(|registry| {
            if let Some(entry) = registry.tasks.get(task_id) {
                entry.cancel.cancel();
            }
        });
        Ok(())
    }

    async fn list(&self, parent_session_id: &str) -> Result<Vec<TaskRun>> {
        Ok(self.with_registry(|registry| {
            let mut runs: Vec<TaskRun> = registry
                .tasks
                .values()
                .filter(|entry| entry.run.parent_session_id == parent_session_id)
                .map(|entry| entry.run.clone())
                .collect();
            runs.sort_by_key(|run| run.started_at);
            runs
        }))
    }

    /// The only session-wide cancellation for background runs, which own their tokens. Has no
    /// production caller: session deletion and shutdown live in `pond-api` and `pond-server`.
    async fn cancel_children_of(&self, parent_session_id: &str) -> Result<usize> {
        Ok(self.with_registry(|registry| {
            let mut stopped = 0usize;
            for entry in registry.tasks.values() {
                if entry.run.parent_session_id == parent_session_id
                    && !entry.run.status.is_terminal()
                {
                    entry.cancel.cancel();
                    stopped += 1;
                }
            }
            stopped
        }))
    }
}

// ── The production runner ───────────────────────────────────────────────────

/// [`ChildRunner`] over the live [`GooseAdapter`].
pub struct GooseChildRunner {
    adapter: Arc<crate::goose_agent::GooseAdapter>,
}

impl GooseChildRunner {
    pub fn new(adapter: Arc<crate::goose_agent::GooseAdapter>) -> Self {
        Self { adapter }
    }
}

#[async_trait]
impl ChildRunner for GooseChildRunner {
    async fn environment(&self, parent_session_id: &str) -> Result<ChildEnvironment> {
        self.adapter.child_environment(parent_session_id).await
    }

    async fn open_child_session(&self, plan_role: &str) -> Result<String> {
        self.adapter.open_child_session(plan_role).await
    }

    async fn run(&self, plan: ChildPlan, cancel: CancellationToken) -> Result<ChildOutcome> {
        self.adapter.run_child_agent(plan, cancel).await
    }

    async fn release(&self, child_session_id: &str) {
        self.adapter.release_child_session(child_session_id).await;
    }
}

#[cfg(test)]
mod tests;
