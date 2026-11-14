//! How a failed tool call reads to the model. The engine drops `is_error`, so a failure has to say
//! so in its words: every one opens with [`NOT_DONE`], and a bad argument names the field, what
//! it takes and that the call can be made again.

use goose::conversation::message::{Message, MessageContent};
use rmcp::model::{CallToolResult, ContentBlock, JsonObject};
use serde_json::Value;

/// The opening of every failed call, which the prompt's tool rules name.
pub const NOT_DONE: &str = "Not done:";

const RMCP_ARGUMENT_ERROR: &str = "failed to deserialize parameters:";

/// `messages` with each failed tool result reworded, or `None` when none needed it. Pure, so a
/// turn re-sends the same words and the engine's cached prefix still matches.
pub(crate) fn reword(messages: &[Message]) -> Option<Vec<Message>> {
    let mut out: Option<Vec<Message>> = None;
    for (i, message) in messages.iter().enumerate() {
        for (j, content) in message.content.iter().enumerate() {
            let MessageContent::ToolResponse(response) = content else {
                continue;
            };
            let said = match &response.tool_result {
                Ok(result) if result.is_error == Some(true) => text_of(result),
                Ok(_) => continue,
                Err(e) => e.message.to_string(),
            };
            let (tool, args) = call_of(messages, &response.id);
            let words = failure_words(&tool, args.as_ref(), &said);
            let unchanged = matches!(&response.tool_result, Ok(r) if text_of(r) == words);
            if unchanged {
                continue;
            }
            let out = out.get_or_insert_with(|| messages.to_vec());
            if let MessageContent::ToolResponse(r) = &mut out[i].content[j] {
                let kept: Vec<ContentBlock> = match &r.tool_result {
                    Ok(result) => result
                        .content
                        .iter()
                        .filter(|b| b.as_text().is_none())
                        .cloned()
                        .collect(),
                    Err(_) => Vec::new(),
                };
                let mut content = vec![ContentBlock::text(words)];
                content.extend(kept);
                r.tool_result = Ok(CallToolResult::error(content));
            }
        }
    }
    out
}

/// What the model is told for a failed call to `tool` with `args`, whose own words were `said`.
pub(crate) fn failure_words(tool: &str, args: Option<&JsonObject>, said: &str) -> String {
    let said = said.trim();
    if said.starts_with(NOT_DONE) {
        return said.to_string();
    }
    let tool = tool.rsplit("__").next().unwrap_or(tool);
    if let Some(detail) = said.strip_prefix(RMCP_ARGUMENT_ERROR) {
        return argument_words(tool, args, detail.trim());
    }
    let said = said
        .strip_prefix("Error:")
        .or_else(|| said.strip_prefix("Tool error:"))
        .unwrap_or(said)
        .trim();
    format!("{NOT_DONE} {said}")
}

/// A serde error from reading `tool`'s arguments, as the field, what it takes and the fix.
fn argument_words(tool: &str, args: Option<&JsonObject>, detail: &str) -> String {
    let detail = detail
        .rsplit_once(" at line ")
        .map_or(detail, |(head, _)| head)
        .trim();
    let again = format!("Call {tool} again with that fixed.");
    if let Some(field) = between(detail, "missing field `", "`") {
        return format!("{NOT_DONE} {tool} needs `{field}`. {again}");
    }
    if let Some(rest) = detail.strip_prefix("unknown variant `") {
        if let Some((value, allowed)) = rest.split_once("`, expected one of ") {
            let field = field_holding(args, &Value::String(value.to_string()));
            let allowed = allowed.replace('`', "");
            return format!(
                "{NOT_DONE} {tool} has no \"{value}\" for `{field}`. It takes one of: {allowed}. \
                 {again}"
            );
        }
    }
    if let Some(rest) = detail.strip_prefix("invalid type: ") {
        if let Some((got, expected)) = rest.split_once(", expected ") {
            let sent = sent_value(got);
            let field = sent
                .as_ref()
                .map_or_else(|| "one argument".to_string(), |v| field_holding(args, v));
            let shown = sent.map_or_else(|| got.to_string(), |v| v.to_string());
            return format!(
                "{NOT_DONE} {tool} could not read `{field}` = {shown}: it takes {}. {again}",
                expected_words(expected)
            );
        }
    }
    format!(
        "{NOT_DONE} {tool} could not read its arguments ({detail}). Check each against the \
         tool's description. {again}"
    )
}

/// The JSON value serde's "invalid type" names: `string "half"`, `integer `5``, `boolean `true``.
fn sent_value(got: &str) -> Option<Value> {
    if let Some(s) = got
        .strip_prefix("string \"")
        .and_then(|s| s.strip_suffix('"'))
    {
        return Some(Value::String(s.to_string()));
    }
    let literal = got.split_once(" `")?.1.strip_suffix('`')?;
    serde_json::from_str(literal).ok()
}

/// The top-level argument that holds `value`, by name; "one argument" when none or several do.
fn field_holding(args: Option<&JsonObject>, value: &Value) -> String {
    let mut holders = args
        .into_iter()
        .flatten()
        .filter(|(_, v)| *v == value)
        .map(|(k, _)| k.clone());
    match (holders.next(), holders.next()) {
        (Some(only), None) => only,
        _ => "one argument".to_string(),
    }
}

/// Serde's expectation in words a model acts on.
fn expected_words(expected: &str) -> String {
    let e = expected.trim();
    let integer = [
        "u8", "u16", "u32", "u64", "usize", "i8", "i16", "i32", "i64", "isize",
    ];
    if integer.contains(&e) || e.contains("integer") {
        "a whole number in digits, like 10".to_string()
    } else if e == "f32" || e == "f64" || e.contains("float") || e.contains("number") {
        "a number in digits, like 2.5".to_string()
    } else if e.contains("bool") {
        "true or false".to_string()
    } else if e.contains("sequence") {
        "a list".to_string()
    } else if e.contains("string") {
        "words in quotes".to_string()
    } else {
        e.to_string()
    }
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let rest = &s[s.find(open)? + open.len()..];
    Some(&rest[..rest.find(close)?])
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|b| b.as_text().map(|t| t.text.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The tool and arguments of the call `id` answers, from the request earlier in `messages`.
fn call_of(messages: &[Message], id: &str) -> (String, Option<JsonObject>) {
    messages
        .iter()
        .flat_map(|m| m.content.iter())
        .find_map(|c| match c {
            MessageContent::ToolRequest(r) if r.id == id => r
                .tool_call
                .as_ref()
                .ok()
                .map(|call| (call.name.to_string(), call.arguments.clone())),
            _ => None,
        })
        .unwrap_or_else(|| ("the tool".to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::{CallToolRequestParams, ErrorCode, ErrorData};

    fn args(json: Value) -> JsonObject {
        json.as_object().cloned().unwrap()
    }

    fn turn(
        tool: &str,
        arguments: Value,
        result: Result<CallToolResult, ErrorData>,
    ) -> Vec<Message> {
        let call = CallToolRequestParams::new(tool.to_string()).with_arguments(args(arguments));
        vec![
            Message::assistant().with_tool_request("c1", Ok(call)),
            Message::user().with_tool_response("c1", result),
        ]
    }

    fn model_reads(messages: &[Message]) -> String {
        let MessageContent::ToolResponse(r) = &messages[1].content[0] else {
            panic!("a tool response")
        };
        text_of(r.tool_result.as_ref().unwrap())
    }

    #[test]
    fn a_word_in_a_number_field_names_the_field_and_what_it_takes() {
        let failed = CallToolResult::error(vec![ContentBlock::text(
            "failed to deserialize parameters: invalid type: string \"half\", expected u8",
        )]);
        let out = reword(&turn(
            "giap-device-control__set_device_state",
            serde_json::json!({"device": "the lamp", "brightness": "half"}),
            Ok(failed),
        ))
        .expect("reworded");
        assert_eq!(
            model_reads(&out),
            "Not done: set_device_state could not read `brightness` = \"half\": it takes a whole \
             number in digits, like 10. Call set_device_state again with that fixed."
        );
    }

    #[test]
    fn a_missing_field_and_an_unknown_choice_say_what_to_send() {
        assert_eq!(
            failure_words(
                "giap-system__send_notification",
                None,
                "failed to deserialize parameters: missing field `body` at line 1 column 20"
            ),
            "Not done: send_notification needs `body`. Call send_notification again with that \
             fixed."
        );
        let sent = args(serde_json::json!({"action": "stop"}));
        assert_eq!(
            failure_words(
                "giap-schedule__schedule_action",
                Some(&sent),
                "failed to deserialize parameters: unknown variant `stop`, expected one of \
                 `delete`, `pause`"
            ),
            "Not done: schedule_action has no \"stop\" for `action`. It takes one of: delete, \
             pause. Call schedule_action again with that fixed."
        );
    }

    #[test]
    fn a_tools_own_failure_opens_with_not_done_and_keeps_its_words() {
        assert_eq!(
            failure_words(
                "giap-weather__get_current_weather",
                None,
                "Weather fetch failed: timeout"
            ),
            "Not done: Weather fetch failed: timeout"
        );
        assert_eq!(
            failure_words("t", None, "Not done: already said"),
            "Not done: already said"
        );
    }

    #[test]
    fn a_protocol_error_becomes_a_failed_result_the_model_reads_once() {
        let err = ErrorData::new(ErrorCode::INVALID_PARAMS, "Unknown action: stop", None);
        let out = reword(&turn(
            "giap-schedule__schedule_action",
            serde_json::json!({}),
            Err(err),
        ))
        .expect("reworded");
        assert_eq!(model_reads(&out), "Not done: Unknown action: stop");
    }

    #[test]
    fn successes_and_already_readable_failures_are_left_alone() {
        let ok = CallToolResult::success(vec![ContentBlock::text("Timer set.")]);
        assert!(reword(&turn("t", serde_json::json!({}), Ok(ok))).is_none());
        let readable = CallToolResult::error(vec![ContentBlock::text("Not done: no place set.")]);
        assert!(reword(&turn("t", serde_json::json!({}), Ok(readable))).is_none());
    }

    #[test]
    fn rewording_twice_reads_the_same_so_the_cached_prefix_holds() {
        let failed = CallToolResult::error(vec![ContentBlock::text("No location is set.")]);
        let once = reword(&turn("t", serde_json::json!({}), Ok(failed))).unwrap();
        assert!(reword(&once).is_none());
    }
}
