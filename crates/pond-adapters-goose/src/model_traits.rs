//! Active-model traits read from its GGUF (local providers; HTTP ones use the name heuristic).
//! Answers must be cheap and stable across turns, or `prefix_hash` moves and forces a re-prefill.

use pond_core::models::domain::model_probe::{probe_cached, ModelProbe};
use std::path::Path;

/// Providers whose "model" is a GGUF on this machine.
fn is_local_gguf(provider: &str) -> bool {
    matches!(provider, "local" | "gguf")
}

/// The active model's probe. `None` = no readable file, not "no": fall back to the name heuristic.
#[must_use]
pub fn probe_for_model(
    provider: &str,
    model_name: &str,
    data_dir: Option<&Path>,
) -> Option<ModelProbe> {
    if !is_local_gguf(provider) {
        return None;
    }
    let gguf_dir = data_dir?.join("models").join("gguf");
    let filename = crate::goose_agent::resolve_gguf_filename(model_name, &gguf_dir);
    let path = gguf_dir.join(filename);
    let probe = probe_cached(&path);
    announce_once(model_name, probe.as_ref());
    probe
}

/// Log each model's classification once per process (this runs every turn).
fn announce_once(model_name: &str, probe: Option<&ModelProbe>) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first_time = match seen.lock() {
        Ok(mut set) => set.insert(model_name.to_string()),
        Err(_) => false,
    };
    if !first_time {
        return;
    }

    match probe {
        Some(p) => tracing::info!(
            model = model_name,
            tools = ?p.tools,
            thinking = ?p.thinking,
            marker = ?p.thinking_marker(),
            architecture = ?p.architecture,
            trained_context = ?p.context_window_tokens,
            "read this model's own account of itself"
        ),
        None => tracing::debug!(
            model = model_name,
            "no readable GGUF for this model; falling back to the name heuristic"
        ),
    }
}

/// Whether the model reasons, for `thinking_mode = "auto"`. `Always` counts too: it emits a
/// reasoning block regardless, which the prompt and filter must handle.
#[must_use]
pub fn model_reasons(provider: &str, model_name: &str, data_dir: Option<&Path>) -> bool {
    match probe_for_model(provider, model_name, data_dir) {
        Some(probe) => probe.thinking_is_selectable() || probe.thinking_marker().is_some(),
        None => {
            pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                model_name,
            )
            .thinking
        }
    }
}

/// The template's reasoning marker; NOT necessarily the output tag to strip (Gemma 4's
/// `<|think|>` is a prompt switch, its output uses `<|channel>thought`).
#[must_use]
pub fn thinking_marker(
    provider: &str,
    model_name: &str,
    data_dir: Option<&Path>,
) -> Option<String> {
    probe_for_model(provider, model_name, data_dir)
        .and_then(|p| p.thinking_marker().map(str::to_string))
}

/// Tool-calling mode to register a GGUF with; must match
/// `LocalInferenceLlmAdapter::tool_and_thinking_for`, which writes the same goose registry.
#[must_use]
pub fn tool_mode_for_gguf(
    path: &Path,
) -> goose::providers::local_inference::local_model_registry::ToolCallingMode {
    use goose::providers::local_inference::local_model_registry::ToolCallingMode;
    use pond_core::models::domain::model_probe::ToolSupport;

    match probe_cached(path).map(|p| p.tools) {
        Some(ToolSupport::Native) => ToolCallingMode::ForceNative,
        // Never ForceNative here: it would also disable the prose fallback.
        Some(ToolSupport::Absent) => ToolCallingMode::ForceEmulated,
        // Unknown: leave goose its own dry-run judgement.
        Some(ToolSupport::Unknown) | None => ToolCallingMode::Auto,
    }
}

/// Native tool support for `ModelCapabilities.tool_calling`: the file, else the name heuristic.
#[must_use]
pub fn model_uses_native_tools(provider: &str, model_name: &str, data_dir: Option<&Path>) -> bool {
    match probe_for_model(provider, model_name, data_dir) {
        Some(probe) => probe.supports_native_tools(),
        None => {
            pond_core::models::domain::model_capabilities::ModelCapabilities::from_model_name(
                model_name,
            )
            .tool_calling
        }
    }
}

/// The context window the weights were TRAINED for, if the file says; a registry pin and the
/// engine's memory cap outrank it.
#[must_use]
pub fn trained_context_window(
    provider: &str,
    model_name: &str,
    data_dir: Option<&Path>,
) -> Option<u32> {
    probe_for_model(provider, model_name, data_dir).and_then(|p| p.context_window_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_providers_have_no_file_and_say_so() {
        assert!(probe_for_model("ollama", "llama3.2", None).is_none());
        assert!(probe_for_model("openai", "gpt-4o", Some(Path::new("/tmp"))).is_none());
    }

    #[test]
    fn a_local_model_with_no_data_dir_falls_back_rather_than_panicking() {
        assert!(probe_for_model("local", "gemma-4-E2B-it", None).is_none());
    }

    #[test]
    fn the_name_heuristic_still_answers_when_there_is_no_file() {
        assert!(model_reasons("ollama", "qwen3-8b", None));
        assert!(model_reasons("ollama", "deepseek-r1:7b", None));
        assert!(!model_reasons("ollama", "llama3.2", None));
        assert!(!model_reasons("ollama", "mistral-small", None));
    }

    /// Fails if the heuristic learns Nemotron, flagging the probe path as redundant.
    #[test]
    fn the_fallback_is_the_thing_that_did_not_know_nemotron() {
        assert!(
            !model_reasons("ollama", "NVIDIA-Nemotron3-Nano-4B-Q4_K_M", None),
            "name heuristic has learned Nemotron; the probe path is now belt-and-braces"
        );
    }

    #[test]
    fn a_template_that_cannot_carry_tools_is_never_forced_native() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;
        use pond_core::models::domain::model_probe::{ModelProbe, Thinking, ToolSupport};

        // Mirrors `LocalInferenceLlmAdapter::tool_and_thinking_for`.
        for (tools, expected) in [
            (ToolSupport::Native, ToolCallingMode::ForceNative),
            (ToolSupport::Absent, ToolCallingMode::ForceEmulated),
            (ToolSupport::Unknown, ToolCallingMode::Auto),
        ] {
            let probe = ModelProbe {
                tools,
                thinking: Thinking::Absent,
                context_window_tokens: None,
                architecture: None,
            };
            let got = match probe.tools {
                ToolSupport::Native => ToolCallingMode::ForceNative,
                ToolSupport::Absent => ToolCallingMode::ForceEmulated,
                ToolSupport::Unknown => ToolCallingMode::Auto,
            };
            assert_eq!(got, expected, "{tools:?} must map to {expected:?}");
        }
    }

    #[test]
    #[ignore = "needs real GGUFs on disk"]
    fn the_registered_tool_mode_comes_from_the_file() {
        use goose::providers::local_inference::local_model_registry::ToolCallingMode;

        let Ok(dir) = std::env::var("GIAP_DATA_DIR") else {
            eprintln!("set GIAP_DATA_DIR to the pond data dir");
            return;
        };
        let gguf = Path::new(&dir).join("models").join("gguf");
        let mut seen = Vec::new();
        for (file, expect) in [
            ("gemma-4-E2B-it-Q4_K_M.gguf", ToolCallingMode::ForceNative),
            (
                "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
                ToolCallingMode::ForceNative,
            ),
            (
                "NVIDIA-Nemotron3-Nano-4B-Q4_K_M.gguf",
                ToolCallingMode::ForceNative,
            ),
            // The one that matters: no `tools` variable in its template.
            (
                "DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M.gguf",
                ToolCallingMode::ForceEmulated,
            ),
        ] {
            let path = gguf.join(file);
            if !path.exists() {
                eprintln!("skip {file}: not on disk");
                continue;
            }
            let got = tool_mode_for_gguf(&path);
            eprintln!("{file}: {got:?}");
            assert_eq!(got, expect, "{file}");
            seen.push(got);
        }
        assert!(
            seen.len() > 1 && seen.iter().any(|m| *m != seen[0]),
            "the probe must SEPARATE these models; one answer for all of them is              the failure mode that looks like success"
        );
    }

    /// Run with `GIAP_DATA_DIR=... cargo test -p pond-adapters-goose -- --ignored`.
    #[test]
    #[ignore = "needs real GGUFs on disk"]
    fn real_files_answer_where_the_name_heuristic_cannot() {
        let Ok(dir) = std::env::var("GIAP_DATA_DIR") else {
            eprintln!("set GIAP_DATA_DIR to the pond data dir");
            return;
        };
        let dd = Path::new(&dir);
        for (model, expect_reasons) in [
            ("gemma-4-E2B-it-Q4_K_M", true),
            ("NVIDIA-Nemotron3-Nano-4B-Q4_K_M", true),
            ("DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M", true),
            ("Nanbeige_Nanbeige4.2-3B-Q4_K_M", true),
        ] {
            let probe = probe_for_model("local", model, Some(dd));
            if probe.is_none() {
                eprintln!("skip {model}: not on disk");
                continue;
            }
            assert_eq!(
                model_reasons("local", model, Some(dd)),
                expect_reasons,
                "{model}"
            );
            eprintln!(
                "{model}: marker={:?} ctx={:?}",
                thinking_marker("local", model, Some(dd)),
                trained_context_window("local", model, Some(dd))
            );
        }
    }
}
