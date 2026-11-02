use super::*;
use crate::shared::mocks::mock_agent_prefill::MockAgentPrefillRegistry;
use crate::shared::ports::agent_prefill::AgentPrefillRegistry;
use crate::shared::services::long_task_roles::{planner_role, verifier_role, worker_role};

fn t(days: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).unwrap() + chrono::Duration::days(days)
}

fn role(name: &str, instructions: &str, groups: &str) -> AgentRole {
    let yaml = format!("instructions: {instructions}\ngiap_role:\n  tool_groups: [{groups}]\n");
    AgentRole::from_recipe_yaml(name, &yaml).unwrap().unwrap()
}

fn record(role: &AgentRole, compiled: Option<i64>, used: Option<i64>) -> PrefillRecord {
    PrefillRecord {
        definition_hash: definition_hash(role),
        compiled_at: compiled.map(t),
        last_used_at: used.map(t),
    }
}

#[test]
fn the_hash_is_fnv_1a_and_does_not_move_between_releases() {
    let mut empty = Fnv1a::new();
    empty.write(b"");
    assert_eq!(empty.hex(), "cbf29ce484222325");
    let mut a = Fnv1a::new();
    a.write(b"a");
    assert_eq!(a.hex(), "af63dc4c8601ec8c");
    let mut foobar = Fnv1a::new();
    foobar.write(b"foobar");
    assert_eq!(foobar.hex(), "85944171f73967e8");
    // Stored in `agent_prefills`: a change here recompiles every agent once, so make it on purpose.
    assert_eq!(
        definition_hash(&role("r", "go", "giap-weather, giap-knowledge")),
        "bf9ee1940579c100"
    );
}

#[test]
fn the_hash_follows_name_instructions_and_tools_and_nothing_else() {
    let base = role("r", "go", "giap-weather");
    let same = definition_hash(&base);
    assert_ne!(definition_hash(&role("s", "go", "giap-weather")), same);
    assert_ne!(definition_hash(&role("r", "stop", "giap-weather")), same);
    assert_ne!(definition_hash(&role("r", "go", "giap-knowledge")), same);
    assert_ne!(definition_hash(&role("r", "go", "")), same);
    // The group order as written does not matter: the set is sorted.
    assert_eq!(
        definition_hash(&role("r", "go", "giap-weather, giap-knowledge")),
        definition_hash(&role("r", "go", "giap-knowledge, giap-weather"))
    );
    // Vacuity control: a field outside the preamble leaves it alone.
    let turns = AgentRole::from_recipe_yaml(
        "r",
        "instructions: go\ngiap_role:\n  tool_groups: [giap-weather]\n  max_turns: 3\n",
    )
    .unwrap()
    .unwrap();
    assert_eq!(definition_hash(&turns), same);
    // Text moved across the separator is a different definition.
    assert_ne!(
        definition_hash(&role("ab", "c", "")),
        definition_hash(&role("a", "bc", ""))
    );
}

#[test]
fn a_default_agent_always_compiles() {
    let planner = planner_role().unwrap();
    let idle = record(&planner, Some(-100), Some(-100));
    assert_eq!(
        prefill_decision(&planner, true, Some(&idle), t(0), 14),
        PrefillDecision::Compile
    );
    // Vacuity control: the same record on a household agent is skipped.
    assert!(matches!(
        prefill_decision(&planner, false, Some(&idle), t(0), 14),
        PrefillDecision::Skip { .. }
    ));
    for shipped in [planner, worker_role().unwrap(), verifier_role().unwrap()] {
        assert!(
            is_default_agent(shipped.name()),
            "{} is shipped",
            shipped.name()
        );
    }
    assert!(!is_default_agent("trip-planner"));
}

#[test]
fn a_new_agent_compiles() {
    let r = role("r", "go", "");
    assert_eq!(
        prefill_decision(&r, false, None, t(0), 14),
        PrefillDecision::Compile
    );
}

#[test]
fn a_changed_definition_compiles_however_idle() {
    let before = role("r", "go", "giap-weather");
    let after = role("r", "go, and more", "giap-weather");
    let idle = record(&before, Some(-100), Some(-100));
    assert_eq!(
        prefill_decision(&after, false, Some(&idle), t(0), 14),
        PrefillDecision::Compile
    );
    // Vacuity control: unchanged and equally idle, it is skipped.
    assert!(matches!(
        prefill_decision(&before, false, Some(&idle), t(0), 14),
        PrefillDecision::Skip { .. }
    ));
    // Used before it was ever compiled: no hash, which reads as changed.
    let unhashed = PrefillRecord {
        definition_hash: String::new(),
        compiled_at: None,
        last_used_at: Some(t(-100)),
    };
    assert_eq!(
        prefill_decision(&before, false, Some(&unhashed), t(0), 14),
        PrefillDecision::Compile
    );
}

#[test]
fn use_within_the_retention_compiles_and_idleness_past_it_skips() {
    let r = role("r", "go", "");
    let within = record(&r, Some(-100), Some(-14));
    assert_eq!(
        prefill_decision(&r, false, Some(&within), t(0), 14),
        PrefillDecision::Compile
    );
    let past = record(&r, Some(-100), Some(-15));
    match prefill_decision(&r, false, Some(&past), t(0), 14) {
        PrefillDecision::Skip { reason } => {
            assert!(reason.contains("used 15 days ago"), "{reason}");
            assert!(reason.contains("14-day"), "{reason}");
        }
        other => panic!("an agent idle 15 days of 14 compiled: {other:?}"),
    }
    // The setting decides: the same agent with a longer retention compiles.
    assert_eq!(
        prefill_decision(&r, false, Some(&past), t(0), 30),
        PrefillDecision::Compile
    );
}

#[test]
fn use_is_read_before_the_last_compile() {
    let r = role("r", "go", "");
    // Compiled yesterday, used long ago: idle, since a compile is not a use.
    let recompiled = record(&r, Some(-1), Some(-40));
    assert!(matches!(
        prefill_decision(&r, false, Some(&recompiled), t(0), 14),
        PrefillDecision::Skip { .. }
    ));
    // Never used: the compile is all there is to go by.
    let fresh = record(&r, Some(-3), None);
    assert_eq!(
        prefill_decision(&r, false, Some(&fresh), t(0), 14),
        PrefillDecision::Compile
    );
    let stale = record(&r, Some(-20), None);
    match prefill_decision(&r, false, Some(&stale), t(0), 14) {
        PrefillDecision::Skip { reason } => assert!(reason.contains("compiled 20 days ago")),
        other => panic!("{other:?}"),
    }
    // Nothing on the record at all: compile.
    let blank = record(&r, None, None);
    assert_eq!(
        prefill_decision(&r, false, Some(&blank), t(0), 14),
        PrefillDecision::Compile
    );
}

#[test]
fn a_retention_below_one_day_counts_as_one() {
    let r = role("r", "go", "");
    let today = record(&r, Some(0), None);
    assert_eq!(
        prefill_decision(&r, false, Some(&today), t(0), 0),
        PrefillDecision::Compile
    );
    let two_days = record(&r, Some(-2), None);
    assert!(matches!(
        prefill_decision(&r, false, Some(&two_days), t(0), 0),
        PrefillDecision::Skip { .. }
    ));
    assert_eq!(DEFAULT_AGENT_PREFILL_RETENTION_DAYS, 14);
    assert_eq!(MIN_AGENT_PREFILL_RETENTION_DAYS, 1);
}

#[test]
fn a_pass_compiles_skips_and_forgets() {
    let worker = worker_role().unwrap();
    let busy = role("busy", "go", "");
    let idle = role("idle", "go", "");
    let roles = vec![worker.clone(), busy.clone(), idle.clone()];
    let mut records = HashMap::new();
    records.insert(worker.name().to_string(), record(&worker, Some(-90), None));
    records.insert("busy".to_string(), record(&busy, Some(-90), Some(-2)));
    records.insert("idle".to_string(), record(&idle, Some(-90), Some(-60)));
    records.insert("deleted-agent".to_string(), record(&idle, Some(-1), None));
    let plan = plan_prefills(&roles, &records, t(0), 14);
    let compiled: Vec<&str> = plan.compile.iter().map(|r| r.name()).collect();
    assert_eq!(compiled, vec![worker.name(), "busy"]);
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].0, "idle");
    assert_eq!(plan.forget, vec!["deleted-agent".to_string()]);
    // Vacuity control: with no records every agent compiles and nothing is forgotten.
    let fresh = plan_prefills(&roles, &HashMap::new(), t(0), 14);
    assert_eq!(fresh.compile.len(), 3);
    assert!(fresh.forget.is_empty());
}

#[tokio::test]
async fn the_registry_keeps_a_compile_and_a_use_apart() {
    let registry = MockAgentPrefillRegistry::new();
    registry.note_used("trip", t(-3)).await.unwrap();
    let row = registry.get_all().await.unwrap()["trip"].clone();
    assert_eq!(row.definition_hash, "");
    assert_eq!(row.compiled_at, None);
    registry.note_compiled("trip", "abc", t(-1)).await.unwrap();
    registry.note_used("trip", t(0)).await.unwrap();
    let row = registry.get_all().await.unwrap()["trip"].clone();
    assert_eq!(row.definition_hash, "abc");
    assert_eq!(
        row.compiled_at,
        Some(t(-1)),
        "a use must not clear the compile"
    );
    assert_eq!(row.last_used_at, Some(t(0)));
    registry.note_compiled("trip", "def", t(1)).await.unwrap();
    assert_eq!(
        registry.get_all().await.unwrap()["trip"].last_used_at,
        Some(t(0)),
        "a compile must not clear the use"
    );
    assert!(registry.forget("trip").await.unwrap());
    assert!(!registry.forget("trip").await.unwrap());
    assert!(registry.note_used(" ", t(0)).await.is_err());
}

#[test]
fn an_agents_state_says_what_the_next_pass_will_do() {
    let agent = role("study-planner", "Plan study.", "giap-task-work");
    let fresh = record(&agent, Some(0), Some(1));
    let state = |record: Option<&PrefillRecord>, building, queued, failed, now| {
        prefill_state(&agent, false, record, building, queued, failed, now, 14)
    };
    assert_eq!(
        state(Some(&fresh), false, false, false, t(2)),
        PrefillState::Ready
    );
    assert_eq!(
        state(Some(&fresh), true, false, false, t(2)),
        PrefillState::Building
    );
    assert_eq!(
        state(Some(&fresh), false, true, false, t(2)),
        PrefillState::Queued,
        "a rebuild a member asked for shows before anything else the record says"
    );
    assert_eq!(
        state(Some(&fresh), false, false, true, t(2)),
        PrefillState::Failed
    );
    assert_eq!(
        state(None, false, false, false, t(2)),
        PrefillState::NotBuilt
    );
    assert_eq!(
        state(Some(&fresh), false, false, false, t(30)),
        PrefillState::Idle,
        "unused past the retention"
    );
    let edited = role(
        "study-planner",
        "Plan study, by the week.",
        "giap-task-work",
    );
    assert_eq!(
        prefill_state(&edited, false, Some(&fresh), false, false, false, t(2), 14),
        PrefillState::Queued,
        "an edited agent is rebuilt on the next pass"
    );
    assert_eq!(
        prefill_state(&agent, true, Some(&fresh), false, false, false, t(30), 14),
        PrefillState::Ready,
        "a default agent never goes idle"
    );
}
