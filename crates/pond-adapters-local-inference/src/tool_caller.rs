//! A small GGUF specialist that fills in tool-call arguments when the main LLM sends `{}`. It
//! sees only the tool schema and user query, never history, to stay under 100 ms at 270M.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::conversation::message::Message;
use goose::providers::base::Provider as GooseProvider;
use goose::providers::local_inference::LocalInferenceProvider;
use goose_providers::model::ModelConfig;
use pond_core::mcp::ports::tools::tool_caller::ToolCaller;
use std::path::Path;
use std::sync::Arc;

/// Kept resident (~200 MB at 270M) in its own slot of the shared `InferenceRuntime`.
pub struct ToolCallerEngine {
    provider: Arc<dyn GooseProvider>,
    model_config: ModelConfig,
}

impl ToolCallerEngine {
    /// `model_id` is the stem or `.gguf` filename of a file under `$data_dir/models/gguf/`.
    pub async fn new(model_id: &str, data_dir: &Path) -> Result<Self> {
        register_tool_model(model_id, data_dir);

        let mut model_config = ModelConfig::new(normalise_model_id(model_id));
        model_config.temperature = Some(0.0);
        model_config.max_tokens = Some(256); // tool calls are short

        println!("[tool_caller] loading specialist model: {}", model_id);
        goose::providers::local_inference::configure_local_inference();
        let provider = LocalInferenceProvider::from_env()
            .await
            .map_err(|e| anyhow!("Failed to load tool-caller model '{}': {e}", model_id))?;
        println!("[tool_caller] specialist model loaded: {}", model_id);

        Ok(Self {
            provider: Arc::new(provider),
            model_config,
        })
    }
}

#[async_trait]
impl ToolCaller for ToolCallerEngine {
    async fn generate_tool_call(
        &self,
        tool_name: &str,
        tool_schema_json: &str,
        user_query: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>> {
        // FunctionGemma's native prompt, bypassing Goose's templates. Ref:
        // https://ai.google.dev/gemma/docs/functiongemma/formatting-and-best-practices

        let declaration = build_functiongemma_declaration(tool_name, tool_schema_json);

        // FunctionGemma was trained on this exact activation phrase + inline declarations.
        let system = format!(
            "You are a model that can do function calling with the following functions{}",
            declaration
        );

        println!("[tool_caller] ┌─────────────────────────────────────");
        println!("[tool_caller] │ tool:   {}", tool_name);
        println!("[tool_caller] │ query:  {:?}", user_query);
        println!(
            "[tool_caller] │ system: {}...({} chars)",
            &system[..system.len().min(120)],
            system.len()
        );
        println!("[tool_caller] └─────────────────────────────────────");

        let messages = vec![Message::user().with_text(user_query)];
        let (response, _usage) = self
            .provider
            .complete(
                &self.model_config,
                &system,
                &messages,
                &[], // No tools — declarations are in the system prompt
            )
            .await
            .map_err(|e| anyhow!("Tool-caller inference failed: {e}"))?;

        let response_text = response.as_concat_text();
        println!(
            "[tool_caller] raw response ({}): {:?}",
            response_text.len(),
            &response_text[..response_text.len().min(300)]
        );

        if let Some(args) = parse_functiongemma_call(&response_text) {
            println!(
                "[tool_caller] parsed FunctionGemma call: {:?}",
                args.keys().collect::<Vec<_>>()
            );
            return Ok(args);
        }
        if let Ok(args) = parse_tool_call_json(&response_text, tool_name) {
            println!(
                "[tool_caller] parsed JSON: {:?}",
                args.keys().collect::<Vec<_>>()
            );
            return Ok(args);
        }

        Err(anyhow!(
            "Tool-caller produced no usable output for '{}': {:?}",
            tool_name,
            &response_text[..response_text.len().min(200)]
        ))
    }
}

/// FunctionGemma `<start_function_declaration>` text for a JSON schema, one line, no spaces.
fn build_functiongemma_declaration(tool_name: &str, schema_json: &str) -> String {
    let schema: serde_json::Value =
        serde_json::from_str(schema_json).unwrap_or_else(|_| serde_json::json!({}));

    let mut decl = String::new();
    decl.push_str("<start_function_declaration>declaration:");
    decl.push_str(tool_name);
    decl.push('{');

    // Description — derive from properties
    let properties = schema.get("properties").and_then(|p| p.as_object());
    if let Some(props) = properties {
        let desc: String = props
            .iter()
            .map(|(k, v)| {
                let d = v.get("description").and_then(|d| d.as_str()).unwrap_or(k);
                format!("{}: {}", k, d)
            })
            .collect::<Vec<_>>()
            .join(", ");
        decl.push_str("description:<escape>");
        decl.push_str(&desc);
        decl.push_str("<escape>,");
    }

    decl.push_str("parameters:{properties:{");
    if let Some(props) = properties {
        let prop_strs: Vec<String> = props
            .iter()
            .map(|(name, spec)| {
                let desc = spec
                    .get("description")
                    .and_then(|d| d.as_str())
                    .unwrap_or(name);
                let typ = spec
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("string")
                    .to_uppercase();
                format!(
                    "{}:{{description:<escape>{}<escape>,type:<escape>{}<escape>}}",
                    name, desc, typ
                )
            })
            .collect();
        decl.push_str(&prop_strs.join(","));
    }
    decl.push_str("},");

    let required = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| format!("<escape>{}<escape>", s))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default();
    decl.push_str("required:[");
    decl.push_str(&required);
    decl.push_str("],type:<escape>OBJECT<escape>}");

    decl.push('}');
    decl.push_str("<end_function_declaration>");
    decl
}

/// Parse `<start_function_call>call:NAME{k:<escape>str<escape>,n:42}<end_function_call>`.
fn parse_functiongemma_call(text: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let call_start = text.find("<start_function_call>call:")?;
    let after_tag = &text[call_start + "<start_function_call>call:".len()..];
    let brace_pos = after_tag.find('{')?;
    let args_start = brace_pos + 1;
    let end_tag_pos = after_tag
        .find("<end_function_call>")
        .unwrap_or(after_tag.len());
    let args_end = after_tag[..end_tag_pos].rfind('}')?;
    let args_block = &after_tag[args_start..args_end];

    if args_block.trim().is_empty() {
        return None;
    }

    let mut map = serde_json::Map::new();
    let mut remaining = args_block;

    while !remaining.is_empty() {
        remaining = remaining.trim_start_matches([',', ' ']);
        if remaining.is_empty() {
            break;
        }
        let colon = match remaining.find(':') {
            Some(p) => p,
            None => break,
        };
        let key = remaining[..colon].trim().to_string();
        remaining = &remaining[colon + 1..];

        let value = if remaining.starts_with("<escape>") {
            remaining = &remaining["<escape>".len()..];
            match remaining.find("<escape>") {
                Some(end) => {
                    let val = remaining[..end].to_string();
                    remaining = &remaining[end + "<escape>".len()..];
                    serde_json::Value::String(val)
                }
                None => {
                    let val = remaining.to_string();
                    remaining = "";
                    serde_json::Value::String(val)
                }
            }
        } else {
            let end = remaining.find(',').unwrap_or(remaining.len());
            let raw = remaining[..end].trim();
            remaining = &remaining[end..];
            if let Ok(n) = raw.parse::<i64>() {
                serde_json::Value::Number(n.into())
            } else if raw.eq_ignore_ascii_case("true") {
                serde_json::Value::Bool(true)
            } else if raw.eq_ignore_ascii_case("false") {
                serde_json::Value::Bool(false)
            } else {
                serde_json::Value::String(raw.to_string())
            }
        };

        if !key.is_empty() {
            map.insert(key, value);
        }
    }

    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// JSON fallback: `{"name", "arguments"}` or a bare arguments object, fenced or not.
fn parse_tool_call_json(
    text: &str,
    tool_name: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let cleaned = text
        .trim()
        .strip_prefix("```json")
        .unwrap_or(text.trim())
        .strip_prefix("```")
        .unwrap_or(text.trim())
        .strip_suffix("```")
        .unwrap_or(text.trim())
        .trim();

    // Tolerate prose around the JSON.
    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    let json_str = match (start, end) {
        (Some(s), Some(e)) if s < e => &cleaned[s..=e],
        _ => {
            return Err(anyhow!(
                "No JSON object found in tool-caller response: {:?}",
                text
            ))
        }
    };

    let value: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow!("Failed to parse tool-caller JSON: {e}\nRaw: {:?}", json_str))?;

    // Case 1: {"name": "tool_name", "arguments": {...}}
    if let Some(args) = value.get("arguments") {
        if let Some(map) = args.as_object() {
            println!(
                "[tool_caller] parsed arguments from 'arguments' field: {:?}",
                map
            );
            return Ok(map.clone());
        }
    }

    // Case 2: {"key": "value"} — bare arguments (no "name" wrapper)
    if let Some(map) = value.as_object() {
        // Filter out "name" if present (it's the tool name, not an argument)
        let args: serde_json::Map<String, serde_json::Value> = map
            .iter()
            .filter(|(k, _)| k.as_str() != "name")
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !args.is_empty() {
            println!("[tool_caller] parsed bare arguments: {:?}", args);
            return Ok(args);
        }
    }

    Err(anyhow!(
        "Tool-caller produced no usable arguments for '{}': {:?}",
        tool_name,
        text
    ))
}

/// Register a GGUF model in Goose's global registry so LocalInferenceProvider can find it.
fn register_tool_model(model_id: &str, data_dir: &Path) {
    use goose::providers::local_inference::local_model_registry::{
        get_registry, LocalModelEntry, LocalModelStorage, ModelSettings, ToolCallingMode,
    };

    let gguf_dir = data_dir.join("models").join("gguf");
    let (stem, filename) = if model_id.ends_with(".gguf") {
        let s = model_id.trim_end_matches(".gguf").to_string();
        (s, model_id.to_string())
    } else {
        (model_id.to_string(), format!("{}.gguf", model_id))
    };

    let local_path = gguf_dir.join(&filename);
    if !local_path.exists() {
        tracing::warn!(
            "Tool-caller GGUF not found at {} — will fail on first call",
            local_path.display()
        );
    }

    match get_registry().lock() {
        Ok(mut registry) => {
            if !registry.has_model(&stem) {
                let entry = LocalModelEntry {
                    id: stem.clone(),
                    repo_id: format!("local/{}", stem),
                    filename,
                    quantization: String::new(),
                    local_path,
                    source_url: String::new(),
                    backend_id: None,
                    storage: LocalModelStorage::ManualPath,
                    settings: ModelSettings {
                        // Affects only response handling; the prompt carries the tools.
                        tool_calling: ToolCallingMode::ForceNative,
                        // Dynamic context from available memory.
                        context_size: None,
                        ..ModelSettings::default()
                    },
                    size_bytes: 0,
                    mmproj_path: None,
                    mmproj_source_url: None,
                    mmproj_size_bytes: 0,
                    mmproj_checked: false,
                    shard_files: vec![],
                };
                match registry.add_model(entry) {
                    Ok(_) => println!("[tool_caller] registered GGUF '{}' in model registry", stem),
                    Err(e) => {
                        tracing::warn!("Could not register tool-caller model '{}': {}", stem, e)
                    }
                }
            }
        }
        Err(e) => tracing::warn!("GGUF registry lock poisoned: {}", e),
    }
}

/// Normalise a model ID to the registry stem format.
fn normalise_model_id(model_id: &str) -> String {
    model_id.trim_end_matches(".gguf").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_format() {
        let input = r#"{"name": "get_wikipedia_article", "arguments": {"topic": "Kenya"}}"#;
        let args = parse_tool_call_json(input, "get_wikipedia_article").unwrap();
        assert_eq!(args.get("topic").unwrap().as_str().unwrap(), "Kenya");
    }

    #[test]
    fn parse_bare_arguments() {
        let input = r#"{"topic": "black holes"}"#;
        let args = parse_tool_call_json(input, "get_wikipedia_article").unwrap();
        assert_eq!(args.get("topic").unwrap().as_str().unwrap(), "black holes");
    }

    #[test]
    fn parse_with_code_fence() {
        let input = "```json\n{\"name\": \"search_wikipedia\", \"arguments\": {\"topic\": \"volcanoes\"}}\n```";
        let args = parse_tool_call_json(input, "search_wikipedia").unwrap();
        assert_eq!(args.get("topic").unwrap().as_str().unwrap(), "volcanoes");
    }

    #[test]
    fn parse_with_surrounding_text() {
        let input = "Here is the tool call: {\"topic\": \"Einstein\"} as requested.";
        let args = parse_tool_call_json(input, "get_wikipedia_article").unwrap();
        assert_eq!(args.get("topic").unwrap().as_str().unwrap(), "Einstein");
    }

    #[test]
    fn parse_empty_json_returns_error() {
        let input = "{}";
        assert!(parse_tool_call_json(input, "test").is_err());
    }

    #[test]
    fn parse_no_json_returns_error() {
        let input = "I don't know how to call tools.";
        assert!(parse_tool_call_json(input, "test").is_err());
    }

    #[test]
    fn build_declaration_wikipedia() {
        let schema = r#"{"type":"object","properties":{"topic":{"type":"string","description":"The topic to look up"}},"required":["topic"]}"#;
        let decl = build_functiongemma_declaration("get_wikipedia_article", schema);
        assert!(decl.starts_with("<start_function_declaration>declaration:get_wikipedia_article{"));
        assert!(decl.ends_with("<end_function_declaration>"));
        assert!(decl.contains(
            "topic:{description:<escape>The topic to look up<escape>,type:<escape>STRING<escape>}"
        ));
        assert!(decl.contains("required:[<escape>topic<escape>]"));
    }

    #[test]
    fn build_declaration_schedule() {
        let schema = r#"{"type":"object","properties":{"cron":{"type":"string","description":"6-field cron"},"prompt":{"type":"string","description":"Action to perform"}},"required":["cron","prompt"]}"#;
        let decl = build_functiongemma_declaration("create_schedule", schema);
        assert!(decl.contains(
            "cron:{description:<escape>6-field cron<escape>,type:<escape>STRING<escape>}"
        ));
        assert!(decl.contains(
            "prompt:{description:<escape>Action to perform<escape>,type:<escape>STRING<escape>}"
        ));
        assert!(decl.contains("required:[<escape>cron<escape>,<escape>prompt<escape>]"));
    }
}
