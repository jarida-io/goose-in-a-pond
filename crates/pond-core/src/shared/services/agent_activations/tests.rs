use super::*;
use crate::shared::domain::long_task::{
    HandBackAsks, HandToAsk, PlanStepParts, StepKey, MAX_HAND_OFF_BRIEF_CHARS,
};
use crate::shared::domain::role_block::Subject;
use crate::shared::domain::time_tick::{TimeBoundary, TimeTick};
use crate::shared::services::long_task_runner::{next_run, NextRun, PlanReason};
use crate::user_data::domain::device::{DeviceId, DeviceStateChanged};
use crate::user_data::domain::sensor::{CameraEvent, SensorReading};
use crate::user_data::domain::session::IdentificationSource;
use chrono::TimeZone;

fn t(secs: i64) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 28, 4, 0, 0).unwrap() + chrono::Duration::seconds(secs)
}

fn role(name: &str, body: &str) -> AgentRole {
    let yaml = format!("instructions: {name} does its job.\ngiap_role:\n  tool_groups: []\n{body}");
    AgentRole::from_recipe_yaml(name, &yaml)
        .unwrap_or_else(|e| panic!("{name}: {e}"))
        .unwrap()
}

fn agent(role: AgentRole, owner: Option<&str>) -> ActivationAgent {
    ActivationAgent {
        role,
        owner_profile_id: owner.map(str::to_string),
    }
}

fn presence(member: &str, transition: PresenceTransition, at: DateTime<Utc>) -> BusEvent {
    BusEvent::Presence(ProfilePresence {
        profile_id: member.to_string(),
        transition,
        source: IdentificationSource::PairedDevice,
        confidence: None,
        session_id: "s-1".to_string(),
        at,
    })
}

fn camera(id: &str, kind: &str) -> BusEvent {
    BusEvent::Camera(CameraEvent {
        id: None,
        camera_id: id.to_string(),
        event_type: kind.to_string(),
        confidence: Some(0.9),
        snapshot_path: None,
        metadata: None,
        acknowledged: false,
        created_at: t(0),
    })
}

fn reading(device: &str, kind: &str, value: f64) -> BusEvent {
    BusEvent::Sensor(SensorReading {
        device_id: device.to_string(),
        sensor_type: kind.to_string(),
        value,
        unit: "C".to_string(),
        recorded_at: t(0),
    })
}

fn device(id: &str, key: &str) -> BusEvent {
    BusEvent::Device(DeviceStateChanged {
        device_id: DeviceId::new(id),
        key: key.to_string(),
        value: DeviceStateValue::Bool(true),
        changed_at: t(0),
    })
}

fn tick() -> BusEvent {
    BusEvent::Time(TimeTick {
        boundary: TimeBoundary::Hour,
        at: t(0),
        local_hour: 7,
    })
}

fn trigger(event: &str) -> EventTrigger {
    let role = role("r", &format!("  activations:\n    - event: {event}\n"));
    match &role.activations()[0] {
        Activation::Event(e) => e.trigger.clone(),
        other => panic!("not an event: {other:?}"),
    }
}

fn step(key: &str, role: &str, ordinal: u32, deps: &[&str], state: StepState) -> PlanStep {
    PlanStep::from_parts(PlanStepParts {
        task_id: "task-1".to_string(),
        key: key.to_string(),
        ordinal,
        role: role.to_string(),
        instructions: format!("do {key}"),
        inputs: Vec::new(),
        depends_on: deps.iter().map(|d| d.to_string()).collect(),
        acceptance: None,
        state,
        attempts: u32::from(state != StepState::Pending),
        summary: (state == StepState::Completed).then(|| format!("{key} found things")),
        failure: (state == StepState::Failed).then(|| "it broke".to_string()),
        started_at: None,
        finished_at: None,
        agent: None,
    })
    .unwrap()
}

fn running_task() -> LongTask {
    let started = LongTask::start(
        ProfileScope::Owner("m-1".to_string()),
        None,
        "plan the trip",
        None,
        LongTaskBudget::new(12, None).unwrap(),
        t(0),
    )
    .unwrap();
    let mut task = LongTask::from_parts(LongTaskParts {
        id: "task-1".to_string(),
        owner_profile_id: started.owner_profile_id().map(str::to_string),
        scope: started.scope().clone(),
        goal: started.goal().to_string(),
        origin_session_id: None,
        state: started.state(),
        max_steps: 12,
        deadline: None,
        kind: Default::default(),
        answer: Default::default(),
        created_at: t(0),
        updated_at: t(0),
        finished_at: None,
    })
    .unwrap();
    task.transition_to(LongTaskState::Running, t(1)).unwrap();
    task
}

fn completed(summary: &str) -> StepOutcome {
    StepOutcome::new(OutcomeKind::Completed, summary, Vec::new(), None)
}

// ── Matching ────────────────────────────────────────────────────────────────

#[test]
fn presence_matches_the_transition_and_the_member_it_names() {
    let any = trigger("{ on: presence.arrive, brief: b }");
    let liz = trigger("{ on: presence.arrive, member: liz, brief: b }");
    let arrive = presence("jerry", PresenceTransition::Arrived, t(0));
    let leave = presence("jerry", PresenceTransition::Departed, t(0));
    let matched = match_event(&any, &arrive, None).expect("an arrival matches presence.arrive");
    assert_eq!(matched.member.as_deref(), Some("jerry"));
    assert_eq!(
        match_event(&any, &leave, None),
        None,
        "a departure is not an arrival"
    );
    assert_eq!(match_event(&liz, &arrive, None), None, "jerry is not liz");
    assert!(match_event(&trigger("{ on: presence.leave, brief: b }"), &leave, None).is_some());
}

#[test]
fn camera_sensor_and_device_events_match_their_own_kind_and_source() {
    let person = trigger("{ on: camera.person, camera: porch, brief: b }");
    assert!(match_event(&person, &camera("porch", "person"), None).is_some());
    assert!(match_event(&person, &camera("porch", "motion"), None).is_none());
    assert!(match_event(&person, &camera("garage", "person"), None).is_none());
    assert!(match_event(
        &trigger("{ on: camera.motion, brief: b }"),
        &camera("x", "MOTION"),
        None
    )
    .is_some());

    let hot = trigger("{ on: sensor.temperature, above: 30, brief: b }");
    let reading_of = |v| match_event(&hot, &reading("kitchen", "Temperature", v), None);
    assert_eq!(reading_of(31.0).and_then(|m| m.past_threshold), Some(true));
    assert_eq!(reading_of(29.0).and_then(|m| m.past_threshold), Some(false));
    assert!(match_event(&hot, &reading("kitchen", "humidity", 99.0), None).is_none());

    let power = trigger("{ on: device.power, device: lamp, brief: b }");
    assert!(match_event(&power, &device("lamp", "power"), None).is_some());
    assert!(match_event(&power, &device("lamp", "brightness"), None).is_none());
    assert!(match_event(&power, &device("fan", "power"), None).is_none());
}

#[test]
fn nothing_on_the_bus_fires_a_time_of_day_trigger() {
    let daily = trigger("{ on: time.daily, at: \"07:00\", brief: b }");
    for event in [
        tick(),
        camera("porch", "person"),
        presence("m", PresenceTransition::Arrived, t(0)),
    ] {
        assert_eq!(match_event(&daily, &event, None), None);
    }
    // Vacuity control: the clock does fire it.
    let agents = [agent(
        role(
            "wake",
            "  activations:\n    - event: { on: time.daily, at: \"07:00\", brief: b }\n",
        ),
        None,
    )];
    let mut book = ActivationBook::default();
    assert!(firings_for_clock(&agents, "Africa/Nairobi", &mut book, t(-3_600)).is_empty());
    assert_eq!(
        firings_for_clock(&agents, "Africa/Nairobi", &mut book, t(1)).len(),
        1
    );
}

// ── The book ────────────────────────────────────────────────────────────────

fn key(agent: &str, index: usize) -> ActivationKey {
    ActivationKey::of(agent, index, &Activation::Delegated)
}

fn threshold_match(source: &str, past: bool) -> EventMatch {
    EventMatch {
        member: None,
        source: Some(source.to_string()),
        past_threshold: Some(past),
        detail: String::new(),
    }
}

#[test]
fn a_threshold_fires_on_the_crossing_and_never_from_the_first_reading() {
    let mut book = ActivationBook::default();
    let k = key("a", 0);
    let minute = Cooldown::parse("1m").unwrap();
    assert_eq!(
        book.admit_event(&k, &threshold_match("s", true), minute, t(0)),
        Admission::Baseline
    );
    assert_eq!(
        book.admit_event(&k, &threshold_match("s", true), minute, t(100)),
        Admission::NoEdge
    );
    assert_eq!(
        book.admit_event(&k, &threshold_match("s", false), minute, t(200)),
        Admission::NoEdge
    );
    assert_eq!(
        book.admit_event(&k, &threshold_match("s", true), minute, t(300)),
        Admission::Fire
    );
    // Each source has its own edge.
    assert_eq!(
        book.admit_event(&k, &threshold_match("t", true), minute, t(301)),
        Admission::Baseline
    );
}

#[test]
fn a_cooldown_holds_back_a_second_run_of_the_same_activation_only() {
    let mut book = ActivationBook::default();
    let edge = EventMatch {
        member: None,
        source: None,
        past_threshold: None,
        detail: String::new(),
    };
    let half_hour = Cooldown::default();
    assert_eq!(
        book.admit_event(&key("a", 0), &edge, half_hour, t(0)),
        Admission::Fire
    );
    assert_eq!(
        book.admit_event(&key("a", 0), &edge, half_hour, t(1_799)),
        Admission::CoolingDown
    );
    assert_eq!(
        book.admit_event(&key("a", 1), &edge, half_hour, t(1_799)),
        Admission::Fire
    );
    assert_eq!(
        book.admit_event(&key("a", 0), &edge, half_hour, t(1_800)),
        Admission::Fire
    );
}

#[test]
fn a_clock_never_fires_a_time_already_past_and_collapses_missed_ones() {
    let mut book = ActivationBook::default();
    let k = key("brief", 0);
    let hourly = CronSchedule::parse("0 0 * * * *").unwrap();
    let minute = Cooldown::parse("1m").unwrap();
    // t(0) is on the hour; the first sight only sets the next one.
    assert_eq!(
        book.admit_clock(&k, &hourly, minute, "UTC", t(0)),
        Admission::Baseline
    );
    assert_eq!(
        book.admit_clock(&k, &hourly, minute, "UTC", t(1_800)),
        Admission::NotDue
    );
    assert_eq!(
        book.admit_clock(&k, &hourly, minute, "UTC", t(3_600)),
        Admission::Fire
    );
    // Asleep for three hours: one run, then the next hour.
    assert_eq!(
        book.admit_clock(&k, &hourly, minute, "UTC", t(4 * 3_600 + 30)),
        Admission::Fire
    );
    assert_eq!(
        book.admit_clock(&k, &hourly, minute, "UTC", t(4 * 3_600 + 60)),
        Admission::NotDue
    );
    // A cooldown longer than the cron holds runs back.
    let mut book = ActivationBook::default();
    let every_minute = CronSchedule::parse("0 * * * * *").unwrap();
    book.admit_clock(&k, &every_minute, Cooldown::default(), "UTC", t(0));
    assert_eq!(
        book.admit_clock(&k, &every_minute, Cooldown::default(), "UTC", t(60)),
        Admission::Fire
    );
    assert_eq!(
        book.admit_clock(&k, &every_minute, Cooldown::default(), "UTC", t(120)),
        Admission::CoolingDown
    );
}

#[test]
fn an_edited_activation_starts_afresh_and_the_book_forgets_the_old_one() {
    let before = role(
        "a",
        "  activations:\n    - event: { on: camera.person, brief: b }\n",
    );
    let after = role(
        "a",
        "  activations:\n    - event: { on: camera.person, brief: changed }\n",
    );
    let old = ActivationKey::of("a", 0, &before.activations()[0]);
    let new = ActivationKey::of("a", 0, &after.activations()[0]);
    assert_ne!(old, new);
    let mut book = ActivationBook::default();
    book.note_task(&old, "t-1");
    book.retain(&live_keys(&[agent(after, None)]));
    assert_eq!(book.task_for(&old), None);
    // Vacuity control: a live key's task is kept.
    book.note_task(&new, "t-2");
    book.retain(&live_keys(&[agent(
        role(
            "a",
            "  activations:\n    - event: { on: camera.person, brief: changed }\n",
        ),
        None,
    )]));
    assert_eq!(book.task_for(&new), Some("t-2"));
}

#[test]
fn events_fire_the_activations_that_listen_for_them() {
    let agents = [
        agent(role("greeter", "  activations:\n    - delegated\n    - event: { on: presence.arrive, brief: Say hello. }\n"), Some("m-1")),
        agent(role("guard", "  activations:\n    - event: { on: camera.person, brief: Look. }\n"), None),
    ];
    let mut book = ActivationBook::default();
    let fired = firings_for_event(
        &agents,
        &presence("m-2", PresenceTransition::Arrived, t(0)),
        "UTC",
        &mut book,
        t(0),
    );
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].agent, "greeter");
    assert_eq!(
        fired[0].key.index, 1,
        "the index is the activation's place in the list"
    );
    assert_eq!(fired[0].brief, "Say hello.");
    assert!(
        matches!(&fired[0].reason, FiringReason::Event(m) if m.member.as_deref() == Some("m-2"))
    );
    // Within its cooldown the same event fires nothing more.
    assert!(firings_for_event(
        &agents,
        &presence("m-2", PresenceTransition::Arrived, t(5)),
        "UTC",
        &mut book,
        t(5)
    )
    .is_empty());
}

#[test]
fn schedules_fire_on_the_households_clock() {
    let agents = [agent(
        role(
            "brief",
            "  activations:\n    - schedule: { cron: \"0 0 7 * * *\", brief: Morning. }\n",
        ),
        None,
    )];
    let mut book = ActivationBook::default();
    // t(0) is 07:00 in Nairobi; seen first a minute before, it fires on the minute.
    assert!(firings_for_clock(&agents, "Africa/Nairobi", &mut book, t(-60)).is_empty());
    let fired = firings_for_clock(&agents, "Africa/Nairobi", &mut book, t(0));
    assert_eq!(fired.len(), 1);
    assert_eq!(
        fired[0].reason,
        FiringReason::Schedule {
            cron: "0 0 7 * * *".to_string()
        }
    );
    // Vacuity control: on UTC's clock 07:00 is three hours away.
    let mut utc = ActivationBook::default();
    firings_for_clock(&agents, "UTC", &mut utc, t(-60));
    assert!(firings_for_clock(&agents, "UTC", &mut utc, t(0)).is_empty());
}

// ── Whose run ───────────────────────────────────────────────────────────────

#[test]
fn a_run_is_the_named_members_else_the_recipe_owners_else_a_household_of_one() {
    assert_eq!(
        activation_owner(Some("liz"), Some("jerry")).as_deref(),
        Some("liz")
    );
    assert_eq!(
        activation_owner(None, Some("jerry")).as_deref(),
        Some("jerry")
    );
    assert_eq!(activation_owner(None, Some("  ")), None);
    assert_eq!(
        activation_scope(Some("jerry"), 3),
        Some(ProfileScope::Owner("jerry".into()))
    );
    assert_eq!(activation_scope(None, 1), Some(ProfileScope::Household));
    assert_eq!(activation_scope(None, 0), Some(ProfileScope::Household));
    assert_eq!(
        activation_scope(None, 2),
        None,
        "nobody's run would read everybody's data"
    );
}

#[test]
fn the_reason_line_names_the_member_and_never_their_id_when_a_name_is_known() {
    let m = EventMatch {
        member: Some("m-1".into()),
        source: None,
        past_threshold: None,
        detail: "arrived home".into(),
    };
    assert_eq!(
        firing_reason_line(&FiringReason::Event(m.clone()), Some("Liz")),
        "Liz arrived home"
    );
    assert_eq!(
        firing_reason_line(&FiringReason::Event(m), None),
        "a household member arrived home"
    );
    assert_eq!(
        activation_instructions(" Say hello. ", "Liz arrived home"),
        "Say hello.\n\nWhy this run started: Liz arrived home."
    );
}

#[test]
fn an_activation_queues_a_one_step_task_the_runner_does_not_plan() {
    let greeter = role(
        "greeter",
        "  activations:\n    - event: { on: presence.arrive, brief: b }\n",
    );
    let (mut task, plan) = activation_task(
        &greeter,
        "Say hello.\n\nWhy this run started: Liz arrived home.",
        ProfileScope::Owner("m-1".into()),
        None,
        t(0),
    )
    .unwrap();
    assert!(is_activation_task(&task));
    assert!(task.id().starts_with(ACTIVATION_TASK_ID_PREFIX));
    assert_eq!(
        task.state(),
        LongTaskState::Paused,
        "stored Paused so the runner skips it"
    );
    assert_eq!(task.owner_profile_id(), Some("m-1"));
    assert_eq!(task.goal(), "greeter: Say hello.");
    assert_eq!(
        task.budget().max_steps() as usize,
        1 + MAX_HAND_OFFS_PER_TASK
    );
    assert!(task.budget().deadline().is_some_and(|d| d > t(0)));
    assert_eq!(plan.steps().len(), 1);
    let only = &plan.steps()[0];
    assert_eq!(only.role(), "greeter");
    assert_eq!(only.key().as_str(), "greeter");
    assert!(only.instructions().contains("Liz arrived home"));

    task.transition_to(LongTaskState::Running, t(1)).unwrap();
    assert!(
        matches!(next_run(&task, plan.steps(), 0, t(2)), NextRun::Do(s) if s.role() == "greeter"),
        "a Running task with its step goes straight to the agent"
    );
    // Vacuity control: left Planning, the same task would be planned over.
    let (planning, plan) =
        activation_task(&greeter, "b", ProfileScope::Household, None, t(0)).unwrap();
    let mut planning = planning;
    planning
        .transition_to(LongTaskState::Planning, t(1))
        .unwrap();
    assert_eq!(
        next_run(&planning, plan.steps(), 0, t(2)),
        NextRun::Plan(PlanReason::Fresh)
    );
}

#[test]
fn a_planners_step_needs_a_role_that_takes_steps_and_an_activations_does_not() {
    let scheduled = role(
        "brief",
        "  activations:\n    - schedule: { cron: \"0 0 7 * * *\", brief: b }\n",
    );
    let ordinary = role("helper", "");
    let planned = step("read", "brief", 1, &[], StepState::Pending);
    assert!(step_role_allowed(&running_task(), &planned, &scheduled).is_err());
    let planned_helper = step("read", "helper", 1, &[], StepState::Pending);
    assert!(step_role_allowed(&running_task(), &planned_helper, &ordinary).is_ok());
    let (task, plan) =
        activation_task(&scheduled, "b", ProfileScope::Household, None, t(0)).unwrap();
    assert!(step_role_allowed(&task, &plan.steps()[0], &scheduled).is_ok());
    let handed = step("hand-to-brief", "brief", 2, &[], StepState::Pending);
    assert!(step_role_allowed(&running_task(), &handed, &scheduled).is_ok());
    assert_eq!(
        plannable_roles(&[scheduled, ordinary])
            .iter()
            .map(AgentRole::name)
            .collect::<Vec<_>>(),
        vec!["helper"]
    );
}

#[test]
fn an_activation_speaks_through_its_hook_and_a_plain_task_as_before() {
    let quiet = role(
        "quiet",
        "  activations:\n    - event: { on: camera.person, brief: b }\n",
    );
    let loud = role("loud", "  activations:\n    - event: { on: camera.person, brief: b }\n  hooks:\n    after: [notify_owner]\n");
    let roles = [quiet.clone(), loud.clone()];
    for (r, gave_up) in [(&quiet, false), (&loud, true)] {
        let (task, plan) = activation_task(r, "b", ProfileScope::Household, None, t(0)).unwrap();
        assert!(
            !runner_notifies_at_end(&task, plan.steps(), &roles, true),
            "a finish is the hook's to tell"
        );
        assert_eq!(
            runner_notifies_at_end(&task, plan.steps(), &roles, false),
            gave_up
        );
    }
    assert!(runner_notifies_at_end(&running_task(), &[], &roles, true));
}

#[test]
fn a_brief_offers_only_the_agents_that_accept_work_from_this_one() {
    let source = role("researcher", "  hooks:\n    after: [start_task]\n");
    let planner = role(
        "trip-planner",
        "  activations:\n    - agent: [researcher]\n",
    );
    let other = role("budgeter", "  activations:\n    - agent: [someone-else]\n");
    let offer = hand_back_offer(&source, &[source.clone(), planner, other]);
    assert!(offer.contains("trip-planner"));
    assert!(!offer.contains("budgeter"));
    assert!(offer.contains("`hand_to`"));
    assert!(offer.contains("`start_task`"));
    assert_eq!(hand_back_offer(&role("alone", ""), &[]), "");
}

#[test]
fn before_hooks_render_in_a_fixed_format_and_withhold_from_a_guest() {
    let context = BeforeHookContext {
        date: Some("Wednesday 28 October 2026".into()),
        schedule: vec!["07:30 Morning brief".into()],
        weather: Some("Light rain, 18 C".into()),
        presence: vec![
            PresenceLine {
                name: "Liz".into(),
                home: true,
                since: "18:02".into(),
            },
            PresenceLine {
                name: "Jerry".into(),
                home: false,
                since: "08:10".into(),
            },
        ],
    };
    let hooks = [BeforeHook::Today, BeforeHook::Weather, BeforeHook::Presence];
    assert_eq!(
        render_before_hooks(&hooks, &context, &ProfileScope::Owner("m".into())),
        "\nGathered before you started:\n\
         - Today: Wednesday 28 October 2026. Scheduled: 07:30 Morning brief.\n\
         - Weather: Light rain, 18 C.\n\
         - Who is home, as last seen: home: Liz (since 18:02); away: Jerry (since 08:10).\n"
    );
    let guest = render_before_hooks(&hooks, &context, &ProfileScope::Guest);
    assert!(
        !guest.contains("Liz") && !guest.contains("Morning brief"),
        "{guest}"
    );
    assert!(
        guest.contains("Light rain"),
        "weather is nobody's personal data"
    );
    assert_eq!(
        render_before_hooks(&[], &context, &ProfileScope::Household),
        ""
    );
    let empty = render_before_hooks(
        &hooks,
        &BeforeHookContext::default(),
        &ProfileScope::Household,
    );
    assert!(empty.contains("could not be read") && empty.contains("Nothing is scheduled"));
}

#[test]
fn dates_and_times_are_the_households_own() {
    // t(0) is 04:00 UTC, 07:00 in Nairobi.
    assert_eq!(
        local_date_label(t(0), "Africa/Nairobi"),
        "Wednesday 28 October 2026"
    );
    assert_eq!(local_clock_label(t(0), t(0), "Africa/Nairobi"), "07:00");
    // 22:00 UTC the day before is 01:00 Wednesday in Nairobi: the same day there, not in UTC.
    let late = t(-6 * 3_600);
    assert!(same_local_day(late, t(0), "Africa/Nairobi"));
    assert!(!same_local_day(late, t(0), "UTC"));
    assert_eq!(local_clock_label(late, t(0), "UTC"), "22:00 on Tuesday");
}

#[test]
fn the_presence_board_keeps_each_members_latest_word() {
    let mut board = PresenceBoard::default();
    if let BusEvent::Presence(p) = presence("liz", PresenceTransition::Arrived, t(100)) {
        board.record(&p);
    }
    if let BusEvent::Presence(p) = presence("liz", PresenceTransition::Departed, t(50)) {
        board.record(&p);
    }
    if let BusEvent::Presence(p) = presence("jerry", PresenceTransition::Departed, t(200)) {
        board.record(&p);
    }
    assert_eq!(
        board.snapshot(),
        vec![
            ("jerry".to_string(), false, t(200)),
            ("liz".to_string(), true, t(100))
        ]
    );
}

// ── After a step ────────────────────────────────────────────────────────────

#[test]
fn the_hand_back_can_ask_to_hand_on_and_to_start_a_task() {
    let value = serde_json::json!({
        "outcome": "completed",
        "summary": "found three routes",
        "hand_to": "trip-planner",
        "brief": "Pick one and book nothing.",
        "start_task": "Compare the three routes in detail"
    });
    let outcome = StepOutcome::from_json(&value).unwrap();
    assert_eq!(
        outcome.asks(),
        &HandBackAsks {
            hand_to: Some(HandToAsk {
                agent: "trip-planner".into(),
                brief: Some("Pick one and book nothing.".into())
            }),
            start_task: Some("Compare the three routes in detail".into()),
        }
    );
    let nested = StepOutcome::from_json(&serde_json::json!({
        "outcome": "completed", "handoff": {"agent": "x", "brief": "y"}
    }))
    .unwrap();
    assert_eq!(
        nested.asks().hand_to.as_ref().map(|h| h.agent.as_str()),
        Some("x")
    );
    let long = StepOutcome::from_json(&serde_json::json!({
        "outcome": "completed", "hand_to": "x", "brief": "z".repeat(MAX_HAND_OFF_BRIEF_CHARS * 2)
    }))
    .unwrap();
    assert!(
        long.asks()
            .hand_to
            .as_ref()
            .unwrap()
            .brief
            .as_ref()
            .unwrap()
            .chars()
            .count()
            <= MAX_HAND_OFF_BRIEF_CHARS
    );
    // Vacuity control: a plain hand-back asks nothing, and serialises as it always did.
    let plain =
        StepOutcome::from_json(&serde_json::json!({"outcome": "completed", "summary": "s"}))
            .unwrap();
    assert!(plain.asks().is_empty());
    assert!(serde_json::to_value(&plain).unwrap().get("asks").is_none());
}

#[test]
fn after_hooks_act_only_on_a_completed_and_checked_step() {
    let r = role(
        "r",
        "  hooks:\n    after: [notify_owner, save_artifact, start_task]\n",
    );
    let task = running_task();
    let done = step("r", "r", 1, &[], StepState::Completed);
    let outcome = completed("all good").with_asks(HandBackAsks {
        hand_to: None,
        start_task: Some("dig deeper".into()),
    });
    assert_eq!(
        after_hook_actions(&task, &r, &done, &outcome),
        vec![
            AfterAction::NotifyOwner("all good".into()),
            AfterAction::SaveArtifact,
            AfterAction::StartTask("dig deeper".into())
        ]
    );
    let failed_check = step("r", "r", 1, &[], StepState::Failed);
    assert!(after_hook_actions(&task, &r, &failed_check, &outcome).is_empty());
    let failed = StepOutcome::new(OutcomeKind::Failed, "no", Vec::new(), Some("no"));
    assert!(after_hook_actions(&task, &r, &done, &failed).is_empty());
    // Without the hook, a hand-back's start_task is ignored.
    let plain = role("r", "");
    assert!(after_hook_actions(&task, &plain, &done, &outcome).is_empty());
}

#[test]
fn a_follow_up_task_cannot_start_another_and_an_ownerless_one_notifies_nobody() {
    let r = role("r", "  hooks:\n    after: [notify_owner, start_task]\n");
    let done = step("r", "r", 1, &[], StepState::Completed);
    let asks_more = completed("ok").with_asks(HandBackAsks {
        hand_to: None,
        start_task: Some("more".into()),
    });
    let follow_up = follow_up_task(&running_task(), "the follow-up", t(0)).unwrap();
    assert!(is_follow_up_task(&follow_up));
    assert_eq!(follow_up.owner_profile_id(), Some("m-1"));
    assert_eq!(follow_up.state(), LongTaskState::Planning);
    assert_eq!(
        after_hook_actions(&follow_up, &r, &done, &asks_more),
        vec![AfterAction::NotifyOwner("ok".into())]
    );
    let (ownerless, _) = activation_task(&r, "b", ProfileScope::Household, None, t(0)).unwrap();
    assert_eq!(
        after_hook_actions(&ownerless, &r, &done, &asks_more),
        vec![AfterAction::StartTask("more".into())]
    );
}

#[test]
fn an_asked_hand_off_wins_over_the_hooked_one_and_there_is_only_one() {
    let r = role("r", "  hooks:\n    after:\n      - hand_to: planner\n");
    let done = step("r", "r", 1, &[], StepState::Completed);
    let task = running_task();
    assert_eq!(
        after_hook_actions(&task, &r, &done, &completed("found it")),
        vec![AfterAction::HandTo {
            agent: "planner".into(),
            brief: "found it".into()
        }]
    );
    let asked = completed("found it").with_asks(HandBackAsks {
        hand_to: Some(HandToAsk {
            agent: "budgeter".into(),
            brief: Some("cost it".into()),
        }),
        start_task: None,
    });
    assert_eq!(
        after_hook_actions(&task, &r, &done, &asked),
        vec![AfterAction::HandTo {
            agent: "budgeter".into(),
            brief: "cost it".into()
        }]
    );
}

#[test]
fn the_saved_result_is_titled_so_task_status_finds_it() {
    let task = running_task();
    let done = step("r", "r", 1, &[], StepState::Completed);
    let artifact = hook_result_artifact(
        &task,
        &done,
        &completed("Three routes; the coast one is best."),
        t(0),
    )
    .unwrap();
    assert_eq!(artifact.title(), RESULT_TITLE);
    assert_eq!(artifact.step_key(), Some(done.key()));
    assert!(hook_result_artifact(&task, &done, &completed(""), t(0)).is_err());
}

// ── Hand-offs ───────────────────────────────────────────────────────────────

fn roster() -> Vec<AgentRole> {
    vec![
        role(
            "researcher",
            "  activations:\n    - task_step\n    - agent: [planner]\n",
        ),
        role(
            "planner",
            "  activations:\n    - task_step\n    - agent: [researcher]\n",
        ),
        role(
            "budgeter",
            "  activations:\n    - agent: [planner, researcher]\n",
        ),
        role("loner", ""),
    ]
}

fn summary(id: &str, step_key: Option<&str>) -> ArtifactSummary {
    ArtifactSummary {
        id: id.to_string(),
        task_id: "task-1".to_string(),
        step_key: step_key.map(|k| StepKey::parse(k).unwrap()),
        kind: ArtifactKind::Findings,
        title: "t".to_string(),
        summary: "s".to_string(),
        tokens: 10,
        created_at: t(0),
    }
}

#[test]
fn a_hand_off_appends_one_step_after_its_source_with_its_artifacts() {
    let task = running_task();
    let steps = vec![
        step("look", "researcher", 1, &[], StepState::Completed),
        step("later", "loner", 2, &["look"], StepState::Pending),
    ];
    let artifacts = [
        summary("a-1", Some("look")),
        summary("a-2", None),
        summary("a-3", Some("later")),
    ];
    let plan = plan_hand_off(
        &task,
        &steps,
        &steps[0],
        "planner",
        "Plan it.",
        &roster(),
        &artifacts,
    )
    .unwrap();
    let added = plan.steps().last().unwrap();
    assert_eq!(added.key().as_str(), "hand-to-planner");
    assert_eq!(added.role(), "planner");
    assert_eq!(added.depends_on(), &[StepKey::parse("look").unwrap()]);
    assert_eq!(added.inputs(), &["a-1".to_string()]);
    assert!(added.instructions().starts_with("Plan it."));
    assert!(added.instructions().contains("look found things"));
    assert_eq!(added.ordinal(), 3);
    assert_eq!(plan.kept_keys(), &[StepKey::parse("look").unwrap()]);
    // The open step is carried over as it is.
    assert_eq!(plan.steps()[0], steps[1]);
    assert!(plan
        .check_current(steps.iter().map(|s| (s.key().as_str(), s.state())))
        .is_ok());
}

#[test]
fn a_hand_off_keeps_failed_and_skipped_steps_as_they_are() {
    let task = running_task();
    let steps = vec![
        step("look", "researcher", 1, &[], StepState::Completed),
        step("broke", "loner", 2, &[], StepState::Failed),
        step("dropped", "loner", 3, &[], StepState::Skipped),
    ];
    let plan = plan_hand_off(
        &task,
        &steps,
        &steps[0],
        "planner",
        "Plan it.",
        &roster(),
        &[],
    )
    .unwrap();
    let carried: Vec<(String, StepState, u32)> = plan
        .steps()
        .iter()
        .take(2)
        .map(|s| (s.key().to_string(), s.state(), s.attempts()))
        .collect();
    assert_eq!(
        carried,
        vec![
            ("broke".to_string(), StepState::Failed, 1),
            ("dropped".to_string(), StepState::Skipped, 1)
        ]
    );
    // Vacuity control: a replan of the same steps would have reset them to Pending.
    let drafts = vec![StepDraft {
        key: "broke".into(),
        role: "loner".into(),
        instructions: "again".into(),
        ..StepDraft::default()
    }];
    assert_eq!(
        validate_plan(&task, drafts, &steps).unwrap().steps()[0].state(),
        StepState::Pending
    );
}

#[test]
fn only_an_agent_that_lists_the_source_takes_its_work() {
    let task = running_task();
    let steps = vec![step("look", "researcher", 1, &[], StepState::Completed)];
    let refused = plan_hand_off(&task, &steps, &steps[0], "loner", "go", &roster(), &[]);
    assert_eq!(
        refused.unwrap_err(),
        HandOffRefused::NotAccepted {
            from: "researcher".into(),
            to: "loner".into()
        }
    );
    assert_eq!(
        plan_hand_off(&task, &steps, &steps[0], "nobody", "go", &roster(), &[]).unwrap_err(),
        HandOffRefused::NoSuchAgent("nobody".into())
    );
    assert_eq!(
        plan_hand_off(&task, &steps, &steps[0], "budgeter", "  ", &roster(), &[]).unwrap_err(),
        HandOffRefused::EmptyBrief
    );
    assert!(plan_hand_off(&task, &steps, &steps[0], "budgeter", "go", &roster(), &[]).is_ok());
}

#[test]
fn a_chain_that_comes_back_round_is_refused() {
    let task = running_task();
    let steps = vec![
        step("look", "researcher", 1, &[], StepState::Completed),
        step(
            "hand-to-planner",
            "planner",
            2,
            &["look"],
            StepState::Completed,
        ),
    ];
    let refused = plan_hand_off(
        &task,
        &steps,
        &steps[1],
        "researcher",
        "again",
        &roster(),
        &[],
    );
    assert_eq!(
        refused.unwrap_err(),
        HandOffRefused::Cycle {
            chain: vec!["researcher".into(), "planner".into(), "researcher".into()]
        }
    );
    assert!(matches!(
        plan_hand_off(&task, &steps, &steps[0], "researcher", "me", &roster(), &[]),
        Err(HandOffRefused::Cycle { .. })
    ));
    // Vacuity control: onward to someone new is fine.
    assert!(plan_hand_off(
        &task,
        &steps,
        &steps[1],
        "budgeter",
        "cost it",
        &roster(),
        &[]
    )
    .is_ok());
}

#[test]
fn a_task_is_handed_on_at_most_four_times() {
    let task = running_task();
    let ping = role("ping", "  activations:\n    - agent: [pong]\n");
    let pong = role("pong", "  activations:\n    - agent: [ping]\n");
    let fresh = role("fresh", "  activations:\n    - agent: [ping, pong]\n");
    let roles = vec![ping, pong, fresh];
    // Four hand-offs already taken, by separate chains so none is a cycle.
    let mut steps = vec![step("start", "ping", 1, &[], StepState::Completed)];
    for i in 0..MAX_HAND_OFFS_PER_TASK {
        steps.push(step(
            &format!("hand-to-x-{i}"),
            "pong",
            2 + i as u32,
            &[],
            StepState::Completed,
        ));
    }
    assert_eq!(
        plan_hand_off(&task, &steps, &steps[0], "fresh", "go", &roles, &[]).unwrap_err(),
        HandOffRefused::CapReached {
            cap: MAX_HAND_OFFS_PER_TASK
        }
    );
    steps.pop();
    assert!(plan_hand_off(&task, &steps, &steps[0], "fresh", "go", &roles, &[]).is_ok());
}

#[test]
fn a_second_hand_off_to_the_same_agent_gets_its_own_key() {
    let task = running_task();
    let steps = vec![
        step("look", "researcher", 1, &[], StepState::Completed),
        step("hand-to-budgeter", "budgeter", 2, &[], StepState::Completed),
        step("again", "researcher", 3, &[], StepState::Completed),
    ];
    let plan = plan_hand_off(&task, &steps, &steps[2], "budgeter", "go", &roster(), &[]).unwrap();
    assert_eq!(
        plan.steps().last().unwrap().key().as_str(),
        "hand-to-budgeter-2"
    );
}

#[test]
fn the_activation_triggers_in_the_roles_are_subjects_the_matcher_reads() {
    // Pins the parser's spelling of `any` to the matcher's.
    let any = trigger("{ on: camera.person, camera: ANY, brief: b }");
    assert_eq!(
        any,
        EventTrigger::CameraPerson {
            camera: Subject::Any
        }
    );
}

#[tokio::test]
async fn an_activation_and_its_hand_off_go_through_the_ledger_as_planned() {
    use crate::shared::mocks::mock_long_task::MockLongTaskRepository;
    use crate::shared::ports::long_task::LongTaskRepository;

    let repo = MockLongTaskRepository::new();
    let greeter = role(
        "greeter",
        "  activations:\n    - event: { on: presence.arrive, brief: b }\n",
    );
    let planner = role("planner", "  activations:\n    - agent: [greeter]\n");
    let (task, plan) = activation_task(
        &greeter,
        "Say hello.",
        ProfileScope::Owner("m-1".into()),
        None,
        t(0),
    )
    .unwrap();
    repo.create_task(&task).await.unwrap();
    repo.replace_open_steps(&plan).await.unwrap();
    let running = repo
        .set_task_state(task.id(), LongTaskState::Running, t(1))
        .await
        .unwrap()
        .expect("the task is stored");

    let mut only = repo.list_steps(task.id()).await.unwrap().remove(0);
    assert!(matches!(
        next_run(&running, std::slice::from_ref(&only), 0, t(2)),
        NextRun::Do(_)
    ));
    only.start(t(2)).unwrap();
    repo.update_step(&only).await.unwrap();
    only.record(&completed("said hello"), t(3)).unwrap();
    repo.update_step(&only).await.unwrap();

    let steps = repo.list_steps(task.id()).await.unwrap();
    let roles = [greeter, planner];
    let handed = plan_hand_off(
        &running,
        &steps,
        &steps[0],
        "planner",
        "Plan the evening.",
        &roles,
        &[],
    )
    .unwrap();
    repo.replace_open_steps(&handed).await.unwrap();

    let steps = repo.list_steps(task.id()).await.unwrap();
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].state(), StepState::Completed);
    assert!(
        matches!(next_run(&running, &steps, 0, t(4)), NextRun::Do(s) if s.role() == "planner"),
        "the runner's next run is the agent handed the work"
    );
}

#[test]
fn an_owner_subject_matches_the_agents_owner_and_nobody_without_one() {
    let mine = trigger("{ on: presence.arrive, member: owner, brief: b }");
    let jerry = presence("jerry", PresenceTransition::Arrived, t(0));
    assert!(match_event(&mine, &jerry, Some("jerry")).is_some());
    assert!(match_event(&mine, &jerry, Some("liz")).is_none());
    assert!(match_event(&mine, &jerry, None).is_none());
}

#[test]
fn an_event_outside_its_window_is_not_seen_and_spends_no_cooldown() {
    let agents = [agent(
        role(
            "guard",
            "  activations:\n    - event: { on: camera.person, between: \"22:00-06:00\", brief: Look. }\n",
        ),
        Some("m-1"),
    )];
    let mut book = ActivationBook::default();
    // t(0) is 07:00 in Nairobi: outside the window.
    assert!(firings_for_event(
        &agents,
        &camera("gate", "person"),
        "Africa/Nairobi",
        &mut book,
        t(0)
    )
    .is_empty());
    // 23:00 there is 20:00 UTC that day; the same book still fires, so nothing was spent.
    let night = Utc.with_ymd_and_hms(2026, 10, 28, 20, 0, 0).unwrap();
    assert_eq!(
        firings_for_event(
            &agents,
            &camera("gate", "person"),
            "Africa/Nairobi",
            &mut book,
            night
        )
        .len(),
        1
    );
}

#[test]
fn a_card_says_when_its_agent_runs_in_words() {
    let r = role(
        "r",
        "  activations:\n\
         \x20   - delegated\n\
         \x20   - schedule: { cron: \"0 30 6 * * *\", brief: b }\n\
         \x20   - schedule: { cron: \"0 0 18 * * MON-FRI\", brief: b }\n\
         \x20   - event: { on: presence.arrive, member: owner, brief: b }\n\
         \x20   - event: { on: camera.person, between: \"22:00-06:00\", brief: b }\n\
         \x20   - event: { on: sensor.water_level, below: 20, brief: b }\n\
         \x20   - agent: [trip-planner]\n",
    );
    assert_eq!(
        describe_activations(r.activations()),
        "When your assistant hands it work; every day at 06:30; on weekdays at 18:00; when you \
         come home; when any camera sees a person between 22:00 and 06:00; when the water level \
         drops below 20; when trip-planner hands it work"
    );
    let odd = role(
        "o",
        "  activations:\n    - schedule: { cron: \"0 */15 * * * *\", brief: b }\n",
    );
    assert_eq!(
        describe_activations(odd.activations()),
        "On the schedule 0 */15 * * * *"
    );
}
