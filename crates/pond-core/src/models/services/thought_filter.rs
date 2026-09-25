//! Streaming filter stripping reasoning and tool markup (Gemma 4, Harmony, Qwen3, DeepSeek-R1)
//! before TTS or display. Reuse one instance per response; see `safe_emit_len` for holdback.

/// Paired tags dropped with their contents; shared with `pond_api::thought_filter`'s SSE filter.
pub const PAIRED_TAGS: &[(&str, &str)] = &[
    ("<|channel>thought", "<channel|>"),
    ("<|tool_call>", "<tool_call|>"),
    ("<think>", "</think>"),
    // Disjoint from `<think>` (the `>`), so order is free; without it voice speaks the reasoning.
    ("<thinking>", "</thinking>"),
    ("<thought>", "</thought>"),
];

/// Sentinels dropped wherever they appear; shared like [`PAIRED_TAGS`].
pub const STANDALONE_SENTINELS: &[&str] = &[
    "<eos>",
    "<|eos|>",
    "<end_of_turn>",
    "</think>",
    "</thinking>",
    "</thought>",
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Normal,
    InsideBlock(&'static str),
}

/// Stateful per-stream filter.
pub struct ThoughtFilter {
    state: State,
    buf: String,
}

impl Default for ThoughtFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ThoughtFilter {
    pub fn new() -> Self {
        Self {
            state: State::Normal,
            buf: String::new(),
        }
    }

    /// Feed a chunk; returns the visible text that should be forwarded.
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
                        self.state = State::InsideBlock(close);
                    } else {
                        let safe = safe_emit_len(&self.buf, &NORMAL_MARKERS);
                        out.push_str(&strip_standalones(&self.buf[..safe]));
                        self.buf.drain(..safe);
                        break;
                    }
                }
                State::InsideBlock(close) => {
                    let close_tag = *close;
                    if let Some(i) = self.buf.find(close_tag) {
                        self.buf.drain(..i + close_tag.len());
                        self.state = State::Normal;
                    } else {
                        let safe = safe_emit_len(&self.buf, &[close_tag]);
                        self.buf.drain(..safe);
                        break;
                    }
                }
            }
        }
        out
    }

    /// Stream-end flush: emits buffered text, or drops it inside an unclosed block.
    pub fn flush(&mut self) -> String {
        let pending = std::mem::take(&mut self.buf);
        match self.state {
            State::Normal => strip_standalones(&pending),
            State::InsideBlock(close) => {
                // Warn, since a silent drop (unspoken text on voice) hides this class of bug.
                tracing::warn!(
                    close_marker = close,
                    dropped_bytes = pending.len(),
                    "stream ended inside an unclosed block; its body is discarded",
                );
                String::new()
            }
        }
    }
}

/// Every marker that can begin in `State::Normal`; built once because `push` runs per token.
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

/// Emittable byte length of `s`: only a tail that is a *proper* marker prefix is withheld,
/// since a complete one would be held forever. Markers are ASCII, so this is a char boundary.
fn safe_emit_len(s: &str, markers: &[&str]) -> usize {
    let longest = markers.iter().map(|m| m.len()).max().unwrap_or(0);
    let earliest = s.len().saturating_sub(longest.saturating_sub(1));

    // First forward hit is the longest withheld tail; `i < s.len()` excludes the empty suffix.
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
    fn passthrough() {
        assert_eq!(run(&["Hello, ", "world!"]), "Hello, world!");
    }

    #[test]
    fn strips_channel_thought() {
        assert_eq!(run(&["<|channel>thought reasoning<channel|>Hi!"]), "Hi!");
    }

    #[test]
    fn strips_think() {
        assert_eq!(run(&["<think>reasoning</think>Answer."]), "Answer.");
    }

    #[test]
    fn strips_thought() {
        assert_eq!(run(&["<thought>reasoning</thought>Answer."]), "Answer.");
    }

    #[test]
    fn strips_tool_call() {
        assert_eq!(run(&["<|tool_call>call:x{}<tool_call|>"]), "");
    }

    #[test]
    fn strips_split_across_chunks() {
        assert_eq!(run(&["<thi", "nk>reason</thi", "nk>ok"]), "ok");
    }

    #[test]
    fn strips_channel_split_across_chunks() {
        assert_eq!(run(&["<|chan", "nel>thought reason<chan", "nel|>Hi"]), "Hi");
    }

    #[test]
    fn strips_eos() {
        assert_eq!(run(&["Hello!<eos>"]), "Hello!");
    }

    #[test]
    fn strips_orphaned_close() {
        assert_eq!(run(&["Hello!</think>"]), "Hello!");
    }

    #[test]
    fn drops_unclosed_block() {
        assert_eq!(run(&["<think>never closes"]), "");
    }

    #[test]
    fn text_before_and_after() {
        assert_eq!(run(&["Sure! <think>hmm</think>Here."]), "Sure! Here.");
    }

    #[test]
    fn per_token_streaming() {
        let raw = "<think>step by step</think>The answer is 7.";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "The answer is 7.");
    }

    #[test]
    fn gemma4_per_token() {
        let raw = "<|channel>thought The user said hi.<channel|>Hello!";
        let chunks: Vec<String> = raw.chars().map(|c| c.to_string()).collect();
        let refs: Vec<&str> = chunks.iter().map(|s| s.as_str()).collect();
        assert_eq!(run(&refs), "Hello!");
    }

    /// With thinking disabled `ThinkingOutputFilter` passes through, so this is TTS's last guard.
    #[test]
    fn every_thinking_spelling_is_stripped_before_speech() {
        for (open, close) in [
            ("<think>", "</think>"),
            ("<thinking>", "</thinking>"),
            ("<thought>", "</thought>"),
        ] {
            let raw = format!("{open}step by step{close}The answer is 7.");
            assert_eq!(
                run(&[&raw]),
                "The answer is 7.",
                "{open} leaked into speech"
            );
        }
    }

    #[test]
    fn the_longer_spelling_leaves_no_fragment() {
        let out = run(&["<thinking>hmm</thinking>Hi."]);
        assert!(!out.contains("ing>"), "fragment left behind: {out:?}");
        assert!(!out.contains('<'), "tag residue: {out:?}");
        assert_eq!(out, "Hi.");
    }

    #[test]
    fn the_longer_spelling_survives_chunk_boundaries() {
        assert_eq!(run(&["<think", "ing>hmm</think", "ing>Done."]), "Done.");
    }
    // ── Holdback behaviour ─────────────────────────────────────────────────
    // A fixed-size holdback would make text trail generation and freeze mid-word on slowdowns.

    #[test]
    fn ordinary_text_is_emitted_with_no_holdback() {
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("Just let me kno"), "Just let me kno");
        assert_eq!(ThoughtFilter::new().push("T"), "T");
    }

    #[test]
    fn every_prefix_of_tag_free_text_is_emitted_as_it_arrives() {
        let raw = "The kettle is on and the door is locked.";
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
        assert_eq!(ThoughtFilter::new().push("a < b"), "a < b");

        let mut g = ThoughtFilter::new();
        assert_eq!(g.push("text <thi"), "text ");
        assert_eq!(g.push("nk>hidden</think>shown"), "shown");
    }

    #[test]
    fn a_partial_sentinel_is_still_withheld_until_it_resolves() {
        // A proper prefix of `<end_of_turn>`: withheld by push, released by flush.
        let mut f = ThoughtFilter::new();
        assert_eq!(f.push("the code is 42<end_of_tu"), "the code is 42");
        assert_eq!(f.flush(), "<end_of_tu");
    }

    #[test]
    fn multibyte_text_is_never_split_mid_character() {
        assert_eq!(ThoughtFilter::new().push("Grüße, 世界"), "Grüße, 世界");

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
        // `safe_emit_len`'s char-boundary reasoning relies on this.
        for (open, close) in PAIRED_TAGS {
            assert!(open.is_ascii(), "non-ASCII open marker: {open}");
            assert!(close.is_ascii(), "non-ASCII close marker: {close}");
        }
        for sentinel in STANDALONE_SENTINELS {
            assert!(sentinel.is_ascii(), "non-ASCII sentinel: {sentinel}");
        }
    }
}
