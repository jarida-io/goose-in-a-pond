//! Formats pre-fetched tool output for the LLM, with a hint not to re-call the same MCP tool.

use pond_core::mcp::domain::tool_result::ToolResult;

/// Goose's MCP tool names for an internal tool, comma-separated; `""` when unmapped.
fn mcp_tool_names(tool: &str) -> &'static str {
    match tool {
        "weather" => "giap__get_current_weather",
        "wikipedia" => "giap__search_wikipedia, giap__get_wikipedia_article",
        "recall_memory" => "giap__recall_memories",
        "save_memory" => "giap__save_memory",
        "create_schedule" => "giap__create_schedule",
        "devices" => "giap__list_registered_devices",
        "schedules" => "giap__list_schedules",
        _ => "",
    }
}

fn dedup_hint(tool: &str) -> String {
    let mcp_names = mcp_tool_names(tool);
    if mcp_names.is_empty() {
        format!("(do not call the {} tool again for this query)", tool)
    } else {
        format!("(do not call {} again for this query)", mcp_names)
    }
}

/// Format one tool result as attributed context for the main LLM, with a dedup hint.
pub fn format_tool_context(tool: &str, query: &str, message: &str, info: &str) -> String {
    let hint = dedup_hint(tool);

    let query_line = if query.is_empty() {
        String::new()
    } else {
        format!("Query: {}\n", query)
    };

    match tool {
        // Action confirmations: relay directly.
        "create_schedule" | "save_memory" | "devices" | "schedules" => {
            format!(
                "{}\n\n\
                --- Pre-fetched result {} ---\n\
                Tool: {}\n\
                {}\
                {}\n\
                --- End pre-fetched result ---\n\n\
                Tell the user what happened. Be concise and natural.",
                message, hint, tool, query_line, info
            )
        }
        "weather" => {
            format!(
                "{}\n\n\
                --- Pre-fetched result {} ---\n\
                Tool: {}\n\
                {}\
                {}\n\
                --- End pre-fetched result ---\n\n\
                Report the weather naturally using the pre-fetched data above.",
                message, hint, tool, query_line, info
            )
        }
        "recall_memory" => {
            format!(
                "{}\n\n\
                --- Pre-fetched result {} ---\n\
                Tool: {}\n\
                {}\
                {}\n\
                --- End pre-fetched result ---\n\n\
                Answer using these memories. Be natural and personal.",
                message, hint, tool, query_line, info
            )
        }
        // Knowledge lookups: truncated to fit small-model context budgets.
        _ => {
            let truncated = if info.len() > 2000 {
                format!("{}...", &info[..2000])
            } else {
                info.to_string()
            };
            format!(
                "{}\n\n\
                --- Pre-fetched result {} ---\n\
                Tool: {}\n\
                {}\
                {}\n\
                --- End pre-fetched result ---\n\n\
                Answer the user's question using the pre-fetched data above. Include specific facts.",
                message, hint, tool, query_line, truncated
            )
        }
    }
}

/// Tool-failure notice, so the LLM answers from its own knowledge instead of assuming data.
pub fn format_tool_failure(tool: &str, query: &str, message: &str) -> String {
    let label = if query.is_empty() {
        format!("[Tool: {} | Status: no result]", tool)
    } else {
        format!("[Tool: {} | Query: {} | Status: no result]", tool, query)
    };

    format!(
        "{}\n\n{}\nThe {} tool did not return results for this query.\n\
        Answer from your own knowledge. If uncertain, say so.",
        message, label, tool
    )
}

/// Format several tool results, one labeled section each, with a combined dedup hint.
pub fn format_multi_tool_context(message: &str, results: &[ToolResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    // Same output as the single-tool path.
    if results.len() == 1 {
        let r = &results[0];
        return format_tool_context(&r.tool_name, "", message, &r.content);
    }

    let all_mcp_names: Vec<&str> = results
        .iter()
        .map(|r| mcp_tool_names(&r.tool_name))
        .filter(|n| !n.is_empty())
        .collect();
    let combined_hint = if all_mcp_names.is_empty() {
        String::from("(do not re-call these tools for this query)")
    } else {
        format!(
            "(do not call {} again for this query)",
            all_mcp_names.join(", ")
        )
    };

    let mut sections = String::new();
    for result in results {
        if !sections.is_empty() {
            sections.push('\n');
        }
        sections.push_str(&format!(
            "[Tool: {}]\n{}\n",
            result.tool_name, result.content
        ));
    }

    format!(
        "{}\n\n\
        --- Pre-fetched results {} ---\n\
        {}\
        --- End pre-fetched results ---\n\n\
        Answer the user's question using all the pre-fetched data above. \
        Address each part of their request. Be concise and natural.",
        message, combined_hint, sections
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_tool_names_known_tools() {
        assert_eq!(mcp_tool_names("weather"), "giap__get_current_weather");
        assert_eq!(
            mcp_tool_names("wikipedia"),
            "giap__search_wikipedia, giap__get_wikipedia_article"
        );
        assert_eq!(mcp_tool_names("recall_memory"), "giap__recall_memories");
        assert_eq!(mcp_tool_names("save_memory"), "giap__save_memory");
    }

    #[test]
    fn mcp_tool_names_unknown_tool_returns_empty() {
        assert_eq!(mcp_tool_names("some_custom_tool"), "");
    }

    #[test]
    fn dedup_hint_known_tool() {
        let hint = dedup_hint("weather");
        assert!(hint.contains("giap__get_current_weather"));
        assert!(!hint.contains("giap__get_forecast"));
        assert!(hint.contains("do not call"));
    }

    #[test]
    fn dedup_hint_unknown_tool_uses_raw_name() {
        let hint = dedup_hint("custom");
        assert!(hint.contains("custom"));
        assert!(hint.contains("do not call"));
    }

    #[test]
    fn format_tool_context_wikipedia_with_query() {
        let output = format_tool_context(
            "wikipedia",
            "John Cena",
            "who is John Cena?",
            "He is a wrestler.",
        );
        assert!(output.contains("Tool: wikipedia"));
        assert!(output.contains("Query: John Cena"));
        assert!(output.contains("He is a wrestler."));
        assert!(output.contains("who is John Cena?"));
        assert!(output.contains("Pre-fetched result"));
        assert!(output
            .contains("do not call giap__search_wikipedia, giap__get_wikipedia_article again"));
    }

    #[test]
    fn format_tool_context_weather_no_query() {
        let output = format_tool_context("weather", "", "what's the weather?", "Sunny, 25C");
        assert!(output.contains("Tool: weather"));
        assert!(!output.contains("Query:"));
        assert!(output.contains("Sunny, 25C"));
        assert!(output.contains("do not call giap__get_current_weather"));
    }

    #[test]
    fn format_tool_context_truncates_long_reference() {
        let long_info = "x".repeat(3000);
        let output = format_tool_context("wikipedia", "topic", "tell me about topic", &long_info);
        // Overhead includes message + pre-fetched block markers + dedup hint
        assert!(output.len() < 2400);
        assert!(output.contains("..."));
    }

    #[test]
    fn format_tool_failure_with_query() {
        let output = format_tool_failure("wikipedia", "nonexistent", "who is nonexistent?");
        assert!(output.contains("[Tool: wikipedia | Query: nonexistent | Status: no result]"));
        assert!(output.contains("did not return results"));
        assert!(output.contains("Answer from your own knowledge"));
    }

    #[test]
    fn format_tool_failure_without_query() {
        let output = format_tool_failure("weather", "", "what's the weather?");
        assert!(output.contains("[Tool: weather | Status: no result]"));
    }

    #[test]
    fn format_multi_empty() {
        let result = format_multi_tool_context("hello", &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn format_multi_single_delegates_to_single_formatter() {
        let results = vec![ToolResult::new("weather", "Sunny, 25C")];
        let output = format_multi_tool_context("what's the weather", &results);
        assert!(output.contains("Tool: weather"));
        assert!(output.contains("Sunny, 25C"));
        assert!(output.contains("Pre-fetched result"));
        assert!(output.contains("do not call giap__get_current_weather"));
    }

    #[test]
    fn format_multi_two_tools_produces_labeled_sections() {
        let results = vec![
            ToolResult::new("weather", "Sunny, 25C"),
            ToolResult::new("schedules", "- Briefing at 8am"),
        ];
        let output = format_multi_tool_context("weather and schedules", &results);
        assert!(output.contains("[Tool: weather]"));
        assert!(output.contains("Sunny, 25C"));
        assert!(output.contains("[Tool: schedules]"));
        assert!(output.contains("- Briefing at 8am"));
        assert!(output.contains("Address each part"));
        assert!(output.contains("Pre-fetched results"));
        assert!(output.contains("do not call"));
        assert!(output.contains("giap__get_current_weather"));
        assert!(output.contains("giap__list_schedules"));
    }

    #[test]
    fn format_multi_dedup_hint_combines_all_mcp_names() {
        let results = vec![
            ToolResult::new("weather", "data"),
            ToolResult::new("wikipedia", "data"),
        ];
        let output = format_multi_tool_context("question", &results);
        assert!(output.contains("giap__get_current_weather"));
        assert!(output.contains("giap__search_wikipedia"));
    }
}
