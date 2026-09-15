//! Native tool-calling support: prompt construction and output parsing.
//!
//! Handles three tool-call output formats emitted by different model families:
//!
//! 1. **Standard JSON**: `{"tool_calls": [{"type": "function", "function": {...}}]}`
//! 2. **XML / Qwen-style**: `<tool_call><function=NAME><parameter=K>V</parameter></function></tool_call>`
//! 3. **Llama3 / Gemma4**: `<|tool_call|>call:NAME{...}<tool_call|>` or `<|tool_call>call:NAME{...}<tool_call|>`

use pond_core::models::ports::inference::ToolDefinition;
use serde_json::Value;

/// A tool call parsed from model output.
#[derive(Debug, Clone)]
pub(crate) struct ParsedToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

// ── Prompt construction ──────────────────────────────────────────────────────

/// Convert GIAP [`ToolDefinition`]s to OpenAI-compatible JSON for chat templates.
///
/// Returns the JSON string suitable for `OpenAIChatTemplateParams::tools_json`.
pub(crate) fn tools_to_json(tools: &[ToolDefinition]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }

    let specs: Vec<Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters_schema,
                }
            })
        })
        .collect();

    serde_json::to_string(&specs).ok()
}

/// Build a compact tools JSON (name + description only, no parameter schemas).
///
/// Used as a fallback when the full schema exceeds the token budget.
pub(crate) fn compact_tools_json(tools: &[ToolDefinition]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }

    let specs: Vec<Value> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                }
            })
        })
        .collect();

    serde_json::to_string(&specs).ok()
}

// ── Output parsing ───────────────────────────────────────────────────────────

/// Parse tool calls from the model's generated text.
///
/// Tries each format in order:
/// 1. Llama3/Gemma4 XML (`<|tool_call|>` or `<|tool_call>`)
/// 2. Qwen/generic XML (`<tool_call>`)
/// 3. Standard JSON (`{"tool_calls": [...]}`)
///
/// Returns an empty vec if no tool calls are found.
pub(crate) fn parse_tool_calls(output: &str) -> Vec<ParsedToolCall> {
    // 1. Llama3 / Gemma4 format
    if let Some((_content, calls)) = split_llama3_tool_calls(output) {
        return calls
            .into_iter()
            .map(|(name, args)| ParsedToolCall {
                name,
                arguments: Value::Object(args),
            })
            .collect();
    }

    // 2. Qwen / generic XML format
    if let Some((_content, calls)) = split_xml_tool_calls(output) {
        return calls
            .into_iter()
            .map(|(name, args)| ParsedToolCall {
                name,
                arguments: Value::Object(args),
            })
            .collect();
    }

    // 3. Standard JSON format
    if let Some(json_str) = extract_json_tool_calls(output) {
        return parse_json_tool_calls(&json_str);
    }

    Vec::new()
}

/// Return the byte offset up to which the generated text is safe to stream.
///
/// Everything before the last unmatched top-level `{` or any incomplete
/// tool-call tag is safe. This prevents streaming partial tool-call JSON
/// or XML to the client.
pub(crate) fn safe_stream_end(text: &str) -> usize {
    // Hold back from the start of any tool_call tag.
    let xml_hold = text.find("<tool_call>").unwrap_or(text.len());
    let llama3_hold = text.find("<|tool_call|>").unwrap_or(text.len());
    let gemma4_hold = text.find("<|tool_call>").unwrap_or(text.len());

    let bytes = text.as_bytes();
    let mut safe_end = bytes.len();
    let mut depth = 0i32;

    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'{' => {
                if depth == 0 {
                    safe_end = i;
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    safe_end = i + 1;
                }
            }
            _ => {
                if depth == 0 {
                    safe_end = i + 1;
                }
            }
        }
    }

    // Hold back partial tag prefixes at the tail.
    let tags: &[&[u8]] = &[b"<tool_call>", b"<|tool_call|>", b"<|tool_call>"];
    let mut tail_hold = safe_end;
    for tag in tags {
        let check_len = tag.len().min(bytes.len());
        for start in (safe_end.saturating_sub(check_len))..safe_end {
            let tail = &bytes[start..safe_end];
            if tag.starts_with(tail) {
                tail_hold = tail_hold.min(start);
                break;
            }
        }
    }

    safe_end
        .min(xml_hold)
        .min(llama3_hold)
        .min(gemma4_hold)
        .min(tail_hold)
}

// ── Format 1: Standard JSON ─────────────────────────────────────────────────

/// Extract the JSON tool_calls object from the end of the text.
#[allow(clippy::string_slice)]
fn extract_json_tool_calls(text: &str) -> Option<String> {
    let trimmed = text.trim_end();
    if !trimmed.ends_with('}') {
        return None;
    }

    // Scan backwards for the matching '{'.
    let bytes = trimmed.as_bytes();
    let mut depth = 0i32;
    let mut json_start = None;
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' => {
                depth -= 1;
                if depth == 0 {
                    json_start = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }

    let start = json_start?;
    let json_str = &trimmed[start..];

    // Validate it contains tool_calls.
    let parsed: Value = serde_json::from_str(json_str).ok()?;
    parsed.get("tool_calls")?.as_array()?;

    Some(json_str.to_string())
}

/// Split text into (content, tool_calls_json).
#[allow(clippy::string_slice)]

/// Parse tool calls from a JSON string containing `"tool_calls"` array.
fn parse_json_tool_calls(json_str: &str) -> Vec<ParsedToolCall> {
    let parsed: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let Some(tool_calls) = parsed.get("tool_calls").and_then(|v| v.as_array()) else {
        return vec![];
    };

    let mut results = Vec::new();
    for tc in tool_calls {
        // Try OpenAI format: {"function": {"name": ..., "arguments": ...}}
        // Then native format: {"name": ..., "arguments": {...}}
        let (name, arguments) = if let Some(func) = tc.get("function") {
            let n = func
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = func
                .get("arguments")
                .and_then(|v| {
                    // Arguments may be a string (OAI) or object (native).
                    if let Some(s) = v.as_str() {
                        serde_json::from_str(s).ok()
                    } else {
                        Some(v.clone())
                    }
                })
                .unwrap_or(Value::Object(serde_json::Map::new()));
            (n, args)
        } else {
            let n = tc
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = if let Some(obj) = tc.get("arguments") {
                if let Some(s) = obj.as_str() {
                    serde_json::from_str(s).unwrap_or(Value::Object(serde_json::Map::new()))
                } else {
                    obj.clone()
                }
            } else {
                Value::Object(serde_json::Map::new())
            };
            (n, args)
        };

        if !name.is_empty() {
            results.push(ParsedToolCall { name, arguments });
        }
    }

    results
}

// ── Format 2: Qwen / generic XML ────────────────────────────────────────────

#[allow(clippy::type_complexity)]
fn split_xml_tool_calls(
    text: &str,
) -> Option<(String, Vec<(String, serde_json::Map<String, Value>)>)> {
    let (content, first_block_and_rest) = text.split_once("<tool_call>")?;
    let content = content.trim_end().to_string();
    let mut tool_calls = Vec::new();

    let mut remaining = first_block_and_rest;
    loop {
        let (block, after_close) = remaining
            .split_once("</tool_call>")
            .unwrap_or((remaining, ""));

        if let Some(tc) =
            parse_xml_function_format(block).or_else(|| parse_xml_arg_key_value_format(block))
        {
            tool_calls.push(tc);
        }

        match after_close.split_once("<tool_call>") {
            Some((_, next)) => remaining = next,
            None => break,
        }
    }

    if tool_calls.is_empty() {
        None
    } else {
        Some((content, tool_calls))
    }
}

fn parse_xml_function_format(block: &str) -> Option<(String, serde_json::Map<String, Value>)> {
    let (_, after_func_eq) = block.split_once("<function=")?;
    let (func_name, func_body) = after_func_eq.split_once('>')?;
    let func_name = func_name.trim().to_string();

    let mut args = serde_json::Map::new();
    let mut rest = func_body;

    while let Some((_, after_param_eq)) = rest.split_once("<parameter=") {
        let Some((param_name, after_name_close)) = after_param_eq.split_once('>') else {
            break;
        };
        let param_name = param_name.trim().to_string();

        let (value, after_value) = after_name_close
            .split_once("</parameter>")
            .unwrap_or((after_name_close, ""));
        let value = value.trim();

        let json_value =
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        args.insert(param_name, json_value);

        rest = after_value;
    }

    Some((func_name, args))
}

/// Parse GLM-style: `NAME<arg_key>K</arg_key><arg_value>V</arg_value>...`
fn parse_xml_arg_key_value_format(block: &str) -> Option<(String, serde_json::Map<String, Value>)> {
    let func_name_end = block.find("<arg_key>").unwrap_or(block.len());
    #[allow(clippy::string_slice)]
    let func_name = block[..func_name_end].trim().to_string();
    if func_name.is_empty() {
        return None;
    }

    let mut args = serde_json::Map::new();
    #[allow(clippy::string_slice)]
    let mut rest = &block[func_name_end..];

    while let Some((_, after_key_open)) = rest.split_once("<arg_key>") {
        let Some((key, after_key_close)) = after_key_open.split_once("</arg_key>") else {
            break;
        };
        let key = key.trim().to_string();

        let Some((_, after_val_open)) = after_key_close.split_once("<arg_value>") else {
            break;
        };
        let (value, after_val_close) = after_val_open
            .split_once("</arg_value>")
            .unwrap_or((after_val_open, ""));
        let value = value.trim();

        let json_value =
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_string()));
        args.insert(key, json_value);

        rest = after_val_close;
    }

    Some((func_name, args))
}

// ── Format 3: Llama3 / Gemma4 ───────────────────────────────────────────────

/// Convert Gemma 4's native tool-call argument format to valid JSON.
///
/// Gemma 4 emits: `{key:<|"|>value<|"|>,key2:<|"|>value2<|"|>}`
/// This needs to become: `{"key":"value","key2":"value2"}`
///
/// The format uses `<|"|>` as string delimiters (instead of `"`) and
/// keys are unquoted identifiers.
fn gemma4_args_to_json(raw: &str) -> String {
    // If it already looks like valid JSON (starts with {"), try as-is first.
    let trimmed = raw.trim();
    if trimmed.starts_with("{\"") || trimmed == "{}" {
        return trimmed.to_string();
    }

    // Replace <|"|> with " (Gemma 4's string delimiter escape)
    let with_quotes = trimmed.replace("<|\"", "\"").replace("\"|>", "\"");

    // Now we have: {key:"value",key2:"value2"}
    // Need to quote the keys: {"key":"value","key2":"value2"}
    let mut result = String::with_capacity(with_quotes.len() + 20);
    let mut chars = with_quotes.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '{' || ch == ',' {
            result.push(ch);
            // Skip whitespace after { or ,
            while chars.peek() == Some(&' ') {
                chars.next();
            }
            // Read the key (unquoted identifier until : or ")
            if chars.peek() == Some(&'"') {
                // Key is already quoted — pass through
            } else if chars.peek() == Some(&'}') {
                // Empty object
            } else {
                // Unquoted key — collect and wrap in quotes
                result.push('"');
                while let Some(&next) = chars.peek() {
                    if next == ':' {
                        break;
                    }
                    result.push(chars.next().unwrap());
                }
                result.push('"');
            }
        } else {
            result.push(ch);
        }
    }

    result
}

#[allow(clippy::type_complexity)]
fn split_llama3_tool_calls(
    text: &str,
) -> Option<(String, Vec<(String, serde_json::Map<String, Value>)>)> {
    // Prefer Llama 3 format (<|tool_call|>); fall back to Gemma 4 (<|tool_call>).
    let tag_open = if text.contains("<|tool_call|>") {
        "<|tool_call|>"
    } else {
        "<|tool_call>"
    };

    let (content, first_block_and_rest) = text.split_once(tag_open)?;
    let content = content.trim_end().to_string();
    let mut tool_calls = Vec::new();

    let mut remaining = first_block_and_rest;
    loop {
        let close_tag = if remaining.contains("<tool_call|>") {
            "<tool_call|>"
        } else if remaining.contains("</tool_call>") {
            "</tool_call>"
        } else {
            "<|eot_id|>"
        };

        let (block, after_close) = remaining.split_once(close_tag).unwrap_or((remaining, ""));
        let block = block.trim();

        // `call:FUNC_NAME{...}` format.
        if let Some(after_call) = block.strip_prefix("call:") {
            if let Some(brace_idx) = after_call.find('{') {
                #[allow(clippy::string_slice)]
                let func_name = after_call[..brace_idx].trim().to_string();
                #[allow(clippy::string_slice)]
                let raw_args = &after_call[brace_idx..];

                // Convert Gemma 4 native format to JSON, then parse.
                let json_str = gemma4_args_to_json(raw_args);
                let args: serde_json::Map<String, Value> = serde_json::from_str(&json_str)
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            raw = %raw_args,
                            normalized = %json_str,
                            error = %e,
                            "failed to parse tool call arguments"
                        );
                        serde_json::Map::new()
                    });
                tool_calls.push((func_name, args));
            }
        } else {
            // Plain JSON: {"name": "...", "arguments": {...}}
            if let Ok(Value::Object(map)) = serde_json::from_str(block) {
                if let (Some(Value::String(name)), Some(Value::Object(args))) =
                    (map.get("name"), map.get("arguments"))
                {
                    tool_calls.push((name.to_string(), args.clone()));
                }
            }
        }

        match after_close.split_once(tag_open) {
            Some((_, next)) => remaining = next,
            None => break,
        }
    }

    if tool_calls.is_empty() {
        None
    } else {
        Some((content, tool_calls))
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_tool_calls_openai_format() {
        let text = r#"Here is the result.
{"tool_calls": [{"function": {"name": "get_weather", "arguments": "{\"location\":\"London\"}"}, "id": "abc"}]}"#;
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments["location"], "London");
    }

    #[test]
    fn parse_json_tool_calls_native_format() {
        let text =
            r#"{"tool_calls": [{"name": "shell", "arguments": {"command": "ls"}, "id": "x"}]}"#;
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
    }

    #[test]
    fn parse_xml_tool_call_single() {
        let text = "I'll search for that.\n\n<tool_call>\n<function=search__files>\n<parameter=pattern>local.*inference</parameter>\n</function>\n</tool_call>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search__files");
        assert_eq!(calls[0].arguments["pattern"], "local.*inference");
    }

    #[test]
    fn parse_xml_tool_call_multiple() {
        let text = "Doing two things:\n<tool_call>\n<function=foo__bar>\n<parameter=x>1</parameter>\n</function>\n</tool_call>\n<tool_call>\n<function=baz__qux>\n<parameter=y>hello</parameter>\n</function>\n</tool_call>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "foo__bar");
        assert_eq!(calls[1].name, "baz__qux");
    }

    #[test]
    fn parse_gemma4_tool_call() {
        let text =
            "Sure!\n<|tool_call>call:giap__get_weather{\"location\":\"Nairobi\"}<tool_call|>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "giap__get_weather");
        assert_eq!(calls[0].arguments["location"], "Nairobi");
    }

    #[test]
    fn parse_gemma4_native_escape_format() {
        // Gemma 4 uses <|"|> as string delimiters and unquoted keys
        let text = "<|tool_call>call:giap-weather__get_current_weather{location:<|\"|\x3eAthens<|\"\x7c>}<tool_call|>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1, "should parse 1 tool call");
        assert_eq!(calls[0].name, "giap-weather__get_current_weather");
        assert_eq!(calls[0].arguments["location"], "Athens");
    }

    #[test]
    fn parse_gemma4_native_multiple_params() {
        let text = "<|tool_call>call:giap-finance__convert_currency{amount:<|\"|\x3e100<|\"\x7c>,from:<|\"|\x3eUSD<|\"\x7c>,to:<|\"|\x3eKES<|\"\x7c>}<tool_call|>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "giap-finance__convert_currency");
        assert_eq!(calls[0].arguments["from"], "USD");
        assert_eq!(calls[0].arguments["to"], "KES");
    }

    #[test]
    fn gemma4_args_to_json_basic() {
        // {location:<|"|>Athens<|"|>} → {"location":"Athens"}
        let raw = "{location:<|\"|>Athens<|\"|>}";
        let json = super::gemma4_args_to_json(raw);
        let parsed: serde_json::Map<String, Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["location"], "Athens");
    }

    #[test]
    fn gemma4_args_to_json_multiple() {
        let raw = "{from:<|\"|>USD<|\"|>,to:<|\"|>KES<|\"|>}";
        let json = super::gemma4_args_to_json(raw);
        let parsed: serde_json::Map<String, Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["from"], "USD");
        assert_eq!(parsed["to"], "KES");
    }

    #[test]
    fn gemma4_args_to_json_passthrough_valid_json() {
        let raw = r#"{"location":"Nairobi"}"#;
        let json = super::gemma4_args_to_json(raw);
        let parsed: serde_json::Map<String, Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["location"], "Nairobi");
    }

    #[test]
    fn gemma4_args_to_json_empty() {
        assert_eq!(super::gemma4_args_to_json("{}"), "{}");
    }

    #[test]
    fn parse_llama3_tool_call() {
        let text = "<|tool_call|>call:giap__get_weather{\"location\":\"London\"}<tool_call|>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "giap__get_weather");
        assert_eq!(calls[0].arguments["location"], "London");
    }

    #[test]
    fn parse_no_tool_calls() {
        let text = "Just regular text, no tools.";
        let calls = parse_tool_calls(text);
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_glm_style_tool_call() {
        let text = "<tool_call>developer__shell<arg_key>command</arg_key><arg_value>ls -la</arg_value></tool_call>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "developer__shell");
        assert_eq!(calls[0].arguments["command"], "ls -la");
    }

    #[test]
    fn safe_stream_end_holds_back_unmatched_brace() {
        let text = "Some text {\"tool_calls\": [";
        assert_eq!(safe_stream_end(text), "Some text ".len());
    }

    #[test]
    fn safe_stream_end_balanced_braces() {
        let text = "Result: {\"key\": \"value\"} done";
        assert_eq!(safe_stream_end(text), text.len());
    }

    #[test]
    fn safe_stream_end_holds_back_tool_call_tag() {
        let text = "Some text before <tool_call>\n<function=foo>";
        let safe = safe_stream_end(text);
        assert!(safe <= text.find("<tool_call>").unwrap());
    }

    #[test]
    fn safe_stream_end_empty() {
        assert_eq!(safe_stream_end(""), 0);
    }

    #[test]
    fn safe_stream_end_plain_text() {
        let text = "plain text here";
        assert_eq!(safe_stream_end(text), text.len());
    }

    #[test]
    fn extract_content_with_json_tool() {
        let text = "Here is the result.\n{\"tool_calls\": [{\"name\": \"x\", \"arguments\": {}}]}";
        let content = extract_content(text);
        assert_eq!(content, "Here is the result.");
    }

    #[test]
    fn extract_content_no_tools() {
        let text = "Just a normal response.";
        assert_eq!(extract_content(text), text);
    }

    #[test]
    fn extract_content_with_xml_tool() {
        let text = "Answer:\n<tool_call>\n<function=foo>\n<parameter=x>1</parameter>\n</function>\n</tool_call>";
        let content = extract_content(text);
        assert_eq!(content, "Answer:");
    }

    #[test]
    fn tools_to_json_empty() {
        assert!(tools_to_json(&[]).is_none());
    }

    #[test]
    fn tools_to_json_roundtrip() {
        let tools = vec![ToolDefinition {
            name: "get_weather".to_string(),
            description: "Get current weather".to_string(),
            parameters_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                }
            }),
        }];
        let json = tools_to_json(&tools).unwrap();
        let parsed: Vec<Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["function"]["name"], "get_weather");
        assert!(parsed[0]["function"]["parameters"].is_object());
    }

    // ── Malformed-input robustness ────────────────────────────────────────────

    #[test]
    fn malformed_json_tool_call_returns_empty_vec() {
        // Completely broken JSON — must not panic, must return nothing.
        let calls = parse_tool_calls("{not valid json at all!!!");
        assert!(calls.is_empty());
    }

    #[test]
    fn malformed_arguments_string_in_openai_format_returns_empty_args() {
        // Valid wrapper but arguments field is not parseable JSON (no braces — avoids
        // confusing the depth scanner in extract_json_tool_calls).
        let text = r#"{"tool_calls": [{"function": {"name": "shell", "arguments": "not valid json at all"}}]}"#;
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(calls[0].arguments.is_object());
        assert!(calls[0].arguments.as_object().unwrap().is_empty());
    }

    #[test]
    fn brace_heavy_malformed_arguments_does_not_panic() {
        // Arguments containing extra `{` confuse the depth scanner so extraction
        // may fail entirely — the important thing is no panic and no crash.
        let text =
            r#"{"tool_calls": [{"function": {"name": "shell", "arguments": "{{bad json"}}]}"#;
        let calls = parse_tool_calls(text);
        // Either 0 calls (extraction failed) or 1 call with empty args — never a panic.
        assert!(calls.len() <= 1);
        if let Some(call) = calls.first() {
            assert!(call.arguments.is_object());
        }
    }

    #[test]
    fn malformed_llama3_args_returns_empty_args() {
        // Llama3 format with unparseable arg JSON — must log + return empty map.
        let text = "<|tool_call|>call:giap__get_weather{NOT VALID JSON}<tool_call|>";
        let calls = parse_tool_calls(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "giap__get_weather");
        assert!(calls[0].arguments.is_object());
        assert!(calls[0].arguments.as_object().unwrap().is_empty());
    }

    #[test]
    fn missing_tool_calls_key_returns_empty_vec() {
        // JSON object present but no tool_calls field — must return nothing.
        let calls = parse_tool_calls(r#"{"result": "ok"}"#);
        assert!(calls.is_empty());
    }

    #[test]
    fn compact_tools_json_omits_params() {
        let tools = vec![ToolDefinition {
            name: "shell".to_string(),
            description: "Run commands".to_string(),
            parameters_schema: serde_json::json!({"type": "object"}),
        }];
        let json = compact_tools_json(&tools).unwrap();
        let parsed: Vec<Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["function"]["name"], "shell");
        assert!(parsed[0]["function"].get("parameters").is_none());
    }

    /// Diagnostic test: prints the full tools JSON as it would be passed to the
    /// Jinja chat template. Run with `cargo test -p pond-inference -- --nocapture render_tools_json`
    /// to see the exact JSON the model receives for tool definitions.
    #[test]
    fn render_tools_json_for_inspection() {
        let tools = vec![
            ToolDefinition {
                name: "giap-weather__get_current_weather".to_string(),
                description: "Get current weather conditions for any city. Pass a location name or omit for default.".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "location": {
                            "type": "string",
                            "description": "City name (e.g. 'Nairobi', 'London'). Omit for home location."
                        }
                    }
                }),
            },
            ToolDefinition {
                name: "giap-knowledge__get_wikipedia_article".to_string(),
                description: "Look up factual, encyclopedic information about any topic.".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "topic": {
                            "type": "string",
                            "description": "The person, place, event, or concept to look up."
                        }
                    },
                    "required": ["topic"]
                }),
            },
            ToolDefinition {
                name: "giap-memory__save_memory".to_string(),
                description: "Save information the user wants remembered.".to_string(),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "The fact, preference, or note to save."
                        },
                        "segment": {
                            "type": "string",
                            "description": "Category: identity, preference, correction, relationship, project, knowledge, context."
                        }
                    },
                    "required": ["content"]
                }),
            },
        ];

        let full_json = tools_to_json(&tools).unwrap();
        let compact_json = compact_tools_json(&tools).unwrap();

        println!("\n=== FULL tools_json (passed to Jinja template) ===");
        let pretty: Value = serde_json::from_str(&full_json).unwrap();
        println!("{}", serde_json::to_string_pretty(&pretty).unwrap());

        println!("\n=== COMPACT tools_json (fallback — NO schemas) ===");
        let pretty: Value = serde_json::from_str(&compact_json).unwrap();
        println!("{}", serde_json::to_string_pretty(&pretty).unwrap());

        // Verify full JSON includes parameter schemas
        let parsed: Vec<Value> = serde_json::from_str(&full_json).unwrap();
        for tool in &parsed {
            let params = &tool["function"]["parameters"];
            assert!(
                params.is_object(),
                "tool '{}' missing parameters schema!",
                tool["function"]["name"]
            );
            assert!(
                params.get("properties").is_some(),
                "tool '{}' has no properties!",
                tool["function"]["name"]
            );
        }
    }
}
