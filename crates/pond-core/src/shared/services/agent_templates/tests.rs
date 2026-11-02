use super::*;
use crate::shared::domain::role_block::{validate_role_recipe, Activation, EventTrigger, Subject};
use crate::shared::services::long_task_runner::LONG_TASK_ROOT_GROUPS;

/// A pond that can do everything any template needs.
fn able() -> PondCapabilities {
    PondCapabilities {
        presence: true,
        identified_member: true,
        camera: true,
        sensor_types: [TANK_SENSOR_TYPE.to_string()].into_iter().collect(),
        weather: true,
        web_search: true,
    }
}

/// `able()` without one need.
fn without(need: Need) -> PondCapabilities {
    let mut pond = able();
    match need {
        Need::Presence => pond.presence = false,
        Need::IdentifiedMember => pond.identified_member = false,
        Need::Camera => pond.camera = false,
        Need::Sensor(kind) => {
            pond.sensor_types.remove(kind);
        }
        Need::Weather => pond.weather = false,
        Need::WebSearch => pond.web_search = false,
    }
    pond
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

#[test]
fn the_first_set_is_the_seven_the_design_names() {
    let ids: Vec<&str> = TEMPLATES.iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        vec![
            "morning-brief",
            "welcome-back",
            "late-night-gate-alert",
            "low-tank-alert",
            "trip-planner",
            "document-reader",
            "study-planner"
        ]
    );
}

#[test]
fn every_template_parses_and_validates_with_no_errors() {
    for t in TEMPLATES {
        let role = t
            .role()
            .unwrap_or_else(|e| panic!("{} does not parse: {e}", t.id));
        assert_eq!(role.name(), t.id);
        let checked = validate_role_recipe(t.id, t.recipe, None);
        assert!(
            checked.ok() && checked.errors.is_empty(),
            "{} has problems: {:?}",
            t.id,
            checked.errors
        );
        assert!(!t.title().is_empty(), "{} has no title", t.id);
        assert!(!t.description().is_empty(), "{} has no description", t.id);
        assert!(!t.why.trim().is_empty(), "{} says nothing about why", t.id);
    }
}

#[test]
fn every_template_is_read_only_and_runs_in_a_lane_step() {
    for t in TEMPLATES {
        let role = t.role().unwrap();
        assert!(
            template_groups_are_read_only(&role),
            "{} holds a group outside the read-only set: {:?}",
            t.id,
            role.requested_tool_groups()
        );
        for group in role.requested_tool_groups() {
            assert!(
                LONG_TASK_ROOT_GROUPS.contains(&group.as_str()),
                "{} asks for {group}, which a lane run never grants",
                t.id
            );
        }
        assert!(role.hooks().hand_to().is_none(), "{} hands work on", t.id);
    }
    // Vacuity control: the check refuses a role that acts on the house.
    let acts = AgentRole::from_recipe_yaml(
        "x",
        "instructions: go\ngiap_role:\n  tool_groups: [giap-device-control]\n",
    )
    .unwrap()
    .unwrap();
    assert!(!template_groups_are_read_only(&acts));
}

#[test]
fn each_template_shows_only_when_every_need_is_met() {
    for t in TEMPLATES {
        assert!(
            t.runs_here(&able()),
            "{} does not run on a pond that has it all",
            t.id
        );
        for need in t.needs {
            assert!(
                !t.runs_here(&without(*need)),
                "{} still shows without {}",
                t.id,
                need.label()
            );
            assert_eq!(t.missing(&without(*need)), vec![need.label()]);
        }
    }
}

#[test]
fn the_needs_are_the_ones_the_design_gives_each_template() {
    let needs = |id: &str| template(id).unwrap().needs.to_vec();
    assert!(needs(WELCOME_BACK).contains(&Need::Presence));
    assert!(needs(WELCOME_BACK).contains(&Need::IdentifiedMember));
    assert!(needs(LATE_NIGHT_GATE_ALERT).contains(&Need::Camera));
    assert!(needs(LOW_TANK_ALERT).contains(&Need::Sensor(TANK_SENSOR_TYPE)));
    assert!(needs(MORNING_BRIEF).contains(&Need::Weather));
    assert!(needs(TRIP_PLANNER).contains(&Need::WebSearch));
    for t in TEMPLATES {
        assert!(
            t.needs.contains(&Need::IdentifiedMember),
            "{} could be offered to a guest, who cannot own it",
            t.id
        );
    }
}

#[test]
fn the_tank_alert_listens_for_the_sensor_it_needs() {
    let role = template(LOW_TANK_ALERT).unwrap().role().unwrap();
    let listens = role.activations().iter().any(|a| {
        matches!(a, Activation::Event(e)
            if matches!(&e.trigger, EventTrigger::Sensor { sensor_type, .. } if sensor_type == TANK_SENSOR_TYPE))
    });
    assert!(
        listens,
        "the tank alert's trigger and its need name different sensors"
    );
    // A sensor of another type does not count.
    let mut pond = able();
    pond.sensor_types = ["temperature".to_string()].into_iter().collect();
    assert!(!template(LOW_TANK_ALERT).unwrap().runs_here(&pond));
}

#[test]
fn welcome_back_greets_only_its_owner() {
    let role = template(WELCOME_BACK).unwrap().role().unwrap();
    assert!(role.activations().iter().any(|a| matches!(a,
        Activation::Event(e) if e.trigger == (EventTrigger::PresenceArrive { member: Subject::Owner }))));
    assert!(
        !role.accepts_delegation(),
        "it runs on arrival, not when handed work"
    );
}

#[test]
fn the_gate_alert_runs_only_at_night() {
    let role = template(LATE_NIGHT_GATE_ALERT).unwrap().role().unwrap();
    let window = role
        .activations()
        .iter()
        .find_map(|a| match a {
            Activation::Event(e) => e.window,
            _ => None,
        })
        .expect("a window");
    assert!(window.contains(23, 0) && window.contains(3, 0));
    assert!(!window.contains(12, 0));
}

#[test]
fn suggestions_leave_out_what_cannot_run_was_dismissed_or_is_already_on() {
    let all: Vec<&str> = suggestions(&able(), &none(), &none())
        .iter()
        .map(|s| s.template)
        .collect();
    assert_eq!(
        all.len(),
        TEMPLATES.len(),
        "vacuity control: a full pond offers every one"
    );

    let no_weather = suggestions(&without(Need::Weather), &none(), &none());
    assert!(!no_weather.iter().any(|s| s.template == MORNING_BRIEF));
    assert!(no_weather.iter().any(|s| s.template == TRIP_PLANNER));

    let dismissed: BTreeSet<String> = [TRIP_PLANNER.to_string()].into_iter().collect();
    let enabled: BTreeSet<String> = [STUDY_PLANNER.to_string()].into_iter().collect();
    let left: Vec<&str> = suggestions(&able(), &dismissed, &enabled)
        .iter()
        .map(|s| s.template)
        .collect();
    assert!(!left.contains(&TRIP_PLANNER) && !left.contains(&STUDY_PLANNER));
    assert_eq!(left.len(), TEMPLATES.len() - 2);

    assert!(
        suggestions(&without(Need::IdentifiedMember), &none(), &none()).is_empty(),
        "a guest is offered nothing"
    );
}

#[test]
fn a_card_carries_what_the_member_reads() {
    let cards = suggestions(&able(), &none(), &none());
    let brief = cards.iter().find(|s| s.template == MORNING_BRIEF).unwrap();
    assert_eq!(brief.title, "Morning brief");
    assert_eq!(brief.needs, vec!["identified_member", "weather"]);
    assert_eq!(
        brief.activations_summary,
        "When your assistant hands it work; every day at 06:30"
    );
    let json = serde_json::to_value(brief).unwrap();
    for key in [
        "template",
        "title",
        "description",
        "why",
        "needs",
        "activations_summary",
    ] {
        assert!(json.get(key).is_some(), "the card has no `{key}`");
    }
}

fn recipe(name: &str, owner: Option<&str>, template: Option<&str>) -> AgentRecipe {
    AgentRecipe {
        id: format!("id-{name}"),
        name: name.to_string(),
        description: String::new(),
        yaml: String::new(),
        active: true,
        created_at: String::new(),
        owner_profile_id: owner.map(str::to_string),
        template: template.map(str::to_string),
    }
}

#[test]
fn a_member_has_a_template_only_through_their_own_copy() {
    let recipes = [
        recipe("trip-planner", Some("liz"), Some(TRIP_PLANNER)),
        recipe("study-planner", Some("jerry"), Some(STUDY_PLANNER)),
        recipe("my-own", Some("jerry"), None),
    ];
    let jerry = enabled_templates(&recipes, "jerry");
    assert_eq!(jerry, [STUDY_PLANNER.to_string()].into_iter().collect());
    assert!(!jerry.contains(TRIP_PLANNER), "liz's copy is not jerry's");
}

#[test]
fn a_second_copy_gets_a_free_name_and_keeps_its_template() {
    let t = template(TRIP_PLANNER).unwrap();
    assert_eq!(copy_name(t, &none()), "trip-planner");
    let taken: BTreeSet<String> = ["trip-planner", "trip-planner-2"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(copy_name(t, &taken), "trip-planner-3");

    let at = chrono::DateTime::from_timestamp(1_790_000_000, 0).unwrap();
    let copy = template_copy(t, "jerry", copy_name(t, &taken), at);
    assert_eq!(copy.name, "trip-planner-3");
    assert_eq!(copy.owner_profile_id.as_deref(), Some("jerry"));
    assert_eq!(copy.template.as_deref(), Some(TRIP_PLANNER));
    assert!(copy.active);
    let role = AgentRole::from_recipe_yaml(&copy.name, &copy.yaml)
        .unwrap()
        .expect("the copy is an agent");
    assert_eq!(
        role.requested_tool_groups(),
        t.role().unwrap().requested_tool_groups()
    );
}

#[test]
fn a_copy_may_narrow_its_tools_but_never_widen_them() {
    let t = template(TRIP_PLANNER).unwrap();
    let narrowed = t.recipe.replace(
        "[giap-task-work, giap-knowledge, giap-weather]",
        "[giap-knowledge]",
    );
    assert!(widened_groups(TRIP_PLANNER, &narrowed).is_empty());
    let widened = t.recipe.replace(
        "[giap-task-work, giap-knowledge, giap-weather]",
        "[giap-task-work, giap-knowledge, giap-weather, giap-memory]",
    );
    assert_ne!(widened, t.recipe, "vacuity control: the replacement took");
    assert_eq!(
        widened_groups(TRIP_PLANNER, &widened),
        vec!["giap-memory".to_string()]
    );
    assert!(widened_groups("no-such-template", &widened).is_empty());
}

#[test]
fn a_camera_counts_only_with_consent_a_pipeline_and_a_source() {
    let mut settings = Settings {
        cameras_enabled: true,
        vision_enabled: true,
        vision_camera_url: "/dev/video0".to_string(),
        ..Settings::default()
    };
    assert!(camera_configured(&settings));
    settings.cameras_enabled = false;
    assert!(!camera_configured(&settings));
    settings.cameras_enabled = true;
    settings.vision_camera_url = "  ".to_string();
    assert!(!camera_configured(&settings));
}
