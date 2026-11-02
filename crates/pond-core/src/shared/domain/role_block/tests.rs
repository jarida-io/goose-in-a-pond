use super::*;
use crate::shared::domain::orchestration::RoleError;
use chrono::TimeZone;

/// The block as section 9.1 of the design draws it.
const DESIGN_BLOCK: &str = r#"
title: Trip helper
giap_role:
  tool_groups: [giap-knowledge, giap-weather, giap-task-work]
  personal_data: inherit
  max_turns: 8
  instructions: |
    Plans day trips for the household.
  context:
    examples:
      - brief: A day out near Naivasha
        answer: Three stops, with drive times.
  activations:
    - delegated
    - task_step
    - schedule: { cron: "0 0 7 * * MON-FRI", brief: "Check the weekend's weather." }
    - event: { on: presence.arrive, member: any, brief: "Welcome them home.", cooldown: 30m }
    - agent: [trip-planner]
  hooks:
    before: [today, weather]
    after: [notify_owner]
  a2a:
    expose: false
"#;

fn role(yaml: &str) -> AgentRole {
    AgentRole::from_recipe_yaml("helper", yaml)
        .unwrap_or_else(|e| panic!("did not parse: {e}"))
        .expect("has a role block")
}

fn paths(yaml: &str) -> Vec<String> {
    validate_role_recipe("helper", yaml, None)
        .errors
        .into_iter()
        .map(|e| e.path)
        .collect()
}

fn block(body: &str) -> String {
    format!("instructions: go\ngiap_role:\n  tool_groups: []\n{body}")
}

#[test]
fn the_block_in_the_design_parses_into_typed_parts() {
    let role = role(DESIGN_BLOCK);
    assert_eq!(role.examples().len(), 1);
    assert!(role.accepts_delegation());
    assert!(role.accepts_task_steps());
    assert!(role.accepts_hand_off_from("trip-planner"));
    assert!(!role.accepts_hand_off_from("someone-else"));
    assert!(role.runs_on_its_own());
    let schedule = role
        .activations()
        .iter()
        .find_map(|a| match a {
            Activation::Schedule(s) => Some(s),
            _ => None,
        })
        .expect("a schedule");
    assert_eq!(schedule.cron.as_str(), "0 0 7 * * MON-FRI");
    assert_eq!(schedule.cooldown, Cooldown::default());
    let event = role
        .activations()
        .iter()
        .find_map(|a| match a {
            Activation::Event(e) => Some(e),
            _ => None,
        })
        .expect("an event");
    assert_eq!(
        event.trigger,
        EventTrigger::PresenceArrive {
            member: Subject::Any
        }
    );
    assert_eq!(
        role.hooks().before,
        vec![BeforeHook::Today, BeforeHook::Weather]
    );
    assert_eq!(role.hooks().after, vec![AfterHook::NotifyOwner]);
    assert!(!role.a2a().expose);
    let checked = validate_role_recipe("helper", DESIGN_BLOCK, None);
    assert!(checked.ok(), "{:?}", checked.errors);
}

#[test]
fn a_block_without_activations_takes_the_two_model_decided_ones() {
    let role = role(&block(""));
    assert_eq!(role.activations(), default_activations().as_slice());
    assert!(!role.runs_on_its_own());
    // Vacuity control: a stated list replaces the default rather than adding to it.
    let only_scheduled =
        role_yaml_with("  activations:\n    - schedule: { cron: \"0 0 7 * * *\", brief: b }\n");
    assert!(!only_scheduled.accepts_delegation());
    assert!(!only_scheduled.accepts_task_steps());
}

fn role_yaml_with(body: &str) -> AgentRole {
    role(&block(body))
}

#[test]
fn examples_join_the_instructions_in_one_fixed_format() {
    let with = role(DESIGN_BLOCK);
    assert_eq!(
        with.instructions(),
        "Plans day trips for the household.\n\n\
         Examples of a brief and the answer it wants:\n\n\
         Brief: A day out near Naivasha\nAnswer: Three stops, with drive times.\n"
    );
    // Vacuity control: without examples the instructions are exactly as written.
    let without = role(&DESIGN_BLOCK.replace(
        "  context:\n    examples:\n      - brief: A day out near Naivasha\n        answer: Three stops, with drive times.\n",
        "",
    ));
    assert!(without.examples().is_empty());
    assert_eq!(
        without.instructions(),
        "Plans day trips for the household.\n"
    );
}

#[test]
fn every_problem_is_reported_at_its_own_path() {
    let yaml = block(
        "  activations:\n\
         \x20   - delegated\n\
         \x20   - task_step\n\
         \x20   - event: { on: presence.arive, brief: hi }\n\
         \x20   - schedule: { cron: \"0 7 * * *\", brief: b, cooldown: 10s }\n\
         \x20 hooks:\n\
         \x20   before: [today, horoscope]\n\
         \x20   after: [notify_owner, tweet]\n\
         \x20 a2a: { expose: maybe }\n",
    );
    let found = paths(&yaml);
    for want in [
        "giap_role.activations[2].event.on",
        "giap_role.activations[3].schedule.cron",
        "giap_role.activations[3].schedule.cooldown",
        "giap_role.hooks.before[1]",
        "giap_role.hooks.after[1]",
        "giap_role.a2a.expose",
    ] {
        assert!(
            found.iter().any(|p| p == want),
            "no error at `{want}`; got {found:?}"
        );
    }
    assert_eq!(found.len(), 6, "one error per problem, no more: {found:?}");
    let message = validate_role_recipe("helper", &yaml, None)
        .errors
        .into_iter()
        .find(|e| e.path == "giap_role.activations[2].event.on")
        .unwrap()
        .message;
    assert!(
        message.contains("presence.arrive"),
        "the typo is not answered with the spelling it meant: {message}"
    );
}

#[test]
fn the_parser_refuses_at_the_first_problem_and_names_its_field() {
    let bad = block("  hooks:\n    after: [tweet]\n");
    match AgentRole::from_recipe_yaml("helper", &bad) {
        Err(RoleError::Field { path, .. }) => assert_eq!(path, "giap_role.hooks.after[0]"),
        other => panic!("an unknown hook was not refused by field: {other:?}"),
    }
    // Vacuity control: the same block with a real hook parses.
    let good = block("  hooks:\n    after: [save_artifact]\n");
    assert_eq!(role(&good).hooks().after, vec![AfterHook::SaveArtifact]);
}

#[test]
fn an_unknown_activation_kind_is_an_error_naming_it() {
    for (item, path) in [
        ("    - delegatd\n", "giap_role.activations[0]"),
        ("    - webhook: { url: x }\n", "giap_role.activations[0]"),
        ("    - schedule\n", "giap_role.activations[0]"),
    ] {
        let yaml = block(&format!("  activations:\n{item}"));
        assert_eq!(paths(&yaml), vec![path.to_string()], "for {item:?}");
        assert!(matches!(
            AgentRole::from_recipe_yaml("helper", &yaml),
            Err(RoleError::Field { .. })
        ));
    }
}

#[test]
fn settings_written_beside_their_kind_are_caught_with_a_hint() {
    let yaml =
        block("  activations:\n    - schedule:\n      cron: \"0 0 7 * * *\"\n      brief: b\n");
    let checked = validate_role_recipe("helper", &yaml, None);
    let error = checked.errors.first().expect("refused");
    assert_eq!(error.path, "giap_role.activations[0]");
    assert!(error.message.contains("indent"), "{}", error.message);
}

#[test]
fn the_tagged_form_serde_yaml_writes_is_read_like_the_map_form() {
    let tagged =
        role_yaml_with("  activations:\n    - !schedule { cron: \"0 0 7 * * *\", brief: b }\n");
    let mapped =
        role_yaml_with("  activations:\n    - schedule: { cron: \"0 0 7 * * *\", brief: b }\n");
    assert_eq!(tagged.activations(), mapped.activations());
}

#[test]
fn cooldowns_default_to_thirty_minutes_and_refuse_less_than_one() {
    assert_eq!(DEFAULT_ACTIVATION_COOLDOWN, Duration::from_secs(1_800));
    assert_eq!(MIN_ACTIVATION_COOLDOWN, Duration::from_secs(60));
    for (raw, secs) in [
        ("30m", 1_800),
        ("1m", 60),
        ("2h", 7_200),
        ("1h30m", 5_400),
        ("90s", 90),
        ("1d", 86_400),
    ] {
        assert_eq!(
            Cooldown::parse(raw).unwrap().duration().as_secs(),
            secs,
            "{raw}"
        );
    }
    for raw in ["59s", "0m", "", "30", "m", "30x", "ten minutes"] {
        assert!(Cooldown::parse(raw).is_err(), "`{raw}` was accepted");
    }
    let yaml =
        block("  activations:\n    - event: { on: camera.person, brief: b, cooldown: 30 }\n");
    assert_eq!(
        paths(&yaml),
        vec!["giap_role.activations[0].event.cooldown".to_string()]
    );
}

#[test]
fn a_schedule_takes_a_six_field_cron_the_scheduler_can_read() {
    assert!(CronSchedule::parse("0 0 7 * * MON-FRI").is_ok());
    assert!(CronSchedule::parse("  0  30 18 * * *  ").is_ok());
    for bad in [
        "0 7 * * *",
        "0 0 7 * * * 2027",
        "@daily",
        "0 0 25 * * *",
        "a b c d e f",
        "0 0 0 30 2 *",
    ] {
        assert!(CronSchedule::parse(bad).is_err(), "`{bad}` was accepted");
    }
    assert_eq!(
        CronSchedule::parse("  0  30 18 * * *  ").unwrap().as_str(),
        "0 30 18 * * *"
    );
}

#[test]
fn a_schedule_comes_round_on_the_households_wall_clock() {
    let cron = CronSchedule::parse("0 0 7 * * *").unwrap();
    // 03:00 UTC is 06:00 in Nairobi, so 07:00 there is 04:00 UTC the same day.
    let at = Utc.with_ymd_and_hms(2026, 10, 28, 3, 0, 0).unwrap();
    assert_eq!(
        cron.next_after(at, "Africa/Nairobi"),
        Some(Utc.with_ymd_and_hms(2026, 10, 28, 4, 0, 0).unwrap())
    );
    // Vacuity control: an unknown zone reads as UTC, as the scheduler reads it.
    assert_eq!(
        cron.next_after(at, "Mars/Olympus"),
        Some(Utc.with_ymd_and_hms(2026, 10, 28, 7, 0, 0).unwrap())
    );
}

#[test]
fn each_event_family_parses_into_its_trigger() {
    let cases = [
        (
            "{ on: presence.leave, member: m-1, brief: b }",
            EventTrigger::PresenceLeave {
                member: Subject::Named("m-1".into()),
            },
        ),
        (
            "{ on: camera.motion, camera: porch, brief: b }",
            EventTrigger::CameraMotion {
                camera: Subject::Named("porch".into()),
            },
        ),
        (
            "{ on: camera.person, brief: b }",
            EventTrigger::CameraPerson {
                camera: Subject::Any,
            },
        ),
        (
            "{ on: sensor.temperature, above: 30, brief: b }",
            EventTrigger::Sensor {
                sensor_type: "temperature".into(),
                device: Subject::Any,
                threshold: Threshold::Above(30.0),
            },
        ),
        (
            "{ on: sensor.co2, device: hall, below: 400.5, brief: b }",
            EventTrigger::Sensor {
                sensor_type: "co2".into(),
                device: Subject::Named("hall".into()),
                threshold: Threshold::Below(400.5),
            },
        ),
        (
            "{ on: device.power, brief: b }",
            EventTrigger::DeviceChange {
                key: "power".into(),
                device: Subject::Any,
            },
        ),
        (
            "{ on: time.daily, at: \"07:30\", brief: b }",
            EventTrigger::TimeDaily {
                hour: 7,
                minute: 30,
            },
        ),
    ];
    for (event, want) in cases {
        let role = role_yaml_with(&format!("  activations:\n    - event: {event}\n"));
        let got = role.activations().iter().find_map(|a| match a {
            Activation::Event(e) => Some(e.trigger.clone()),
            _ => None,
        });
        assert_eq!(got, Some(want), "for {event}");
    }
}

#[test]
fn a_field_of_another_event_family_is_refused_rather_than_ignored() {
    for (event, field) in [
        ("{ on: camera.person, member: any, brief: b }", "member"),
        ("{ on: presence.arrive, camera: porch, brief: b }", "camera"),
        ("{ on: device.power, above: 1, brief: b }", "above"),
        ("{ on: presence.arrive, at: \"07:00\", brief: b }", "at"),
    ] {
        let yaml = block(&format!("  activations:\n    - event: {event}\n"));
        assert_eq!(
            paths(&yaml),
            vec![format!("giap_role.activations[0].event.{field}")],
            "for {event}"
        );
    }
}

#[test]
fn a_sensor_event_needs_exactly_one_threshold_and_a_daily_one_a_time() {
    for event in [
        "{ on: sensor.temperature, brief: b }",
        "{ on: sensor.temperature, above: 1, below: 2, brief: b }",
        "{ on: time.daily, brief: b }",
        "{ on: time.daily, at: \"7.30\", brief: b }",
        "{ on: time.daily, at: \"24:00\", brief: b }",
        "{ on: sensor., above: 1, brief: b }",
        "{ on: presence.arrive }",
    ] {
        let yaml = block(&format!("  activations:\n    - event: {event}\n"));
        assert!(!paths(&yaml).is_empty(), "`{event}` was accepted");
    }
}

#[test]
fn hand_off_names_are_checked_against_the_saved_agents() {
    let yaml = block(
        "  activations:\n    - agent: [trip-planer, helper]\n  hooks:\n    after:\n      - hand_to: budgeter\n",
    );
    let known: BTreeSet<String> = ["trip-planner", "budgeter", "helper"]
        .into_iter()
        .map(String::from)
        .collect();
    let checked = validate_role_recipe("helper", &yaml, Some(&known));
    let found: Vec<&str> = checked.errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        found,
        vec![
            "giap_role.activations[0].agent[0]",
            "giap_role.activations[0].agent[1]"
        ],
        "{:?}",
        checked.errors
    );
    assert!(checked.errors[0].message.contains("trip-planner"));
    assert!(checked.errors[1].message.contains("itself"));
    // Vacuity control: at run time names are not looked up, so the parser accepts them.
    assert!(AgentRole::from_recipe_yaml("helper", &yaml).is_ok());
}

#[test]
fn one_hand_to_per_agent_and_never_a_bare_one() {
    let two = block("  hooks:\n    after:\n      - hand_to: a\n      - hand_to: b\n");
    assert_eq!(
        paths(&two),
        vec!["giap_role.hooks.after[1].hand_to".to_string()]
    );
    let bare = block("  hooks:\n    after: [hand_to]\n");
    assert_eq!(paths(&bare), vec!["giap_role.hooks.after[0]".to_string()]);
    let one = role(&block("  hooks:\n    after:\n      - hand_to: a\n"));
    assert_eq!(one.hooks().hand_to(), Some("a"));
}

#[test]
fn an_empty_activation_list_is_refused_and_agent_lists_merge() {
    assert_eq!(
        paths(&block("  activations: []\n")),
        vec!["giap_role.activations".to_string()]
    );
    let merged = role_yaml_with("  activations:\n    - agent: [a]\n    - agent: [b, a]\n");
    assert_eq!(
        merged.activations(),
        &[Activation::Agent {
            from: vec!["a".to_string(), "b".to_string()]
        }]
    );
}

#[test]
fn role_block_fields_match_the_parser() {
    let samples = [
        ("tool_groups", "[]"),
        ("personal_data", "deny"),
        ("instructions", "go"),
        ("max_turns", "3"),
        ("context_fraction", "0.5"),
        ("model", "some-model"),
        ("context", "{ examples: [] }"),
        ("activations", "[delegated]"),
        ("hooks", "{ before: [today] }"),
        ("a2a", "{ expose: false }"),
    ];
    assert_eq!(samples.len(), ROLE_BLOCK_FIELDS.len());
    for (field, value) in samples {
        assert!(ROLE_BLOCK_FIELDS.contains(&field), "{field} is not listed");
        let body = if field == "tool_groups" {
            format!("  tool_groups: {value}\n")
        } else {
            format!("  tool_groups: []\n  {field}: {value}\n")
        };
        let yaml = format!("instructions: go\ngiap_role:\n{body}");
        assert!(
            AgentRole::from_recipe_yaml("r", &yaml).is_ok(),
            "the parser refuses the listed field `{field}`"
        );
        assert!(
            paths(&yaml).is_empty(),
            "the validator refuses `{field}`: {:?}",
            paths(&yaml)
        );
    }
    // Vacuity control: a field on neither list is refused by both.
    let yaml = block("  triggers: [delegated]\n");
    assert!(AgentRole::from_recipe_yaml("r", &yaml).is_err());
    assert_eq!(paths(&yaml), vec!["giap_role.triggers".to_string()]);
}

#[test]
fn the_validator_refuses_whatever_the_parser_refuses() {
    let corpus = [
        "giap_role:\n  personal_data: deny\n",
        "instructions: go\ngiap_role:\n  tool_groups: giap-weather\n",
        "instructions: go\ngiap_role:\n  tool_groups: []\n  max_turns: 0\n",
        "instructions: go\ngiap_role:\n  tool_groups: []\n  max_turns: 99\n",
        "instructions: go\ngiap_role:\n  tool_groups: []\n  context_fraction: 1.5\n",
        "instructions: go\ngiap_role:\n  tool_groups: []\n  model: \"  \"\n",
        "instructions: go\ngiap_role:\n  tool_groups: []\n  personal_data: share\n",
        "title: t\ngiap_role:\n  tool_groups: []\n",
        "instructions: go\ngiap_role:\n  tool_groups: []\n  personaldata: deny\n",
        "instructions: go\ngiap_role:\n  tool_groups: [\n",
        "instructions: go\ngiap_role:\n  tool_groups: []\n  hooks: { before: [tea] }\n",
    ];
    for yaml in corpus {
        assert!(
            AgentRole::from_recipe_yaml("r", yaml).is_err(),
            "corpus entry no longer refused by the parser, so it tests nothing: {yaml:?}"
        );
        let checked = validate_role_recipe("r", yaml, None);
        assert!(
            !checked.ok(),
            "the validator passed what the parser refuses: {yaml:?}"
        );
        assert!(!checked.errors.is_empty());
    }
}

#[test]
fn tool_groups_an_agent_never_gets_are_named_and_typos_get_a_suggestion() {
    let yaml = block("").replace(
        "tool_groups: []",
        "tool_groups: [giap-knowledg, giap-device-control, my-own-server, giap-weather]",
    );
    let checked = validate_role_recipe("r", &yaml, None);
    let found: Vec<&str> = checked.errors.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        found,
        vec!["giap_role.tool_groups[0]", "giap_role.tool_groups[1]"]
    );
    assert!(checked.errors[0].message.contains("giap-knowledge"));
    // Vacuity control: the parser takes all four, which is why the validator must speak up.
    assert!(AgentRole::from_recipe_yaml("r", &yaml).is_ok());
}

#[test]
fn a_role_that_denies_personal_data_is_never_shown_presence() {
    let yaml = block("  personal_data: deny\n  hooks:\n    before: [weather, presence]\n");
    assert_eq!(paths(&yaml), vec!["giap_role.hooks.before[1]".to_string()]);
    let inherits = yaml.replace("personal_data: deny", "personal_data: inherit");
    assert!(paths(&inherits).is_empty());
}

#[test]
fn the_report_is_the_shape_the_route_returns() {
    let checked = validate_role_recipe("helper", DESIGN_BLOCK, None);
    let json = serde_json::to_value(checked.report()).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["role"]["name"], "helper");
    assert_eq!(json["role"]["tool_groups"][0], "giap-knowledge");
    assert_eq!(json["role"]["activations"][0]["kind"], "delegated");
    assert_eq!(json["role"]["activations"][2]["kind"], "schedule");
    assert_eq!(json["role"]["activations"][2]["cooldown_secs"], 1_800);
    assert_eq!(json["role"]["activations"][3]["on"], "presence.arrive");
    assert_eq!(json["role"]["activations"][3]["member"], "any");
    assert_eq!(json["role"]["activations"][4]["from"][0], "trip-planner");
    assert_eq!(json["role"]["hooks"]["after"][0], "notify_owner");
    assert_eq!(
        json["role"]["preamble_chars"],
        role(DESIGN_BLOCK).instructions().chars().count()
    );
    assert!(json["errors"].as_array().unwrap().is_empty());

    let bad = validate_role_recipe(
        "helper",
        &DESIGN_BLOCK.replace("presence.arrive", "presence.arive"),
        None,
    );
    let json = serde_json::to_value(bad.report()).unwrap();
    assert_eq!(json["ok"], false);
    assert!(json["role"].is_null());
    assert_eq!(
        json["errors"][0]["path"],
        "giap_role.activations[3].event.on"
    );
}

#[test]
fn yaml_that_does_not_parse_or_has_no_block_says_so_at_the_root() {
    assert_eq!(paths("giap_role: [\n"), vec![String::new()]);
    assert_eq!(paths("title: a routine\n"), vec!["giap_role".to_string()]);
}

#[test]
fn a_member_may_be_the_owner_and_only_a_member_may() {
    let mine = role_yaml_with(
        "  activations:\n    - event: { on: presence.arrive, member: owner, brief: b }\n",
    );
    assert!(matches!(
        &mine.activations()[0],
        Activation::Event(e) if e.trigger == (EventTrigger::PresenceArrive { member: Subject::Owner })
    ));
    // Vacuity control: on a camera `owner` is just a camera's name.
    let camera = role_yaml_with(
        "  activations:\n    - event: { on: camera.person, camera: owner, brief: b }\n",
    );
    assert!(matches!(
        &camera.activations()[0],
        Activation::Event(e)
            if e.trigger == (EventTrigger::CameraPerson { camera: Subject::Named("owner".into()) })
    ));
}

#[test]
fn an_event_may_be_held_to_a_part_of_the_day() {
    let night = role_yaml_with(
        "  activations:\n    - event: { on: camera.person, between: \"22:00-06:00\", brief: b }\n",
    );
    let Activation::Event(e) = &night.activations()[0] else {
        panic!("an event");
    };
    let window = e.window.expect("a window");
    assert!(window.contains(23, 30) && window.contains(2, 0));
    assert!(!window.contains(6, 0) && !window.contains(12, 0) && !window.contains(21, 59));
    let day = ClockWindow::parse("09:00-17:00").unwrap();
    assert!(day.contains(9, 0) && !day.contains(17, 0));
    assert_eq!(window.to_string(), "22:00-06:00");
    for (event, field) in [
        (
            "{ on: camera.person, between: \"22:00\", brief: b }",
            "between",
        ),
        (
            "{ on: camera.person, between: \"22:00-22:00\", brief: b }",
            "between",
        ),
        ("{ on: camera.person, between: 22, brief: b }", "between"),
        (
            "{ on: time.daily, at: \"07:00\", between: \"06:00-08:00\", brief: b }",
            "between",
        ),
    ] {
        let yaml = block(&format!("  activations:\n    - event: {event}\n"));
        assert_eq!(
            paths(&yaml),
            vec![format!("giap_role.activations[0].event.{field}")],
            "for {event}"
        );
    }
}
