//! What a model can be asked to do, read from its own Jinja chat template, not a name table.
//! Only whole words inside `{% ... %}` count; emitted text is not evidence.

use super::gguf::GgufInfo;

/// Whether tool declarations can be rendered at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSupport {
    /// The template renders a `tools` variable, so declarations can be passed natively.
    Native,
    /// No `tools` variable: describe tools in the system prompt, or offer none.
    Absent,
    /// No template to read. Says nothing either way.
    Unknown,
}

/// Whether the model reasons before answering, and how that is controlled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thinking {
    /// An `enable_thinking` flag lets the caller decide (Gemma 4, Nemotron, Nanbeige).
    Gated { marker: String },
    /// Always opens a reasoning block, with no flag to clear it (DeepSeek-R1 distills).
    Always { marker: String },
    /// No reasoning markers.
    Absent,
    /// No template to read.
    Unknown,
}

/// What a model's own file says it can do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProbe {
    pub tools: ToolSupport,
    pub thinking: Thinking,
    /// `{arch}.context_length`: the trained window, not what this machine can afford.
    pub context_window_tokens: Option<u32>,
    /// `general.architecture`, for rules that legitimately need the family.
    pub architecture: Option<String>,
}

/// Reasoning markers, most specific first. Gemma 4's template emits `<|think|>`, not the
/// `<|channel>thought` that `ModelCapabilities` documents.
const THINKING_MARKERS: &[&str] = &["<|think|>", "<think>", "<|channel|>", "<reasoning>"];

impl ModelProbe {
    /// No `chat_template` yields `Unknown`, not `Absent`: only "cannot" justifies withholding.
    pub fn from_gguf(info: &GgufInfo) -> Self {
        let Some(template) = info.chat_template.as_deref() else {
            return Self {
                tools: ToolSupport::Unknown,
                thinking: Thinking::Unknown,
                context_window_tokens: info.context_length,
                architecture: info.architecture.clone(),
            };
        };

        let tools = if mentions_in_control_flow(template, "tools") {
            ToolSupport::Native
        } else {
            ToolSupport::Absent
        };

        let marker = THINKING_MARKERS
            .iter()
            .find(|m| template.contains(**m))
            .map(|m| (*m).to_string());

        let thinking = match marker {
            Some(marker) if mentions_in_control_flow(template, "enable_thinking") => {
                Thinking::Gated { marker }
            }
            Some(marker) => Thinking::Always { marker },
            None => Thinking::Absent,
        };

        Self {
            tools,
            thinking,
            context_window_tokens: info.context_length,
            architecture: info.architecture.clone(),
        }
    }

    /// Can tools be passed natively? `Unknown` answers no: silence is not evidence of support.
    pub fn supports_native_tools(&self) -> bool {
        matches!(self.tools, ToolSupport::Native)
    }

    pub fn thinking_is_selectable(&self) -> bool {
        matches!(self.thinking, Thinking::Gated { .. })
    }

    /// The tag this model opens reasoning with, when it has one.
    pub fn thinking_marker(&self) -> Option<&str> {
        match &self.thinking {
            Thinking::Gated { marker } | Thinking::Always { marker } => Some(marker),
            _ => None,
        }
    }
}

/// Probe a GGUF on disk, cached on `(path, mtime, len)`, `None` included. Asked every turn, so
/// it must be cheap and stable: a changed answer moves `prefix_hash`, forcing a full re-prefill.
pub fn probe_cached(path: &std::path::Path) -> Option<ModelProbe> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// `mtime` (nanos) is `None` when the filesystem won't say, which only makes the key coarser.
    type Key = (std::path::PathBuf, Option<u128>, u64);

    static CACHE: OnceLock<Mutex<HashMap<Key, Option<ModelProbe>>>> = OnceLock::new();

    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos());
    let key: Key = (path.to_path_buf(), mtime, meta.len());

    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(map) = cache.lock() {
        if let Some(hit) = map.get(&key) {
            return hit.clone();
        }
    }

    let probe = super::gguf::parse_gguf_file(path).map(|info| ModelProbe::from_gguf(&info));
    if let Ok(mut map) = cache.lock() {
        map.insert(key, probe.clone());
    }
    probe
}

/// Whether `ident` is a whole word inside a `{% ... %}` block; emitted text is not consumption.
fn mentions_in_control_flow(template: &str, ident: &str) -> bool {
    let mut rest = template;
    while let Some(open) = rest.find("{%") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("%}") else {
            return false;
        };
        if contains_word(&after[..close], ident) {
            return true;
        }
        rest = &after[close + 2..];
    }
    false
}

/// Whole-word match: `tools` does not match `tool_calls`, nor `tool` match `tools`.
fn contains_word(haystack: &str, word: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(word) {
        let start = from + rel;
        let end = start + word.len();
        let before_ok = start == 0 || !is_ident_byte(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_ident_byte(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
        if from >= haystack.len() {
            break;
        }
    }
    false
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim excerpts from the real templates.
    const GEMMA4: &str = r#"{{- bos_token -}}
        {%- if (enable_thinking is defined and enable_thinking) or tools or messages[0]['role'] in ['system'] -%}
        {{- '<|turn>system\n' -}}
        {%- if enable_thinking is defined and enable_thinking -%}{{- '<|think|>\n' -}}{%- endif -%}
        {%- if tools -%}{%- for tool in tools %}{{- '<|tool>' -}}{%- endfor -%}{%- endif -%}"#;

    const DEEPSEEK_R1: &str = r#"{% if ns.is_tool %}{{'<|tool outputs end|>'}}{% endif %}
        {% if add_generation_prompt and not ns.is_tool %}{{'<|Assistant|><think>\n'}}{% endif %}"#;

    const NO_THINKING: &str = r#"{%- if tools %}{%- for tool in tools %}{{ tool }}{%- endfor %}{%- endif %}
        {%- for message in messages %}{{ message['content'] }}{%- endfor %}"#;

    fn info(template: Option<&str>) -> GgufInfo {
        GgufInfo {
            architecture: Some("test".into()),
            context_length: Some(131072),
            chat_template: template.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn gemma_is_a_tool_user_and_a_gated_thinker() {
        let p = ModelProbe::from_gguf(&info(Some(GEMMA4)));
        assert_eq!(p.tools, ToolSupport::Native);
        assert!(p.supports_native_tools());
        assert_eq!(
            p.thinking,
            Thinking::Gated {
                marker: "<|think|>".into()
            }
        );
        assert!(p.thinking_is_selectable());
        assert_eq!(p.context_window_tokens, Some(131072));
    }

    #[test]
    fn deepseek_reasons_but_is_not_a_tool_user() {
        let p = ModelProbe::from_gguf(&info(Some(DEEPSEEK_R1)));
        assert_eq!(
            p.tools,
            ToolSupport::Absent,
            "DeepSeek-R1 has no `tools` variable; forcing native tool calling puts \
             declarations nowhere"
        );
        assert!(!p.supports_native_tools());
        assert_eq!(
            p.thinking,
            Thinking::Always {
                marker: "<think>".into()
            },
            "its <think> block is opened unconditionally -- there is no flag to clear"
        );
        assert!(!p.thinking_is_selectable());
    }

    /// The false positive that a substring test produces, pinned.
    #[test]
    fn a_mention_of_tool_call_is_not_tool_support() {
        assert!(
            DEEPSEEK_R1.contains("tool"),
            "fixture must contain the word, or this test proves nothing"
        );
        assert!(!mentions_in_control_flow(DEEPSEEK_R1, "tools"));
    }

    #[test]
    fn a_tool_user_that_does_not_reason() {
        let p = ModelProbe::from_gguf(&info(Some(NO_THINKING)));
        assert_eq!(p.tools, ToolSupport::Native);
        assert_eq!(p.thinking, Thinking::Absent);
        assert_eq!(p.thinking_marker(), None);
    }

    #[test]
    fn no_template_is_unknown_not_absent() {
        let p = ModelProbe::from_gguf(&info(None));
        assert_eq!(p.tools, ToolSupport::Unknown);
        assert_eq!(p.thinking, Thinking::Unknown);
        assert!(
            !p.supports_native_tools(),
            "Unknown must not be treated as permission to force native tools"
        );
        assert_eq!(
            p.context_window_tokens,
            Some(131072),
            "metadata still answers even when the template is missing"
        );
    }

    /// Only the real files prove the rules survive 19 KB of real Jinja.
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn probes_every_model_on_disk() {
        use crate::models::domain::gguf::parse_gguf_file;

        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        let (mut tool_users, mut thinkers, mut seen) = (0, 0, 0);
        for entry in std::fs::read_dir(&dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }
            let Some(info) = parse_gguf_file(&path) else {
                continue;
            };
            let p = ModelProbe::from_gguf(&info);
            eprintln!(
                "{:<44} {:<8?} {:?}",
                path.file_name().unwrap().to_string_lossy(),
                p.tools,
                p.thinking
            );
            seen += 1;
            if p.supports_native_tools() {
                tool_users += 1;
            }
            if p.thinking_marker().is_some() {
                thinkers += 1;
            }

            if info.chat_template.is_none() {
                assert!(
                    !p.supports_native_tools(),
                    "no template must not grant tools"
                );
            }
        }
        assert!(seen > 0, "no GGUF files under {dir}");
        assert!(
            tool_users > 0 && tool_users < seen,
            "the probe should separate the collection, not answer the same for all {seen}: \
             {tool_users} tool users"
        );
        assert!(
            thinkers > 0,
            "no reasoning markers found across {seen} models"
        );
    }

    #[test]
    fn whole_words_only() {
        assert!(contains_word("{% if tools %}", "tools"));
        assert!(!contains_word("{% if tool_calls %}", "tools"));
        assert!(!contains_word("{% if tools %}", "tool"));
        assert!(contains_word("{%- for tool in tools %}", "tool"));
        assert!(!contains_word("{{ mytools }}", "tools"));
    }

    #[test]
    fn emitted_text_is_not_control_flow() {
        assert!(!mentions_in_control_flow(
            "{{ 'tools are great' }}",
            "tools"
        ));
        assert!(mentions_in_control_flow("{% if tools %}", "tools"));
        // An unterminated block must not loop or panic.
        assert!(!mentions_in_control_flow("{% if tools", "tools"));
    }
}
