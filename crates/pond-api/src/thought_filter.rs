//! Streaming filter that strips two flavours of Harmony-style markup from
//! token streams before they reach the SSE consumer:
//!
//!   1. Reasoning-channel preambles: `<|channel>thought ... <channel|>...`
//!   2. Inline tool-call markup:     `<|tool_call> ... <tool_call|>`
//!      (and the Gemma-style end-of-sequence sentinel `<eos>` that some
//!      models keep emitting after they stop generating useful tokens)
//!
//! Rationale: Llamafile + Ollama implement `LlmProvider::stream_complete`
//! natively, so the per-token output bypasses the post-`complete()` strip in
//! `pond-adapters-local-inference::strip_thinking_tokens`. Reasoning-capable
//! models (Gemma 4, gpt-oss, similar Harmony-channel families) leak their
//! thinking preamble — and now their inline tool-call gibberish — straight
//! to the user.
//!
//! This filter buffers the minimum lookahead needed to detect partial open/close
//! tags spanning chunk boundaries, suppresses everything between the tags, and
//! flushes whatever remains (post-close-tag) on stream end.

/// Paired tags whose entire contents (and the tags themselves) are dropped.
/// Each pair = (open marker, close marker). The first pair encountered wins
/// — we don't expect nesting in practice.
const PAIRED_TAGS: &[(&str, &str)] = &[
    ("<|channel>thought", "<channel|>"),
    ("<|tool_call>",      "<tool_call|>"),
];

/// Standalone sentinels that get silently dropped wherever they appear in
/// the stream. Some models (Gemma-family especially) keep emitting `<eos>`
/// after the real reply ends; the chat UI then renders them literally.
const STANDALONE_SENTINELS: &[&str] = &["<eos>", "<|eos|>", "<end_of_turn>"];

/// Maximum tag length across PAIRED_TAGS (open + close) and STANDALONE_SENTINELS.
/// Used to decide how many trailing bytes to hold back as lookahead. Computed
/// at runtime in `safe_emit_len_max` — this constant is just a fast upper bound.
const _MAX_TAG_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Normal,
    /// Inside a paired-tag block. The `&'static str` holds the close tag we
    /// are looking for, so we don't have to remember which open tag matched.
    InsideBlock(&'static str),
}

/// Stateful per-stream filter. Reuse a single instance across all chunks of
/// one response; allocate a new one per response.
pub struct ThoughtFilter {
    state: State,
    buf: String,
    /// Per-block accumulator for the body of a paired-tag envelope.  The
    /// `<|channel>thought ...` block is discarded; the `<|tool_call> ...`
    /// block is captured here so the SSE handler can surface a "the model
    /// tried to call X but didn't use the proper protocol" notice instead
    /// of silently dropping it.
    block_body: String,
    /// Open tag of the block we are currently inside, so we can decide
    /// whether to keep the body (tool_call) or discard it (channel/thought).
    inside_open_tag: Option<&'static str>,
    /// Tool-call envelope bodies completed since the last `take_tool_calls`.
    captured_tool_calls: Vec<String>,
    /// When true, thinking/reasoning blocks are captured (not just discarded)
    /// so they can be forwarded as SSE thinking events.
    capture_thinking: bool,
    /// Thinking blocks captured since the last `take_thinking`.
    captured_thinking: Vec<String>,
}

impl Default for ThoughtFilter {
    fn default() -> Self {
        Self {
            state: State::Normal,
            buf: String::new(),
            block_body: String::new(),
            inside_open_tag: None,
            captured_tool_calls: Vec::new(),
            capture_thinking: false,
            captured_thinking: Vec::new(),
        }
    }
}

impl ThoughtFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a filter that captures thinking blocks for forwarding as events.
    pub fn with_thinking_capture(mut self) -> Self {
        self.capture_thinking = true;
        self
    }

    /// Feed a chunk; returns the (possibly empty) substring that should be
    /// forwarded downstream right now. Tokens that overlap a partial tag are
    /// held back until the next call resolves the ambiguity.
    pub fn push(&mut self, chunk: &str) -> String {
        self.buf.push_str(chunk);
        let mut out = String::new();
        loop {
            match &self.state {
                State::Normal => {
                    // Find the earliest open tag among the paired set.
                    let earliest_pair = PAIRED_TAGS
                        .iter()
                        .filter_map(|&(open, close)| self.buf.find(open).map(|i| (i, open, close)))
                        .min_by_key(|&(i, _, _)| i);

                    if let Some((i, open, close)) = earliest_pair {
                        // Strip standalone sentinels from the chunk we're about
                        // to emit (they can appear anywhere in Normal text).
                        out.push_str(&strip_standalones(&self.buf[..i]));
                        self.buf.drain(..i + open.len());
                        self.inside_open_tag = Some(open);
                        self.block_body.clear();
                        self.state = State::InsideBlock(close);
                    } else {
                        // No paired tag visible. Hold back a lookahead tail
                        // long enough to detect any partial tag (open + sentinel).
                        let look = max_normal_lookahead();
                        let safe = safe_emit_len(&self.buf, look);
                        out.push_str(&strip_standalones(&self.buf[..safe]));
                        self.buf.drain(..safe);
                        break;
                    }
                }
                State::InsideBlock(close) => {
                    let close_tag = *close;
                    if let Some(i) = self.buf.find(close_tag) {
                        // Capture the body of paired-tag envelopes:
                        // - tool_call: always captured for UI surfacing
                        // - channel/thought: captured only when capture_thinking is on
                        self.block_body.push_str(&self.buf[..i]);
                        if matches!(self.inside_open_tag, Some("<|tool_call>")) {
                            let body = std::mem::take(&mut self.block_body);
                            let trimmed = body.trim();
                            if !trimmed.is_empty() {
                                self.captured_tool_calls.push(trimmed.to_string());
                            }
                        } else if self.capture_thinking {
                            // Thinking block — capture for SSE thinking events
                            let body = std::mem::take(&mut self.block_body);
                            let trimmed = body.trim()
                                .strip_prefix("thought").unwrap_or(body.trim())
                                .trim();
                            if !trimmed.is_empty() {
                                self.captured_thinking.push(trimmed.to_string());
                            }
                        } else {
                            self.block_body.clear();
                        }
                        self.inside_open_tag = None;
                        self.buf.drain(..i + close_tag.len());
                        self.state = State::Normal;
                    } else {
                        // Discard everything but a tail that may start close.
                        let safe = safe_emit_len(&self.buf, close_tag.len());
                        // Accumulate the safely-discarded portion for tool_call
                        // envelopes so we can surface it once close arrives.
                        self.block_body.push_str(&self.buf[..safe]);
                        self.buf.drain(..safe);
                        break;
                    }
                }
            }
        }
        out
    }

    /// Stream-end flush. Anything still buffered in Normal state is emitted
    /// (after stripping standalone sentinels); anything buffered inside a
    /// paired-tag block is dropped (the model never closed it).
    pub fn flush(&mut self) -> String {
        let pending = std::mem::take(&mut self.buf);
        match self.state {
            State::Normal => strip_standalones(&pending),
            State::InsideBlock(_) => String::new(),
        }
    }

    /// Drain any tool-call envelopes captured since the last call. Returns
    /// the raw body text (everything between `<|tool_call>` and `<tool_call|>`),
    /// without the wrapping markers. Use [`parse_tool_envelope`] to extract
    /// the tool name and JSON arguments.
    pub fn take_tool_calls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.captured_tool_calls)
    }

    /// Drain any thinking/reasoning blocks captured since the last call.
    /// Only populated when `with_thinking_capture()` was called. Returns
    /// the reasoning text with the "thought" prefix stripped.
    pub fn take_thinking(&mut self) -> Vec<String> {
        std::mem::take(&mut self.captured_thinking)
    }
}

/// Best-effort parser for the body of a Harmony-style `<|tool_call>...
/// <tool_call|>` envelope. Common shapes seen in the wild:
///
///   `call:NAME{ARGS_JSON}`
///   `NAME{ARGS_JSON}`
///   `NAME(ARGS_JSON)`
///   `{"name": "NAME", "arguments": {...}}`
///
/// Returns `(tool_name, args_json_str)` when a recognised shape parses;
/// otherwise `None`. The args string is left as-is so callers can pass it
/// straight to a JSON parser or surface the raw text in a UI message.
pub fn parse_tool_envelope(body: &str) -> Option<(String, String)> {
    let s = body.trim();
    // Strip an optional `call:` prefix.
    let s = s.strip_prefix("call:").unwrap_or(s);

    // Form 1/2/3: NAME followed by ({...}) or {...}
    if let Some(open_idx) = s.find(|c: char| c == '{' || c == '(') {
        let name = s[..open_idx].trim().trim_end_matches(':').to_string();
        if !name.is_empty() {
            // Lift {} out of () wrapping if present.
            let raw_args = &s[open_idx..];
            let args = if let Some(stripped) = raw_args.strip_prefix('(').and_then(|x| x.strip_suffix(')')) {
                stripped.to_string()
            } else {
                raw_args.to_string()
            };
            return Some((name, args));
        }
    }

    // Form 4: full JSON object with `name` + `arguments`.
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(s) {
        if let (Some(name), args) = (v.get("name").and_then(|n| n.as_str()), v.get("arguments")) {
            let args_str = args.map(|a| a.to_string()).unwrap_or_else(|| "{}".to_string());
            return Some((name.to_string(), args_str));
        }
    }

    None
}

/// Worst-case lookahead in Normal state: must be ≥ longest open tag and
/// ≥ longest standalone sentinel so neither can be missed when split.
fn max_normal_lookahead() -> usize {
    let opens = PAIRED_TAGS.iter().map(|(o, _)| o.len()).max().unwrap_or(0);
    let stand = STANDALONE_SENTINELS.iter().map(|s| s.len()).max().unwrap_or(0);
    opens.max(stand)
}

/// Remove every occurrence of every standalone sentinel from the input.
/// Cheap because the sentinel set is tiny.
fn strip_standalones(s: &str) -> String {
    let mut out = s.to_string();
    for sentinel in STANDALONE_SENTINELS {
        if out.contains(sentinel) {
            out = out.replace(sentinel, "");
        }
    }
    out
}

/// Return the byte index up to which `s` can be safely emitted given that the
/// next tag we're scanning for is `tag_len` bytes long. We hold back the last
/// `tag_len - 1` bytes so a tag straddling the buffer boundary is still
/// detectable on the next push. Snaps down to the nearest UTF-8 char boundary.
fn safe_emit_len(s: &str, tag_len: usize) -> usize {
    let mut cut = s.len().saturating_sub(tag_len.saturating_sub(1));
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> String {
        let mut f = ThoughtFilter::new();
        let mut out = String::new();
        for c in chunks {
            out.push_str(&f.push(c));
        }
        out.push_str(&f.flush());
        out
    }

    #[test]
    fn passes_through_when_no_tags_present() {
        assert_eq!(run(&["Hello, ", "world!"]), "Hello, world!");
    }

    #[test]
    fn strips_full_thought_preamble_in_one_chunk() {
        assert_eq!(
            run(&["<|channel>thought reasoning here<channel|>Hi there!"]),
            "Hi there!",
        );
    }

    #[test]
    fn strips_thought_split_across_chunks() {
        let chunks = &["<|chan", "nel>thought ", "reason", "<chan", "nel|>", "Hello!"];
        assert_eq!(run(chunks), "Hello!");
    }

    #[test]
    fn strips_thought_split_token_boundaries() {
        // Mimic real per-token streaming where each chunk is a few chars.
        let raw = "<|channel>thought The user said hi.<channel|>Hello! I am Goose.";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "Hello! I am Goose.");
    }

    #[test]
    fn drops_unclosed_thought_block_on_flush() {
        // No closing tag — model misbehaved; we conservatively drop the buffer.
        assert_eq!(run(&["<|channel>thought never closes"]), "");
    }

    #[test]
    fn handles_text_before_thought_block() {
        assert_eq!(
            run(&["Sure! <|channel>thought hmm<channel|>Here you go."]),
            "Sure! Here you go.",
        );
    }

    #[test]
    fn strips_inline_tool_call_markup() {
        // Real-world leak: model emits the Harmony tool-call envelope as
        // plain text instead of using the structural tool-call mechanism.
        let raw = "<|tool_call>call:giap__get_current_weather{}<tool_call|>";
        assert_eq!(run(&[raw]), "");
    }

    #[test]
    fn captures_tool_call_envelope_body() {
        let mut f = ThoughtFilter::new();
        let _ = f.push("<|tool_call>call:giap__get_current_weather{}<tool_call|>");
        let calls = f.take_tool_calls();
        assert_eq!(calls.len(), 1);
        let (name, args) = parse_tool_envelope(&calls[0]).expect("parse");
        assert_eq!(name, "giap__get_current_weather");
        assert_eq!(args, "{}");
    }

    #[test]
    fn parse_tool_envelope_handles_brace_form() {
        let (n, a) = parse_tool_envelope("weather{\"city\":\"Nairobi\"}").unwrap();
        assert_eq!(n, "weather");
        assert_eq!(a, "{\"city\":\"Nairobi\"}");
    }

    #[test]
    fn parse_tool_envelope_handles_paren_form() {
        let (n, a) = parse_tool_envelope("weather({\"city\":\"X\"})").unwrap();
        assert_eq!(n, "weather");
        assert_eq!(a, "{\"city\":\"X\"}");
    }

    #[test]
    fn parse_tool_envelope_handles_full_json_form() {
        let (n, a) = parse_tool_envelope(r#"{"name":"weather","arguments":{"x":1}}"#).unwrap();
        assert_eq!(n, "weather");
        assert!(a.contains("\"x\""));
    }

    #[test]
    fn strips_eos_sentinel_anywhere_in_stream() {
        assert_eq!(run(&["Hello!<eos>"]), "Hello!");
        assert_eq!(run(&["one<eos> two<eos>"]), "one two");
        // Split across chunks — flush handles the tail.
        assert_eq!(run(&["bye", "<eo", "s>"]), "bye");
    }

    #[test]
    fn strips_thought_then_tool_call_back_to_back() {
        let raw = "<|channel>thought planning<channel|>OK <|tool_call>call:x{}<tool_call|>";
        assert_eq!(run(&[raw]), "OK ");
    }

    #[test]
    fn idempotent_on_empty_chunks() {
        // Short pushes get held back as lookahead in case they're the start of
        // a partial tag — the buffered tail is released by flush() at stream end.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push(""), "");
        let _ = f.push("hi");
        assert_eq!(f.push(""), "");
        // Combined emitted + flush == original input.
        let mut g = ThoughtFilter::new();
        let mut total = g.push("hi");
        total.push_str(&g.flush());
        assert_eq!(total, "hi");
    }
}
