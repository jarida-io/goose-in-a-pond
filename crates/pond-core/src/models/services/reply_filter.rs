//! A reply's text with the model's echo of GIAP's prompt taken out, and a tool call the model
//! wrote as text caught before it is shown or spoken. Reasoning and tool-call blocks pass through
//! untouched for the downstream `ThoughtFilter`s, which strip them across chunks themselves.

use super::thought_filter::{safe_emit_len, PAIRED_TAGS};
pub use pond_voice::text::SCAFFOLD_TAGS;

/// A reply that opens with a tag is held this long, at most, before it is judged prose.
const OPENING_HOLD_CHARS: usize = 400;

enum Inside {
    Nothing,
    /// A reasoning or native tool-call block, passed through raw until this close.
    Passthrough(&'static str),
    /// An echoed scaffold block, dropped until this close.
    Scaffold(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Opening {
    /// No visible character outside a block has arrived yet.
    Undecided,
    Prose,
    /// The reply opened with a tool call written as text; the rest of it is dropped.
    TextCall,
}

/// Per-attempt filter over a reply's streamed text.
pub struct ReplyFilter {
    buf: String,
    inside: Inside,
    opening: Opening,
    held: String,
    scaffold_blocks: usize,
    text_call: Option<String>,
}

impl Default for ReplyFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplyFilter {
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            inside: Inside::Nothing,
            opening: Opening::Undecided,
            held: String::new(),
            scaffold_blocks: 0,
            text_call: None,
        }
    }

    /// Feed a chunk; returns what may be forwarded now.
    pub fn push(&mut self, chunk: &str) -> String {
        self.buf.push_str(chunk);
        let mut out = String::new();
        loop {
            match &self.inside {
                Inside::Passthrough(close) => {
                    let close = *close;
                    if let Some(i) = self.buf.find(close) {
                        let end = i + close.len();
                        out.push_str(&self.buf[..end]);
                        self.buf.drain(..end);
                        self.inside = Inside::Nothing;
                    } else {
                        let safe = safe_emit_len(&self.buf, &[close]);
                        out.push_str(&self.buf[..safe]);
                        self.buf.drain(..safe);
                        break;
                    }
                }
                Inside::Scaffold(close) => {
                    if let Some(i) = self.buf.find(close.as_str()) {
                        let end = i + close.len();
                        self.buf.drain(..end);
                        self.inside = Inside::Nothing;
                        self.scaffold_blocks += 1;
                    } else {
                        let safe = safe_emit_len(&self.buf, &[close.as_str()]);
                        self.buf.drain(..safe);
                        break;
                    }
                }
                Inside::Nothing => match self.earliest_marker() {
                    Some((i, open, inside)) => {
                        let before = self.buf[..i].to_string();
                        self.visible(&before, &mut out);
                        let opened = &self.buf[i..i + open.len()];
                        if matches!(inside, Inside::Passthrough(_)) {
                            out.push_str(opened);
                        }
                        self.buf.drain(..i + open.len());
                        self.inside = inside;
                    }
                    None => {
                        let safe = safe_emit_len(&self.buf, &OPEN_MARKERS)
                            .min(partial_snake_tag_start(&self.buf));
                        let ready = self.buf[..safe].to_string();
                        self.buf.drain(..safe);
                        self.visible(&ready, &mut out);
                        break;
                    }
                },
            }
        }
        out
    }

    /// Stream end: forwards what is left, dropping an unclosed scaffold block.
    pub fn finish(&mut self) -> String {
        let pending = std::mem::take(&mut self.buf);
        let mut out = String::new();
        match std::mem::replace(&mut self.inside, Inside::Nothing) {
            Inside::Passthrough(_) => out.push_str(&pending),
            Inside::Scaffold(_) => self.scaffold_blocks += 1,
            Inside::Nothing => self.visible(&pending, &mut out),
        }
        if self.opening == Opening::Undecided && !self.held.is_empty() {
            self.decide(&mut out, true);
        }
        out
    }

    /// Scaffold blocks dropped so far, each the model echoing GIAP's prompt.
    pub fn scaffold_blocks(&self) -> usize {
        self.scaffold_blocks
    }

    /// The tool call the reply opened with, written as text instead of called.
    pub fn text_call(&self) -> Option<&str> {
        self.text_call.as_deref()
    }

    fn earliest_marker(&self) -> Option<(usize, String, Inside)> {
        let passthrough = PAIRED_TAGS.iter().filter_map(|&(open, close)| {
            self.buf
                .find(open)
                .map(|i| (i, open.to_string(), Inside::Passthrough(close)))
        });
        let scaffold = SCAFFOLD_OPENS.iter().filter_map(|&open| {
            self.buf.find(open).map(|i| {
                let close = format!("</{}", &open[1..]);
                (i, open.to_string(), Inside::Scaffold(close))
            })
        });
        let invented = snake_tag(&self.buf).map(|(i, name)| {
            (
                i,
                format!("<{name}>"),
                Inside::Scaffold(format!("</{name}>")),
            )
        });
        passthrough
            .chain(scaffold)
            .chain(invented)
            .min_by_key(|(i, _, _)| *i)
    }

    fn visible(&mut self, text: &str, out: &mut String) {
        match self.opening {
            Opening::Prose => out.push_str(text),
            Opening::TextCall => {}
            Opening::Undecided => {
                self.held.push_str(text);
                self.decide(out, false);
            }
        }
    }

    /// Settles how the reply opens once enough of it is held, or at the end.
    fn decide(&mut self, out: &mut String, at_end: bool) {
        let opening = self.held.trim_start();
        if opening.is_empty() {
            if at_end {
                self.held.clear();
            }
            return;
        }
        let tagged =
            opening.starts_with('<') && opening[1..].starts_with(|c: char| c.is_ascii_alphabetic());
        if tagged {
            let tag_closed = opening.contains('>');
            let call_closed = opening.contains('}') || opening.contains(')');
            let enough = (tag_closed && !opening.starts_with("<call>"))
                || (opening.starts_with("<call>") && call_closed)
                || opening.chars().count() >= OPENING_HOLD_CHARS;
            if !enough && !at_end {
                return;
            }
            if is_text_call(opening) {
                self.text_call = Some(opening.to_string());
                self.opening = Opening::TextCall;
                self.held.clear();
                return;
            }
        }
        self.opening = Opening::Prose;
        out.push_str(&std::mem::take(&mut self.held));
    }
}

/// `SCAFFOLD_TAGS` as opening tags, spelled out so the markers can be `&'static str`.
const SCAFFOLD_OPENS: &[&str] = &[
    "<system-context>",
    "<user-message>",
    "<answer-contract>",
    "<memories>",
    "<turn-context>",
    "<identity>",
    "<instructions>",
    "<context-handling>",
    "<tool-usage>",
    "<tool-failure>",
    "<memory-rules>",
    "<output-quality>",
    "<voice-mode>",
    "<vision>",
    "<conversation-summary>",
    "<home-devices>",
];

/// Every marker that can open a block; built once because `push` runs per token.
static OPEN_MARKERS: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
    PAIRED_TAGS
        .iter()
        .map(|&(open, _)| open)
        .chain(SCAFFOLD_OPENS.iter().copied())
        .collect()
});

/// The first `<snake_case>` opening tag: models invent these for tool syntax and turn records
/// (`<search_web>`, `<turn_history>`), and no HTML or markdown tag has an underscore.
fn snake_tag(buf: &str) -> Option<(usize, &str)> {
    let mut from = 0;
    while let Some(rel) = buf[from..].find('<') {
        let i = from + rel;
        let rest = &buf[i + 1..];
        if let Some(end) = rest.find('>') {
            let name = &rest[..end];
            if is_snake_name(name) {
                return Some((i, name));
            }
        }
        from = i + 1;
    }
    None
}

fn is_snake_name(name: &str) -> bool {
    name.contains('_')
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Where a trailing `<snake_ca` that may still become a tag starts; the buffer's end if none.
fn partial_snake_tag_start(buf: &str) -> usize {
    match buf.rfind('<') {
        Some(i)
            if buf.len() - i <= 48
                && buf[i + 1..]
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') =>
        {
            i
        }
        _ => buf.len(),
    }
}

/// A tag naming a `group__tool`, or the engine's own `call:` form written in plain text.
fn is_text_call(opening: &str) -> bool {
    let rest = opening
        .strip_prefix("<call>")
        .or_else(|| opening.strip_prefix('<'))
        .unwrap_or(opening);
    let name: String = rest
        .trim_start_matches("call:")
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .collect();
    name.contains("__")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `text` one character at a time, as a token stream would split it at worst.
    fn per_char(text: &str) -> (String, ReplyFilter) {
        let mut f = ReplyFilter::new();
        let mut out = String::new();
        for c in text.chars() {
            out.push_str(&f.push(&c.to_string()));
        }
        out.push_str(&f.finish());
        (out, f)
    }

    #[test]
    fn prose_passes_through_unchanged() {
        let reply = "The sky is blue because air scatters short wavelengths.";
        let (out, f) = per_char(reply);
        assert_eq!(out, reply);
        assert_eq!(f.scaffold_blocks(), 0);
        assert!(f.text_call().is_none());
    }

    /// Replies a guest's voice turns began with on the nano (2026-11-02), verbatim.
    /// Verbatim opening of a member's voice reply on the nano (2026-11-02, 3f3b6c5b).
    #[test]
    fn echoed_system_prompt_sections_are_dropped() {
        let (out, f) = per_char(
            "<context-handling>\nAnswer <user-message> only.\n</context-handling>\n<tool-usage>\n\
             Only tools in your schema.\n</tool-usage>\nHerons hunt by standing still.",
        );
        assert_eq!(out.trim(), "Herons hunt by standing still.");
        assert_eq!(f.scaffold_blocks(), 2);
    }

    #[test]
    fn an_echoed_user_message_is_dropped_and_the_answer_kept() {
        let (out, f) = per_char(
            "<user-message>\nWhat is the difference between a crocodile and an alligator?\n\
             </user-message>\nA crocodile and an alligator are both large reptiles.",
        );
        assert_eq!(
            out.trim(),
            "A crocodile and an alligator are both large reptiles."
        );
        assert_eq!(f.scaffold_blocks(), 1);
    }

    #[test]
    fn an_echoed_answer_contract_then_a_written_call_shows_nothing() {
        let (out, f) = per_char(
            "<answer-contract>\nReply with the result, not the route to it. If part of it \
             failed, name that part in ordinary words.\n</answer-contract>\n\
             <call>giap-knowledge__get_wikipedia_article{topic:\"crocodile vs alligator\"}",
        );
        assert_eq!(out, "");
        assert_eq!(f.scaffold_blocks(), 1);
        assert!(f.text_call().unwrap().contains("get_wikipedia_article"));
    }

    #[test]
    fn every_written_call_seen_on_the_nano_is_caught() {
        for leaked in [
            r#"<giap-knowledge__search_web question="what to pack for a day hike" />"#,
            r#"<giap-knowledge__get_wikipedia_article topic="Why is the sky blue?"/>"#,
            r#"<knowledge__get_wikipedia_article topic="why is the sky blue?"/>"#,
            r#"<giap-knowledge__get_wikipedia_article topic="steeping green tea time" />"#,
        ] {
            let (out, f) = per_char(leaked);
            assert_eq!(out, "", "{leaked}");
            assert!(f.text_call().is_some(), "{leaked}");
        }
    }

    #[test]
    fn a_reply_opening_with_ordinary_markup_is_prose() {
        let (out, f) = per_char("<b>Two</b> things matter here.");
        assert_eq!(out, "<b>Two</b> things matter here.");
        assert!(f.text_call().is_none());
    }

    /// The SSE and voice `ThoughtFilter`s strip reasoning across chunks; this one must not eat it,
    /// nor take a quoted scaffold tag inside reasoning for an echo.
    #[test]
    fn reasoning_passes_through_raw_and_scaffold_inside_it_is_not_touched() {
        let reply = "<|channel>thought\nThe <answer-contract> says reply plainly.<channel|>\
                     Here is the answer.";
        let (out, f) = per_char(reply);
        assert_eq!(out, reply);
        assert_eq!(f.scaffold_blocks(), 0);
    }

    #[test]
    fn a_written_call_after_reasoning_is_still_caught() {
        let (out, f) = per_char(
            "<|channel>thought\nI should search.<channel|><giap-knowledge__search_web question=\"x\"/>",
        );
        assert_eq!(out, "<|channel>thought\nI should search.<channel|>");
        assert!(f.text_call().is_some());
    }

    #[test]
    fn an_unclosed_scaffold_block_is_dropped_at_the_end() {
        let (out, f) = per_char("<system-context>\nCURRENT CONTEXT: Monday");
        assert_eq!(out, "");
        assert_eq!(f.scaffold_blocks(), 1);
    }

    /// Verbatim from the nano's clause-gap bench on main 41b4fb48 (2026-11-02).
    #[test]
    fn an_invented_snake_case_tag_is_dropped_with_its_body() {
        let (out, f) = per_char(
            "<search_web>What is the story of how the Great Rift Valley formed</search_web>",
        );
        assert_eq!(out, "");
        assert_eq!(f.scaffold_blocks(), 1);
        let (out, _) = per_char(
            "<turn_history>[{\"turn\":1,\"tool\":\"giap-knowledge__get_wikipedia_article\"}]\
             </turn_history>\nThe Rift Valley formed as plates pulled apart.",
        );
        assert_eq!(out.trim(), "The Rift Valley formed as plates pulled apart.");
    }

    #[test]
    fn prose_and_html_keep_their_underscores_and_tags() {
        let reply = "Use snake_case names, and <b>bold</b> for emphasis; 3 < 5.";
        assert_eq!(per_char(reply).0, reply);
    }

    #[test]
    fn the_spelled_out_openers_match_the_scaffold_tags() {
        let derived: Vec<String> = SCAFFOLD_TAGS.iter().map(|t| format!("<{t}>")).collect();
        assert_eq!(
            SCAFFOLD_OPENS,
            derived.iter().map(String::as_str).collect::<Vec<_>>()
        );
    }

    #[test]
    fn whole_chunks_give_the_same_result_as_single_characters() {
        let reply = "<user-message>\nhi\n</user-message>\nHello there.";
        let mut f = ReplyFilter::new();
        let mut out = f.push(reply);
        out.push_str(&f.finish());
        assert_eq!(out, per_char(reply).0);
    }
}
