//! Tool-calling specialist engine — uses a small GGUF model to generate
//! structured tool-call arguments when the main LLM sends empty `{}`.
//!
//! The specialist sees ONLY the tool schema + user query (no conversation
//! history). This keeps inference fast (<100ms for a 270M model) and reliable.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use goose::model::ModelConfig;
use goose::providers::base::Provider as GooseProvider;
use goose::providers::local_inference::LocalInferenceProvider;
use goose::conversation::message::Message;
use pond_core::ports::tool_caller::ToolCaller;
use std::path::Path;
use std::sync::Arc;

/// In-process GGUF specialist for tool-call argument generation.
///
/// Loaded once at startup (~200MB for FunctionGemma 270M), kept resident
/// permanently. Uses the same `InferenceRuntime` singleton as the main model
/// but in a separate model slot.
pub struct ToolCallerEngine {
    provider: Arc<dyn GooseProvider>,
    model_config: ModelConfig,
    session_id: String,
}

impl ToolCallerEngine {
    /// Build the engine for the given GGUF model file.
    ///
    /// `model_id` is a filename stem (e.g. `"functiongemma-270m-q4_k_m"`) or
    /// a raw `.gguf` filename. The file must exist under `$data_dir/models/gguf/`.
    pub async fn new(model_id: &str, data_dir: &Path) -> Result<Self> {
        // Register the GGUF model in Goose's global registry (same pattern as
        // GooseAdapter::register_gguf_model in goose_agent.rs).
        register_tool_model(model_id, data_dir);

        let model_config = ModelConfig {
            model_name: normalise_model_id(model_id),
            temperature: Some(0.0), // deterministic output
            max_tokens: Some(256),  // tool calls are short
            ..Default::default()
        };

        println!("[tool_caller] loading specialist model: {}", model_id);
        let provider = LocalInferenceProvider::from_env(model_config.clone(), vec![]).await
            .map_err(|e| anyhow!("Failed to load tool-caller model '{}': {e}", model_id))?;
        println!("[tool_caller] specialist model loaded: {}", model_id);

        Ok(Self {
            provider: Arc::new(provider),
            model_config,
            session_id: "tool-caller-static".to_string(),
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
        use rmcp::model::Tool;

        let system = "You are a tool-calling assistant. Call the appropriate tool for the user's request.";

        // Build a proper rmcp Tool so the GGUF chat template renders it via
        // its native tool-calling path (Gemma's <tool_call> format, etc.).
        // Passing tools as freeform text causes FunctionGemma's Jinja template
        // to enter a degenerate recursive rendering loop.
        let tool: Tool = serde_json::from_value(serde_json::json!({
            "name": tool_name,
            "description": format!("Look up information about a topic using {}", tool_name),
            "inputSchema": serde_json::from_str::<serde_json::Value>(tool_schema_json)
                .unwrap_or_else(|_| serde_json::json!({"type": "object", "properties": {}})),
        })).unwrap_or_else(|_| {
            // Fallback: construct minimal tool manually
            serde_json::from_value(serde_json::json!({
                "name": tool_name,
                "inputSchema": {"type": "object", "properties": {"topic": {"type": "string"}}},
            })).expect("hardcoded tool schema must parse")
        });

        let prompt = format!("User request: {user_query}");
        println!("[tool_caller] generating args for tool={}, query={:?}", tool_name, user_query);

        let messages = vec![Message::user().with_text(&prompt)];
        let (response, _usage) = self.provider
            .complete(&self.model_config, &self.session_id, system, &messages, &[tool])
            .await
            .map_err(|e| anyhow!("Tool-caller inference failed: {e}"))?;

        let response_text = response.as_concat_text();
        println!("[tool_caller] raw response: {:?}", response_text);

        parse_tool_call_json(&response_text, tool_name)
    }
}

/// Parse the specialist model's output into a tool-call arguments map.
///
/// Handles multiple output formats:
/// - `{"name": "tool", "arguments": {"key": "value"}}`
/// - `{"key": "value"}` (bare arguments object)
/// - JSON embedded in markdown code fences
fn parse_tool_call_json(
    text: &str,
    tool_name: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    // Strip markdown code fences if present
    let cleaned = text
        .trim()
        .strip_prefix("```json").unwrap_or(text.trim())
        .strip_prefix("```").unwrap_or(text.trim())
        .strip_suffix("```").unwrap_or(text.trim())
        .trim();

    // Find the first '{' and last '}' to extract JSON even with surrounding text
    let start = cleaned.find('{');
    let end = cleaned.rfind('}');
    let json_str = match (start, end) {
        (Some(s), Some(e)) if s < e => &cleaned[s..=e],
        _ => return Err(anyhow!("No JSON object found in tool-caller response: {:?}", text)),
    };

    let value: serde_json::Value = serde_json::from_str(json_str)
        .map_err(|e| anyhow!("Failed to parse tool-caller JSON: {e}\nRaw: {:?}", json_str))?;

    // Case 1: {"name": "tool_name", "arguments": {...}}
    if let Some(args) = value.get("arguments") {
        if let Some(map) = args.as_object() {
            println!("[tool_caller] parsed arguments from 'arguments' field: {:?}", map);
            return Ok(map.clone());
        }
    }

    // Case 2: {"key": "value"} — bare arguments (no "name" wrapper)
    if let Some(map) = value.as_object() {
        // Filter out "name" if present (it's the tool name, not an argument)
        let args: serde_json::Map<String, serde_json::Value> = map.iter()
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
        tool_name, text
    ))
}

/// Register a GGUF model in Goose's global registry so LocalInferenceProvider can find it.
fn register_tool_model(model_id: &str, data_dir: &Path) {
    use goose::providers::local_inference::local_model_registry::{
        get_registry, LocalModelEntry, ModelSettings,
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
                    settings: ModelSettings::default(),
                    size_bytes: 0,
                };
                match registry.add_model(entry) {
                    Ok(_) => println!("[tool_caller] registered GGUF '{}' in model registry", stem),
                    Err(e) => tracing::warn!("Could not register tool-caller model '{}': {}", stem, e),
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
}
