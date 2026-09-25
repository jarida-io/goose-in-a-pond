//! Strips reasoning blocks, inline `<|tool_call>` markup and `<eos>` sentinels from streamed
//! tokens: llamafile and ollama stream natively, bypassing `strip_thinking_tokens`.

/// Owned by `pond-core` and shared by both streaming filters; never keep a local copy.
use pond_core::models::services::thought_filter::{PAIRED_TAGS, STANDALONE_SENTINELS};

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Normal,
    /// Inside a paired-tag block; holds the close tag being looked for.
    InsideBlock(&'static str),
}

/// Stateful filter: one instance per response, fed every chunk of it.
pub struct ThoughtFilter {
    state: State,
    buf: String,
    /// Current block's body, kept for tool calls (reported by the SSE handler) and thinking.
    block_body: String,
    inside_open_tag: Option<&'static str>,
    /// Tool-call envelope bodies completed since the last `take_tool_calls`.
    captured_tool_calls: Vec<String>,
    /// Capture thinking blocks for SSE thinking events instead of discarding them.
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

    /// Feed a chunk; returns what can be forwarded now, holding back only a possible partial tag.
    pub fn push(&mut self, chunk: &str) -> String {
        self.buf.push_str(chunk);
        let mut out = String::new();
        loop {
            match &self.state {
                State::Normal => {
                    let earliest_pair = PAIRED_TAGS
                        .iter()
                        .filter_map(|&(open, close)| self.buf.find(open).map(|i| (i, open, close)))
                        .min_by_key(|&(i, _, _)| i);

                    if let Some((i, open, close)) = earliest_pair {
                        out.push_str(&strip_standalones(&self.buf[..i]));
                        self.buf.drain(..i + open.len());
                        self.inside_open_tag = Some(open);
                        self.block_body.clear();
                        self.state = State::InsideBlock(close);
                    } else {
                        // Hold back only a tail that could still become a marker; usually none.
                        let safe = safe_emit_len(&self.buf, &NORMAL_MARKERS);
                        out.push_str(&strip_standalones(&self.buf[..safe]));
                        self.buf.drain(..safe);
                        break;
                    }
                }
                State::InsideBlock(close) => {
                    let close_tag = *close;
                    if let Some(i) = self.buf.find(close_tag) {
                        self.block_body.push_str(&self.buf[..i]);
                        if matches!(self.inside_open_tag, Some("<|tool_call>")) {
                            let body = std::mem::take(&mut self.block_body);
                            let trimmed = body.trim();
                            if !trimmed.is_empty() {
                                self.captured_tool_calls.push(trimmed.to_string());
                            }
                        } else if self.capture_thinking {
                            let body = std::mem::take(&mut self.block_body);
                            let trimmed = body
                                .trim()
                                .strip_prefix("thought")
                                .unwrap_or(body.trim())
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
                        let safe = safe_emit_len(&self.buf, &[close_tag]);
                        self.block_body.push_str(&self.buf[..safe]);
                        self.buf.drain(..safe);
                        break;
                    }
                }
            }
        }
        out
    }

    /// Stream-end flush: emits buffered text, but drops an unclosed block's body.
    pub fn flush(&mut self) -> String {
        let pending = std::mem::take(&mut self.buf);
        match self.state {
            State::Normal => strip_standalones(&pending),
            State::InsideBlock(close) => {
                // Never drop model output silently.
                tracing::warn!(
                    close_marker = close,
                    dropped_bytes = pending.len() + self.block_body.len(),
                    "stream ended inside an unclosed block; its body is discarded",
                );
                String::new()
            }
        }
    }

    /// Drain raw tool-call bodies captured so far; see [`parse_tool_envelope`].
    pub fn take_tool_calls(&mut self) -> Vec<String> {
        std::mem::take(&mut self.captured_tool_calls)
    }

    /// Drain captured thinking, "thought" prefix stripped; empty without `with_thinking_capture`.
    pub fn take_thinking(&mut self) -> Vec<String> {
        std::mem::take(&mut self.captured_thinking)
    }
}

/// Best-effort `(name, raw_args)` from a tool-call body: `call:NAME{ARGS}`, `NAME{ARGS}`,
/// `NAME(ARGS)` or `{"name": ..., "arguments": {...}}`.
pub fn parse_tool_envelope(body: &str) -> Option<(String, String)> {
    let s = body.trim();
    let s = s.strip_prefix("call:").unwrap_or(s);

    // Form 1/2/3: NAME followed by ({...}) or {...}
    if let Some(open_idx) = s.find(|c: char| c == '{' || c == '(') {
        let name = s[..open_idx].trim().trim_end_matches(':').to_string();
        if !name.is_empty() {
            // Lift {} out of () wrapping if present.
            let raw_args = &s[open_idx..];
            let args = if let Some(stripped) =
                raw_args.strip_prefix('(').and_then(|x| x.strip_suffix(')'))
            {
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
            let args_str = args
                .map(|a| a.to_string())
                .unwrap_or_else(|| "{}".to_string());
            return Some((name.to_string(), args_str));
        }
    }

    None
}

/// Markers that can begin in `State::Normal`: open tags and standalone sentinels (no close tags).
static NORMAL_MARKERS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    PAIRED_TAGS
        .iter()
        .map(|&(open, _)| open)
        .chain(STANDALONE_SENTINELS.iter().copied())
        .collect()
});

fn strip_standalones(s: &str) -> String {
    let mut out = s.to_string();
    for sentinel in STANDALONE_SENTINELS {
        if out.contains(sentinel) {
            out = out.replace(sentinel, "");
        }
    }
    out
}

/// Emittable byte length of `s`: all but the longest suffix that is a *proper* marker prefix
/// (complete markers would be withheld forever). Always a char boundary; runs per token.
fn safe_emit_len(s: &str, markers: &[&str]) -> usize {
    let longest = markers.iter().map(|m| m.len()).max().unwrap_or(0);
    // A proper prefix is at most `longest - 1` bytes.
    let earliest = s.len().saturating_sub(longest.saturating_sub(1));

    // The first hit from the left is the longest withheld tail.
    for i in earliest..s.len() {
        if !s.is_char_boundary(i) {
            continue;
        }
        let tail = &s[i..];
        if markers
            .iter()
            .any(|m| m.len() > tail.len() && m.starts_with(tail))
        {
            return i;
        }
    }
    s.len()
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
        let chunks = &[
            "<|chan",
            "nel>thought ",
            "reason",
            "<chan",
            "nel|>",
            "Hello!",
        ];
        assert_eq!(run(chunks), "Hello!");
    }

    #[test]
    fn strips_thought_split_token_boundaries() {
        let raw = "<|channel>thought The user said hi.<channel|>Hello! I am Goose.";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "Hello! I am Goose.");
    }

    #[test]
    fn drops_unclosed_thought_block_on_flush() {
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
        // Models do emit the Harmony tool-call envelope as plain text.
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

    // ── <think> / <thought> tag tests ──────────────────────────────────────

    #[test]
    fn strips_think_block_in_one_chunk() {
        assert_eq!(
            run(&["<think>reasoning here</think>The answer is 42."]),
            "The answer is 42.",
        );
    }

    #[test]
    fn strips_thought_block_in_one_chunk() {
        assert_eq!(
            run(&["<thought>internal reasoning</thought>Hello!"]),
            "Hello!",
        );
    }

    #[test]
    fn strips_think_block_split_across_chunks() {
        let chunks = &["<thi", "nk>reason", "ing</thi", "nk>answer"];
        assert_eq!(run(chunks), "answer");
    }

    #[test]
    fn strips_thought_block_split_across_chunks() {
        let chunks = &["<thou", "ght>reason", "</thou", "ght>ok"];
        assert_eq!(run(chunks), "ok");
    }

    #[test]
    fn strips_think_per_token_streaming() {
        let raw = "<think>Let me think step by step.</think>The answer is 7.";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "The answer is 7.");
    }

    #[test]
    fn strips_mixed_channel_and_think_tags() {
        let raw = "<|channel>thought planning<channel|>text <think>more reasoning</think> final";
        assert_eq!(run(&[raw]), "text  final");
    }

    #[test]
    fn strips_orphaned_close_think_tag() {
        assert_eq!(run(&["Hello!</think>"]), "Hello!");
    }

    #[test]
    fn strips_orphaned_close_thought_tag() {
        assert_eq!(run(&["result</thought> done"]), "result done");
    }

    #[test]
    fn captures_think_block_when_capture_enabled() {
        let mut f = ThoughtFilter::new().with_thinking_capture();
        let out = f.push("<think>step by step reasoning</think>The answer.");
        let out2 = f.flush();
        assert_eq!(format!("{}{}", out, out2), "The answer.");
        let thinking = f.take_thinking();
        assert_eq!(thinking.len(), 1);
        assert_eq!(thinking[0], "step by step reasoning");
    }

    #[test]
    fn captures_thought_block_when_capture_enabled() {
        let mut f = ThoughtFilter::new().with_thinking_capture();
        let out = f.push("<thought>internal monologue</thought>Response.");
        let out2 = f.flush();
        assert_eq!(format!("{}{}", out, out2), "Response.");
        let thinking = f.take_thinking();
        assert_eq!(thinking.len(), 1);
        assert_eq!(thinking[0], "internal monologue");
    }

    #[test]
    fn drops_unclosed_think_block_on_flush() {
        assert_eq!(run(&["<think>never closes"]), "");
    }

    #[test]
    fn text_before_think_block() {
        assert_eq!(
            run(&["Sure! <think>hmm</think>Here you go."]),
            "Sure! Here you go.",
        );
    }

    #[test]
    fn idempotent_on_empty_chunks() {
        // "hi" cannot begin a marker, so it is emitted at once.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push(""), "");
        assert_eq!(f.push("hi"), "hi");
        assert_eq!(f.push(""), "");
        let mut g = ThoughtFilter::new();
        let mut total = g.push("hi");
        total.push_str(&g.flush());
        assert_eq!(total, "hi");
    }
    // ── Holdback behaviour ─────────────────────────────────────────────────
    // A fixed holdback makes the answer trail generation and freeze mid-word.

    #[test]
    fn ordinary_text_is_emitted_with_no_holdback_on_the_very_first_push() {
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("Just let me kno"), "Just let me kno");

        let mut g = ThoughtFilter::new();
        assert_eq!(
            g.push("The current president of the United States"),
            "The current president of the United States",
        );

        let mut h = ThoughtFilter::new();
        assert_eq!(h.push("T"), "T");
    }

    #[test]
    fn every_prefix_of_tag_free_text_is_emitted_as_it_arrives() {
        let raw = "Hello! I can help with reminders, sensors and the news.";
        let mut f = ThoughtFilter::new();
        let mut emitted = String::new();
        for (k, c) in raw.chars().enumerate() {
            emitted.push_str(&f.push(&c.to_string()));
            let expected: String = raw.chars().take(k + 1).collect();
            assert_eq!(emitted, expected, "lagged after {} chars", k + 1);
        }
        assert_eq!(f.flush(), "", "nothing should be left to flush");
    }

    #[test]
    fn holds_back_only_a_suffix_that_could_begin_a_marker() {
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("a < b"), "a < b");

        // A genuine partial marker is withheld in full, then resolved.
        let mut g = ThoughtFilter::new();
        assert_eq!(g.push("text <thi"), "text ");
        assert_eq!(g.push("nk>hidden</think>shown"), "shown");
    }

    #[test]
    fn holds_the_longest_matching_suffix_not_a_shorter_one() {
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("ok <think"), "ok ");
        assert_eq!(f.push(">reasoning</think>done"), "done");
    }

    #[test]
    fn a_complete_sentinel_is_not_withheld_as_a_partial() {
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("bye<eos>"), "bye");
    }

    #[test]
    fn a_complete_close_tag_that_extends_into_a_longer_one_resolves_next_push() {
        // "</think>" is also a proper prefix of "</thinking>".
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("done</think>"), "done");
        assert_eq!(f.push(" more"), " more");
    }

    #[test]
    fn multibyte_text_is_never_split_mid_character() {
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("Grüße, 世界"), "Grüße, 世界");

        // Fed one char at a time, the multibyte chars must survive intact.
        let raw = "héllo 世界 — ok";
        let mut g = ThoughtFilter::new();
        let mut out = String::new();
        for c in raw.chars() {
            out.push_str(&g.push(&c.to_string()));
        }
        out.push_str(&g.flush());
        assert_eq!(out, raw);
    }

    #[test]
    fn every_marker_is_ascii_so_a_partial_never_starts_mid_character() {
        // `safe_emit_len` relies on this.
        for (open, close) in PAIRED_TAGS {
            assert!(open.is_ascii(), "non-ASCII open marker: {open}");
            assert!(close.is_ascii(), "non-ASCII close marker: {close}");
        }
        for sentinel in STANDALONE_SENTINELS {
            assert!(sentinel.is_ascii(), "non-ASCII sentinel: {sentinel}");
        }
    }

    #[test]
    fn holdback_never_exceeds_the_longest_marker() {
        let longest = NORMAL_MARKERS.iter().map(|m| m.len()).max().unwrap();
        for probe in ["plain text", "a < b", "x <thi", "<|channel", "<end_of_tur"] {
            let held = probe.len() - safe_emit_len(probe, &NORMAL_MARKERS);
            assert!(held < longest, "{probe:?} held {held} bytes");
        }
    }

    // ── The `<thinking>` spelling ──────────────────────────────────────────

    #[test]
    fn strips_the_long_thinking_spelling() {
        assert_eq!(run(&["<thinking>reasoning</thinking>Answer."]), "Answer.");
    }

    #[test]
    fn strips_the_long_thinking_spelling_split_across_chunks() {
        assert_eq!(run(&["<thin", "king>hmm</think", "ing>ok"]), "ok");
    }

    #[test]
    fn strips_orphaned_long_close_thinking_tag() {
        assert_eq!(run(&["result</thinking> done"]), "result done");
    }

    #[test]
    fn the_two_thinking_spellings_stay_disjoint() {
        assert_eq!(run(&["<think>a</think>X<thinking>b</thinking>Y"]), "XY");
    }
}
