//! The tool block as the model sees it (tools, order, sizes), pinned: it is the KV prompt
//! prefix, so any move costs a full re-prefill. Sizes are the RAW pre-minify router form,
//! since this crate must not depend on `pond-adapters-goose`; don't quote them as prompt cost.

use pond_core::mcp::domain::tool_group::prefix_sort_key;
use rmcp::model::Tool;

/// Every tool GIAP can offer, from the real routers, unordered (see [`ordered_tools`]).
pub fn all_tools() -> Vec<(&'static str, Tool)> {
    let mut out: Vec<(&'static str, Tool)> = Vec::new();
    let mut push = |ext: &'static str, tools: Vec<Tool>| {
        for t in tools {
            out.push((ext, t));
        }
    };

    push("giap-memory", crate::memory::MemoryMcpServer::tool_defs());
    push(
        "giap-weather",
        crate::weather::WeatherMcpServer::tool_defs(),
    );
    push("giap-system", crate::system::SystemMcpServer::tool_defs());
    push(
        "giap-schedule",
        crate::schedule::ScheduleMcpServer::tool_defs(),
    );
    push("giap-device", crate::device::DeviceMcpServer::tool_defs());
    push(
        "giap-device-control",
        crate::device_control::DeviceControlMcpServer::tool_defs(),
    );
    push(
        "giap-sensors",
        crate::sensors::SensorsMcpServer::tool_defs(),
    );
    push(
        "giap-knowledge",
        crate::knowledge::KnowledgeMcpServer::tool_defs(),
    );
    push(
        "giap-toolkit",
        crate::toolkit::ToolkitMcpServer::tool_defs(),
    );
    push(
        "giap-context",
        crate::context::ContextMcpServer::tool_defs(),
    );
    push(
        "giap-orchestrator",
        crate::orchestrator::OrchestratorMcpServer::tool_defs(),
    );

    out
}

/// One entry of the pinned prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixEntry {
    /// Prefixed name, as the model sees it.
    pub name: String,
    /// Serialized JSON length of the whole tool object.
    pub bytes: usize,
}

/// `groups`' tools in the order the provider shim sends them (`prefix_sort_key`, then name).
pub fn ordered_tools(groups: &[&str]) -> Vec<PrefixEntry> {
    let mut kept: Vec<(String, usize)> = all_tools()
        .into_iter()
        .filter(|(ext, _)| groups.contains(ext))
        .map(|(ext, t)| {
            let name = format!("{ext}__{}", t.name);
            let bytes = serde_json::to_string(&t).map(|s| s.len()).unwrap_or(0);
            (name, bytes)
        })
        .collect();

    kept.sort_by(|a, b| prefix_sort_key(&a.0).cmp(&prefix_sort_key(&b.0)));

    kept.into_iter()
        .map(|(name, bytes)| PrefixEntry { name, bytes })
        .collect()
}

pub fn total_bytes(entries: &[PrefixEntry]) -> usize {
    entries.iter().map(|e| e.bytes).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Groups registered by default; `giap-context` and `giap-orchestrator` are off.
    const DEFAULT_GROUPS: &[&str] = &[
        "giap-memory",
        "giap-weather",
        "giap-system",
        "giap-schedule",
        "giap-device",
        "giap-device-control",
        "giap-sensors",
        "giap-knowledge",
        "giap-toolkit",
    ];

    /// The fixture. Regenerate deliberately: a change means the next cold turn re-prefills.
    #[test]
    fn the_default_tool_prefix_is_unchanged() {
        let entries = ordered_tools(DEFAULT_GROUPS);
        let rendered: Vec<String> = entries
            .iter()
            .map(|e| format!("{} {}", e.bytes, e.name))
            .collect();

        // Core groups first (`prefix_sort_key` tier 0), then by name.
        let expected_head = [
            "giap-memory__forget_memory",
            "giap-memory__recall_memories",
            "giap-memory__save_memory",
            "giap-system__get_current_time",
            "giap-system__get_system_info",
            "giap-system__send_notification",
            "giap-toolkit__enable_tool_group",
            "giap-toolkit__list_tool_groups",
        ];
        let head: Vec<&str> = entries
            .iter()
            .take(expected_head.len())
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            head,
            expected_head,
            "the core block moved. Two conversations share the preamble only up \
             to their first difference, so anything that reorders this costs \
             every warm turn its KV prefix.\nfull order:\n{}",
            rendered.join("\n")
        );

        // One number for the whole block: a reworded description passes, a bigger one fails.
        let total = total_bytes(&entries);
        assert_eq!(
            total,
            14_478,
            "the default tool block is now {total} raw bytes, was 14,478. That is \
             the PRE-minification form (see the module docs); the shim ships \
             about 7.7% less. If the change is deliberate, update this number and \
             say why in the commit.\nfull order:\n{}",
            rendered.join("\n")
        );

        assert_eq!(entries.len(), 27, "tool count moved");
    }

    #[test]
    fn dropping_a_non_core_group_leaves_the_core_block_byte_identical() {
        let full = ordered_tools(DEFAULT_GROUPS);
        let narrowed: Vec<&str> = DEFAULT_GROUPS
            .iter()
            .copied()
            .filter(|g| *g != "giap-weather")
            .collect();
        let narrow = ordered_tools(&narrowed);

        let shared = narrow
            .iter()
            .zip(full.iter())
            .take_while(|(a, b)| a == b)
            .count();

        let core_len = full
            .iter()
            .take_while(|e| prefix_sort_key(&e.name).0 == 0)
            .count();
        assert!(core_len > 0, "no core tools — the sort key stopped working");
        assert!(
            shared >= core_len,
            "narrowing moved the prefix at entry {shared}, inside the core block \
             of {core_len}. Core-first ordering is what makes a narrowed \
             conversation reuse a wide one's KV cache"
        );
    }

    #[test]
    fn the_oracle_reads_real_tools_not_an_empty_list() {
        let all = all_tools();
        assert!(
            all.len() >= 27,
            "only {} tools enumerated — a router stopped being reachable and \
             every assertion above would pass by being empty",
            all.len()
        );
        assert!(
            all.iter().all(|(_, t)| !t.name.is_empty()),
            "a tool came back unnamed"
        );
    }
}
