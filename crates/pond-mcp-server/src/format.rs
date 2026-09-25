//! Formatting for MCP tool results, each capped to a BYTE budget (see [`truncate_to_budget`]).

/// Truncate to a byte budget on a char boundary, appending a notice if cut. Bytes, not chars:
/// the budget bounds prompt tokens, and BPE spends more tokens per char on non-Latin script.
pub fn truncate_to_budget(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}...\n\n[Truncated — full content at source]",
        &text[..cut]
    )
}

/// Format items under a header within `max_bytes`; no items yields a [`format_no_results`] miss.
pub fn format_list_result(items: &[String], header: &str, max_bytes: usize) -> String {
    let mut result = if header.is_empty() {
        String::new()
    } else {
        format!("{}\n\n", header)
    };

    let mut wrote_item = false;
    for item in items {
        let line = format!("- {}\n", item);
        if result.len() + line.len() > max_bytes {
            result.push_str("...\n[More results available]");
            break;
        }
        result.push_str(&line);
        wrote_item = true;
    }

    // Decided by items, not `result`: a header alone would read to the model as a success.
    if !wrote_item {
        let what = if header.is_empty() {
            "this search".to_string()
        } else {
            header.trim_end().trim_end_matches(':').to_string()
        };
        return format_no_results(&what, &[]);
    }

    result.trim_end().to_string()
}

/// Format a miss that says it is not the answer and what to try, so the model chains instead
/// of apologising. `alternatives` must be real, FULLY-QUALIFIED tool names (a 2B model won't
/// map a bare suffix); `what` is a plain noun phrase, since advice in it beats the instruction.
pub fn format_no_results(what: &str, alternatives: &[&str]) -> String {
    if alternatives.is_empty() {
        return format!(
            "No results for {what}. This is NOT the answer. If another tool in your \
             schema can answer the question, call it now instead of replying."
        );
    }
    format!(
        "No results for {what}. This is NOT the answer — call {} now. \
         Only after those also return nothing may you tell the user you could not find it.",
        join_tool_names(alternatives)
    )
}

/// A miss where chaining would be wrong (e.g. an unstored personal fact), with the reason why.
pub fn format_dead_end(what: &str, why: &str) -> String {
    format!("No results: {what}. {why}")
}

/// Join tool names as "a", "a or b", "a, b, or c".
fn join_tool_names(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => (*only).to_string(),
        [a, b] => format!("{a} or {b}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

/// Signup guidance for a tool that CANNOT work without a key (else use [`format_degraded`]).
pub fn format_not_configured(feature: &str, signup_url: &str) -> String {
    format!(
        "{feature} requires an API key to work. Get one free at {signup_url} — \
         then add it in Settings under 'Knowledge & Discovery'."
    )
}

/// Append a note that `result` came from a keyless fallback. A fact, not advice, and placed
/// after the answer: a model told to relay setup advice tends to lead with it.
pub fn format_degraded(result: &str, what_is_missing: &str, signup_url: &str) -> String {
    format!(
        "{result}\n\n[Source note: this came from a free fallback because no \
         {what_is_missing} is configured. Better results are available with one — \
         free at {signup_url}, added in Settings. Mention this only if asked \
         about the source or the quality.]"
    )
}

/// Append [`format_degraded`]'s note to a successful result; one with no text is left untouched.
pub fn degrade_result(
    result: rmcp::model::CallToolResult,
    what_is_missing: &str,
    signup_url: &str,
) -> rmcp::model::CallToolResult {
    use rmcp::model::{Content, RawContent};
    let existing: String = result
        .content
        .iter()
        .filter_map(|c| match &c.raw {
            RawContent::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if existing.trim().is_empty() {
        return result;
    }
    rmcp::model::CallToolResult::success(vec![Content::text(format_degraded(
        &existing,
        what_is_missing,
        signup_url,
    ))])
}

/// API error as model-facing text (not ErrorData) that steers to another tool, not a retry.
pub fn format_api_error(service: &str, error: &str) -> String {
    format!(
        "{service} is temporarily unavailable: {error}. This did not answer the \
         question — if another tool in your schema can answer it, call that tool \
         now rather than retrying this one."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_within_budget_unchanged() {
        let text = "short text";
        assert_eq!(truncate_to_budget(text, 100), text);
    }

    #[test]
    fn truncate_over_budget_cuts_and_appends_notice() {
        let text = "Hello, world! This is a long string.";
        let result = truncate_to_budget(text, 13);
        assert!(result.starts_with("Hello, world!"));
        assert!(result.contains("[Truncated"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let text = "caf\u{00e9} au lait";
        let result = truncate_to_budget(text, 5);
        assert!(result.contains("[Truncated"));
    }

    #[test]
    fn the_budget_is_counted_in_bytes_not_characters() {
        let cyrillic = "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440}";
        assert_eq!(cyrillic.chars().count(), 10, "fixture is not 10 chars");
        assert_eq!(cyrillic.len(), 19, "fixture is not 19 bytes");

        assert_eq!(truncate_to_budget(cyrillic, 19), cyrillic);

        // 12 sits between the 10 chars and 19 bytes, so only byte counting cuts.
        let cut = truncate_to_budget(cyrillic, 12);
        assert!(
            cut.contains("[Truncated"),
            "a 19-byte string survived a 12-byte budget, so the budget is being \
             counted in characters: {cut:?}"
        );

        assert!(
            cut.starts_with("\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}"),
            "cut mid-character: {cut:?}"
        );
    }

    #[test]
    fn format_list_with_header() {
        let items = vec!["Item 1".into(), "Item 2".into(), "Item 3".into()];
        let result = format_list_result(&items, "Results:", 200);
        assert!(result.starts_with("Results:"));
        assert!(result.contains("- Item 1"));
        assert!(result.contains("- Item 3"));
    }

    #[test]
    fn format_list_truncates_when_over_budget() {
        let items: Vec<String> = (0..100).map(|i| format!("Item number {}", i)).collect();
        let result = format_list_result(&items, "Results:", 100);
        assert!(result.contains("[More results available]"));
    }

    #[test]
    fn format_list_empty_returns_no_results() {
        let items: Vec<String> = vec![];
        let result = format_list_result(&items, "", 200);
        assert!(result.starts_with("No results for this search."));
        assert!(result.contains("NOT the answer"));
    }

    #[test]
    fn format_list_with_header_and_no_items_is_a_miss_not_a_header() {
        let items: Vec<String> = vec![];
        let result = format_list_result(&items, "Guardian search: \"kenya election\":", 200);
        assert!(result.starts_with("No results for"), "got: {result}");
        assert!(result.contains("Guardian search"));
        assert!(!result.ends_with("election\":"));
    }

    #[test]
    fn no_results_without_alternatives_still_licenses_a_different_tool() {
        let msg = format_no_results("books for 'ubuntu'", &[]);
        assert!(msg.contains("NOT the answer"));
        assert!(msg.contains("another tool in your schema"));
    }

    #[test]
    fn no_results_names_the_alternatives_it_was_given() {
        let msg = format_no_results(
            "Wikipedia articles for 'x'",
            &[
                "giap-knowledge__search_wikipedia",
                "giap-knowledge__get_wikipedia_article",
            ],
        );
        assert!(
            msg.contains(
                "call giap-knowledge__search_wikipedia or giap-knowledge__get_wikipedia_article now"
            ),
            "got: {msg}"
        );
        assert!(
            msg.contains("Only after those also return nothing"),
            "got: {msg}"
        );
    }

    #[test]
    fn join_tool_names_reads_naturally_at_each_arity() {
        assert_eq!(join_tool_names(&["a"]), "a");
        assert_eq!(join_tool_names(&["a", "b"]), "a or b");
        assert_eq!(join_tool_names(&["a", "b", "c"]), "a, b, or c");
    }

    #[test]
    fn no_server_is_spawned_outside_the_supervised_helper() {
        let mut offenders: Vec<String> = Vec::new();
        let mut served = 0usize;

        for (name, src) in TOOL_SOURCES {
            let code = strip_line_comments(src);
            if code.contains("crate::serve_builtin(") {
                served += 1;
            }
            // The shape that discards the exit.
            if code.contains("running.waiting()") {
                offenders.push((*name).to_string());
            }
            if code.contains("tokio::spawn(") && !code.contains("crate::serve_builtin(") {
                offenders.push(format!("{name} (raw tokio::spawn)"));
            }
        }

        assert!(
            served >= 9,
            "only {served} servers go through serve_builtin — the scan is looking \
             for the wrong name, so an empty offender list proves nothing"
        );
        assert!(
            offenders.is_empty(),
            "these spawn a server without supervision, so its death is silent: {offenders:?}"
        );
    }

    #[test]
    fn every_suggested_alternative_is_fully_qualified() {
        let mut checked = 0;
        for (name, src) in suggestion_sources() {
            for quoted in suggestions_in(src) {
                assert!(
                    quoted.starts_with("giap-") && quoted.contains("__"),
                    "{name}: suggested alternative '{quoted}' is not a \
                     fully-qualified schema name; the model cannot call it"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "the scan matched nothing — parser is broken");
    }

    /// Every server that can report a miss belongs here; one left out is unchecked.
    fn suggestion_sources() -> &'static [(&'static str, &'static str)] {
        &[
            ("knowledge.rs", include_str!("knowledge.rs")),
            ("wolfram.rs", include_str!("wolfram.rs")),
            ("memory.rs", include_str!("memory.rs")),
            ("sensors.rs", include_str!("sensors.rs")),
        ]
    }

    /// Quoted `format_no_results` alternatives; matched delimiters keep it out of the next call.
    fn suggestions_in(src: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, _) in src.match_indices("format_no_results(") {
            let args = balanced(src, i + "format_no_results(".len(), '(', ')');
            let Some(open) = args.find("&[") else {
                continue;
            };
            let slice = balanced(&args, open + 2, '[', ']');
            out.extend(slice.split('"').skip(1).step_by(2).map(str::to_string));
        }
        out
    }

    /// Every MCP server source, keyed by extension; one list shared by the tool inventory and
    /// the supervision guard. `wolfram.rs` is a second router composed onto `giap-knowledge`.
    const TOOL_SOURCES: &[(&str, &str)] = &[
        ("giap-context", include_str!("context.rs")),
        ("giap-device", include_str!("device.rs")),
        ("giap-device-control", include_str!("device_control.rs")),
        ("giap-knowledge", include_str!("knowledge.rs")),
        ("giap-knowledge", include_str!("wolfram.rs")),
        ("giap-memory", include_str!("memory.rs")),
        ("giap-orchestrator", include_str!("orchestrator.rs")),
        ("giap-schedule", include_str!("schedule.rs")),
        ("giap-sensors", include_str!("sensors.rs")),
        ("giap-system", include_str!("system.rs")),
        ("giap-toolkit", include_str!("toolkit.rs")),
        ("giap-weather", include_str!("weather.rs")),
    ];

    /// Every registered `giap-*` tool as `extension__tool`, parsed from source (building the
    /// servers needs goose). Line comments are stripped: a disabled tool's doc quotes `#[tool]`.
    fn registered_tools() -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for (ext, src) in TOOL_SOURCES {
            let code = strip_line_comments(src);
            for (i, _) in code.match_indices("#[tool(") {
                if let Some(name) = next_fn_ident(&code[i + "#[tool(".len()..]) {
                    out.insert(format!("{ext}__{name}"));
                }
            }
        }
        out
    }

    fn strip_line_comments(src: &str) -> String {
        src.lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Name of the first `fn <ident>(` in `s`; a `#[tool]` attribute directly precedes its fn.
    fn next_fn_ident(s: &str) -> Option<String> {
        let mut idx = 0;
        while let Some(rel) = s[idx..].find("fn ") {
            let at = idx + rel;
            let boundary = s[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if boundary {
                let after = &s[at + 3..];
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() && after[name.len()..].trim_start().starts_with('(') {
                    return Some(name);
                }
            }
            idx = at + 3;
        }
        None
    }

    #[test]
    fn the_tool_inventory_parser_sees_the_tools_that_are_there() {
        let tools = registered_tools();
        assert!(
            tools.contains("giap-weather__get_current_weather"),
            "{tools:?}"
        );
        // A composed second router: the case a file-name-to-extension mapping gets wrong.
        assert!(
            tools.contains("giap-knowledge__compute_answer"),
            "{tools:?}"
        );
        assert_eq!(
            tools.len(),
            32,
            "the tool inventory changed. Update this count in the same commit as \
             the tool — the number is the record, and a stale one is how a count \
             read 64 for months while the real number was 65. This assertion has \
             been wrong twice itself: once written as 65 on a branch 17 commits \
             behind main, and once pointing at an AGENTS.md that had since been \
             taken out of the tree entirely (b971f6ab), which is why it no longer \
             names a file to go and edit."
        );
    }

    #[test]
    fn every_suggested_alternative_is_a_tool_that_exists() {
        let tools = registered_tools();
        let mut checked = 0;
        for (name, src) in suggestion_sources() {
            for quoted in suggestions_in(src) {
                assert!(
                    tools.contains(&quoted),
                    "{name}: suggests '{quoted}', which no server registers. \
                     Either the tool was removed or renamed and this call site \
                     was missed, or the name is a typo the model will burn a \
                     retry on."
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "the scan matched nothing — parser is broken");
    }

    /// Text from `start` up to the delimiter closing the one opened before `start`.
    fn balanced(s: &str, start: usize, open: char, close: char) -> &str {
        let mut depth = 1usize;
        for (offset, ch) in s[start..].char_indices() {
            if ch == open {
                depth += 1;
            } else if ch == close {
                depth -= 1;
                if depth == 0 {
                    return &s[start..start + offset];
                }
            }
        }
        &s[start..]
    }

    #[test]
    fn dead_end_states_the_reason_and_names_no_tool() {
        let msg = format_dead_end("memories about the user", "Nothing is stored about this.");
        assert!(msg.contains("Nothing is stored"));
        assert!(!msg.contains("call "));
    }

    #[test]
    fn api_error_steers_to_a_different_tool_not_a_retry() {
        let msg = format_api_error("Finnhub", "connection timeout");
        assert!(msg.contains("another tool in your schema"));
        assert!(!msg.contains("Try again in a moment"));
    }

    #[test]
    fn a_degraded_result_keeps_its_answer_and_names_what_is_missing() {
        let out = format_degraded(
            "Top story: Kenya election",
            "Guardian API key",
            "https://x.test",
        );
        assert!(
            out.starts_with("Top story: Kenya election"),
            "the answer must come first — a note that displaces the result is a \
             regression, not a warning: {out}"
        );
        assert!(out.contains("Guardian API key"), "{out}");
        assert!(out.contains("https://x.test"), "{out}");
        assert!(
            !out.contains("requires an API key to work"),
            "a degraded result must not claim the tool is non-functional: {out}"
        );
    }

    #[test]
    fn degrading_an_empty_result_changes_nothing() {
        use rmcp::model::CallToolResult;
        let empty = CallToolResult::success(vec![]);
        let out = degrade_result(empty, "Some key", "https://x.test");
        assert!(
            out.content.is_empty(),
            "a note was hung on a result with no body, turning 'no answer' into \
             'an answer plus advice'"
        );
    }

    #[test]
    fn format_not_configured_includes_url() {
        let msg = format_not_configured("News search", "https://open-platform.theguardian.com");
        assert!(msg.contains("News search"));
        assert!(msg.contains("open-platform.theguardian.com"));
        assert!(msg.contains("API key"));
    }

    #[test]
    fn format_api_error_includes_service_name() {
        let msg = format_api_error("Finnhub", "connection timeout");
        assert!(msg.contains("Finnhub"));
        assert!(msg.contains("connection timeout"));
    }
}
