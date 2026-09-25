//! Catalog of GIAP tool groups, one per `giap-*` MCP extension: the unit of relevance selection.
//! Groups, not tools: a schema is indivisible and costs ~100 tokens per turn on an 8K budget.

/// A selectable group of tools, backed by exactly one `giap-*` MCP extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolGroup {
    /// MCP extension name (`"giap-weather"`); tool names are `"<extension>__<tool>"`.
    pub extension: &'static str,
    /// What gets embedded and scored, so phrase it like user requests, not an API summary.
    pub description: &'static str,
    /// Always loaded, never scored, never removable. See [`CORE_RATIONALE`].
    pub core: bool,
}

/// Why each core group is core: no opening message predicts a need for memory, `giap-system`
/// holds `get_current_time`, and `giap-toolkit` keeps narrowing reversible.
pub const CORE_RATIONALE: &str = "memory=cross-cutting, system=time, toolkit=escape";

/// The extension providing the discovery / enable escape hatch.
pub const TOOLKIT_EXTENSION: &str = "giap-toolkit";

/// Extension carrying `delegate`. A const because a typo is silent: an unknown extension is
/// treated as a user-added MCP server, which selection never narrows.
pub const ORCHESTRATOR_EXTENSION: &str = "giap-orchestrator";

/// Separator in a prefixed tool name (`giap-weather__get_forecast`); Goose's convention.
pub const TOOL_NAME_SEPARATOR: &str = "__";

/// Env var that offers the model no tools; defined once so every reader parses it the same.
pub const NO_TOOLS_ENV: &str = "GIAP_NO_TOOLS";

/// Whether a value of [`NO_TOOLS_ENV`] means "no tools". Unrecognised values count as on:
/// a turn that quietly kept its tools is the worse failure.
pub fn no_tools_from(value: Option<&str>) -> bool {
    match value.map(str::trim) {
        None | Some("") | Some("0") | Some("false") | Some("no") => false,
        Some(_) => true,
    }
}

pub fn no_tools_env_set() -> bool {
    no_tools_from(std::env::var(NO_TOOLS_ENV).ok().as_deref())
}

/// Every known group: what COULD be registered (`ext_*_enabled` gates that), not what was.
pub const TOOL_GROUPS: &[ToolGroup] = &[
    ToolGroup {
        extension: "giap-memory",
        description: "The user's long-term memories: remember a fact or preference about them, \
                      recall what they have told you before, or forget something they no longer \
                      want kept.",
        core: true,
    },
    ToolGroup {
        extension: "giap-system",
        description: "This machine and the current moment: the date, time and timezone, operating \
                      system and hostname, memory and disk usage, and desktop notifications.",
        core: true,
    },
    ToolGroup {
        extension: TOOLKIT_EXTENSION,
        description: "Which groups of tools are loaded for this conversation, and loading another \
                      group when a capability you need is not currently available.",
        core: true,
    },
    ToolGroup {
        extension: "giap-schedule",
        // The one-shot timer clause relies on `set_timer`; a 6-field cron cannot express it.
        description: "Reminders, alarms, timers, recurring routines and scheduled tasks: set a \
                      one-shot timer for a few minutes or hours from now, create a repeating \
                      schedule, list or inspect what is scheduled, change or pause or delete one, \
                      run one now, and review past runs. Anything about doing something later or \
                      every day at a certain time.",
        core: false,
    },
    ToolGroup {
        extension: "giap-weather",
        description: "The weather: current conditions and the forecast for the days ahead, \
                      temperature, rain, whether to take an umbrella, for here or another place.",
        core: false,
    },
    ToolGroup {
        extension: "giap-knowledge",
        description: "General reference knowledge and factual lookup: encyclopedia and Wikipedia \
                      articles, definitions of words, books and authors, short factual answers \
                      about history, science, geography, people and places, and computed answers \
                      such as arithmetic, unit and currency conversion, dates and statistics.",
        core: false,
    },
    ToolGroup {
        extension: "giap-device",
        description: "The smart-home devices in this house: which lights, plugs, sensors, \
                      thermostats and appliances are registered, which rooms they are in, whether \
                      they are online, and what state they report.",
        core: false,
    },
    ToolGroup {
        extension: "giap-device-control",
        description: "Actually operating the smart-home devices: turning a light or plug or \
                      appliance on and off, changing brightness or colour or temperature, opening \
                      or closing something, setting a device to a new state.",
        core: false,
    },
    ToolGroup {
        extension: "giap-sensors",
        description:
            "Sensor readings over time: temperature, humidity, air quality, power use and \
                      other measurements from sensors in the house, their latest values and their \
                      history.",
        core: false,
    },
    ToolGroup {
        extension: "giap-context",
        description: "The speaker's own personal context, from sources they connected: what a \
                      camera or sensor of theirs has recorded, searched by meaning or listed by \
                      recency. Read-only, and scoped to whoever is speaking.",
        core: false,
    },
    ToolGroup {
        extension: ORCHESTRATOR_EXTENSION,
        description: "Handing a piece of work to a named specialist agent that runs on its own \
                      and reports back: research a question in depth, work through a longer task \
                      under a saved role, or have a second agent do something while this \
                      conversation carries on.",
        core: false,
    },
];

/// Sort key putting core tools first, then by name, so turns share the longest prompt prefix.
pub fn prefix_sort_key(tool_name: &str) -> (u8, &str) {
    // A user-added server ranks non-core: nothing guarantees it is there next turn.
    let extension = tool_name.split("__").next().unwrap_or("");
    let tier = match find_group(extension) {
        Some(group) if group.core => 0,
        _ => 1,
    };
    (tier, tool_name)
}

pub fn find_group(extension: &str) -> Option<&'static ToolGroup> {
    TOOL_GROUPS.iter().find(|g| g.extension == extension)
}

/// Groups an unidentified speaker must never be given. Subtract AFTER selection, since
/// `select_groups` puts core groups back; a denylist, so new groups pass.
pub fn groups_denied_to_guests() -> &'static [&'static str] {
    &[
        // Reads and deletes the household's long-term memory.
        "giap-memory",
        // Sensor history: when the house was empty, when somebody came home.
        "giap-sensors",
        // A member's connected sources. Scope refuses a guest anyway; not offering it saves a turn.
        "giap-context",
        // Not about personal data: `delegate` runs an autonomous agent on the household's one GPU.
        ORCHESTRATOR_EXTENSION,
    ]
}

/// Groups a subagent must never be given, however wide its parent. It has no identity or approval
/// path, so the tool set is the only boundary; subtract from the DERIVED set or core groups return.
pub fn groups_denied_to_subagents() -> &'static [&'static str] {
    &[
        // `enable_tool_group` widens; a child's grant is fixed up front by `narrow_child_groups`.
        TOOLKIT_EXTENSION,
        // Actuates the house, and a subagent has no approval path.
        "giap-device-control",
        // `send_notification` reaches the member directly with no approval path.
        "giap-system",
        // Schedules work that runs with household authority after the delegation ends.
        "giap-schedule",
        // Depth already refuses it; not offering it saves a small model's turn budget.
        ORCHESTRATOR_EXTENSION,
    ]
}

/// Extension names of the always-on core groups.
pub fn core_group_names() -> Vec<&'static str> {
    TOOL_GROUPS
        .iter()
        .filter(|g| g.core)
        .map(|g| g.extension)
        .collect()
}

/// The group of a prefixed tool name (`giap-weather__get_forecast` → `giap-weather`).
pub fn group_of_tool(tool_name: &str) -> Option<&str> {
    tool_name
        .find(TOOL_NAME_SEPARATOR)
        .map(|sep| &tool_name[..sep])
}

/// Whether `extension` is a catalog builtin; anything else is user-added and never narrowed.
pub fn is_catalog_extension(extension: &str) -> bool {
    find_group(extension).is_some()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_no_tools_switch_reads_presence_not_truthiness() {
        assert!(no_tools_from(Some("1")));
        assert!(no_tools_from(Some("yes")));
        assert!(no_tools_from(Some("maybe")));
    }

    #[test]
    fn the_no_tools_switch_is_off_when_unset_or_explicitly_off() {
        assert!(!no_tools_from(None));
        assert!(!no_tools_from(Some("")));
        assert!(!no_tools_from(Some("0")));
        assert!(!no_tools_from(Some("false")));
        assert!(!no_tools_from(Some("no")));
        // Whitespace is trimmed, so an env var set from a shell heredoc still reads.
        assert!(!no_tools_from(Some("  0  ")));
    }

    use super::*;

    #[test]
    fn catalog_has_no_duplicate_extensions() {
        let mut names: Vec<&str> = TOOL_GROUPS.iter().map(|g| g.extension).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate extension in TOOL_GROUPS");
    }

    /// Pinned so a change to the core set is deliberate.
    #[test]
    fn core_groups_are_exactly_the_documented_three() {
        let mut core = core_group_names();
        core.sort_unstable();
        assert_eq!(core, vec!["giap-memory", "giap-system", "giap-toolkit"]);
    }

    #[test]
    fn descriptions_are_substantive_enough_to_embed() {
        for g in TOOL_GROUPS {
            assert!(
                g.description.len() > 60,
                "{} has a description too short to score meaningfully",
                g.extension
            );
        }
    }

    #[test]
    fn tool_names_map_back_to_their_group() {
        assert_eq!(
            group_of_tool("giap-weather__get_forecast"),
            Some("giap-weather")
        );
        assert_eq!(group_of_tool("platform__manage_schedule"), Some("platform"));
        assert_eq!(group_of_tool("unprefixed"), None);
    }

    #[test]
    fn only_catalog_extensions_are_recognised() {
        assert!(!is_catalog_extension("some-user-mcp-server"));
    }
}

#[cfg(test)]
mod prefix_order_tests {
    use super::*;

    fn ordered(mut names: Vec<&str>) -> Vec<&str> {
        names.sort_by_key(|n| prefix_sort_key(n));
        names
    }

    /// The on-disk KV snapshot depends on this shared prefix.
    #[test]
    fn core_tools_come_before_the_ones_a_turn_might_not_have() {
        let got = ordered(vec![
            "giap-weather__get_current_weather",
            "giap-memory__recall_memories",
            "giap-toolkit__enable_tool_group",
        ]);
        let first_two: Vec<&str> = got.iter().take(2).copied().collect();
        assert_eq!(
            first_two,
            vec![
                "giap-memory__recall_memories",
                "giap-toolkit__enable_tool_group"
            ],
            "core groups must lead, or a turn that drops weather truncates the shared \
             prefix at the first tool"
        );
    }

    #[test]
    fn two_different_selections_agree_for_their_whole_core_block() {
        let a = ordered(vec![
            "giap-weather__get_current_weather",
            "giap-memory__recall_memories",
            "giap-toolkit__enable_tool_group",
        ]);
        let b = ordered(vec![
            "giap-knowledge__compute_answer",
            "giap-memory__recall_memories",
            "giap-toolkit__enable_tool_group",
        ]);
        let shared = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        assert_eq!(
            shared, 2,
            "the two core tools must be a common prefix of both selections; got {a:?} vs {b:?}"
        );
    }

    #[test]
    fn the_order_is_stable_whatever_order_the_selection_arrives_in() {
        let forward = ordered(vec![
            "giap-weather__get_current_weather",
            "giap-knowledge__compute_answer",
        ]);
        let backward = ordered(vec![
            "giap-knowledge__compute_answer",
            "giap-weather__get_current_weather",
        ]);
        assert_eq!(forward, backward);
    }

    #[test]
    fn an_unknown_extension_does_not_lead() {
        let got = ordered(vec![
            "some-user-server__do_thing",
            "giap-memory__recall_memories",
        ]);
        assert_eq!(got.first(), Some(&"giap-memory__recall_memories"));
    }
}

#[cfg(test)]
mod guest_denylist_tests {
    use super::*;

    #[test]
    fn every_denied_group_actually_exists() {
        for name in groups_denied_to_guests() {
            assert!(
                TOOL_GROUPS.iter().any(|g| g.extension == *name),
                "denylist names a group that does not exist: {name} -- a typo here \
                 silently grants a guest the access it was meant to deny"
            );
        }
    }

    #[test]
    fn the_denylist_covers_groups_that_are_otherwise_unremovable() {
        let core = core_group_names();
        let denied_core: Vec<_> = groups_denied_to_guests()
            .iter()
            .filter(|n| core.contains(n))
            .collect();
        assert!(
            !denied_core.is_empty(),
            "no denied group is core, so subtracting after selection is pointless"
        );
        assert!(
            denied_core.contains(&&"giap-memory"),
            "giap-memory is core and reads the household's memory; it must be denied"
        );
    }

    /// Denying everything would be a boundary nobody keeps switched on.
    #[test]
    fn a_guest_keeps_the_neutral_groups() {
        let denied = groups_denied_to_guests();
        for neutral in [
            "giap-weather",
            "giap-knowledge",
            "giap-device-control",
            "giap-toolkit",
            "giap-system",
        ] {
            assert!(
                !denied.contains(&neutral),
                "{neutral} carries no personal data and a guest should keep it"
            );
        }
    }

    /// A name that matches no group silently removes nothing.
    #[test]
    fn every_subagent_denied_group_actually_exists() {
        for name in groups_denied_to_subagents() {
            assert!(
                TOOL_GROUPS.iter().any(|g| g.extension == *name),
                "the subagent denylist names a group that does not exist: {name} -- a typo \
                 here silently hands a subagent the access it was meant to withhold"
            );
        }
    }

    #[test]
    fn the_subagent_denylist_covers_widening_and_actuating() {
        let denied = groups_denied_to_subagents();
        for required in [TOOLKIT_EXTENSION, "giap-device-control"] {
            assert!(
                denied.contains(&required),
                "{required} must be withheld from subagents; see the doc comment for the \
                 mechanism each one breaks"
            );
        }
    }

    /// Neither list backstops the other, and removing either entry breaks no other test.
    #[test]
    fn neither_a_guest_nor_a_subagent_is_offered_the_delegation_tool() {
        assert!(
            groups_denied_to_subagents().contains(&ORCHESTRATOR_EXTENSION),
            "{ORCHESTRATOR_EXTENSION} must be withheld from subagents: depth already refuses the \
             call, so offering the tool only spends a small model's turn budget discovering that"
        );
        assert!(
            groups_denied_to_guests().contains(&ORCHESTRATOR_EXTENSION),
            "{ORCHESTRATOR_EXTENSION} must be withheld from guests: delegating starts minutes of \
             unattended agent work on the household's own GPU, which an unidentified speaker has \
             no business commanding"
        );
    }

    /// Vacuity control: a denylist covering everything would amount to "no subagents".
    #[test]
    fn a_subagent_keeps_the_read_only_research_groups() {
        let denied = groups_denied_to_subagents();
        for kept in [
            "giap-weather",
            "giap-knowledge",
            "giap-sensors",
            "giap-device",
            "giap-memory",
        ] {
            assert!(
                is_catalog_extension(kept),
                "{kept} is not a group any more, so asserting it is not denied proves nothing"
            );
            assert!(
                !denied.contains(&kept),
                "{kept} reads rather than decides; denying it leaves subagents unable to do \
                 the one job they were built for"
            );
        }
    }
}
