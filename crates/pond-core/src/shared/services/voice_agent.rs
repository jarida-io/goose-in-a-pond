//! The Voice agent: the shipped default that answers every spoken turn, from the room and from
//! the phone. Shipped, it is the prompt style's own `<voice-mode>` text with the prime's tools and
//! the household's `voice_max_turns`. A household's edit is one `agent_recipes` row named `voice`,
//! which can replace the text, add examples, turn tool groups off and set a turn cap, and never
//! turn on a group the prime lacks.

use std::borrow::Cow;
use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::mcp::domain::tool_group::{
    find_group, INTERVIEW_EXTENSION, TASK_WORK_EXTENSION, TOOLKIT_EXTENSION, TOOL_GROUPS,
};
use crate::shared::domain::orchestration::{
    AgentRole, RolePersonalData, DEFAULT_CONTEXT_FRACTION, DEFAULT_ROLE_MAX_TURNS,
    MAX_ROLE_MAX_TURNS,
};
use crate::shared::domain::role_block::{
    render_examples, FieldError, RoleExample, MAX_ROLE_EXAMPLES, MAX_ROLE_EXAMPLE_CHARS,
};
use crate::user_data::domain::recipe::AgentRecipe;

pub const VOICE_AGENT: &str = "voice";
pub const VOICE_AGENT_TITLE: &str = "Voice";
/// The template its row records, so the row is told from a member's own agent.
pub const VOICE_TEMPLATE: &str = "voice";
/// Its own block, not `giap_role`: no role reader may take the Voice agent for a delegate or a
/// task step.
pub const VOICE_YAML_KEY: &str = "giap_voice";
/// Every voice turn's preamble carries the text, so it is capped; a guess, like the role caps.
pub const MAX_VOICE_INSTRUCTIONS_CHARS: usize = 4_000;

const DESCRIPTION: &str = "Answers every spoken turn, in the room and on the phone.";

/// A household's edit; the default is the shipped agent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoiceAgent {
    /// In place of the style's `<voice-mode>` text; `None` keeps it.
    pub instructions: Option<String>,
    pub examples: Vec<RoleExample>,
    /// Groups the prime has that voice turns go without.
    pub tool_groups_off: BTreeSet<String>,
    /// In place of `voice_max_turns`; `None` keeps it.
    pub max_turns: Option<u32>,
}

/// The groups a voice turn may have, with what a person calls them. The toolkit stays on, as it
/// keeps narrowing reversible; a helper's and an interview's groups are never the prime's.
pub fn voice_tool_groups() -> Vec<(&'static str, &'static str)> {
    TOOL_GROUPS
        .iter()
        .map(|g| g.extension)
        .filter(|g| ![TOOLKIT_EXTENSION, INTERVIEW_EXTENSION, TASK_WORK_EXTENSION].contains(g))
        .map(|g| (g, group_label(g)))
        .collect()
}

fn group_label(group: &str) -> &'static str {
    match group {
        "giap-memory" => "What the household has said",
        "giap-system" => "The time and this machine",
        "giap-schedule" => "Timers, reminders and schedules",
        "giap-weather" => "Weather",
        "giap-knowledge" => "Web search and Wikipedia",
        "giap-device" => "Devices and skills",
        "giap-device-control" => "Switching devices",
        "giap-sensors" => "Sensors",
        "giap-context" => "What the household shared",
        "giap-vision" => "Cameras",
        "giap-travel" => "Directions and rides",
        "giap-orchestrator" => "Helper agents",
        "giap-tasks" => "Long tasks",
        _ => "Other tools",
    }
}

/// What the `giap_voice` block holds.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VoiceBlock {
    #[serde(default)]
    personal_data: RolePersonalData,
    /// Always empty: nothing delegates to it and no task step runs it.
    #[serde(default)]
    activations: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    examples: Vec<RoleExample>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    tool_groups_off: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_turns: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VoiceRecipe {
    version: String,
    title: String,
    description: String,
    #[serde(rename = "giap_voice")]
    block: VoiceBlock,
}

impl VoiceAgent {
    pub fn is_edited(&self) -> bool {
        *self != VoiceAgent::default()
    }

    /// The `agent_recipes` row's YAML.
    pub fn to_recipe_yaml(&self) -> String {
        let recipe = VoiceRecipe {
            version: "1.0.0".into(),
            title: VOICE_AGENT_TITLE.into(),
            description: DESCRIPTION.into(),
            block: VoiceBlock {
                personal_data: RolePersonalData::Inherit,
                activations: Vec::new(),
                instructions: self.instructions.clone(),
                examples: self.examples.clone(),
                tool_groups_off: self.tool_groups_off.clone(),
                max_turns: self.max_turns,
            },
        };
        serde_yaml::to_string(&recipe).unwrap_or_default()
    }

    /// Read the row back. A row that does not read is the shipped agent, never a wider one.
    pub fn from_recipe_yaml(yaml: &str) -> Result<VoiceAgent, String> {
        let recipe: VoiceRecipe = serde_yaml::from_str(yaml).map_err(|e| e.to_string())?;
        let block = recipe.block;
        Ok(VoiceAgent {
            instructions: block.instructions.filter(|t| !t.trim().is_empty()),
            examples: block.examples,
            tool_groups_off: block.tool_groups_off,
            max_turns: block.max_turns,
        })
    }

    /// The text a voice turn's `<voice-mode>` section carries, given the style's own.
    pub fn effective_instructions<'a>(&'a self, shipped: &'a str) -> &'a str {
        self.instructions.as_deref().unwrap_or(shipped)
    }

    /// The `<voice-mode>` body with the examples rendered as a role's are; `None` while neither
    /// the text nor the examples were edited, so the prompt stays byte for byte the style's.
    pub fn voice_body(&self, shipped: &str) -> Option<String> {
        if self.instructions.is_none() && self.examples.is_empty() {
            return None;
        }
        let mut body = self.effective_instructions(shipped).trim_end().to_string();
        body.push_str(&render_examples(&self.examples));
        Some(body.trim_end().to_string())
    }

    /// Whether a voice turn keeps the tools of `group`.
    pub fn keeps_group(&self, group: &str) -> bool {
        !self.tool_groups_off.contains(group)
    }

    /// The role its prefill is tracked under: the text and groups its preamble is built from.
    pub fn prefill_role(&self, shipped: &str) -> Option<AgentRole> {
        let text = self
            .voice_body(shipped)
            .unwrap_or_else(|| shipped.to_string());
        let groups = voice_tool_groups()
            .into_iter()
            .map(|(g, _)| g)
            .filter(|g| self.keeps_group(g))
            .map(str::to_string)
            .collect();
        AgentRole::new(
            VOICE_AGENT,
            if text.trim().is_empty() {
                VOICE_AGENT_TITLE
            } else {
                &text
            },
            groups,
            RolePersonalData::Inherit,
            self.max_turns.unwrap_or(DEFAULT_ROLE_MAX_TURNS),
            DEFAULT_CONTEXT_FRACTION,
        )
        .ok()
    }

    /// This edit with `patch` applied, or every problem in the patch, each at its field.
    pub fn patched(&self, patch: &VoicePatch) -> Result<VoiceAgent, Vec<FieldError>> {
        let mut errors = Vec::new();
        let mut next = self.clone();
        if let Some(text) = &patch.instructions {
            let count = text.chars().count();
            if count > MAX_VOICE_INSTRUCTIONS_CHARS {
                errors.push(FieldError {
                    path: "instructions".into(),
                    message: format!(
                        "is {count} characters; the most is {MAX_VOICE_INSTRUCTIONS_CHARS}, since \
                         every spoken turn carries it"
                    ),
                });
            }
            next.instructions = Some(text.clone()).filter(|t| !t.trim().is_empty());
        }
        if let Some(examples) = &patch.examples {
            if examples.len() > MAX_ROLE_EXAMPLES {
                errors.push(FieldError {
                    path: "examples".into(),
                    message: format!(
                        "has {} examples; the most is {MAX_ROLE_EXAMPLES}, since every spoken turn \
                         carries them",
                        examples.len()
                    ),
                });
            }
            for (i, example) in examples.iter().enumerate() {
                for (field, text) in [("brief", &example.brief), ("answer", &example.answer)] {
                    let path = format!("examples[{i}].{field}");
                    let count = text.chars().count();
                    if text.trim().is_empty() {
                        errors.push(FieldError {
                            path,
                            message: "is empty".into(),
                        });
                    } else if count > MAX_ROLE_EXAMPLE_CHARS {
                        errors.push(FieldError {
                            path,
                            message: format!(
                                "is {count} characters; the most is {MAX_ROLE_EXAMPLE_CHARS}"
                            ),
                        });
                    }
                }
            }
            next.examples = examples.clone();
        }
        if let Some(off) = &patch.tool_groups_off {
            let known: Vec<&str> = voice_tool_groups().into_iter().map(|(g, _)| g).collect();
            for (i, group) in off.iter().enumerate() {
                if !known.contains(&group.as_str()) {
                    let message = match find_group(group) {
                        Some(_) => format!("`{group}` is never part of a spoken turn"),
                        None => format!("`{group}` is not a tool group; the groups are {known:?}"),
                    };
                    errors.push(FieldError {
                        path: format!("tool_groups_off[{i}]"),
                        message,
                    });
                }
            }
            next.tool_groups_off = off.iter().cloned().collect();
        }
        if let Some(cap) = patch.max_turns {
            if let Some(n) = cap {
                if n == 0 || n > MAX_ROLE_MAX_TURNS {
                    errors.push(FieldError {
                        path: "max_turns".into(),
                        message: format!("is {n}; it must be from 1 to {MAX_ROLE_MAX_TURNS}"),
                    });
                }
            }
            next.max_turns = cap;
        }
        if errors.is_empty() {
            Ok(next)
        } else {
            Err(errors)
        }
    }
}

/// `PUT /agents/voice`: each field absent keeps what is there, and a null one goes back to the
/// shipped agent's.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct VoicePatch {
    pub instructions: Option<String>,
    pub examples: Option<Vec<RoleExample>>,
    pub tool_groups_off: Option<Vec<String>>,
    /// `Some(None)` drops the cap.
    pub max_turns: Option<Option<u32>>,
}

impl VoicePatch {
    /// Read a request body, naming the field of every value that does not read.
    pub fn from_json(body: &serde_json::Value) -> Result<VoicePatch, Vec<FieldError>> {
        use serde_json::Value;
        let error = |path: &str, message: &str| FieldError {
            path: path.to_string(),
            message: message.to_string(),
        };
        let Some(fields) = body.as_object() else {
            return Err(vec![error("", "the body must be a JSON object")]);
        };
        let mut patch = VoicePatch::default();
        let mut errors = Vec::new();
        for (key, value) in fields {
            match (key.as_str(), value) {
                ("instructions", Value::Null) => patch.instructions = Some(String::new()),
                ("instructions", Value::String(text)) => patch.instructions = Some(text.clone()),
                ("instructions", _) => errors.push(error(key, "must be text")),
                ("examples", Value::Null) => patch.examples = Some(Vec::new()),
                ("examples", Value::Array(items)) => {
                    let mut examples = Vec::with_capacity(items.len());
                    for (i, item) in items.iter().enumerate() {
                        match serde_json::from_value::<RoleExample>(item.clone()) {
                            Ok(example) => examples.push(example),
                            Err(_) => errors.push(error(
                                &format!("examples[{i}]"),
                                "must be {\"brief\": text, \"answer\": text}",
                            )),
                        }
                    }
                    patch.examples = Some(examples);
                }
                ("examples", _) => errors.push(error(key, "must be a list")),
                ("tool_groups_off", Value::Null) => patch.tool_groups_off = Some(Vec::new()),
                ("tool_groups_off", Value::Array(items)) => {
                    let mut groups = Vec::with_capacity(items.len());
                    for (i, item) in items.iter().enumerate() {
                        match item.as_str() {
                            Some(group) => groups.push(group.to_string()),
                            None => errors.push(error(
                                &format!("tool_groups_off[{i}]"),
                                "must be a tool group's name",
                            )),
                        }
                    }
                    patch.tool_groups_off = Some(groups);
                }
                ("tool_groups_off", _) => errors.push(error(key, "must be a list")),
                ("max_turns", Value::Null) => patch.max_turns = Some(None),
                ("max_turns", value) => match value.as_u64().and_then(|n| u32::try_from(n).ok()) {
                    Some(n) => patch.max_turns = Some(Some(n)),
                    None => errors.push(error(key, "must be a whole number")),
                },
                _ => errors.push(error(key, "is not a field of the Voice agent")),
            }
        }
        if errors.is_empty() {
            Ok(patch)
        } else {
            Err(errors)
        }
    }
}

/// Whether `recipe` is the Voice agent's row, not a member's own agent that happens to be named
/// `voice`.
pub fn is_voice_row(recipe: &AgentRecipe) -> bool {
    recipe.name == VOICE_AGENT && recipe.template.as_deref() == Some(VOICE_TEMPLATE)
}

/// The edit `row` holds, or the shipped agent for no row or another agent's row.
pub fn voice_agent_from_row(row: Option<&AgentRecipe>) -> Result<VoiceAgent, String> {
    match row {
        Some(row) if is_voice_row(row) => VoiceAgent::from_recipe_yaml(&row.yaml),
        _ => Ok(VoiceAgent::default()),
    }
}

/// The template a voice turn renders: the style's own while the text and examples are unedited,
/// so the prompt and its cached prefix stay exactly as they were.
pub fn voice_template<'a>(template: &'a str, agent: &VoiceAgent) -> Cow<'a, str> {
    let Some(shipped) = crate::prompts::voice_mode_body(template) else {
        return Cow::Borrowed(template);
    };
    match agent.voice_body(shipped) {
        Some(body) => crate::prompts::with_voice_mode_body(template, &body)
            .map(Cow::Owned)
            .unwrap_or(Cow::Borrowed(template)),
        None => Cow::Borrowed(template),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::services::prompt_builder::{
        build_prompt_partition, compute_prefix_hash_fast,
    };
    use crate::prompts::{
        PromptState, PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM,
    };
    use crate::user_data::domain::settings::Settings;

    const STYLES: [(&str, &str); 4] = [
        ("balanced", PROMPT_BALANCED),
        ("concise", PROMPT_CONCISE),
        ("technical", PROMPT_TECHNICAL),
        ("warm", PROMPT_WARM),
    ];

    fn voice_state() -> PromptState {
        PromptState {
            current_date: "Monday, 2 November 2026".into(),
            current_time: "half past five".into(),
            voice_mode: true,
            available_tools: Vec::new(),
            thinking_enabled: false,
            compact_prompt: false,
            native_tools_json: true,
            tools_offered: true,
            prefix_hash: None,
        }
    }

    fn prefix(template: &str) -> String {
        build_prompt_partition(&Settings::default(), None, &voice_state(), template).static_prefix
    }

    #[test]
    fn unedited_every_style_renders_byte_for_byte_as_it_did() {
        for (style, template) in STYLES {
            let rendered = voice_template(template, &VoiceAgent::default());
            assert!(matches!(rendered, Cow::Borrowed(_)), "{style}");
            assert_eq!(prefix(&rendered), prefix(template), "{style}");
            // The substitution itself moves nothing around the section either.
            let shipped = crate::prompts::voice_mode_body(template).expect("a voice-mode section");
            let same = crate::prompts::with_voice_mode_body(template, shipped).unwrap();
            assert_eq!(
                prefix(&same),
                prefix(template),
                "{style}: the raw substitution"
            );
        }
    }

    #[test]
    fn edited_text_and_examples_replace_the_section_and_move_the_prefix_hash() {
        let agent = VoiceAgent {
            instructions: Some("Speak like a tour guide. {{ not a tag }} {% if x %}".into()),
            examples: vec![RoleExample {
                brief: "What's on today?".into(),
                answer: "A quiet day: one call at three.".into(),
            }],
            ..VoiceAgent::default()
        };
        for (style, template) in STYLES {
            let edited = voice_template(template, &agent);
            let out = prefix(&edited);
            assert!(
                out.contains(
                    "<voice-mode>\nSpeak like a tour guide. {{ not a tag }} { % if x %}\n\n\
                     Examples of a brief and the answer it wants:\n\n\
                     Brief: What's on today?\nAnswer: A quiet day: one call at three.\n</voice-mode>"
                ),
                "{style}: {out}"
            );
            let shipped = crate::prompts::voice_mode_body(template).unwrap();
            assert!(
                !out.contains(shipped),
                "{style}: the style's own text is gone"
            );
            let s = Settings::default();
            assert_ne!(
                compute_prefix_hash_fast(&s, &voice_state(), &edited),
                compute_prefix_hash_fast(&s, &voice_state(), template),
                "{style}: the cached prefix must be rebuilt"
            );
        }
    }

    #[test]
    fn examples_alone_keep_the_styles_text_before_them() {
        let agent = VoiceAgent {
            examples: vec![RoleExample {
                brief: "Is it cold?".into(),
                answer: "Twelve degrees: take a jacket.".into(),
            }],
            ..VoiceAgent::default()
        };
        let shipped = crate::prompts::voice_mode_body(PROMPT_BALANCED).unwrap();
        let body = agent.voice_body(shipped).unwrap();
        assert!(body.starts_with(shipped));
        assert!(body.ends_with("Answer: Twelve degrees: take a jacket."));
    }

    #[test]
    fn the_row_reads_back_as_it_was_saved_and_is_no_role() {
        let agent = VoiceAgent {
            instructions: Some("Be brief.\nAlways.".into()),
            examples: vec![RoleExample {
                brief: "b".into(),
                answer: "a".into(),
            }],
            tool_groups_off: ["giap-vision".to_string()].into(),
            max_turns: Some(4),
        };
        let yaml = agent.to_recipe_yaml();
        assert_eq!(VoiceAgent::from_recipe_yaml(&yaml).unwrap(), agent);
        assert_eq!(
            AgentRole::from_recipe_yaml(VOICE_AGENT, &yaml),
            Ok(None),
            "no delegate or task step may be read out of it"
        );
        assert_eq!(
            VoiceAgent::from_recipe_yaml(&VoiceAgent::default().to_recipe_yaml()).unwrap(),
            VoiceAgent::default()
        );
    }

    #[test]
    fn a_patch_is_checked_field_by_field() {
        let long = "x".repeat(MAX_VOICE_INSTRUCTIONS_CHARS + 1);
        let patch = VoicePatch {
            instructions: Some(long),
            examples: Some(vec![
                RoleExample {
                    brief: String::new(),
                    answer: "ok".into(),
                };
                MAX_ROLE_EXAMPLES + 1
            ]),
            tool_groups_off: Some(vec![
                "giap-weather".into(),
                "giap-task-work".into(),
                "giap-nonsense".into(),
            ]),
            max_turns: Some(Some(MAX_ROLE_MAX_TURNS + 1)),
        };
        let errors = VoiceAgent::default().patched(&patch).unwrap_err();
        let paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
        for want in [
            "instructions",
            "examples",
            "examples[0].brief",
            "tool_groups_off[1]",
            "tool_groups_off[2]",
            "max_turns",
        ] {
            assert!(paths.contains(&want), "{want} missing from {paths:?}");
        }
        assert!(!paths.contains(&"tool_groups_off[0]"));
    }

    #[test]
    fn a_patch_keeps_what_it_leaves_out_and_a_null_cap_drops_the_cap() {
        let agent = VoiceAgent {
            instructions: Some("Be brief.".into()),
            max_turns: Some(3),
            ..VoiceAgent::default()
        };
        let patch =
            VoicePatch::from_json(&serde_json::json!({"tool_groups_off": ["giap-weather"]}))
                .unwrap();
        let next = agent.patched(&patch).unwrap();
        assert_eq!(next.instructions.as_deref(), Some("Be brief."));
        assert_eq!(next.max_turns, Some(3));
        assert!(!next.keeps_group("giap-weather"));

        let cleared =
            VoicePatch::from_json(&serde_json::json!({"max_turns": null, "instructions": ""}))
                .unwrap();
        let next = next.patched(&cleared).unwrap();
        assert_eq!(next.max_turns, None);
        assert_eq!(
            next.instructions, None,
            "blank text is the style's own again"
        );
    }

    #[test]
    fn a_body_that_does_not_read_names_each_field() {
        let errors = VoicePatch::from_json(&serde_json::json!({
            "tools": [],
            "instructions": 3,
            "examples": [{"brief": "hi"}],
            "tool_groups_off": ["giap-weather", 7],
            "max_turns": -1,
        }))
        .unwrap_err();
        let mut paths: Vec<&str> = errors.iter().map(|e| e.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            [
                "examples[0]",
                "instructions",
                "max_turns",
                "tool_groups_off[1]",
                "tools"
            ]
        );
        assert!(VoicePatch::from_json(&serde_json::json!([])).is_err());
        assert_eq!(
            VoicePatch::from_json(&serde_json::json!({"examples": null})).unwrap(),
            VoicePatch {
                examples: Some(Vec::new()),
                ..VoicePatch::default()
            }
        );
    }

    #[test]
    fn the_offered_groups_never_include_a_helpers_or_the_escape_hatch() {
        let groups: Vec<&str> = voice_tool_groups().into_iter().map(|(g, _)| g).collect();
        for never in [TOOLKIT_EXTENSION, INTERVIEW_EXTENSION, TASK_WORK_EXTENSION] {
            assert!(!groups.contains(&never), "{never}");
        }
        assert!(groups.contains(&"giap-weather"));
        assert!(voice_tool_groups()
            .iter()
            .all(|(_, label)| *label != "Other tools"));
    }

    #[test]
    fn the_prefill_role_follows_the_text_and_the_groups() {
        let shipped = crate::prompts::voice_mode_body(PROMPT_BALANCED).unwrap();
        let plain = VoiceAgent::default().prefill_role(shipped).unwrap();
        assert_eq!(plain.name(), VOICE_AGENT);
        assert_eq!(plain.instructions(), shipped);
        let narrowed = VoiceAgent {
            tool_groups_off: ["giap-weather".to_string()].into(),
            ..VoiceAgent::default()
        }
        .prefill_role(shipped)
        .unwrap();
        assert!(!narrowed.requested_tool_groups().contains("giap-weather"));
        use crate::shared::services::agent_prefill::definition_hash;
        assert_ne!(definition_hash(&plain), definition_hash(&narrowed));
    }
}
