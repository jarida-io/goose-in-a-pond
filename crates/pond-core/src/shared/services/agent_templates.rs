//! The agents the pond ships and offers. Shipped as `const` recipes, like the long-task roles, so
//! nobody edits them; a member who turns one on gets a copy in `agent_recipes` that the builder
//! may narrow but never widen. A card shows only when this pond can run the agent, and nothing
//! runs until a member turns it on.

use crate::mcp::domain::tool_group::groups_denied_to_subagents;
use crate::shared::domain::orchestration::{AgentRole, RoleError, ROLE_YAML_KEY};
use crate::shared::services::agent_activations::describe_activations;
use crate::shared::services::long_task_roles::TASK_WORK_EXTENSION;
use crate::user_data::domain::recipe::AgentRecipe;
use crate::user_data::domain::settings::Settings;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeSet;

/// Tool groups a template may hold: each reads, or writes only its own run's ledger. Nothing here
/// sends, buys, moves or remembers.
pub const TEMPLATE_TOOL_GROUPS: &[&str] = &[
    TASK_WORK_EXTENSION,
    "giap-knowledge",
    "giap-weather",
    "giap-sensors",
];

/// The sensor type a tank alert listens for; a guess at what a tank's level sensor posts, since
/// nothing in the pond names one yet. The builder can change it.
pub const TANK_SENSOR_TYPE: &str = "water_level";

/// What a template needs from this pond before its card shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The pond can tell this member has come home: a paired phone or face recognition.
    Presence,
    /// The caller is a household member, not an unidentified speaker.
    IdentifiedMember,
    Camera,
    /// A sensor of this type has reported.
    Sensor(&'static str),
    Weather,
    WebSearch,
}

impl Need {
    pub fn met_by(self, pond: &PondCapabilities) -> bool {
        match self {
            Need::Presence => pond.presence,
            Need::IdentifiedMember => pond.identified_member,
            Need::Camera => pond.camera,
            Need::Sensor(kind) => pond.sensor_types.contains(&kind.to_ascii_lowercase()),
            Need::Weather => pond.weather,
            Need::WebSearch => pond.web_search,
        }
    }

    /// A stable name for the card and the client: `sensor:water_level`.
    pub fn label(self) -> String {
        match self {
            Need::Presence => "presence".to_string(),
            Need::IdentifiedMember => "identified_member".to_string(),
            Need::Camera => "camera".to_string(),
            Need::Sensor(kind) => format!("sensor:{kind}"),
            Need::Weather => "weather".to_string(),
            Need::WebSearch => "web_search".to_string(),
        }
    }
}

/// What this pond can do for the member asking; gathered at the edge, judged here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PondCapabilities {
    pub presence: bool,
    pub identified_member: bool,
    pub camera: bool,
    /// Sensor types that have reported, lowercased.
    pub sensor_types: BTreeSet<String>,
    pub weather: bool,
    pub web_search: bool,
}

/// Whether a camera can feed events: the household's consent, the pipeline, and a source.
pub fn camera_configured(settings: &Settings) -> bool {
    settings.cameras_enabled
        && settings.vision_enabled
        && !settings.vision_camera_url.trim().is_empty()
}

/// One shipped agent.
#[derive(Debug)]
pub struct AgentTemplate {
    /// Also the name its first copy is saved under.
    pub id: &'static str,
    pub recipe: &'static str,
    pub needs: &'static [Need],
    /// Why the card shows, said to the member.
    pub why: &'static str,
}

impl AgentTemplate {
    pub fn runs_here(&self, pond: &PondCapabilities) -> bool {
        self.needs.iter().all(|need| need.met_by(pond))
    }

    /// The needs this pond does not meet.
    pub fn missing(&self, pond: &PondCapabilities) -> Vec<String> {
        self.needs
            .iter()
            .filter(|need| !need.met_by(pond))
            .map(|need| need.label())
            .collect()
    }

    pub fn role(&self) -> Result<AgentRole, RoleError> {
        AgentRole::from_recipe_yaml(self.id, self.recipe)?.ok_or(RoleError::Yaml {
            role: self.id.to_string(),
            message: format!("the shipped template has no `{ROLE_YAML_KEY}` block"),
        })
    }

    pub fn title(&self) -> String {
        self.field("title")
    }

    pub fn description(&self) -> String {
        self.field("description")
    }

    fn field(&self, key: &str) -> String {
        serde_yaml::from_str::<serde_yaml::Value>(self.recipe)
            .ok()
            .and_then(|doc| doc.get(key).and_then(|v| v.as_str()).map(str::to_string))
            .unwrap_or_default()
    }
}

pub const MORNING_BRIEF: &str = "morning-brief";
pub const WELCOME_BACK: &str = "welcome-back";
pub const LATE_NIGHT_GATE_ALERT: &str = "late-night-gate-alert";
pub const LOW_TANK_ALERT: &str = "low-tank-alert";
pub const TRIP_PLANNER: &str = "trip-planner";
pub const DOCUMENT_READER: &str = "document-reader";
pub const STUDY_PLANNER: &str = "study-planner";

const MORNING_BRIEF_RECIPE: &str = r#"
version: 1.0.0
title: Morning brief
description: Each morning, the day's weather and what is on the household's schedule, on your phone.
giap_role:
  tool_groups: [giap-task-work, giap-weather]
  personal_data: inherit
  max_turns: 3
  instructions: |
    You write a short morning brief for one member of a household. The brief above you gives
    today's date, today's schedule and the weather. In three or four plain sentences, say what the
    day holds and anything worth getting ready for, such as rain or an early start. Do not invent
    plans that are not on the schedule. Finish with step_report: outcome "completed", with the
    brief as the summary.
  activations:
    - delegated
    - schedule: { cron: "0 30 6 * * *", brief: "Write this morning's brief." }
  hooks:
    before: [today, weather]
    after: [notify_owner]
"#;

const WELCOME_BACK_RECIPE: &str = r#"
version: 1.0.0
title: Welcome back
description: When you get home, a short note on the evening's weather and anything odd in the house.
giap_role:
  tool_groups: [giap-task-work, giap-weather, giap-sensors]
  personal_data: inherit
  max_turns: 4
  instructions: |
    You greet one member of a household who has just come home. In two or three sentences, say
    what is worth knowing now: the weather this evening, and anything the house's sensors show
    that is out of the ordinary. If nothing is, give a short welcome and nothing more. Finish with
    step_report: outcome "completed", with the note as the summary.
  activations:
    - event: { on: presence.arrive, member: owner, brief: "Welcome them home.", cooldown: 4h }
  hooks:
    before: [today, weather]
    after: [notify_owner]
"#;

const LATE_NIGHT_GATE_ALERT_RECIPE: &str = r#"
version: 1.0.0
title: Late-night gate alert
description: Tells you when a camera sees someone at night. Point it at your gate camera in the builder.
giap_role:
  tool_groups: [giap-task-work]
  personal_data: inherit
  max_turns: 2
  instructions: |
    A camera has just seen a person at night. Write one short sentence for the member saying which
    camera saw someone and when, so they can look. Do not guess who it was. Finish with
    step_report: outcome "completed", with that sentence as the summary.
  activations:
    - event: { on: camera.person, camera: any, between: "22:00-06:00", brief: "Tell them a camera saw someone.", cooldown: 15m }
  hooks:
    after: [notify_owner]
"#;

const LOW_TANK_ALERT_RECIPE: &str = r#"
version: 1.0.0
title: Low tank alert
description: Tells you when the water tank runs low, and how fast it has been falling.
giap_role:
  tool_groups: [giap-task-work, giap-sensors]
  personal_data: inherit
  max_turns: 3
  instructions: |
    The water tank's level has just dropped below its alert line. Read the tank sensor's recent
    history and say, in one or two sentences, how full it is now and how quickly it has been
    falling. Finish with step_report: outcome "completed", with that note as the summary.
  activations:
    - event: { on: sensor.water_level, below: 20, brief: "Tell them the tank is low.", cooldown: 6h }
  hooks:
    after: [notify_owner]
"#;

const TRIP_PLANNER_RECIPE: &str = r#"
version: 1.0.0
title: Trip planner
description: Plans a day out or a weekend away, with where to go, in what order, the weather and what to book.
giap_role:
  tool_groups: [giap-task-work, giap-knowledge, giap-weather]
  personal_data: inherit
  max_turns: 8
  instructions: |
    You plan trips for a household. Search for the places, check how long each journey takes and
    what the weather will be, and give a short plan: where to go, in what order, at what times,
    and what would need booking. Never book, buy or contact anyone; say what the person should do.
    When artifact_write is among your tools, save the plan there titled "Result", and when
    step_report is, finish with it.
  activations:
    - delegated
    - task_step
"#;

const DOCUMENT_READER_RECIPE: &str = r#"
version: 1.0.0
title: Document reader
description: Reads a long document a part at a time and answers questions about it, saying where it says so.
giap_role:
  tool_groups: [giap-task-work, giap-knowledge]
  personal_data: inherit
  max_turns: 8
  instructions: |
    You read documents for a household: a lease, a manual, a school letter, a report. Read a long
    one a part at a time, note what matters, and answer the question asked, saying which part of
    the document says so. If the document does not answer it, say so plainly. When artifact_read
    and artifact_write are among your tools, page through the document with the first and keep
    your notes with the second; when step_report is, finish with it.
  activations:
    - delegated
    - task_step
"#;

const STUDY_PLANNER_RECIPE: &str = r#"
version: 1.0.0
title: Study planner
description: Turns a syllabus and an exam date into a study timetable, a topic at a time.
giap_role:
  tool_groups: [giap-task-work, giap-knowledge]
  personal_data: inherit
  max_turns: 6
  instructions: |
    You help a student in the household plan their study. From the syllabus or topic list and the
    exam date you are given, make a timetable: which topic on which day, short sessions, the
    hardest topics early, and a review in the last days. Ask for nothing you were not given; where
    something is missing, say what you assumed. When artifact_write is among your tools, save the
    timetable there titled "Result", and when step_report is, finish with it.
  activations:
    - delegated
    - task_step
"#;

pub const TEMPLATES: &[AgentTemplate] = &[
    AgentTemplate {
        id: MORNING_BRIEF,
        recipe: MORNING_BRIEF_RECIPE,
        needs: &[Need::IdentifiedMember, Need::Weather],
        why: "Your pond can read the weather, so it can tell you each morning what the day holds.",
    },
    AgentTemplate {
        id: WELCOME_BACK,
        recipe: WELCOME_BACK_RECIPE,
        needs: &[Need::Presence, Need::IdentifiedMember],
        why: "Your pond can tell when you get home.",
    },
    AgentTemplate {
        id: LATE_NIGHT_GATE_ALERT,
        recipe: LATE_NIGHT_GATE_ALERT_RECIPE,
        needs: &[Need::IdentifiedMember, Need::Camera],
        why: "Your pond has a camera that can see people.",
    },
    AgentTemplate {
        id: LOW_TANK_ALERT,
        recipe: LOW_TANK_ALERT_RECIPE,
        needs: &[Need::IdentifiedMember, Need::Sensor(TANK_SENSOR_TYPE)],
        why: "A water-level sensor reports to your pond.",
    },
    AgentTemplate {
        id: TRIP_PLANNER,
        recipe: TRIP_PLANNER_RECIPE,
        needs: &[Need::IdentifiedMember, Need::WebSearch],
        why: "Web search is set up, so your pond can look places up.",
    },
    AgentTemplate {
        id: DOCUMENT_READER,
        recipe: DOCUMENT_READER_RECIPE,
        needs: &[Need::IdentifiedMember],
        why: "Long documents can be read a part at a time on this pond.",
    },
    AgentTemplate {
        id: STUDY_PLANNER,
        recipe: STUDY_PLANNER_RECIPE,
        needs: &[Need::IdentifiedMember],
        why: "Your pond can turn a syllabus into a timetable.",
    },
];

pub fn template(id: &str) -> Option<&'static AgentTemplate> {
    TEMPLATES.iter().find(|t| t.id == id)
}

/// One card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentSuggestion {
    pub template: &'static str,
    pub title: String,
    pub description: String,
    pub why: &'static str,
    pub needs: Vec<String>,
    pub activations_summary: String,
}

/// The cards for one member: templates this pond can run, minus those dismissed and those the
/// member already has. A template that does not parse is never offered.
pub fn suggestions(
    pond: &PondCapabilities,
    dismissed: &BTreeSet<String>,
    enabled: &BTreeSet<String>,
) -> Vec<AgentSuggestion> {
    TEMPLATES
        .iter()
        .filter(|t| t.runs_here(pond))
        .filter(|t| !dismissed.contains(t.id) && !enabled.contains(t.id))
        .filter_map(|t| {
            let role = t.role().ok()?;
            Some(AgentSuggestion {
                template: t.id,
                title: t.title(),
                description: t.description(),
                why: t.why,
                needs: t.needs.iter().map(|need| need.label()).collect(),
                activations_summary: describe_activations(role.activations()),
            })
        })
        .collect()
}

/// The templates `member` already has a copy of.
pub fn enabled_templates(recipes: &[AgentRecipe], member: &str) -> BTreeSet<String> {
    recipes
        .iter()
        .filter(|r| r.owner_profile_id.as_deref() == Some(member))
        .filter_map(|r| r.template.clone())
        .collect()
}

/// A free recipe name for a copy: the template's own, else with `-2`, `-3`, ... after it, since
/// names are unique across the household and a second member may turn the same one on.
pub fn copy_name(template: &AgentTemplate, taken: &BTreeSet<String>) -> String {
    if !taken.contains(template.id) {
        return template.id.to_string();
    }
    let mut n: usize = 2;
    loop {
        let name = format!("{}-{n}", template.id);
        if !taken.contains(&name) {
            return name;
        }
        n += 1;
    }
}

/// The recipe a member's tap saves: the template as shipped, owned by them, remembering which
/// template it came from.
pub fn template_copy(
    template: &AgentTemplate,
    owner_profile_id: &str,
    name: String,
    at: DateTime<Utc>,
) -> AgentRecipe {
    AgentRecipe {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        description: template.description(),
        yaml: template.recipe.trim_start().to_string(),
        active: true,
        created_at: at.to_rfc3339(),
        owner_profile_id: Some(owner_profile_id.to_string()),
        template: Some(template.id.to_string()),
    }
}

/// Tool groups an edited copy asks for beyond its template's. A copy may narrow, never widen; a
/// copy whose block no longer parses asks for nothing, since it no longer runs.
pub fn widened_groups(template_id: &str, copy_yaml: &str) -> Vec<String> {
    let Some(base) = template(template_id).and_then(|t| t.role().ok()) else {
        return Vec::new();
    };
    let Ok(Some(copy)) = AgentRole::from_recipe_yaml(template_id, copy_yaml) else {
        return Vec::new();
    };
    copy.requested_tool_groups()
        .difference(base.requested_tool_groups())
        .cloned()
        .collect()
}

/// Whether every template stays read-only and within what an agent is ever given.
pub fn template_groups_are_read_only(role: &AgentRole) -> bool {
    role.requested_tool_groups().iter().all(|group| {
        TEMPLATE_TOOL_GROUPS.contains(&group.as_str())
            && !groups_denied_to_subagents().contains(&group.as_str())
    })
}

#[cfg(test)]
mod tests;
