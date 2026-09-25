//! Everything that decides, driven through the real [`GooseOrchestrator::spawn`] against a fake
//! [`ChildRunner`]; `run_child_agent` itself needs a live engine.

use super::*;
use pond_core::models::services::context::model_class::ON_DEVICE_PROVIDERS;
use pond_core::shared::domain::orchestration::{
    AgentRole, DelegationAuthority, RolePersonalData, TaskRequest, DEFAULT_ROLE_MAX_TURNS,
};
use pond_core::shared::services::turn_authority::TurnAuthorityLease;
use pond_core::user_data::domain::profile::ProfileScope;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Taken first by every test that spawns: the semaphore is process-wide, so concurrent on-device
/// tests would make the remote-overlap controls fail at random.
static ONE_RUN_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

// ── Fixtures ────────────────────────────────────────────────────────────────

fn role(name: &str, groups: &[&str]) -> AgentRole {
    role_with_fraction(name, groups, 0.3)
}

/// A role with its own `context_fraction`, so a hardcoded fraction in `spawn` cannot pass.
fn role_with_fraction(name: &str, groups: &[&str], context_fraction: f32) -> AgentRole {
    AgentRole::new(
        name,
        "Answer the question from the tools you have, then stop.",
        groups.iter().map(|g| g.to_string()).collect(),
        RolePersonalData::Inherit,
        4,
        context_fraction,
    )
    .expect("fixture role is valid")
}

/// Built through a real `DelegationAuthority`, the only way to make a `TaskSpec`.
fn spec_for(role: &AgentRole, parent_groups: &[&str]) -> TaskSpec {
    spec_for_parent(role, parent_groups, PARENT_SESSION)
}

/// For a named parent: the ledger is process-wide, so reservation tests need their own session.
fn spec_for_parent(role: &AgentRole, parent_groups: &[&str], parent: &str) -> TaskSpec {
    DelegationAuthority::root(
        parent,
        ProfileScope::Household,
        parent_groups.iter().map(|g| g.to_string()).collect(),
    )
    .delegate(
        role,
        TaskRequest {
            role: role.name().to_string(),
            instructions: "what is the weather".to_string(),
            inputs: serde_json::Value::Null,
            background: false,
        },
    )
    .expect("fixture delegation is authorised")
}

/// The GIAP session id every fixture spec names as its parent.
const PARENT_SESSION: &str = "parent-session";

/// An orchestrator with a live parent turn, whose authority lasts until the lease is dropped.
fn live_turn(runner: Arc<dyn ChildRunner>) -> (Arc<GooseOrchestrator>, TurnAuthorityLease) {
    let (orchestrator, lease, _token) = live_turn_with_token(runner);
    (orchestrator, lease)
}

fn live_turn_with_token(
    runner: Arc<dyn ChildRunner>,
) -> (
    Arc<GooseOrchestrator>,
    TurnAuthorityLease,
    CancellationToken,
) {
    live_turn_for(PARENT_SESSION, runner)
}

fn live_turn_for(
    parent: &str,
    runner: Arc<dyn ChildRunner>,
) -> (
    Arc<GooseOrchestrator>,
    TurnAuthorityLease,
    CancellationToken,
) {
    let authorities = Arc::new(TurnAuthorityRegistry::new());
    let token = CancellationToken::new();
    let lease = authorities.publish(
        &format!("{parent}-goose-session"),
        DelegationAuthority::root(
            parent,
            ProfileScope::Household,
            ["giap-weather".to_string()].into_iter().collect(),
        ),
        token.clone(),
    );
    (
        Arc::new(GooseOrchestrator::new(runner, authorities)),
        lease,
        token,
    )
}

fn parent_tools(entries: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
    entries
        .iter()
        .map(|(ext, tools)| {
            (
                (*ext).to_string(),
                tools.iter().map(|t| (*t).to_string()).collect(),
            )
        })
        .collect()
}

fn env_with(provider: &str, tools: BTreeMap<String, BTreeSet<String>>) -> ChildEnvironment {
    ChildEnvironment {
        provider_name: provider.to_string(),
        base_system_prefix: "You are Pond, a privacy-first local assistant.".to_string(),
        parent_tools: tools,
    }
}

fn granted_tools(config: &ExtensionConfig) -> &[String] {
    match config {
        ExtensionConfig::Builtin {
            available_tools, ..
        } => available_tools,
        other => panic!("expected a Builtin extension config, got {other:?}"),
    }
}

fn extension_name(config: &ExtensionConfig) -> String {
    config.name().to_string()
}

// ── The fake engine ─────────────────────────────────────────────────────────

#[derive(Default)]
struct FakeRunnerState {
    plans: Mutex<Vec<ChildPlan>>,
    /// Child sessions opened, so a refusal is distinguishable from a run that failed.
    opened: Mutex<Vec<String>>,
    released: Mutex<Vec<String>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
    session_seq: AtomicUsize,
    /// The parent's reserved fraction, read inside the run: the only moment the claim exists.
    reserved_during_run: Mutex<Vec<f32>>,
}

struct FakeRunner {
    state: Arc<FakeRunnerState>,
    env: ChildEnvironment,
    /// What the fake engine returns; `None` panics if run.
    outcome: Option<ChildOutcome>,
    /// Cancel the run's token from inside and still return `Ok`, as Goose's reply loop does.
    cancel_from_inside: bool,
    /// Milliseconds to hold the concurrency permit, so overlap is observable.
    hold_ms: u64,
}

impl FakeRunner {
    fn new(env: ChildEnvironment) -> Self {
        Self {
            state: Arc::new(FakeRunnerState::default()),
            env,
            outcome: Some(ChildOutcome {
                last_text: Some("the weather is fine".to_string()),
                assistant_turns: 1,
                loaded_extensions: BTreeSet::new(),
            }),
            cancel_from_inside: false,
            hold_ms: 0,
        }
    }

    fn returning(mut self, outcome: ChildOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    fn cancelling_itself(mut self) -> Self {
        self.cancel_from_inside = true;
        self
    }

    fn holding_for(mut self, ms: u64) -> Self {
        self.hold_ms = ms;
        self
    }
}

#[async_trait]
impl ChildRunner for FakeRunner {
    async fn environment(&self, _parent_session_id: &str) -> Result<ChildEnvironment> {
        Ok(self.env.clone())
    }

    async fn open_child_session(&self, plan_role: &str) -> Result<String> {
        let n = self.state.session_seq.fetch_add(1, Ordering::SeqCst);
        let id = format!("child-{plan_role}-{n}");
        self.state
            .opened
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(id.clone());
        Ok(id)
    }

    async fn run(&self, plan: ChildPlan, cancel: CancellationToken) -> Result<ChildOutcome> {
        let now = self.state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.max_in_flight.fetch_max(now, Ordering::SeqCst);
        self.state
            .reserved_during_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(process_device_ledger().reserved_fraction(&plan.parent_session_id));
        self.state
            .plans
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(plan);
        if self.hold_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.hold_ms)).await;
        }
        if self.cancel_from_inside {
            cancel.cancel();
        }
        self.state.in_flight.fetch_sub(1, Ordering::SeqCst);
        Ok(self
            .outcome
            .clone()
            .expect("this fake was not meant to be run"))
    }

    async fn release(&self, child_session_id: &str) {
        self.state
            .released
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(child_session_id.to_string());
    }
}

// ── A turn is a run of messages, not a message ──────────────────────────────

/// One `AgentEvent::Message` as the drain loop sees it.
enum Frag<'a> {
    /// `role == Assistant`, already reduced by `as_concat_text()`.
    Assistant(&'a str),
    /// Any other role; Goose returns a tool response as a `User` message.
    ToolResponse,
}

/// Drive the reduction through `child_stream_step`, as the drain loop does, to test its mapping.
fn assemble(stream: &[Frag<'_>]) -> (Option<String>, u32) {
    let mut turns = ChildTurns::default();
    for fragment in stream {
        let (is_assistant, text) = match fragment {
            Frag::Assistant(text) => (true, *text),
            Frag::ToolResponse => (false, ""),
        };
        child_stream_step(&mut turns, is_assistant, text);
    }
    turns.finish()
}

#[test]
fn three_fragments_of_one_turn_are_one_turn_and_the_whole_answer() {
    let (answer, turns) = assemble(&[
        Frag::Assistant("It is 18 degrees "),
        Frag::Assistant("and clear "),
        Frag::Assistant("in Nairobi."),
    ]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear in Nairobi."),
        "the child's answer is not the concatenation of its fragments - assigning per message \
         hands the parent the last streamed word instead of the message"
    );
    assert_eq!(
        turns, 1,
        "three streamed fragments of one answer were counted as three turns"
    );
}

#[test]
fn a_tool_response_closes_a_turn_and_the_answer_is_the_second_turns_full_text() {
    let (answer, turns) = assemble(&[
        Frag::Assistant("Let me "),
        Frag::Assistant("check the forecast."),
        Frag::ToolResponse,
        Frag::Assistant("It is 18 degrees "),
        Frag::Assistant("and clear."),
    ]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear."),
        "the answer is not the whole of the child's LAST turn - a per-message assignment gives \
         its final fragment, and a first-turn answer gives what the child said before it had \
         looked anything up"
    );
    assert_eq!(
        turns, 2,
        "a tool response did not close the turn, so two provider calls counted as one"
    );
}

#[test]
fn a_single_complete_message_is_one_turn_and_is_the_answer() {
    let (answer, turns) = assemble(&[Frag::Assistant("It is 18 degrees and clear.")]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear."),
        "a child whose provider sends one complete block per message lost its answer"
    );
    assert_eq!(
        turns, 1,
        "one complete message was not one turn, so the shape that needs no assembly at all is \
         the one the assembler gets wrong"
    );
}

#[test]
fn an_empty_final_turn_does_not_erase_the_answer_before_it() {
    let (answer, turns) = assemble(&[
        Frag::Assistant("It is 18 degrees and clear."),
        Frag::ToolResponse,
        Frag::Assistant(""),
        Frag::Assistant("   "),
    ]);
    assert_eq!(
        answer.as_deref(),
        Some("It is 18 degrees and clear."),
        "a blank final turn overwrote the child's answer with nothing, which classify_outcome \
         then reports as Failed"
    );
    assert_eq!(
        turns, 2,
        "two provider calls were not counted as two turns: either a turn that produced only \
         reasoning did not count at all, or the four messages were counted one apiece"
    );
}

#[test]
fn a_per_token_stream_of_forty_fragments_is_one_turn_and_completes() {
    let words: Vec<String> = (0..40).map(|n| format!("w{n} ")).collect();
    let stream: Vec<Frag<'_>> = words.iter().map(|w| Frag::Assistant(w)).collect();
    let (answer, turns) = assemble(&stream);

    assert_eq!(
        turns, 1,
        "a 40-token streamed answer was counted as 40 assistant turns; with a role cap of at \
         most {DEFAULT_ROLE_MAX_TURNS} turns every real delegation is then reported as \
         TurnBudgetExhausted"
    );
    let (status, result) =
        classify_outcome(false, turns, DEFAULT_ROLE_MAX_TURNS, answer.as_deref());
    assert_eq!(
        status,
        TaskStatus::Completed,
        "the delegation answered and was reported as {status:?}"
    );
    assert_eq!(
        result.as_deref(),
        Some(words.join("").trim()),
        "the parent was handed something other than the child's whole answer"
    );
}

/// Vacuity control for the tests above: the assembler can report zero and several turns.
#[test]
fn the_turn_assembler_reports_nothing_for_an_empty_stream() {
    assert_eq!(assemble(&[]), (None, 0));
    assert_eq!(assemble(&[Frag::ToolResponse]), (None, 0));
    let (_, turns) = assemble(&[
        Frag::Assistant("one"),
        Frag::ToolResponse,
        Frag::Assistant("two"),
        Frag::ToolResponse,
        Frag::Assistant("three"),
    ]);
    assert_eq!(turns, 3);
}

#[test]
fn an_assistant_fragment_accumulates_and_any_other_role_closes_the_run() {
    let mut turns = ChildTurns::default();
    child_stream_step(&mut turns, true, "half ");
    child_stream_step(&mut turns, true, "an answer");
    child_stream_step(&mut turns, false, "");
    child_stream_step(&mut turns, true, "the answer");

    assert_eq!(
        turns.finish(),
        (Some("the answer".to_string()), 2),
        "a fragment's role decides whether it extends the open turn or ends it; getting that \
         backwards, or dropping the text, is exactly what made every delegation report \
         TurnBudgetExhausted with a word for an answer"
    );
}

/// The Goose behaviour `ChildTurns` rests on, read from the submodule.
#[test]
fn goose_still_accumulates_its_own_assistant_text_per_message() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goose/crates/goose/src/agents/agent.rs"
    ))
    .expect("the goose submodule must be initialised - `git submodule update --init --recursive`");
    assert!(
        source.contains("last_assistant_text.push_str(&text);"),
        "goose no longer accumulates streamed assistant text per message, so the fragment/turn \
         distinction ChildTurns is built on may no longer hold"
    );
    assert!(
        source.contains("turns_taken += 1;"),
        "goose's own turn counter is gone; GIAP's assistant_turns is an approximation of it and \
         the comparison in classify_outcome needs rechecking"
    );
}

// ── Invariant 3: concurrency 1 on anything that runs on this device ─────────

#[test]
fn every_provider_that_runs_on_this_device_takes_the_whole_semaphore() {
    for provider in ON_DEVICE_PROVIDERS {
        assert_eq!(
            subagent_permits(provider),
            SUBAGENT_PERMITS as u32,
            "provider `{provider}` runs on this device, so one child of it must hold every \
             permit - anything less lets two subagents share one GPU and one retained KV prefix"
        );
    }
}

/// Vacuity control for the test above.
#[test]
fn a_provider_somewhere_else_takes_only_one_permit() {
    assert_eq!(subagent_permits("anthropic"), 1);
    assert!(
        subagent_permits("local") > subagent_permits("anthropic"),
        "the on-device and remote cases ask for the same number of permits, so the assertion \
         next door is satisfied by everything and proves nothing"
    );
}

#[tokio::test]
async fn only_one_on_device_child_runs_at_a_time() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (orchestrator, _turn) = live_turn(runner);

    let mut handles = Vec::new();
    for _ in 0..3 {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert_eq!(
        state.max_in_flight.load(Ordering::SeqCst),
        1,
        "three subagents ran concurrently on an on-device provider - they interleave on one \
         GPU and overwrite each other's retained KV prefix, so the parent pays a full \
         re-prefill on its next turn"
    );
}

/// Vacuity control for the test above.
#[tokio::test]
async fn the_overlap_detector_can_actually_see_overlap() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (orchestrator, _turn) = live_turn(runner);

    let mut handles = Vec::new();
    for _ in 0..3 {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert!(
        state.max_in_flight.load(Ordering::SeqCst) > 1,
        "a remote provider is allowed concurrency and the harness saw none, so the on-device \
         assertion next door proves nothing"
    );
}

#[tokio::test]
async fn two_orchestrators_cannot_both_run_an_on_device_child() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    // One fake engine, so both orchestrators' children share one in-flight meter.
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (first, _turn_a) = live_turn(runner.clone());
    let (second, _turn_b) = live_turn(runner);

    let mut handles = Vec::new();
    for orchestrator in [first, second] {
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert_eq!(
        state.max_in_flight.load(Ordering::SeqCst),
        1,
        "two GooseOrchestrator instances each ran an on-device child at the same time, so the \
         subagent semaphore is per-instance rather than process-wide - two subagents then share \
         one GPU and overwrite each other's retained KV prefix, which is what invariant 3 \
         forbids"
    );
}

/// Vacuity control for the test above.
#[tokio::test]
async fn two_orchestrators_on_a_remote_provider_do_overlap() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (first, _turn_a) = live_turn(runner.clone());
    let (second, _turn_b) = live_turn(runner);

    let mut handles = Vec::new();
    for orchestrator in [first, second] {
        let spec = spec_for(&role, &["giap-weather"]);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert!(
        state.max_in_flight.load(Ordering::SeqCst) > 1,
        "two orchestrators on a hosted provider never overlapped, so the on-device assertion \
         next door proves nothing about the semaphore"
    );
}

#[tokio::test]
async fn a_child_waiting_for_a_permit_is_queued_rather_than_running() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(200));
    let (orchestrator, _turn) = live_turn(runner);

    let running = spec_for(&role, &["giap-weather"]);
    let running_id = running.id().to_string();
    let waiting = spec_for(&role, &["giap-weather"]);
    let waiting_id = waiting.id().to_string();

    let first = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move { orchestrator.spawn(running).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    let second = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move { orchestrator.spawn(waiting).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    let ahead = orchestrator
        .poll(&running_id)
        .await
        .unwrap()
        .expect("known");
    let behind = orchestrator
        .poll(&waiting_id)
        .await
        .unwrap()
        .expect("known");
    assert_eq!(
        behind.status,
        TaskStatus::Queued,
        "a child that has not been given a concurrency permit reported itself as {:?}; Queued is \
         the one state that variant exists to distinguish and nothing constructed it",
        behind.status
    );
    // Vacuity control: the run ahead of it is Running at the same instant.
    assert_eq!(
        ahead.status,
        TaskStatus::Running,
        "the run holding the permit was not reported as Running, so the assertion above is not \
         about queueing"
    );

    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    assert_eq!(
        orchestrator
            .poll(&waiting_id)
            .await
            .unwrap()
            .expect("known")
            .status,
        TaskStatus::Completed,
        "the queued run never left Queued, so the transition when the permit arrives is missing"
    );
}

// ── Invariant 1: a child's tools are a subset of its parent's ───────────────

#[test]
fn a_child_gets_only_what_its_spec_and_its_parent_both_have() {
    let role = role("researcher", &["giap-weather", "giap-knowledge"]);
    // giap-knowledge is outside the parent's authority; giap-news is not loaded on the parent.
    let spec = spec_for(&role, &["giap-weather", "giap-news"]);
    let tools = parent_tools(&[
        ("giap-weather", &["get_weather", "get_forecast"]),
        ("giap-knowledge", &["search"]),
        ("giap-news", &["headlines"]),
    ]);

    let configs = child_extensions(&spec, &tools).expect("nothing forbidden here");
    let names: Vec<String> = configs.iter().map(extension_name).collect();
    assert_eq!(
        names,
        vec!["giap-weather".to_string()],
        "the child was given an extension its spec did not authorise or its parent did not have \
         loaded - PAI-6 invariant 1 is that a subagent's scope is a SUBSET of its parent's"
    );
    // Sorted, so the allowlist (and so the tools JSON and KV prefix) is stable between runs.
    assert_eq!(
        granted_tools(&configs[0]),
        ["get_forecast".to_string(), "get_weather".to_string()]
    );
}

#[test]
fn an_extension_is_never_emitted_with_an_empty_tool_allowlist() {
    let role = role("researcher", &["giap-weather", "giap-knowledge"]);
    let spec = spec_for(&role, &["giap-weather", "giap-knowledge"]);
    // giap-knowledge is authorised, loaded on the parent, and has no tools.
    let tools = parent_tools(&[("giap-weather", &["get_weather"]), ("giap-knowledge", &[])]);

    let configs = child_extensions(&spec, &tools).unwrap();
    for config in &configs {
        assert!(
            !granted_tools(config).is_empty(),
            "extension `{}` was emitted with an empty available_tools, which Goose reads as \
             ALL TOOLS of that extension - the scope-widening default this phase exists to \
             avoid",
            extension_name(config)
        );
    }
    assert_eq!(
        configs.len(),
        1,
        "the empty extension must be dropped, not emitted"
    );
}

#[test]
fn available_tools_are_unprefixed_because_that_is_what_goose_matches() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let configs =
        child_extensions(&spec, &parent_tools(&[("giap-weather", &["get_weather"])])).unwrap();
    assert_eq!(granted_tools(&configs[0]), ["get_weather".to_string()]);
    assert!(
        !granted_tools(&configs[0])
            .iter()
            .any(|t| t.contains(TOOL_NAME_SEPARATOR)),
        "available_tools carried a prefixed name; ExtensionConfig::is_tool_available is called \
         with resolved.actual_tool_name, so every tool would be refused at dispatch"
    );
}

// ── Invariant 2: the ten stripped builtins stay stripped ───────────────────

#[test]
fn the_stripped_builtins_are_still_these_ten() {
    assert_eq!(
        GOOSE_STRIPPED_BUILTINS,
        [
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
        ]
    );
}

/// Only prefixed builtins (`todo`, `orchestrator`) can survive a failed strip into `parent_tools`.
#[test]
fn a_parent_whose_strip_failed_does_not_hand_its_builtins_to_a_child() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let tools = parent_tools(&[
        ("giap-weather", &["get_weather"]),
        ("todo", &["todo_write"]),
        ("orchestrator", &["start_agent", "send_message"]),
    ]);

    let configs = child_extensions(&spec, &tools).unwrap();
    let names: Vec<String> = configs.iter().map(extension_name).collect();
    for stripped in GOOSE_STRIPPED_BUILTINS {
        assert!(
            !names.iter().any(|n| n == stripped),
            "the child was handed `{stripped}`, one of the ten builtins GIAP strips from every \
             session - `orchestrator` is how a subagent drives another agent"
        );
    }
    // Vacuity control: the authorised extension still comes through.
    assert_eq!(
        names,
        vec!["giap-weather".to_string()],
        "the refusal above is passing because nothing came out at all"
    );
}

#[test]
fn a_bare_tool_name_belongs_to_no_extension_and_cannot_enter_a_plan() {
    for bare in ["shell", "delegate", "final_output", "text_editor", ""] {
        assert_eq!(
            split_extension_tool(bare),
            None,
            "`{bare}` was given an owning extension; goose exposes the unprefixed platform \
             extensions' tools under exactly these names and guessing an owner for one would \
             invent an extension the engine never named"
        );
    }
    // Vacuity control: real prefixed names do resolve.
    assert_eq!(
        split_extension_tool("giap-weather__get_weather"),
        Some(("giap-weather", "get_weather"))
    );
    assert_eq!(
        split_extension_tool("todo__todo_write"),
        Some(("todo", "todo_write"))
    );
    // A separator with nothing in front of it names no extension either.
    assert_eq!(split_extension_tool("__orphan"), None);
}

/// The failed-strip fixture above depends on these builtins keeping bare tool names.
#[test]
fn the_goose_builtins_that_expose_bare_tool_names_are_still_these() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goose/crates/goose/src/agents/platform_extensions/mod.rs"
    ))
    .expect("the goose submodule must be initialised - `git submodule update --init --recursive`");

    for bare in ["developer", "summon"] {
        assert!(
            goose_extension_is_unprefixed(&source, bare),
            "goose no longer exposes `{bare}`'s tools unprefixed, so a parent whose strip failed \
             now lists `{bare}__...` and the failed-strip fixture must use it"
        );
    }
    for prefixed in ["todo", "orchestrator"] {
        assert!(
            !goose_extension_is_unprefixed(&source, prefixed),
            "goose now exposes `{prefixed}`'s tools unprefixed, so the failed-strip fixture uses \
             a name production can no longer produce"
        );
    }
}

/// Reads one `PLATFORM_EXTENSIONS` entry's `unprefixed_tools` flag from Goose's literal map.
fn goose_extension_is_unprefixed(source: &str, extension: &str) -> bool {
    let key = format!("{extension}::EXTENSION_NAME,");
    let at = source
        .find(&key)
        .unwrap_or_else(|| panic!("goose's PLATFORM_EXTENSIONS no longer registers `{extension}`"));
    let entry = &source[at..];
    let end = entry.find("map.insert(").unwrap_or(entry.len());
    entry[..end].contains("unprefixed_tools: true")
}

/// Synthetic: three upstream narrowings would all have to fail first.
#[test]
fn a_spec_naming_a_stripped_builtin_refuses_to_produce_a_plan() {
    let role = role("saboteur", &["summon"]);
    let spec = spec_for(&role, &["summon"]);
    let refused = child_extensions(&spec, &parent_tools(&[("summon", &["delegate"])]))
        .expect_err("a spec naming `summon` must refuse");
    assert!(
        matches!(
            &refused,
            PlanRefused::ForbiddenExtension { extension, .. } if extension == "summon"
        ),
        "expected a ForbiddenExtension refusal naming summon, got {refused:?}"
    );
}

#[tokio::test]
async fn a_child_that_ended_up_with_a_stripped_builtin_has_its_result_discarded() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(
        FakeRunner::new(env).returning(ChildOutcome {
            last_text: Some("I ran a shell command for you".to_string()),
            assistant_turns: 1,
            loaded_extensions: ["giap-weather".to_string(), "summon".to_string()]
                .into_iter()
                .collect(),
        }),
    );
    let (orchestrator, _turn) = live_turn(runner);

    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Failed,
        "a child that loaded a stripped builtin was reported as a success"
    );
    assert_eq!(
        run.result_for_parent(),
        None,
        "the answer of a child that loaded `summon` reached the parent"
    );
}

#[test]
fn goose_agent_reads_the_one_stripped_builtin_list_rather_than_its_own() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);

    assert!(
        code.matches("GOOSE_STRIPPED_BUILTINS").count() >= 2,
        "goose_agent.rs must read the shared list in BOTH places that name these builtins - \
         the strip guard and the prompt filter"
    );
    for stripped in GOOSE_STRIPPED_BUILTINS {
        assert!(
            !code.contains(&format!("\"{stripped}\"")),
            "goose_agent.rs hardcodes the builtin name `{stripped}` again outside \
             GOOSE_STRIPPED_BUILTINS; two copies of this list is how one of them drifts while \
             the other is the only one that enforces anything"
        );
    }
}

/// Source with `//` comments removed, so prose cannot satisfy a guard.
fn strip_line_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| match line.find("//") {
            // Naive: also cuts at a `//` inside a string literal, which these guards tolerate.
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_goose_turn_cap_message_is_still_verbatim() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../goose/crates/goose/src/agents/agent.rs"
    ))
    .expect("the goose submodule must be initialised - `git submodule update --init --recursive`");
    let expected = format!("const MAX_TURNS_MESSAGE: &str = \"{GOOSE_MAX_TURNS_MESSAGE}\";");
    assert!(
        source.contains(&expected),
        "goose's MAX_TURNS_MESSAGE no longer matches the string this adapter checks for, so an \
         exhausted turn budget will be handed to the user as the subagent's answer"
    );
}

// ── Cancellation and budget exhaustion both return Ok from Goose ────────────

#[test]
fn a_cancelled_run_is_not_an_answer_however_much_text_came_back() {
    let (status, result) = classify_outcome(true, 2, 4, Some("here is half an answer"));
    assert_eq!(status, TaskStatus::Cancelled);
    assert_eq!(result, None);
}

#[test]
fn an_exhausted_turn_budget_is_not_an_answer() {
    let (by_sentinel, sentinel_result) =
        classify_outcome(false, 3, 4, Some(GOOSE_MAX_TURNS_MESSAGE));
    assert_eq!(
        by_sentinel,
        TaskStatus::TurnBudgetExhausted,
        "goose's own turn-cap sentence was taken for the subagent's answer - goose returns Ok \
         with MAX_TURNS_MESSAGE as the text when a child runs out of turns"
    );
    assert_eq!(
        sentinel_result, None,
        "the turn-cap sentence would have been reported to the user as the delegated result"
    );

    // Goose trips at `turns_taken > max_turns`.
    let (by_count, count_result) = classify_outcome(false, 5, 4, Some("still thinking"));
    assert_eq!(
        by_count,
        TaskStatus::TurnBudgetExhausted,
        "a child that took more turns than its budget was reported as having finished"
    );
    assert_eq!(count_result, None);
}

/// Vacuity control for the two above.
#[test]
fn an_ordinary_run_produces_an_answer() {
    let (status, result) = classify_outcome(false, 2, 4, Some("  the weather is fine  "));
    assert_eq!(status, TaskStatus::Completed);
    assert_eq!(result.as_deref(), Some("the weather is fine"));
    assert!(status.produced_an_answer());
}

#[test]
fn a_run_that_said_nothing_is_a_failure_rather_than_an_empty_answer() {
    assert_eq!(classify_outcome(false, 1, 4, None).0, TaskStatus::Failed);
    assert_eq!(
        classify_outcome(false, 1, 4, Some("   ")).0,
        TaskStatus::Failed
    );
}

#[tokio::test]
async fn a_child_cancelled_mid_run_reports_cancelled_not_completed() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(
        FakeRunner::new(env)
            .cancelling_itself()
            .returning(ChildOutcome {
                last_text: Some("half an answer".to_string()),
                assistant_turns: 1,
                loaded_extensions: BTreeSet::new(),
            }),
    );
    let (orchestrator, _turn) = live_turn(runner);

    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Cancelled,
        "the engine returned Ok with text after cancellation and it was taken at face value"
    );
    assert_eq!(run.result_for_parent(), None);
}

#[tokio::test]
async fn cancelling_a_parent_cancels_the_child_that_is_still_running() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(120));
    let (orchestrator, _turn) = live_turn(runner);

    let spawner = {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        tokio::spawn(async move { orchestrator.spawn(spec).await })
    };
    // Let the run reach the engine before cancelling the parent.
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    let stopped = orchestrator
        .cancel_children_of("parent-session")
        .await
        .unwrap();
    assert_eq!(stopped, 1, "cancelling the parent stopped no children");

    let run = spawner.await.unwrap().unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Cancelled,
        "the parent was cancelled and its child still reported an answer - PAI-6 invariant 5 is \
         that cancelling a parent cancels its children"
    );
    assert_eq!(run.result_for_parent(), None);
}

#[tokio::test]
async fn cancelling_a_different_parent_leaves_this_ones_children_alone() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(120));
    let (orchestrator, _turn) = live_turn(runner);

    let spawner = {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        tokio::spawn(async move { orchestrator.spawn(spec).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    assert_eq!(
        orchestrator
            .cancel_children_of("somebody-elses-session")
            .await
            .unwrap(),
        0,
        "cancel_children_of stopped a run belonging to a different parent session - one user \
         ending their turn would kill another household member's delegation"
    );

    let run = spawner.await.unwrap().unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Completed,
        "cancelling one parent's children stopped another parent's"
    );
}

// ── The prompt: THE respecification of this phase ───────────────────────────

#[test]
fn the_child_prompt_is_giaps_own_and_states_the_limits_the_child_runs_under() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let plan = build_child_plan(&spec, "child-1", &env, None).unwrap();

    assert!(
        plan.system_prompt.starts_with(&env.base_system_prefix),
        "the child's prompt must start from the SAME `build_prompt_partition` prefix the parent \
         uses; the phase bullet said to render `subagent_system.md`, which is deleted"
    );
    assert!(
        plan.system_prompt
            .contains("You are the `researcher` helper."),
        "the role is not named to the child"
    );
    assert!(
        plan.system_prompt.contains("at most 4 turns"),
        "the child is not told its turn budget"
    );
    assert!(
        plan.system_prompt.contains("cannot delegate"),
        "the child is not told it is at the depth cap"
    );
    assert!(
        plan.system_prompt.contains("giap-weather__get_weather"),
        "the child is not told which tools it has"
    );
    for leak in ["goose AI framework", "AAIF", "Agentic AI Foundation"] {
        assert!(
            !plan.system_prompt.contains(leak),
            "the child's prompt carries `{leak}` - that is Goose's own subagent_system.md, which \
             GiapProviderShim does not veto because its GOOSE_DEFAULT_MARKER is not in it"
        );
    }
    assert_eq!(plan.max_turns, 4);
    assert_eq!(plan.user_message, "what is the weather");
}

#[test]
fn the_child_is_told_to_return_a_finding_not_a_travelogue() {
    use pond_core::models::services::answer_contract::ANSWER_RULE;

    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let plan = build_child_plan(&spec, "child-1", &env, None).unwrap();

    assert!(
        !ANSWER_RULE.trim().is_empty(),
        "ANSWER_RULE is empty — this assertion would pass against any prompt"
    );
    assert!(
        plan.system_prompt.contains(ANSWER_RULE),
        "the child's prompt does not carry the answer rule. Its own user message has \
         no <system-context> to restate it in, so the envelope is the only place it \
         can arrive. Prompt:\n{}",
        plan.system_prompt
    );
}

/// Production passes `None` until `TaskSpec` carries the role's instructions.
#[test]
fn the_envelope_renders_a_roles_persona_when_it_is_given_one() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let persona = "You check the forecast and answer in one sentence, in Celsius.";

    let with_persona = build_child_plan(&spec, "child-1", &env, Some(persona)).unwrap();
    assert!(
        with_persona.system_prompt.contains(persona),
        "the role's stored instructions did not reach the child's prompt, so a role is still \
         just a name plus a tool list"
    );
    assert!(
        with_persona
            .system_prompt
            .contains("You are the `researcher` helper."),
        "the persona replaced the role's name instead of following it"
    );
    assert_eq!(with_persona.user_message, "what is the weather");

    // Vacuity control: without a persona the sentence is absent.
    let without = build_child_plan(&spec, "child-1", &env, None).unwrap();
    assert!(!without.system_prompt.contains(persona));
    assert!(without
        .system_prompt
        .contains("You are the `researcher` helper."));
}

#[test]
fn a_child_with_no_tools_is_told_so_rather_than_shown_an_empty_list() {
    let role = role("summariser", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather"]);
    // Authorised for giap-weather, but the parent has nothing loaded.
    let env = env_with("ollama", parent_tools(&[]));
    let plan = build_child_plan(&spec, "child-1", &env, None).unwrap();
    assert!(plan.extensions.is_empty());
    assert!(plan.system_prompt.contains("no tools on this run"));
}

// ── The registry ────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_finished_run_can_be_polled_and_listed_and_an_unknown_one_cannot() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let state = runner.state.clone();
    let (orchestrator, _turn) = live_turn(runner);

    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .unwrap();

    let polled = orchestrator.poll(&run.id).await.unwrap().expect("known id");
    assert_eq!(polled.status, TaskStatus::Completed);
    assert_eq!(polled.result_for_parent(), Some("the weather is fine"));
    assert!(orchestrator.poll("no-such-task").await.unwrap().is_none());

    let listed = orchestrator.list("parent-session").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(orchestrator.list("other-session").await.unwrap().is_empty());

    assert_eq!(
        state.released.lock().unwrap().len(),
        1,
        "the child's engine session was not released - every delegation would leak a row into \
         goose's sessions.db that nothing in GIAP will ever read or clean up"
    );
}

#[test]
fn eviction_never_removes_a_run_that_is_still_going() {
    let role = role("researcher", &["giap-weather"]);
    let mut registry = TaskRegistry::default();
    let mut running_ids = Vec::new();

    for _ in 0..MAX_TRACKED_TASKS {
        let spec = spec_for(&role, &["giap-weather"]);
        let run = TaskRun::started(&spec, chrono::Utc::now());
        running_ids.push(run.id.clone());
        registry.insert(run, CancellationToken::new());
    }
    // One more than the cap, with nothing terminal to evict.
    let spec = spec_for(&role, &["giap-weather"]);
    let extra = TaskRun::started(&spec, chrono::Utc::now());
    let extra_id = extra.id.clone();
    registry.insert(extra, CancellationToken::new());

    for id in &running_ids {
        assert!(
            registry.tasks.contains_key(id),
            "a RUNNING task was evicted; it can no longer be cancelled or polled, which is the \
             failure this cap exists to bound rather than cause"
        );
    }
    assert!(registry.tasks.contains_key(&extra_id));

    // Now finish one and add another: the terminal one is what goes.
    registry.finish(&running_ids[0], TaskStatus::Completed, None, None);
    let spec = spec_for(&role, &["giap-weather"]);
    registry.insert(
        TaskRun::started(&spec, chrono::Utc::now()),
        CancellationToken::new(),
    );
    assert!(
        !registry.tasks.contains_key(&running_ids[0]),
        "eviction did not reclaim the finished run, so the registry grows without bound"
    );
}

// ── PAI-6 P3: scope inheritance at the edge ─────────────────────────────────

#[tokio::test]
async fn a_delegation_whose_parent_turn_has_ended_does_not_run() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let state = runner.state.clone();
    let (orchestrator, lease) = live_turn(runner);

    let spec = spec_for(&role, &["giap-weather"]);
    drop(lease);

    let refused = orchestrator
        .spawn(spec)
        .await
        .expect_err("a delegation ran after its parent turn had ended");
    assert!(
        refused.to_string().contains("parent-session"),
        "the refusal does not name the session whose turn is gone: {refused}"
    );
    assert!(
        state.plans.lock().unwrap().is_empty(),
        "the engine ran a delegation nobody live had authorised"
    );
    assert!(
        state.opened.lock().unwrap().is_empty(),
        "a child engine session was created for a delegation that was refused"
    );
}

/// Vacuity control for the test above.
#[tokio::test]
async fn the_same_delegation_runs_while_its_parent_turn_is_live() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let (orchestrator, _turn) = live_turn(runner);
    let run = orchestrator
        .spawn(spec_for(&role, &["giap-weather"]))
        .await
        .expect("a live parent turn must be able to delegate");
    assert_eq!(run.status, TaskStatus::Completed);
}

#[tokio::test]
async fn cancelling_the_parents_turn_cancels_a_child_derived_from_it() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "anthropic",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(120));
    let (orchestrator, _turn, parent_token) = live_turn_with_token(runner);

    let spawner = {
        let orchestrator = orchestrator.clone();
        let spec = spec_for(&role, &["giap-weather"]);
        tokio::spawn(async move { orchestrator.spawn(spec).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    parent_token.cancel();

    let run = spawner.await.unwrap().unwrap();
    assert_eq!(
        run.status,
        TaskStatus::Cancelled,
        "the parent's TURN was cancelled and its child carried on - the child's token is not \
         derived from the parent's"
    );
    assert_eq!(run.result_for_parent(), None);
}

#[test]
fn the_plans_allowed_tool_names_are_its_available_tools_and_nothing_else() {
    let role = role("researcher", &["giap-weather"]);
    let spec = spec_for(&role, &["giap-weather", "giap-memory"]);
    let env = env_with(
        "ollama",
        parent_tools(&[
            ("giap-weather", &["get_weather", "get_forecast"]),
            ("giap-memory", &["recall_memories"]),
        ]),
    );
    let plan = build_child_plan(&spec, "child-1", &env, None).unwrap();

    let from_extensions: BTreeSet<String> = plan
        .extensions
        .iter()
        .flat_map(|config| match config {
            ExtensionConfig::Builtin {
                name,
                available_tools,
                ..
            } => available_tools
                .iter()
                .map(|tool| format!("{name}__{tool}"))
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect();
    let published: BTreeSet<String> = plan.allowed_tool_names.iter().cloned().collect();
    assert_eq!(
        published, from_extensions,
        "the shim allow-set and available_tools disagree; one of the two layers of invariant \
         1 is guarding a different set from the other"
    );
    assert_eq!(
        published,
        ["giap-weather__get_weather", "giap-weather__get_forecast"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "the role asked for giap-weather only and the plan published something else"
    );
    assert!(
        !published.contains("giap-memory__recall_memories"),
        "a tool the role never asked for was published to the child's shim entry"
    );
}

/// Source check: the root authority must be built from the turn's guest-subtracted allow-set.
#[test]
fn the_turn_authority_is_built_from_the_published_allow_set() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);

    let subtract = code
        .find("subtract_guest_denied_tools")
        .expect("PAI-1 P5's guest subtraction is gone from the turn path");
    let publish_to_shim = code
        .find("set_allowed_tools(allowed_tools.clone())")
        .expect("the turn's allow-set is no longer published to the shim");
    let publish_authority = code
        .find("turn_authorities.publish")
        .expect("no turn publishes a delegation authority, so PAI-6 P3 is inert");

    assert!(
        publish_authority > subtract,
        "the delegation authority is built BEFORE the guest subtraction, so a Guest turn would \
         hand a subagent the personal-data groups the turn itself was denied"
    );
    assert!(
        publish_authority > publish_to_shim,
        "the delegation authority is built from an allow-set that is not the one published to \
         the shim; the two can now disagree"
    );

    // The entitlement (the delegation ceiling) must be guest-subtracted too.
    let subtract_entitlement = code
        .find("entitled_tools.map(|tools|")
        .expect("the delegation ceiling no longer goes through a guest subtraction at all");
    assert!(
        subtract_entitlement > subtract,
        "the entitlement's guest subtraction runs before the allow-set's, so the two \
         can disagree about who a Guest is"
    );
    assert!(
        publish_authority > subtract_entitlement,
        "the delegation authority is built BEFORE the entitlement is guest-subtracted, so \
         a Guest turn would hand a subagent the personal-data groups it was denied"
    );

    let call = &code[publish_authority..(publish_authority + 700).min(code.len())];
    assert!(
        call.contains("turn_scope.clone()"),
        "the child's profile scope no longer comes from the turn's resolved scope"
    );
    // The entitlement, not the narrowed allow-set: narrowing only trims this turn's prompt, and
    // the parent can reach any permitted group via `enable_tool_group` anyway.
    assert!(
        call.contains("entitled_tools") && call.contains("unwrap_or(&allowed_tools)"),
        "the authority's tool set is no longer the turn's entitlement falling back to its \
         allow-set: {call}"
    );
    assert!(
        call.contains("cancel_token.clone()"),
        "the turn's cancellation token is not published, so a child cannot derive one from it"
    );
}

#[test]
fn the_child_agent_publishes_its_boundary_before_it_replies() {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);

    let allow = code
        .find("child_controls.set_allowed_tools(plan.allowed_tool_names")
        .expect("the child's allow-set is not published to the shim at all");
    let system = code
        .find("child_controls.set_system_override(Some(plan.system_prompt")
        .expect("the child's system prompt is not claimed, so the shim rebuilds it away");
    let reply = code
        .find(".reply(user_message.clone()")
        .expect("the child loop no longer replies");
    assert!(
        allow < reply && system < reply,
        "the child's boundary is published after its first provider call, so that call is \
         pass-through"
    );

    assert!(
        code.contains("self.shim_controls.forget_session(child_session_id)"),
        "a finished child's shim entry is left to age out; sixty-four of them evict a live \
         parent's allow-set, oldest-first"
    );
}

// ── Tripwires over the child loop ───────────────────────────────────────────
//
// Source reads, because `run_child_agent` needs a live engine; they assert call arguments.

#[test]
fn the_child_drain_loop_reduces_every_event_through_the_tested_step() {
    let drain = child_drain_loop();

    assert_eq!(
        drain.matches("child_stream_step(").count(),
        1,
        "the child drain loop no longer reduces its stream through exactly one \
         child_stream_step call, so whatever it does instead is untested: nothing without a \
         provider can reach that loop:\n{drain}"
    );
    let args = call_args(&drain, "child_stream_step(");
    assert!(
        args.contains("msg.role == rmcp::model::Role::Assistant"),
        "the drain loop no longer decides assistant-or-not from the message's own role, so a \
         tool response may no longer close a turn - and a run that never closes reads as one \
         turn with the wrong answer:\n{args}"
    );
    assert!(
        args.contains("msg.as_concat_text()"),
        "the drain loop passes something other than the message's concatenated TEXT. An empty \
         or constant argument makes every delegation report `subagent produced no answer`, and \
         anything that is not as_concat_text() may carry MessageContent::Thinking, which is how \
         a child's reasoning reaches the parent as its result:\n{args}"
    );
    assert!(
        !drain.contains("turns."),
        "something in the drain loop touches the turn assembler directly again. Every reduction \
         has to go through child_stream_step, or the part that decides is back inside a \
         function no test can run:\n{drain}"
    );
}

#[test]
fn the_invariant_two_audit_reads_the_child_after_it_has_run() {
    let child_loop = child_loop_source();
    let reply = child_loop
        .find(".reply(user_message.clone()")
        .expect("the child loop no longer replies");
    let returned = child_loop
        .find("Ok(crate::orchestrator::ChildOutcome {")
        .expect("the child loop no longer returns a ChildOutcome");
    assert!(
        reply < returned,
        "the child loop returns its outcome before it replies, so this window is not the run"
    );
    let after_run = &child_loop[reply..returned];

    let extend_at = after_run.find("loaded_extensions.extend(").expect(
        "nothing adds to the audited extension set after the child has replied, so \
         ChildOutcome::loaded_extensions is the pre-run snapshot again - an audit that can only \
         report what add_extension was handed one line earlier, which child_extensions had \
         already refused",
    );
    let statement = &after_run[extend_at..];
    let statement = &statement[..statement.find(';').unwrap_or(statement.len())];
    assert!(
        statement.contains("list_extensions()"),
        "the audited set is extended by something that is not a post-run read of the child's \
         extensions. A read whose result is dropped leaves the audit blind to an extension that \
         arrived after add_extension - which is the whole reason it is read twice:\n{statement}"
    );
}

/// Argument text of the first `callee` call in `source`; `callee` includes its opening paren.
fn call_args(source: &str, callee: &str) -> String {
    let at = source
        .find(callee)
        .unwrap_or_else(|| panic!("`{callee}` is not called here:\n{source}"));
    let after = &source[at + callee.len()..];
    let mut depth = 1usize;
    for (i, c) in after.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return after[..i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unterminated call to `{callee}`:\n{after}");
}

#[test]
fn a_child_run_tells_the_prefix_cache_that_it_moved_the_prefix() {
    let child_loop = child_loop_source();
    let reply = child_loop
        .find(".reply(user_message.clone()")
        .expect("the child loop no longer replies");
    let noted = child_loop.find("note_prefix_invalidated").expect(
        "a child run no longer tells the prefix-cache state machine anything, so the \
                parent's next turn will record a cold prefix as warm and PAI-4's age rung will \
                not fire",
    );
    assert!(
        noted < reply,
        "the prefix invalidation is recorded after the child replies; a reply that fails \
         part-way has still prefilled, and this programme's rule is that the failure direction \
         narrows"
    );
}

/// `run_child_agent`'s body with comments stripped, so tripwires see only that method's code.
fn child_loop_source() -> String {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);
    let at = code
        .find("pub(crate) async fn run_child_agent")
        .expect("GooseAdapter no longer owns the child loop");
    let body = &code[at..];
    // Ends at the column-zero `}` closing the `impl` block, whose last member this method is.
    let end = body.find("\n}\n").map(|at| at + 3).unwrap_or(body.len());
    body[..end].to_string()
}

/// Just the drain loop: the method as a whole legitimately calls `turns.finish()`.
fn child_drain_loop() -> String {
    let body = child_loop_source();
    let at = body
        .find("while let Some(event) = stream.next().await")
        .expect("the child loop no longer drains the reply stream");
    let rest = &body[at..];
    let end = rest
        .find("drop(stream);")
        .expect("the drain loop no longer ends by dropping the stream");
    rest[..end].to_string()
}

/// `GooseAdapter::chat_stream`'s body (the live turn), comments stripped.
fn chat_stream_source() -> String {
    let source =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/goose_agent.rs"))
            .expect("goose_agent.rs is next door");
    let code = strip_line_comments(&source);
    let at = code
        .find("pub async fn chat_stream(")
        .expect("GooseAdapter no longer owns the live chat stream");
    let body = &code[at..];
    // The method ends at the first `}` indented exactly four spaces.
    let end = body.find("\n    }").map(|at| at + 6).unwrap_or(body.len());
    let body = body[..end].to_string();
    assert!(
        body.contains("async_stream::stream!") && !body.contains("fn run_child_agent"),
        "the chat_stream slice is not chat_stream: it either lost its stream block or ran past \
         the end of the method, and every assertion over it is then about the wrong text"
    );
    body
}

#[test]
fn the_live_turn_claims_the_device_before_it_streams_anything() {
    let body = chat_stream_source();

    assert_eq!(
        body.matches("claim_device_for_turn(").count(),
        1,
        "the live turn does not take PAI-6 P4's device claim exactly once. With no claim, a \
         subagent replies between two of this turn's provider calls and overwrites the one \
         retained KV prefix, and the parent pays a full re-prefill it never sees the cause of"
    );
    let claim_at = body
        .find("claim_device_for_turn(")
        .expect("counted one just now");
    let stream_at = body
        .find("async_stream::stream!")
        .expect("chat_stream no longer builds an async stream");
    let first_yield = body.find("yield ").expect("chat_stream no longer yields");
    assert!(
        stream_at < claim_at,
        "the device claim is taken before the stream is returned, so the client waits on a \
         request that looks hung rather than on a stream that is waiting"
    );
    assert!(
        claim_at < first_yield,
        "the turn yields before it holds the device, so a child can be replying on this GPU \
         while this turn prefills"
    );

    // `let _ = claim_device_for_turn(..)` would drop the permit on the same line.
    let binding: String = body[..claim_at]
        .rsplit("let ")
        .next()
        .unwrap_or_default()
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    assert!(
        !binding.is_empty() && binding != "_",
        "the device claim is bound to `{binding}`; a wildcard binding drops the permit at the \
         end of the statement, so the turn holds the device for no time at all and every other \
         assertion in this test still passes"
    );
}

// ── PAI-6 P4: the budget, and the other half of invariant 3 ─────────────────

#[test]
fn a_parent_turn_on_this_device_takes_the_whole_semaphore() {
    for provider in ON_DEVICE_PROVIDERS {
        assert_eq!(
            parent_turn_permits(provider),
            SUBAGENT_PERMITS as u32,
            "provider `{provider}` runs on this device, so a parent turn must hold every permit \
             - anything less lets a subagent reply between two of the parent's provider calls \
             and overwrite the one retained KV prefix, which costs the parent a full re-prefill"
        );
        assert_eq!(
            parent_turn_permits(provider),
            subagent_permits(provider),
            "a parent turn and a child of `{provider}` ask for different numbers of permits, so \
             one of them can start while the other holds the device"
        );
    }
}

/// Vacuity control for the test above.
#[test]
fn a_parent_turn_on_a_hosted_provider_claims_nothing() {
    assert_eq!(
        parent_turn_permits("anthropic"),
        0,
        "a hosted provider has no shared KV prefix on this box, so a turn of it must not take \
         the device semaphore at all"
    );
    assert!(
        parent_turn_permits("local") > parent_turn_permits("anthropic"),
        "on-device and hosted parent turns claim the same thing, so the assertion next door is \
         about nothing"
    );
}

#[test]
fn a_reservation_is_released_exactly_and_the_budget_comes_back_whole() {
    let ledger = process_device_ledger();
    let session = "p4-ledger-arithmetic";
    assert_eq!(ledger.reserved_fraction(session), 0.0);

    let first = ledger.reserve(session, 0.3);
    let second = ledger.reserve(session, 0.3);
    assert!(
        (ledger.reserved_fraction(session) - 0.6).abs() < 1e-6,
        "two children of one parent must claim both their shares, got {}",
        ledger.reserved_fraction(session)
    );

    drop(first);
    assert!(
        (ledger.reserved_fraction(session) - 0.3).abs() < 1e-6,
        "releasing one child released more than its own share, got {}",
        ledger.reserved_fraction(session)
    );

    drop(second);
    assert_eq!(
        ledger.reserved_fraction(session),
        0.0,
        "the parent's budget did not come back to whole after every child ended"
    );
    assert_eq!(ledger.live_children(session), 0, "an entry was left behind");
}

#[test]
fn children_cannot_reserve_more_of_a_window_than_it_has() {
    let ledger = process_device_ledger();
    let session = "p4-ledger-oversubscribed";
    let _a = ledger.reserve(session, 0.5);
    let _b = ledger.reserve(session, 0.5);
    let _c = ledger.reserve(session, 0.5);
    assert_eq!(
        ledger.reserved_fraction(session),
        1.0,
        "an oversubscribed parent reported a fraction above 1.0, which is not a share of \
         anything"
    );
}

#[test]
fn a_reservation_belongs_to_the_conversation_that_made_it() {
    let ledger = process_device_ledger();
    let _held = ledger.reserve("p4-mine", 0.5);
    assert_eq!(
        ledger.reserved_fraction("p4-someone-elses"),
        0.0,
        "a delegation in one conversation shrank another conversation's history budget"
    );
}

/// Two fractions, neither the fixture's 0.3, so a hardcoded fraction in `spawn` cannot pass.
#[tokio::test]
async fn while_a_child_runs_its_parents_history_budget_is_reserved() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    for (fraction, parent) in [
        (0.75_f32, "p4-live-child-parent-three-quarters"),
        (0.4_f32, "p4-live-child-parent-two-fifths"),
    ] {
        let role = role_with_fraction("researcher", &["giap-weather"], fraction);
        let env = env_with(
            "ollama",
            parent_tools(&[("giap-weather", &["get_weather"])]),
        );
        let runner = Arc::new(FakeRunner::new(env));
        let state = runner.state.clone();
        let (orchestrator, _turn, _token) = live_turn_for(parent, runner);

        let run = orchestrator
            .spawn(spec_for_parent(&role, &["giap-weather"], parent))
            .await
            .expect("the delegation runs");
        assert_eq!(run.status, TaskStatus::Completed);

        let observed = state
            .reserved_during_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(observed.len(), 1, "the fake engine did not run");
        assert!(
            (observed[0] - fraction).abs() < 1e-6,
            "while the child was replying its parent had {} of its history budget reserved and \
             the role states context_fraction {fraction}. 0.0 means the parent's next turn \
             budgets as though it still owned the whole window; any other constant means the \
             fraction a role states is read by nobody",
            observed[0]
        );
        assert_eq!(
            process_device_ledger().reserved_fraction(parent),
            0.0,
            "the child finished and its claim on the parent's window outlived it"
        );
    }
}

#[tokio::test]
async fn a_delegation_that_is_refused_still_gives_the_budget_back() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let parent = "p4-refused-parent";
    let role = role("saboteur", &["summon"]);
    let env = env_with("ollama", parent_tools(&[("summon", &["delegate"])]));
    let runner = Arc::new(FakeRunner::new(env));
    let (orchestrator, _turn, _token) = live_turn_for(parent, runner);

    let refused = orchestrator
        .spawn(spec_for_parent(&role, &["summon"], parent))
        .await;
    assert!(refused.is_err(), "a spec naming a stripped builtin ran");
    assert_eq!(
        process_device_ledger().reserved_fraction(parent),
        0.0,
        "a refused delegation left its claim on the parent's history budget behind, so the \
         parent's every later turn trims against a window it is told it does not have"
    );
}

#[tokio::test]
async fn a_child_runs_under_its_parents_device_claim_rather_than_deadlocking_behind_it() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let parent = "p4-inheriting-parent";
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let (orchestrator, _turn, token) = live_turn_for(parent, runner);

    // Exactly what `chat_stream` holds for the whole of an on-device turn.
    let claim = claim_device_for_turn(parent, "ollama", &token).await;
    assert!(
        claim.is_some(),
        "an on-device turn took no device claim, so this test is not about inheritance"
    );
    assert!(process_device_ledger().session_holds_device(parent));

    let run = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        orchestrator.spawn(spec_for_parent(&role, &["giap-weather"], parent)),
    )
    .await
    .expect(
        "the child never started: it queued for the device permits its own parent's turn is \
         holding, which is a deadlock that also strands an sse_semaphore permit",
    )
    .expect("the delegation runs");
    assert_eq!(run.status, TaskStatus::Completed);

    drop(claim);
    assert!(
        !process_device_ledger().session_holds_device(parent),
        "the turn's device claim outlived it"
    );
}

#[tokio::test]
async fn siblings_of_one_delegating_turn_still_run_one_at_a_time() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let parent = "p4-sibling-parent";
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env).holding_for(60));
    let state = runner.state.clone();
    let (orchestrator, _turn, token) = live_turn_for(parent, runner);

    // Exactly what `chat_stream` holds for the whole of an on-device turn.
    let claim = claim_device_for_turn(parent, "ollama", &token).await;
    assert!(
        claim.is_some(),
        "an on-device turn took no device claim, so this test is not about inheritance at all"
    );

    let mut handles = Vec::new();
    for _ in 0..3 {
        let orchestrator = orchestrator.clone();
        let spec = spec_for_parent(&role, &["giap-weather"], parent);
        handles.push(tokio::spawn(async move { orchestrator.spawn(spec).await }));
    }
    for handle in handles {
        let run = tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect(
                "a sibling never finished: inheriting has become queueing behind the parent's \
                 own claim, which is the deadlock that also strands an sse_semaphore permit",
            )
            .unwrap()
            .expect("the delegation runs");
        assert_eq!(run.status, TaskStatus::Completed);
    }

    assert_eq!(
        state.max_in_flight.load(Ordering::SeqCst),
        1,
        "three children of ONE parent turn ran concurrently on an on-device provider. Each of \
         them inherited the parent's device hold, which belongs to the session rather than to \
         one child, so nothing serialised them against each other: they interleave on one GPU \
         and overwrite each other's retained KV prefix"
    );
    // Vacuity control: all three really reached the engine.
    assert_eq!(
        state.plans.lock().unwrap_or_else(|e| e.into_inner()).len(),
        3,
        "fewer than three children reached the engine, so max_in_flight is about a delegation \
         that never happened"
    );
    drop(claim);
}

/// Vacuity control for the inheritance tests above.
#[tokio::test]
async fn a_child_of_another_session_waits_for_the_turn_that_holds_the_device() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let parent = "p4-waiting-parent";
    let role = role("researcher", &["giap-weather"]);
    let env = env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_weather"])]),
    );
    let runner = Arc::new(FakeRunner::new(env));
    let (orchestrator, _turn, _token) = live_turn_for(parent, runner);

    let other = CancellationToken::new();
    let claim = claim_device_for_turn("p4-unrelated-conversation", "ollama", &other)
        .await
        .expect("an on-device turn claims the device");

    let spec = spec_for_parent(&role, &["giap-weather"], parent);
    let task_id = spec.id().to_string();
    let spawned = {
        let orchestrator = orchestrator.clone();
        tokio::spawn(async move { orchestrator.spawn(spec).await })
    };

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert_eq!(
        orchestrator
            .poll(&task_id)
            .await
            .unwrap()
            .expect("the run is registered")
            .status,
        TaskStatus::Queued,
        "a child ran while an UNRELATED conversation's turn held the device - inheritance is \
         being granted to every session rather than to the child's own parent, which is \
         invariant 3 with the parent half removed"
    );

    drop(claim);
    let run = tokio::time::timeout(std::time::Duration::from_secs(5), spawned)
        .await
        .expect("the child never ran after the device was released")
        .unwrap()
        .expect("the delegation runs");
    assert_eq!(run.status, TaskStatus::Completed);
    assert_eq!(
        process_device_ledger().reserved_fraction(parent),
        0.0,
        "the queued-then-run child left its reservation behind"
    );
}

#[test]
fn the_turn_profile_reads_the_live_child_ledger() {
    let source = strip_line_comments(include_str!("../goose_agent.rs"));

    let start = source
        .find("async fn turn_profile(")
        .expect("turn_profile is gone; the adapter's single budget producer has moved");
    let body = &source[start..start + 600];
    let end = body.find("\n    }").expect("unterminated turn_profile");
    let body = &body[..end];

    // Behaviour is covered in goose_agent.rs's tests; this checks only the key passed.
    let args = call_args(body, "Self::profile_for_session(");
    assert!(
        args.contains("process_device_ledger()"),
        "turn_profile no longer asks the process ledger what this session's live children have \
         reserved, so PAI-6 P4's budget is carried on the spec and read by nobody - exactly the \
         state P2 and P3 left it in:\n{args}"
    );
    assert!(
        args.contains("giap_session_id"),
        "turn_profile no longer passes the session it was asked about, so every turn on the \
         pond reads one session's reservations:\n{args}"
    );
    assert!(
        !args.contains("format!") && !args.contains("resolve_goose_session"),
        "turn_profile DERIVES a lookup key instead of passing the GIAP session id it was given. \
         Reservations are filed under that id, so a Goose-side id reads 0.0 for every session \
         and no live child ever shrinks anything - and this codebase has made exactly that \
         mix-up before, see resolve_goose_session:\n{args}"
    );

    let producers: usize = adapter_sources()
        .iter()
        .map(|(_, code)| code.matches("CompactionProfile::for_windows(").count())
        .sum();
    assert_eq!(
        producers, 1,
        "there is more than one place in this adapter that builds a CompactionProfile from \
         windows; only the one inside profile_for applies the subagent reservation, so the \
         others budget as though no child were live"
    );
    let appliers: usize = adapter_sources()
        .iter()
        .map(|(_, code)| code.matches("with_history_reserved(").count())
        .sum();
    assert_eq!(
        appliers, 1,
        "the reservation is applied in more than one place, so two paths can disagree about \
         how much of this parent's window is already claimed"
    );
}

/// Every non-test source file in this crate, comments and inline test modules stripped.
fn adapter_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<(String, String)>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("this crate's src directory must be readable: {e}"));
        for entry in entries {
            let path = entry.expect("a readable directory entry").path();
            if path.is_dir() {
                walk(&path, out);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            // Skipped: this file names the very constructs the canaries forbid.
            if path.file_name().and_then(|n| n.to_str()) == Some("tests.rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).expect("a readable source file");
            let production = source
                .split("\nmod tests {")
                .next()
                .unwrap_or("")
                .to_string();
            out.push((path.display().to_string(), strip_line_comments(&production)));
        }
    }

    let mut out = Vec::new();
    walk(
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src")),
        &mut out,
    );
    // Vacuity control: a near-empty walk would satisfy any count-based guard.
    assert!(
        out.len() >= 8,
        "the crate source walk found {} files; whatever it is scanning is not this adapter, and \
         every count asserted over it is meaningless",
        out.len()
    );
    out
}

#[test]
fn nothing_in_this_adapter_can_construct_a_delegation_depth() {
    for (path, source) in adapter_sources() {
        assert!(
            !source.contains("DelegationDepth("),
            "{path} constructs a DelegationDepth. P1 made the cap structural by leaving no \
             public constructor from a number; an adapter-side one puts the depth back in the \
             hands of whichever caller writes the literal"
        );
    }
    assert_eq!(
        pond_core::shared::domain::orchestration::MAX_DELEGATION_DEPTH,
        1,
        "the delegation depth cap moved; a subagent that can spawn is a recursive loop on a \
         home server"
    );
}

// ── PAI-6 P6: what a child may say, and how it reaches its parent ───────────

#[test]
fn a_progress_frame_carries_the_tool_name_and_nothing_else() {
    const REASONING: &str = "her blood-pressure medication is in the household memory";
    const ANSWER: &str = "Let me look that up for you.";
    const ARGUMENT: &str = "blood pressure medication";

    let arguments: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&format!(r#"{{"query":"{ARGUMENT}"}}"#)).expect("fixture args parse");
    let msg = goose::conversation::message::Message::assistant()
        .with_thinking(REASONING, "")
        .with_text(ANSWER)
        .with_tool_request(
            "call-1",
            Ok(
                rmcp::model::CallToolRequestParams::new("giap-memory__recall_memories".to_string())
                    .with_arguments(arguments),
            ),
        );

    let names = child_tool_names(&msg);

    // Leak checks first, so a leak fails with a message naming it rather than a bare diff.
    let everything = names.join(" ");
    for (what, leaked) in [
        ("the child's REASONING", REASONING),
        ("the child's answer TEXT", ANSWER),
        ("the tool call's ARGUMENTS", ARGUMENT),
    ] {
        assert!(
            !everything.contains(leaked),
            "{what} reached the parent's stream. `as_concat_text()` is what used to \
             stop this on the child path and reading `msg.content` gave it up; the \
             replacement is that this function cannot express anything but a name"
        );
    }

    assert_eq!(
        names,
        vec!["giap-memory__recall_memories".to_string()],
        "the tool name is what a parent's stream is told a child is doing"
    );
}

/// Vacuity control for the test above.
#[test]
fn a_message_with_no_tool_call_contributes_no_frame() {
    let thinking_only = goose::conversation::message::Message::assistant()
        .with_thinking("I should check the weather first", "");
    assert!(
        child_tool_names(&thinking_only).is_empty(),
        "a reasoning-only message is not a tool call and must produce no frame"
    );

    let malformed = goose::conversation::message::Message::assistant().with_tool_request(
        "call-2",
        Err(rmcp::model::ErrorData::invalid_params(
            "bad arguments",
            None,
        )),
    );
    assert!(
        child_tool_names(&malformed).is_empty(),
        "a tool call the engine could not parse has no name to report, and inventing \
         one puts a model's malformed output on the wire as though it were a call"
    );
}

#[tokio::test]
async fn a_delegation_reports_its_lifecycle_to_the_parent_that_authorised_it() {
    let _serialised = ONE_RUN_AT_A_TIME.lock().await;
    const PARENT: &str = "progress-lifecycle-parent";

    let mut stream = process_progress_bus().subscribe(PARENT);

    let runner = Arc::new(FakeRunner::new(env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_forecast"])]),
    )));
    let (orchestrator, _lease, _token) = live_turn_for(PARENT, runner);
    let role = role("researcher", &["giap-weather"]);
    let run = orchestrator
        .spawn(spec_for_parent(&role, &["giap-weather"], PARENT))
        .await
        .expect("the delegation runs");

    let mut seen = Vec::new();
    while let Ok(frame) = stream.rx.try_recv() {
        seen.push(frame);
    }

    assert_eq!(
        seen.iter()
            .map(|frame| frame.status)
            .collect::<Vec<SubagentStatus>>(),
        vec![
            SubagentStatus::Queued,
            SubagentStatus::Running,
            SubagentStatus::Completed,
        ],
        "the parent must hear the run queue, start and finish -- got {seen:?}"
    );
    for frame in &seen {
        assert_eq!(
            frame.task_id, run.id,
            "every frame names the run it is about"
        );
        assert_eq!(frame.role, "researcher");
        assert_eq!(frame.parent_session_id, PARENT);
        assert_eq!(
            frame.detail, None,
            "a lifecycle frame carries no detail; the child's answer is the \
             delegate tool's RESULT and reaches the parent through that"
        );
    }
}

#[tokio::test]
async fn a_frame_reaches_only_the_parent_it_belongs_to() {
    let _serialised = ONE_RUN_AT_A_TIME.lock().await;
    const MINE: &str = "progress-routing-mine";
    const THEIRS: &str = "progress-routing-theirs";

    let mut mine = process_progress_bus().subscribe(MINE);
    let mut theirs = process_progress_bus().subscribe(THEIRS);

    let runner = Arc::new(FakeRunner::new(env_with(
        "ollama",
        parent_tools(&[("giap-weather", &["get_forecast"])]),
    )));
    let (orchestrator, _lease, _token) = live_turn_for(MINE, runner);
    let role = role("researcher", &["giap-weather"]);
    orchestrator
        .spawn(spec_for_parent(&role, &["giap-weather"], MINE))
        .await
        .expect("the delegation runs");

    assert!(
        mine.rx.try_recv().is_ok(),
        "the parent that authorised the delegation heard nothing about it"
    );
    assert!(
        theirs.rx.try_recv().is_err(),
        "another live turn was told about a delegation it did not authorise"
    );
}

#[tokio::test]
async fn a_subscription_that_ends_takes_only_its_own_channel() {
    const SESSION: &str = "progress-restamp-session";
    let stale = process_progress_bus().subscribe(SESSION);
    let mut live = process_progress_bus().subscribe(SESSION);
    drop(stale);

    report_child_progress(
        SESSION,
        "task-1",
        "researcher",
        SubagentStatus::Running,
        None,
    );

    let frame = live
        .rx
        .try_recv()
        .expect("the live turn's channel must survive the stale turn's Drop");
    assert_eq!(frame.task_id, "task-1");

    drop(live);
    // And the live turn's own `Drop` really does unsubscribe.
    report_child_progress(
        SESSION,
        "task-2",
        "researcher",
        SubagentStatus::Running,
        None,
    );
    let mut after = process_progress_bus().subscribe(SESSION);
    assert!(
        after.rx.try_recv().is_err(),
        "a frame published while nobody was subscribed was delivered to the next \
         turn of that session, which would open a delegation tree it never asked for"
    );
}

/// Cancel-safety: the fixture publishes from inside the engine's poll, as a real child does.
#[tokio::test]
async fn an_engine_item_survives_a_progress_frame() {
    const SESSION: &str = "progress-interleave-session";
    let mut progress = process_progress_bus().subscribe(SESSION);

    let engine = async_stream::stream! {
        for item in 1..=3u32 {
            for call in 0..2u32 {
                report_child_progress(
                    SESSION,
                    &format!("task-{item}"),
                    "researcher",
                    SubagentStatus::Tool,
                    Some(format!("tool-{item}-{call}")),
                );
            }
            // The parent's `Next` future is dropped here each time; the item must still arrive.
            tokio::task::yield_now().await;
            yield item;
        }
    };
    let mut engine = Box::pin(engine);

    let mut seen: Vec<String> = Vec::new();
    loop {
        match next_parent_step(&mut engine, &mut progress).await {
            ParentStep::Progress(frame) => {
                seen.push(frame.detail.expect("a tool frame names its tool"))
            }
            ParentStep::Engine(item) => seen.push(format!("item-{item}")),
            ParentStep::EngineEnded => break,
        }
    }

    assert_eq!(
        seen,
        vec![
            "tool-1-0", "tool-1-1", "item-1", "tool-2-0", "tool-2-1", "item-2", "tool-3-0",
            "tool-3-1", "item-3",
        ],
        "every engine item and every frame must arrive; a missing item-N is the \
         select dropping an in-flight poll, which is the cancel-safety this loop rests on"
    );
}

/// A never-idle engine (a background-run shape) would starve frames under engine-first bias.
#[tokio::test]
async fn a_frame_does_not_wait_for_the_engine_to_go_idle() {
    const SESSION: &str = "progress-hot-engine-session";
    let mut progress = process_progress_bus().subscribe(SESSION);

    let engine = async_stream::stream! {
        for item in 1..=3u32 {
            report_child_progress(
                SESSION,
                &format!("task-{item}"),
                "researcher",
                SubagentStatus::Tool,
                Some(format!("tool-{item}")),
            );
            yield item;
        }
    };
    let mut engine = Box::pin(engine);

    let mut seen: Vec<String> = Vec::new();
    loop {
        match next_parent_step(&mut engine, &mut progress).await {
            ParentStep::Progress(frame) => {
                seen.push(frame.detail.expect("a tool frame names its tool"))
            }
            ParentStep::Engine(item) => seen.push(format!("item-{item}")),
            ParentStep::EngineEnded => break,
        }
    }

    assert_eq!(
        seen,
        vec!["item-1", "tool-1", "item-2", "tool-2", "item-3", "tool-3"],
        "a frame published while the engine still had events queued was not \
         delivered within one event of being published"
    );
}

#[test]
fn the_child_drain_loop_reads_message_content_only_through_the_named_producer() {
    let drain = child_drain_loop();

    assert_eq!(
        drain.matches("child_tool_names(").count(),
        1,
        "the child drain loop no longer takes its tool names from exactly one \
         child_tool_names call:\n{drain}"
    );
    assert_eq!(
        call_args(&drain, "child_tool_names(").trim(),
        "&msg",
        "child_tool_names is called with something other than the message the loop \
         just received"
    );
    assert!(
        !drain.contains("msg.content"),
        "the drain loop reads a child message's content list directly again. \
         `as_concat_text()` is what has been standing in for PAI-5's reasoning gate \
         on this path -- the child path does not go through the GooseAdapter producer \
         that gate lives at -- so a direct read is how a subagent's reasoning reaches \
         the UI on an install with show_thinking OFF:\n{drain}"
    );

    let frame = call_args(&drain, "report_child_progress(");
    for (what, expected) in [
        ("the parent it belongs to", "plan.parent_session_id"),
        ("the run it is about", "plan.task_id"),
        ("the role to label it with", "plan.role"),
        ("the status", "SubagentStatus::Tool"),
    ] {
        assert!(
            frame.contains(expected),
            "the drain's progress frame does not name {what} (`{expected}`). A frame \
             keyed on anything but the parent's GIAP session id is published to a \
             channel nobody is listening on, which is a feature that silently does \
             nothing:\n{frame}"
        );
    }
    assert!(
        !frame.contains("as_concat_text") && !frame.contains("arguments"),
        "the drain's progress frame carries the child's text or its tool call's \
         arguments; `detail` is a tool NAME (PAI-2 minimisation):\n{frame}"
    );
}

#[test]
fn the_live_turn_subscribes_to_its_own_delegations() {
    let stream = chat_stream_source();

    assert_eq!(
        stream.matches("process_progress_bus().subscribe(").count(),
        1,
        "the live turn no longer subscribes exactly once to the progress bus"
    );
    assert_eq!(
        call_args(&stream, "process_progress_bus().subscribe(").trim(),
        "&session_id",
        "the turn subscribes with something other than its GIAP session id, which is \
         what a TaskSpec names as its parent -- so every frame is published to a key \
         nobody is listening on"
    );

    let select = call_args(&stream, "next_parent_step(");
    let engine = select
        .find("&mut goose_stream")
        .expect("the drain no longer selects over the engine's own stream");
    let progress = select.find("&mut progress").expect(
        "the drain no longer selects over the progress channel, so a frame \
                 published during a delegation waits for the child to finish",
    );
    assert!(
        engine < progress,
        "next_parent_step takes the engine first and the progress channel second; \
         swapped, this call does not type-check today and would silently reverse the \
         two the day both are generic:\n{select}"
    );

    let arm = &stream[stream
        .find("ParentStep::Progress(frame)")
        .expect("the drain no longer handles a progress frame")..];
    let arm = &arm[..arm.find("ParentStep::EngineEnded").unwrap_or(arm.len())];
    assert!(
        arm.contains("yield Ok(frame.into())"),
        "a progress frame is no longer yielded straight to the client:\n{arm}"
    );
    for forbidden in ["total_output_chars", "produced_visible", "turn_stats"] {
        assert!(
            !arm.contains(forbidden),
            "the progress arm touches `{forbidden}`. A frame about a CHILD is not this \
             turn's output: it must not be counted as tokens the parent produced, nor \
             make an otherwise-empty turn look answered:\n{arm}"
        );
    }
}

// ── PAI-6 P7: the role's model ──────────────────────────────────────────────
//
// On-device a role model means a second GGUF load plus a re-prefill, so it is refused.

/// A role that asks for a model of its own.
fn role_wanting_model(name: &str, groups: &[&str], model: &str) -> AgentRole {
    role(name, groups)
        .with_model(Some(model.to_string()))
        .expect("a non-blank model is valid")
}

/// A parent-like `ModelConfig`; `request_headers` is set because the JSON comparison skips it.
fn parent_model_config() -> goose_providers::model::ModelConfig {
    goose_providers::model::ModelConfig::new("parent-model")
        .with_context_limit(Some(8192))
        .with_temperature(Some(0.4))
        .with_max_tokens(Some(1024))
        .with_request_headers(Some(
            [("x-pond".to_string(), "1".to_string())]
                .into_iter()
                .collect(),
        ))
}

/// `ModelConfig` has no `PartialEq`: compare its serialisation plus the field serde skips.
fn same_config(
    left: &goose_providers::model::ModelConfig,
    right: &goose_providers::model::ModelConfig,
) -> bool {
    serde_json::to_value(left).unwrap() == serde_json::to_value(right).unwrap()
        && left.request_headers == right.request_headers
}

#[test]
fn a_role_model_never_reaches_the_engine_on_a_provider_that_runs_here() {
    for provider in ON_DEVICE_PROVIDERS {
        let role = role_wanting_model("researcher", &["giap-weather"], "qwen3-14b");
        let spec = spec_for(&role, &["giap-weather"]);
        let env = env_with(
            provider,
            parent_tools(&[("giap-weather", &["get_forecast"])]),
        );
        let plan = build_child_plan(&spec, "child-1", &env, None).expect("the plan is authorised");

        assert_eq!(
            plan.model,
            ChildModel::RefusedOnDevice {
                requested: "qwen3-14b".to_string(),
                provider: provider.to_string(),
            },
            "{provider}: the plan did not record the refusal, so nothing downstream can say why \
             the role's model was not used"
        );
        // And the thing that actually decides what the engine is handed.
        let applied = child_model_config(&plan.model, parent_model_config());
        assert_eq!(
            applied.model_name, "parent-model",
            "{provider}: a delegation swapped the resident model. On this device that is a full \
             model load plus a re-prefill, and the parent's next turn pays for both"
        );
        assert!(
            same_config(&applied, &parent_model_config()),
            "{provider}: a refused role model changed the child's config anyway"
        );
    }
}

#[test]
fn a_role_model_never_reaches_the_engine_on_a_provider_this_pond_cannot_place() {
    for provider in ["mock", "lmstudio", "pond-spark", ""] {
        let role = role_wanting_model("researcher", &["giap-weather"], "qwen3-14b");
        let spec = spec_for(&role, &["giap-weather"]);
        let env = env_with(
            provider,
            parent_tools(&[("giap-weather", &["get_forecast"])]),
        );
        let plan = build_child_plan(&spec, "child-1", &env, None).expect("the plan is authorised");

        assert_eq!(
            plan.model,
            ChildModel::RefusedUnknownProvider {
                requested: "qwen3-14b".to_string(),
                provider: provider.to_string(),
            },
            "`{provider}`: the plan honoured a role's model for a provider nothing can place"
        );
        let applied = child_model_config(&plan.model, parent_model_config());
        assert_eq!(
            applied.model_name, "parent-model",
            "`{provider}`: a delegation swapped the resident model on the strength of an \
             unrecognised provider name"
        );
        assert!(same_config(&applied, &parent_model_config()));
    }
}

/// Vacuity control for the two tests above.
#[test]
fn a_role_model_is_used_when_the_provider_runs_somewhere_else() {
    for provider in ["anthropic", "openai", "openrouter"] {
        let role = role_wanting_model("researcher", &["giap-weather"], "claude-haiku-4");
        let spec = spec_for(&role, &["giap-weather"]);
        let env = env_with(
            provider,
            parent_tools(&[("giap-weather", &["get_forecast"])]),
        );
        let plan = build_child_plan(&spec, "child-1", &env, None).expect("the plan is authorised");

        assert_eq!(
            plan.model,
            ChildModel::Assigned("claude-haiku-4".to_string()),
            "{provider}: the role's model was dropped, so P7 is inert everywhere"
        );
        let applied = child_model_config(&plan.model, parent_model_config());
        assert_eq!(applied.model_name, "claude-haiku-4");
        assert_eq!(
            applied.context_limit, None,
            "{provider}: the child carried the PARENT model's window onto a different model. \
             Goose backfills a None from the registry entry for the model actually named; a Some \
             is budgeted against the wrong one"
        );
        // Everything that is not the model or its window survives the swap.
        assert_eq!(applied.temperature, Some(0.4));
        assert_eq!(applied.max_tokens, Some(1024));
        assert_eq!(
            applied.request_headers,
            parent_model_config().request_headers,
            "{provider}: the swap dropped the provider's per-request headers, which serde skips \
             and no JSON comparison would have seen"
        );
    }
}

#[test]
fn a_role_that_names_no_model_leaves_the_parents_config_alone() {
    for provider in ON_DEVICE_PROVIDERS
        .iter()
        .chain(["anthropic", "openai"].iter())
    {
        let spec = spec_for(&role("researcher", &["giap-weather"]), &["giap-weather"]);
        let env = env_with(
            provider,
            parent_tools(&[("giap-weather", &["get_forecast"])]),
        );
        let plan = build_child_plan(&spec, "child-1", &env, None).expect("the plan is authorised");
        assert_eq!(plan.model, ChildModel::Inherited);
        assert!(
            same_config(
                &child_model_config(&plan.model, parent_model_config()),
                &parent_model_config()
            ),
            "{provider}: a role that asked for nothing changed the child's config anyway"
        );
    }
}

#[test]
fn the_child_is_given_the_config_the_plan_decided_before_the_provider_is_set() {
    let body = child_loop_source();

    let args = call_args(&body, "crate::orchestrator::child_model_config(");
    assert!(
        args.contains("&plan.model"),
        "the child's model config is derived from something other than the plan's decision, so \
         the on-device refusal is being re-decided at the engine: {args}"
    );
    assert!(
        args.contains("model_config"),
        "the parent's own config is no longer the base of the child's: {args}"
    );

    let applied = body
        .find("crate::orchestrator::child_model_config(")
        .expect("run_child_agent no longer derives the child's model config");
    let set = body
        .find("child\n            .update_provider(")
        .or_else(|| body.find(".update_provider("))
        .expect("the child is no longer given a provider");
    assert!(
        applied < set,
        "the model config is derived AFTER the child's provider is set. `update_provider` is \
         what persists the model onto the child's session row, so a swap after it never reaches \
         a provider call and P7 is silently inert"
    );

    assert_eq!(
        body.matches("plan.model").count(),
        1,
        "`plan.model` is read more than once in the child loop. One read, through \
         `child_model_config`, is what keeps `RefusedOnDevice` from being handled a second time \
         by somebody matching on the variant:\n{body}"
    );
}

/// Vacuity control for the tripwires over `child_loop_source()`.
#[test]
fn the_child_loop_slice_is_the_child_loop() {
    let body = child_loop_source();
    assert!(body.contains("plan.child_session_id"));
    assert!(body.contains(".reply(user_message.clone()"));
    assert!(
        !body.contains("pub async fn chat_stream("),
        "the child-loop slice ran past the end of run_child_agent"
    );
}

// ── PAI-6 P8: background delegations ────────────────────────────────────────

/// A spec that asked to run in the background, for a named parent session.
fn background_spec_for(role: &AgentRole, parent_groups: &[&str], parent: &str) -> TaskSpec {
    DelegationAuthority::root(
        parent,
        ProfileScope::Household,
        parent_groups.iter().map(|g| g.to_string()).collect(),
    )
    .delegate(
        role,
        TaskRequest {
            role: role.name().to_string(),
            instructions: "what is the weather".to_string(),
            inputs: serde_json::Value::Null,
            background: true,
        },
    )
    .expect("fixture delegation is authorised")
}

fn weather_env(provider: &str) -> ChildEnvironment {
    env_with(
        provider,
        parent_tools(&[("giap-weather", &["get_forecast"])]),
    )
}

/// Also covers unplaceable names; the message is asserted because a small model must act on it.
#[tokio::test]
async fn a_background_run_is_refused_on_every_provider_that_runs_on_this_device() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    for provider in ON_DEVICE_PROVIDERS
        .iter()
        .copied()
        .chain(["mock", "lmstudio"])
    {
        let runner = Arc::new(FakeRunner::new(weather_env(provider)));
        let state = runner.state.clone();
        let (orchestrator, _lease, _token) =
            live_turn_for("p8-refusal", runner as Arc<dyn ChildRunner>);
        let spec = background_spec_for(
            &role("researcher", &["giap-weather"]),
            &["giap-weather"],
            "p8-refusal",
        );

        let refusal = match orchestrator.spawn(spec).await {
            Err(refused) => refused.to_string(),
            Ok(run) => panic!(
                "`{provider}` is not known to run somewhere else, so a background delegation on \
                 it must be refused - it started anyway ({:?}), and the parent's next reply is \
                 now queueing behind a child nobody is waiting for",
                run.status
            ),
        };
        assert!(
            refusal.contains(provider) && refusal.contains("without `background`"),
            "{provider}: the refusal neither names the provider nor says what to do instead, so \
             the model will retry the identical call: {refusal}"
        );
        // Refused before the engine was touched: no child session was opened.
        assert!(
            state.opened.lock().unwrap().is_empty(),
            "{provider}: a refused background delegation still created a child engine session"
        );
        assert!(
            state.plans.lock().unwrap().is_empty(),
            "{provider}: a refused background delegation ran anyway"
        );
        assert_eq!(
            process_device_ledger().reserved_fraction("p8-refusal"),
            0.0,
            "{provider}: a refused background delegation left a reservation behind, so the \
             parent's history budget stays shrunk for the rest of the conversation"
        );
    }
}

/// Vacuity control for the refusal; also sees the reservation from outside the child. 0.6 so no
/// single hardcoded fraction in `spawn` passes every test.
#[tokio::test]
async fn a_background_run_returns_before_the_child_finishes_and_can_be_polled() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let runner = Arc::new(FakeRunner::new(weather_env("anthropic")).holding_for(120));
    let state = runner.state.clone();
    let (orchestrator, _lease, _token) =
        live_turn_for("p8-background", runner as Arc<dyn ChildRunner>);
    let spec = background_spec_for(
        &role_with_fraction("researcher", &["giap-weather"], 0.6),
        &["giap-weather"],
        "p8-background",
    );
    let task_id = spec.id().to_string();

    let started = std::time::Instant::now();
    let run = orchestrator.spawn(spec).await.expect("hosted, so allowed");
    let returned_in = started.elapsed();
    // Read first: the claim is only observable while the child holds (120ms).
    let reserved_while_running = process_device_ledger().reserved_fraction("p8-background");

    assert!(
        !run.status.is_terminal(),
        "a background spawn returned a finished run, so it waited for the child after all: {:?}",
        run.status
    );
    assert!(
        (reserved_while_running - 0.6).abs() < 1e-6,
        "spawn returned with a background child running and its parent had \
         {reserved_while_running} of its history budget reserved; the role states 0.6. 0.0 means \
         a background delegation takes no claim on the window it is about to spend, so the \
         parent's next turn trims as though nothing else were live"
    );
    assert_eq!(
        run.result_for_parent(),
        None,
        "a run that has not finished must hand the parent no answer"
    );
    assert!(
        returned_in < std::time::Duration::from_millis(100),
        "spawn took {returned_in:?} against a child that holds for 120ms, so it is still \
         synchronous"
    );

    // And it really did run, and `poll` really does see it get there.
    for _ in 0..100 {
        if orchestrator
            .poll(&task_id)
            .await
            .unwrap()
            .is_some_and(|r| r.status.is_terminal())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let finished = orchestrator
        .poll(&task_id)
        .await
        .unwrap()
        .expect("the run is still tracked");
    assert_eq!(finished.status, TaskStatus::Completed);
    assert_eq!(finished.result_for_parent(), Some("the weather is fine"));
    assert_eq!(
        state.plans.lock().unwrap().len(),
        1,
        "the background task did not drive the child exactly once"
    );
    // The same claim as seen from inside the run.
    let observed = state
        .reserved_during_run
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    assert_eq!(observed.len(), 1, "the fake engine did not run");
    assert!(
        (observed[0] - 0.6).abs() < 1e-6,
        "while the background child was replying its parent had {} of its history budget \
         reserved, and the role states 0.6",
        observed[0]
    );
    assert_eq!(
        process_device_ledger().reserved_fraction("p8-background"),
        0.0,
        "the background child's claim on its parent's history budget outlived the child"
    );
}

#[tokio::test]
async fn a_background_run_survives_its_turn_and_is_still_cancellable_by_its_session() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let runner = Arc::new(FakeRunner::new(weather_env("anthropic")).holding_for(400));
    let (orchestrator, lease, turn_token) =
        live_turn_for("p8-outlives", runner as Arc<dyn ChildRunner>);
    let spec = background_spec_for(
        &role("researcher", &["giap-weather"]),
        &["giap-weather"],
        "p8-outlives",
    );
    let task_id = spec.id().to_string();
    orchestrator.spawn(spec).await.expect("hosted, so allowed");

    // End the turn as `chat_stream` does: cancel the token, drop the lease.
    turn_token.cancel();
    drop(lease);
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    let still = orchestrator.poll(&task_id).await.unwrap().unwrap();
    assert!(
        !still.status.is_terminal(),
        "the background run died with the turn that asked for it, which makes it not a \
         background run: {:?}",
        still.status
    );

    // ... and the cascade that owns it now is the SESSION's.
    let stopped = orchestrator
        .cancel_children_of("p8-outlives")
        .await
        .unwrap();
    assert_eq!(stopped, 1, "cancel_children_of did not reach the run");
    for _ in 0..100 {
        if orchestrator
            .poll(&task_id)
            .await
            .unwrap()
            .is_some_and(|r| r.status.is_terminal())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let ended = orchestrator.poll(&task_id).await.unwrap().unwrap();
    assert_eq!(
        ended.status,
        TaskStatus::Cancelled,
        "a cancelled background run must not look like a completion"
    );
    assert_eq!(ended.result_for_parent(), None);
}

/// Vacuity control for the test above.
#[tokio::test]
async fn a_synchronous_run_still_dies_with_the_turn_that_asked_for_it() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let runner = Arc::new(FakeRunner::new(weather_env("anthropic")).holding_for(400));
    let (orchestrator, _lease, turn_token) =
        live_turn_for("p8-sync-dies", runner as Arc<dyn ChildRunner>);
    let spec = spec_for_parent(
        &role("researcher", &["giap-weather"]),
        &["giap-weather"],
        "p8-sync-dies",
    );

    let driver = tokio::spawn({
        let orchestrator = orchestrator.clone();
        async move { orchestrator.spawn(spec).await.unwrap() }
    });
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    turn_token.cancel();

    let run = driver.await.expect("the synchronous run returns");
    assert_eq!(
        run.status,
        TaskStatus::Cancelled,
        "cancelling a parent turn no longer cancels the child it is waiting on"
    );
}

#[tokio::test]
async fn a_background_run_can_be_cancelled_by_its_own_id() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let runner = Arc::new(FakeRunner::new(weather_env("anthropic")).holding_for(400));
    let (orchestrator, _lease, _token) = live_turn_for("p8-by-id", runner as Arc<dyn ChildRunner>);
    let spec = background_spec_for(
        &role("researcher", &["giap-weather"]),
        &["giap-weather"],
        "p8-by-id",
    );
    let task_id = spec.id().to_string();
    orchestrator.spawn(spec).await.expect("hosted, so allowed");
    tokio::time::sleep(std::time::Duration::from_millis(40)).await;

    orchestrator.cancel(&task_id).await.unwrap();
    for _ in 0..100 {
        if orchestrator
            .poll(&task_id)
            .await
            .unwrap()
            .is_some_and(|r| r.status.is_terminal())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        orchestrator.poll(&task_id).await.unwrap().unwrap().status,
        TaskStatus::Cancelled
    );
}

#[tokio::test]
async fn a_background_run_still_needs_a_live_parent_turn() {
    let _serial = ONE_RUN_AT_A_TIME.lock().await;
    let runner = Arc::new(FakeRunner::new(weather_env("anthropic")));
    let state = runner.state.clone();
    let (orchestrator, lease, _token) = live_turn_for("p8-no-turn", runner as Arc<dyn ChildRunner>);
    let spec = background_spec_for(
        &role("researcher", &["giap-weather"]),
        &["giap-weather"],
        "p8-no-turn",
    );
    drop(lease);

    let refusal = orchestrator
        .spawn(spec)
        .await
        .expect_err("no live turn holds the authority")
        .to_string();
    assert!(
        refusal.contains("already ended"),
        "expected the stale-parent refusal, got: {refusal}"
    );
    assert!(state.opened.lock().unwrap().is_empty());
}
