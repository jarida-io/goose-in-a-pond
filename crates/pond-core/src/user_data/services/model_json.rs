//! Reading one JSON object out of a small model's reply: fenced, prefaced, or followed by prose.

use crate::models::services::thought_filter::strip_reasoning;

/// The first JSON object the reply holds, however the model wrapped it.
pub fn object_in(raw: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let cleaned = strip_reasoning(raw);
    let cleaned = cleaned.trim();
    let mut candidates: Vec<&str> = vec![cleaned];
    if let Some(start) = cleaned.find("```") {
        let after = &cleaned[start + 3..];
        let after = after.strip_prefix("json").unwrap_or(after);
        candidates.push(match after.find("```") {
            Some(end) => after[..end].trim(),
            None => after.trim(),
        });
    }
    if let (Some(a), Some(b)) = (cleaned.find('{'), cleaned.rfind('}')) {
        if b > a {
            candidates.push(&cleaned[a..=b]);
        }
    }
    candidates.into_iter().find_map(|candidate| {
        match serde_json::from_str::<serde_json::Value>(candidate) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            _ => None,
        }
    })
}

/// A string field, trimmed to one line; `None` when absent, empty or the schema's placeholder.
pub fn text_of(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    let value = map.get(key)?.as_str()?;
    let line = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!line.is_empty() && line != "..." && !line.starts_with("<")).then_some(line)
}

/// A list of strings, each trimmed to one line, placeholders and empties dropped.
pub fn texts_of(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    map.get(key)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(|item| item.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|item| !item.is_empty() && item != "...")
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shapes_a_small_model_answers_with_all_read() {
        for raw in [
            r#"{"a":"x"}"#,
            "```json\n{\"a\":\"x\"}\n```",
            "Here you go:\n{\"a\":\"x\"}\nHope that helps.",
            "<think>hmm</think>{\"a\":\"x\"}",
        ] {
            let map = object_in(raw).unwrap_or_else(|| panic!("unread: {raw}"));
            assert_eq!(text_of(&map, "a").as_deref(), Some("x"));
        }
        assert!(object_in("no json here").is_none());
        assert!(object_in("[1,2]").is_none());
    }

    #[test]
    fn placeholders_are_not_answers() {
        let map = object_in(r#"{"a":"...","b":["...","real"," "]}"#).unwrap();
        assert_eq!(text_of(&map, "a"), None);
        assert_eq!(texts_of(&map, "b"), vec!["real"]);
    }
}
