//! Turning model output into speakable text.
//!
//! TypeScript ports of these functions live under `pond-desktop/src/`; keep them in step.

fn is_abbrev_suffix(prefix: &str) -> bool {
    let Some(last) = prefix.split_whitespace().next_back() else {
        return false;
    };
    let clean = last.trim_start_matches(|c: char| !c.is_alphanumeric());
    if clean.is_empty() {
        return false;
    }
    let lower = clean.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "dr" | "mr"
            | "mrs"
            | "ms"
            | "prof"
            | "st"
            | "jr"
            | "sr"
            | "vs"
            | "etc"
            | "approx"
            | "dept"
            | "govt"
            | "inc"
            | "ltd"
            | "corp"
            | "min"
            | "max"
            | "temp"
            | "est"
            | "jan"
            | "feb"
            | "mar"
            | "apr"
            | "jun"
            | "jul"
            | "aug"
            | "sep"
            | "sept"
            | "oct"
            | "nov"
            | "dec"
            | "e.g"
            | "i.e"
            | "u.s"
            | "a.m"
            | "p.m"
    ) || (clean.len() == 1 && clean.chars().all(|c| c.is_ascii_alphabetic()))
}

/// What ends a speech unit, which decides the silence played after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// A cut inside a phrase, where no mark came in time.
    Word,
    Clause,
    /// The end of a line in a list.
    ListLine,
    Sentence,
    Paragraph,
    /// The last unit of the text: what follows it is not speech.
    End,
}

impl Boundary {
    /// Silence after a unit, at speed 1.0. Inside one forward pass Kokoro pauses ~0.35 s at a
    /// comma and ~0.5-0.6 s at a full stop, so units joined by these sound like one passage.
    pub fn pause_secs(self) -> f32 {
        match self {
            Self::Word => 0.10,
            Self::Clause => 0.30,
            Self::ListLine => 0.40,
            Self::Sentence => 0.50,
            Self::Paragraph => 0.80,
            Self::End => 0.0,
        }
    }

    /// The boundary a unit's last mark implies, for text that arrives without one (`/tts`).
    pub fn from_text_end(text: &str) -> Self {
        let last = text
            .trim_end()
            .trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}'])
            .chars()
            .next_back();
        match last {
            Some('.' | '?' | '!' | '\u{2026}') => Self::Sentence,
            Some(',' | ';' | ':' | '\u{2014}') => Self::Clause,
            _ => Self::Word,
        }
    }
}

/// Text to say and what follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechUnit {
    pub text: String,
    pub boundary: Boundary,
}

impl SpeechUnit {
    pub fn new(text: impl Into<String>, boundary: Boundary) -> Self {
        Self {
            text: text.into(),
            boundary,
        }
    }

    /// A unit with nothing said about what follows it: its own last mark decides.
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let boundary = Boundary::from_text_end(&text);
        Self { text, boundary }
    }
}

/// Kokoro tokens for `text`, roughly: espeak IPA runs about 1.1 symbols per character of English.
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() * 11).div_ceil(10)
}

/// A unit is cut before it passes this many estimated tokens; Kokoro is most even at 100-200.
pub const UNIT_TOKEN_BUDGET: usize = 200;
/// A clause mark ends a unit only after this many words, so "Sure," stays with its sentence.
const MIN_CLAUSE_WORDS: usize = 3;
/// How much a sentence is assumed to grow past a clause mark, when deciding whether to cut there.
const CLAUSE_LOOKAHEAD_TOKENS: usize = 40;
/// Kept after a sentence's last mark, like the quote in `He said "go."`.
const CLOSERS: &[char] = &['"', '\'', ')', ']', '\u{201d}', '\u{2019}'];

/// How far ahead of the speaker synthesis is, for deciding whether a sentence can wait to be whole.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pace {
    /// Seconds of audio waiting to play, less the synthesis still owed for units handed over.
    pub slack_secs: f32,
    /// Seconds of synthesis per token, as measured.
    pub synth_secs_per_token: f32,
}

impl Pace {
    /// Time to spare: whole sentences, with only the first unit cut short.
    pub const UNHURRIED: Pace = Pace {
        slack_secs: f32::INFINITY,
        synth_secs_per_token: 0.06,
    };
    /// Nothing queued: any clause mark gets sound out sooner.
    pub const BEHIND: Pace = Pace {
        slack_secs: f32::NEG_INFINITY,
        synth_secs_per_token: 0.06,
    };

    /// Whether `tokens` more can be synthesised before the queued audio runs out.
    pub fn can_wait_for(&self, tokens: usize) -> bool {
        self.slack_secs >= tokens as f32 * self.synth_secs_per_token
    }
}

/// Splits a reply into speech units as it streams: the first ends early, at its first clause mark
/// after three words, and later ones are whole sentences unless synthesis has fallen behind.
#[derive(Debug, Default)]
pub struct ReplySplitter {
    buf: String,
    said_one: bool,
}

impl ReplySplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add streamed text and take the units now ready to say.
    pub fn push(&mut self, text: &str, pace: Pace) -> Vec<SpeechUnit> {
        self.buf.push_str(text);
        self.drain(pace, Flush::Streaming)
    }

    /// The stream paused (a tool is running): say the sentences already whole, judging their
    /// breaks by what has arrived, and keep an unfinished one for when it resumes.
    pub fn pause(&mut self, pace: Pace) -> Vec<SpeechUnit> {
        self.drain(pace, Flush::Paused)
    }

    /// The stream is over: whatever is left, the last unit marked [`Boundary::End`].
    pub fn finish(&mut self) -> Vec<SpeechUnit> {
        let mut units = self.drain(Pace::UNHURRIED, Flush::Ended);
        if let Some(last) = units.last_mut() {
            last.boundary = Boundary::End;
        }
        units
    }

    /// Text not yet said.
    pub fn pending(&self) -> &str {
        &self.buf
    }

    fn drain(&mut self, pace: Pace, flush: Flush) -> Vec<SpeechUnit> {
        let mut units = Vec::new();
        let settled = flush != Flush::Streaming;
        loop {
            let skip = self.buf.len() - self.buf.trim_start().len();
            self.buf.drain(..skip);
            if self.buf.is_empty() {
                break;
            }
            let cut = match next_cut(&self.buf, !self.said_one, pace, settled) {
                Some(cut) => cut,
                None if flush == Flush::Ended => Cut {
                    end: self.buf.len(),
                    resume: self.buf.len(),
                    boundary: Boundary::from_text_end(&self.buf),
                },
                None => break,
            };
            let mut text = self.buf[..cut.end].trim().to_string();
            self.buf.drain(..cut.resume);
            if cut.boundary == Boundary::ListLine && !ends_in_mark(&text) {
                text.push(',');
            }
            if !text.is_empty() {
                units.push(SpeechUnit::new(text, cut.boundary));
                self.said_one = true;
            }
        }
        units
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flush {
    /// More text is coming any moment: wait to see what follows a mark.
    Streaming,
    /// More may come later: judge marks by what has arrived.
    Paused,
    /// Nothing more is coming.
    Ended,
}

/// A whole text as speech units, with no time pressure past the first unit.
pub fn split_text(text: &str) -> Vec<SpeechUnit> {
    let mut splitter = ReplySplitter::new();
    let mut units = splitter.push(text, Pace::UNHURRIED);
    units.extend(splitter.finish());
    if let Some(last) = units.last_mut() {
        last.boundary = Boundary::End;
    }
    units
}

/// Take the next unit to synthesise, joining it with the ones queued behind it in the same
/// paragraph while the joined text stays in budget and can be ready before the queued audio ends.
pub fn pack_units(
    queue: &mut std::collections::VecDeque<SpeechUnit>,
    pace: Pace,
) -> Option<SpeechUnit> {
    let mut unit = queue.pop_front()?;
    while let Some(next) = queue.front() {
        if matches!(unit.boundary, Boundary::Paragraph | Boundary::End) {
            break;
        }
        // A list line stays a line, so the markdown pass still sees its marker.
        let joint = if unit.boundary == Boundary::ListLine {
            "\n"
        } else {
            " "
        };
        let tokens = estimate_tokens(&unit.text) + estimate_tokens(&next.text);
        if tokens > UNIT_TOKEN_BUDGET || !pace.can_wait_for(tokens) {
            break;
        }
        let next = queue.pop_front()?;
        unit.text.push_str(joint);
        unit.text.push_str(&next.text);
        unit.boundary = next.boundary;
    }
    Some(unit)
}

fn ends_in_mark(text: &str) -> bool {
    text.trim_end_matches(CLOSERS)
        .ends_with(['.', ',', ';', ':', '!', '?', '\u{2026}', '\u{2014}'])
}

/// A list item: `-`, `*`, `+` or a bullet, or a number with `.` or `)`, then a space.
fn is_list_item(line: &str) -> bool {
    let line = line.trim_start();
    if ["- ", "* ", "+ ", "\u{2022} "]
        .iter()
        .any(|m| line.starts_with(m))
    {
        return true;
    }
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    digits > 0
        && digits <= 3
        && matches!(line[digits..].chars().next(), Some('.' | ')'))
        && line[digits + 1..].starts_with(' ')
}

/// Whether more text could still make `rest` a list item.
fn could_become_list_item(rest: &str) -> bool {
    let rest = rest.trim_start_matches([' ', '\t']);
    rest.len() < 3
        && rest
            .chars()
            .all(|c| c.is_ascii_digit() || "-*+.)\u{2022}".contains(c))
}

struct Cut {
    /// End of the unit's text in the buffer.
    end: usize,
    /// Where the next unit starts.
    resume: usize,
    boundary: Boundary,
}

/// The first place `text` can end a unit, or `None` until more text says where. `settled` means
/// nothing more is about to arrive, so a mark at the end of `text` is judged on what is there.
fn next_cut(text: &str, first: bool, pace: Pace, settled: bool) -> Option<Cut> {
    let at_end = settled;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let (mut words, mut in_word) = (0usize, false);
    let mut line_start = 0usize;
    let mut last_clause: Option<usize> = None;
    let mut last_space: Option<usize> = None;
    let ws_end = |from: usize| from + (text.len() - from - text[from..].trim_start().len());

    for (k, &(i, ch)) in chars.iter().enumerate() {
        if ch.is_whitespace() {
            if in_word {
                in_word = false;
                words += 1;
            }
            last_space = Some(i);
        } else {
            in_word = true;
        }

        if estimate_tokens(&text[..i]) > UNIT_TOKEN_BUDGET {
            let (end, boundary) = match last_clause {
                Some(end) => (end, Boundary::Clause),
                None => (last_space?, Boundary::Word),
            };
            return Some(Cut {
                end,
                resume: end,
                boundary,
            });
        }

        let next = chars.get(k + 1).map(|&(_, c)| c);
        let prev = text[..i].chars().next_back();
        match ch {
            '.' | '?' | '!' | '\u{2026}' => {
                if ch == '.' {
                    let prefix = &text[..i];
                    let digit_before = prev.is_some_and(|c| c.is_ascii_digit());
                    if digit_before && (next.is_none_or(|c| c.is_ascii_digit())) {
                        continue;
                    }
                    if is_abbrev_suffix(prefix) {
                        continue;
                    }
                    let marker = text[line_start..i].trim_start();
                    if !marker.is_empty()
                        && marker.len() <= 3
                        && marker.chars().all(|c| c.is_ascii_digit())
                    {
                        continue;
                    }
                }
                let mut end = i + ch.len_utf8();
                end += text[end..].len() - text[end..].trim_start_matches(['.', '?', '!']).len();
                end += text[end..].len() - text[end..].trim_start_matches(CLOSERS).len();
                let after = ws_end(end);
                if after == end {
                    if end == text.len() && at_end {
                        return Some(Cut {
                            end,
                            resume: end,
                            boundary: Boundary::Sentence,
                        });
                    }
                    if end == text.len() {
                        return None;
                    }
                    continue;
                }
                if after == text.len() && !at_end {
                    return None;
                }
                let gap = &text[end..after];
                let boundary = match gap.matches('\n').count() {
                    0 => Boundary::Sentence,
                    1 if is_list_item(&text[line_start..]) || is_list_item(&text[after..]) => {
                        Boundary::ListLine
                    }
                    1 => Boundary::Sentence,
                    _ => Boundary::Paragraph,
                };
                return Some(Cut {
                    end,
                    resume: after,
                    boundary,
                });
            }
            ',' | ';' | ':' | '\u{2014}' => {
                let digits_around = prev.is_some_and(|c| c.is_ascii_digit())
                    && next.is_some_and(|c| c.is_ascii_digit());
                if matches!(ch, ',' | ':') && digits_around {
                    continue;
                }
                // Only once whitespace follows: the next token may continue the number or word.
                if !next.is_some_and(char::is_whitespace) {
                    continue;
                }
                let end = i + ch.len_utf8();
                // A mark that ends its line is the line's end, which the newline decides.
                if text[end..]
                    .trim_start_matches([' ', '\t'])
                    .starts_with('\n')
                {
                    continue;
                }
                if words + usize::from(in_word) < MIN_CLAUSE_WORDS {
                    continue;
                }
                last_clause = Some(end);
                let lookahead = estimate_tokens(&text[..end]) + CLAUSE_LOOKAHEAD_TOKENS;
                if first || !pace.can_wait_for(lookahead) {
                    return Some(Cut {
                        end,
                        resume: end,
                        boundary: Boundary::Clause,
                    });
                }
            }
            '\n' => {
                let after = ws_end(i);
                if after == text.len() && !at_end {
                    return None;
                }
                let line = &text[line_start..i];
                if text[i..after].matches('\n').count() >= 2 {
                    return Some(Cut {
                        end: i,
                        resume: after,
                        boundary: Boundary::Paragraph,
                    });
                }
                let rest = &text[after..];
                if !at_end && !rest.contains('\n') && could_become_list_item(rest) {
                    return None;
                }
                if !line.trim().is_empty() && (is_list_item(line) || is_list_item(rest)) {
                    return Some(Cut {
                        end: i,
                        resume: after,
                        boundary: Boundary::ListLine,
                    });
                }
                line_start = i + 1;
            }
            _ => {}
        }
    }
    None
}

/// The tags GIAP wraps its prompt and a turn's context in; any of them in a reply is an echo.
pub const SCAFFOLD_TAGS: &[&str] = &[
    "system-context",
    "user-message",
    "answer-contract",
    "memories",
    "turn-context",
    "identity",
    "instructions",
    "context-handling",
    "tool-usage",
    "tool-failure",
    "memory-rules",
    "output-quality",
    "voice-mode",
    "vision",
    "conversation-summary",
    "home-devices",
];

/// Drop markup a model wrote as text, so it is never spoken: a closed scaffold block with its body,
/// any tag-shaped span, and a `group__tool` name with the arguments written after it.
pub fn strip_markup_for_speech(text: &str) -> String {
    let mut s = text.to_string();
    for tag in SCAFFOLD_TAGS {
        s = drop_closed_blocks(&s, &format!("<{tag}>"), &format!("</{tag}>"));
    }
    s = drop_snake_case_blocks(&s);
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = tag_span_end(&chars, i).or_else(|| tool_call_end(&chars, i)) {
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Closed `<snake_case>` blocks with their bodies: models invent them for tool syntax and turn
/// records, and no HTML or markdown tag has an underscore.
fn drop_snake_case_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    'scan: while let Some(i) = rest.find('<') {
        let after = &rest[i + 1..];
        if let Some(end) = after.find('>') {
            let name = &after[..end];
            let snake = name.contains('_')
                && name.starts_with(|c: char| c.is_ascii_lowercase())
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
            if snake {
                let close = format!("</{name}>");
                if let Some(len) = after[end + 1..].find(&close) {
                    out.push_str(&rest[..i]);
                    rest = &after[end + 1 + len + close.len()..];
                    continue 'scan;
                }
            }
        }
        out.push_str(&rest[..=i]);
        rest = after;
    }
    out.push_str(rest);
    out
}

fn drop_closed_blocks(text: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let Some(len) = rest[start..].find(close) else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + len + close.len()..];
    }
    out.push_str(rest);
    out
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':')
}

/// End of a `<name attr="v" .../>` span at `i`. An unclosed one counts only for a tool-like name,
/// because a clause split can cut a written call in half but rarely an HTML tag.
fn tag_span_end(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&'<') {
        return None;
    }
    let mut j = i + 1;
    if chars.get(j) == Some(&'/') {
        j += 1;
    }
    if !chars.get(j).is_some_and(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    let name_start = j;
    while chars.get(j).copied().is_some_and(is_name_char) {
        j += 1;
    }
    let tool_like = chars[name_start..j].windows(2).any(|w| w == ['_', '_']);
    loop {
        while chars.get(j).is_some_and(|c| c.is_whitespace()) {
            j += 1;
        }
        match chars.get(j) {
            None => return tool_like.then_some(chars.len()),
            Some('>') => return Some(j + 1),
            Some('/') if chars.get(j + 1) == Some(&'>') => return Some(j + 2),
            Some('/') if j + 1 == chars.len() => return tool_like.then_some(chars.len()),
            Some(c) if is_name_char(*c) => {
                while chars.get(j).copied().is_some_and(is_name_char) {
                    j += 1;
                }
                while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                    j += 1;
                }
                if chars.get(j) == Some(&'=') {
                    j += 1;
                    while chars.get(j).is_some_and(|c| c.is_whitespace()) {
                        j += 1;
                    }
                    match chars.get(j) {
                        Some(q @ ('"' | '\'')) => {
                            let q = *q;
                            j += 1;
                            while chars.get(j).is_some_and(|c| *c != q) {
                                j += 1;
                            }
                            if j >= chars.len() {
                                return tool_like.then_some(chars.len());
                            }
                            j += 1;
                        }
                        _ => {
                            while chars
                                .get(j)
                                .is_some_and(|c| !c.is_whitespace() && *c != '>' && *c != '/')
                            {
                                j += 1;
                            }
                        }
                    }
                }
            }
            Some(_) => return None,
        }
    }
}

/// End of a `group__tool` name at a word start, with any `{...}` or `(...)` written after it.
fn tool_call_end(chars: &[char], i: usize) -> Option<usize> {
    if (i > 0 && chars[i - 1].is_ascii_alphanumeric()) || !chars[i].is_ascii_alphanumeric() {
        return None;
    }
    let mut j = i;
    while chars
        .get(j)
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
    {
        j += 1;
    }
    if j == i || chars.get(j) != Some(&'_') || chars.get(j + 1) != Some(&'_') {
        return None;
    }
    j += 2;
    let tool_start = j;
    while chars
        .get(j)
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
    {
        j += 1;
    }
    if j == tool_start {
        return None;
    }
    let (open, close) = match chars.get(j) {
        Some('{') => ('{', '}'),
        Some('(') => ('(', ')'),
        _ => return Some(j),
    };
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    while let Some(&c) = chars.get(j) {
        j += 1;
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == open => depth += 1,
            None if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            None => {}
        }
    }
    Some(chars.len())
}

/// Convert markdown to plain text for TTS, then apply `normalize_for_speech`.
///
/// Drops markup written as text, code blocks, images, link URLs and horizontal rules; other
/// markdown keeps its text.
pub fn strip_markdown_for_speech(text: &str) -> String {
    let text = strip_markup_for_speech(text);
    let mut out = String::with_capacity(text.len());
    let mut in_code_fence = false;
    let mut previous_was_list_item = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Code fence toggle — skip body of code blocks
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_fence = !in_code_fence;
            continue;
        }
        if in_code_fence {
            continue;
        }

        // Horizontal rules: --- *** ___ (3+ of the same char, nothing else)
        if is_hr(trimmed) {
            continue;
        }

        let list_item = is_list_item(trimmed);
        let content = strip_line_prefix(trimmed);
        let content = strip_inline_md(content);
        let content = content.trim().to_string();
        if !content.is_empty() {
            if !out.is_empty() {
                // A list line with no mark of its own would run into the next one unpaused.
                let list_join = list_item || previous_was_list_item;
                out.push_str(if list_join && !ends_in_mark(&out) {
                    ", "
                } else {
                    " "
                });
            }
            out.push_str(&content);
            previous_was_list_item = list_item;
        }
    }

    normalize_for_speech(out.trim())
}

pub fn is_hr(s: &str) -> bool {
    if s.len() < 3 {
        return false;
    }
    let first = s.chars().next().unwrap_or(' ');
    if !matches!(first, '-' | '*' | '_') {
        return false;
    }
    s.chars().all(|c| c == first || c == ' ')
}

/// Strip leading structural markdown from a line (header `#`, blockquote `>`, list marker).
pub fn strip_line_prefix(line: &str) -> &str {
    // Headers: ### text → text
    if line.starts_with('#') {
        return line.trim_start_matches('#').trim_start();
    }
    // Blockquotes: > text
    if let Some(rest) = line.strip_prefix("> ").or_else(|| line.strip_prefix('>')) {
        return rest.trim_start();
    }
    // Unordered lists: - / * / + / •
    if let Some(rest) = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
        .or_else(|| line.strip_prefix("• "))
    {
        return rest
            .strip_prefix("[ ] ")
            .or_else(|| rest.strip_prefix("[x] "))
            .or_else(|| rest.strip_prefix("[X] "))
            .unwrap_or(rest);
    }
    // Ordered lists: 1. 2. 10. etc.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        let rest = &line[i + 2..];
        return rest
            .strip_prefix("[ ] ")
            .or_else(|| rest.strip_prefix("[x] "))
            .or_else(|| rest.strip_prefix("[X] "))
            .unwrap_or(rest);
    }
    line
}

/// Strip inline markdown markers from a string, handling bold, italic, code, links, images.
pub fn strip_inline_md(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        // Images: ![alt](url) → dropped
        if chars[i] == '!' && chars.get(i + 1) == Some(&'[') {
            if let Some((end, _)) = find_link(&chars, i + 1) {
                i = end;
                continue;
            }
        }

        // Links: [text](url) → text
        if chars[i] == '[' {
            if let Some((end, text)) = find_link(&chars, i) {
                out.push_str(&text);
                i = end;
                continue;
            }
        }

        // Strikethrough: ~~text~~
        if chars.get(i..i + 2) == Some(&['~', '~']) {
            if let Some(close) = find_marker_close(&chars, i + 2, &['~', '~']) {
                out.push_str(&chars[i + 2..close].iter().collect::<String>());
                i = close + 2;
                continue;
            }
        }

        // Bold: **text** or __text__
        if chars.get(i..i + 2) == Some(&['*', '*']) || chars.get(i..i + 2) == Some(&['_', '_']) {
            let marker = [chars[i], chars[i + 1]];
            if let Some(close) = find_marker_close(&chars, i + 2, &marker) {
                out.push_str(&chars[i + 2..close].iter().collect::<String>());
                i = close + 2;
                continue;
            }
        }

        // Italic: *text* or _text_
        if chars[i] == '*' || chars[i] == '_' {
            let marker = [chars[i]];
            if let Some(close) = find_marker_close(&chars, i + 1, &marker) {
                out.push_str(&chars[i + 1..close].iter().collect::<String>());
                i = close + 1;
                continue;
            }
        }

        // Inline code: `text`
        if chars[i] == '`' {
            if let Some(close) = find_marker_close(&chars, i + 1, &['`']) {
                out.push_str(&chars[i + 1..close].iter().collect::<String>());
                i = close + 1;
                continue;
            }
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

/// Parse a `[text](url)` link at `start`; returns `(end, text)`, `end` one past the `)`.
pub fn find_link(chars: &[char], start: usize) -> Option<(usize, String)> {
    if chars.get(start) != Some(&'[') {
        return None;
    }
    // Find closing ]
    let mut depth = 0usize;
    let mut j = start;
    while j < chars.len() {
        match chars[j] {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        j += 1;
    }
    if j >= chars.len() {
        return None;
    }
    let text_end = j; // index of `]`
    if chars.get(j + 1) != Some(&'(') {
        return None;
    }
    // Find closing )
    let mut k = j + 2;
    let mut depth2 = 1usize;
    while k < chars.len() && depth2 > 0 {
        match chars[k] {
            '(' => depth2 += 1,
            ')' => depth2 -= 1,
            _ => {}
        }
        k += 1;
    }
    if depth2 != 0 {
        return None;
    }
    let link_text: String = chars[start + 1..text_end].iter().collect();
    Some((k, link_text))
}

/// Find the closing `marker` from `start`; returns where it begins, not one past its end.
pub fn find_marker_close(chars: &[char], start: usize, marker: &[char]) -> Option<usize> {
    let mlen = marker.len();
    let limit = chars.len().saturating_sub(mlen - 1);
    for i in start..limit {
        if &chars[i..i + mlen] == marker {
            return Some(i);
        }
    }
    None
}

// ── Lookup tables for TTS normalization ──────────────────────────────────────

/// Unit suffixes matched after a number. First match wins: a unit must precede its prefixes.
pub const UNIT_SUFFIXES: &[(&str, &str)] = &[
    // ── Compound / slash units ──
    ("km/h", " kilometers per hour"),
    ("mi/h", " miles per hour"),
    ("KB/s", " kilobytes per second"),
    ("MB/s", " megabytes per second"),
    ("m/s", " meters per second"),
    ("ft/s", " feet per second"),
    ("fl oz", " fluid ounces"),
    // ── Data (IEC binary) ──
    ("KiB", " kibibytes"),
    ("MiB", " mebibytes"),
    ("GiB", " gibibytes"),
    ("TiB", " tebibytes"),
    // ── Data speed ──
    ("kbps", " kilobits per second"),
    ("Mbps", " megabits per second"),
    ("Gbps", " gigabits per second"),
    // ── Energy (long) ──
    ("kWh", " kilowatt hours"),
    ("kcal", " kilocalories"),
    ("BTU", " B T U"),
    // ── Frequency ──
    ("THz", " terahertz"),
    ("GHz", " gigahertz"),
    ("MHz", " megahertz"),
    ("kHz", " kilohertz"),
    // ── Power ──
    ("GW", " gigawatts"),
    ("MW", " megawatts"),
    ("kW", " kilowatts"),
    ("mW", " milliwatts"),
    // ── Voltage ──
    ("kV", " kilovolts"),
    ("mV", " millivolts"),
    // ── Current ──
    ("mA", " milliamps"),
    ("μA", " microamps"),
    // ── Resistance ──
    ("MΩ", " megaohms"),
    ("kΩ", " kilohms"),
    // ── Pressure ──
    ("MPa", " megapascals"),
    ("kPa", " kilopascals"),
    ("mmHg", " millimeters of mercury"),
    ("atm", " atmospheres"),
    ("bar", " bar"),
    ("psi", " P S I"),
    // ── Energy ──
    ("MJ", " megajoules"),
    ("kJ", " kilojoules"),
    ("cal", " calories"),
    ("eV", " electron volts"),
    ("Wh", " watt hours"),
    // ── Sound ──
    ("dBA", " D B A"),
    ("dB", " decibels"),
    // ── Duration ──
    ("hrs", " hours"),
    ("sec", " seconds"),
    ("min", " minutes"),
    ("ms", " milliseconds"),
    ("ns", " nanoseconds"),
    ("μs", " microseconds"),
    ("hr", " hours"),
    // ── Data storage ──
    ("KB", " kilobytes"),
    ("MB", " megabytes"),
    ("GB", " gigabytes"),
    ("TB", " terabytes"),
    ("PB", " petabytes"),
    ("EB", " exabytes"),
    // ── Speed ──
    ("mph", " miles per hour"),
    ("bps", " bits per second"),
    // ── Area (with superscript) ──
    ("km²", " square kilometers"),
    ("cm²", " square centimeters"),
    ("m²", " square meters"),
    ("ft²", " square feet"),
    ("in²", " square inches"),
    ("cm³", " cubic centimeters"),
    ("m³", " cubic meters"),
    ("ha", " hectares"),
    // ── Length ──
    ("km", " kilometers"),
    ("cm", " centimeters"),
    ("mm", " millimeters"),
    ("nm", " nanometers"),
    ("μm", " micrometers"),
    ("mi", " miles"),
    ("ft", " feet"),
    ("yd", " yards"),
    // ── Weight ──
    ("kg", " kilograms"),
    ("mg", " milligrams"),
    ("μg", " micrograms"),
    ("lbs", " pounds"),
    ("lb", " pounds"),
    ("oz", " ounces"),
    ("st", " stone"),
    // ── Currency ──
    ("KES", " Kenyan shillings"),
    ("kes", " Kenyan shillings"),
    ("KSh", " Kenyan shillings"),
    ("Ksh", " Kenyan shillings"),
    ("ksh", " Kenyan shillings"),
    ("KSH", " Kenyan shillings"),
    // ── Volume ──
    ("mL", " milliliters"),
    ("dL", " deciliters"),
    ("kL", " kiloliters"),
    ("gal", " gallons"),
    ("qt", " quarts"),
    ("pt", " pints"),
    // ── Short units (last: shortest match) ──
    ("Hz", " hertz"),
    ("Pa", " pascals"),
    ("W", " watts"),
    ("V", " volts"),
    ("A", " amps"),
    ("J", " joules"),
    ("Ω", " ohms"),
    ("L", " liters"),
    ("m", " meters"),
    ("g", " grams"),
];

/// Currency symbols: (char, singular, plural).
pub const CURRENCY_SYMBOLS: &[(char, &str, &str)] = &[
    ('$', "dollar", "dollars"),
    ('£', "pound", "pounds"),
    ('€', "euro", "euros"),
    ('¥', "yen", "yen"),
    ('₹', "rupee", "rupees"),
    ('₽', "ruble", "rubles"),
    ('₩', "won", "won"),
    ('₪', "shekel", "shekels"),
    ('₦', "naira", "naira"),
    ('₱', "peso", "pesos"),
    ('₺', "lira", "lira"),
    ('₴', "hryvnia", "hryvnias"),
    ('₵', "cedi", "cedis"),
    ('₡', "colon", "colones"),
    ('₫', "dong", "dong"),
    ('₭', "kip", "kip"),
    ('₮', "tugrik", "tugriks"),
    ('₧', "peseta", "pesetas"),
    ('₣', "franc", "francs"),
];

/// Standalone single-character symbols.
pub const STANDALONE_SYMBOLS: &[(char, &str)] = &[
    // Math
    ('±', "plus or minus "),
    ('×', " times "),
    ('÷', " divided by "),
    ('∞', "infinity"),
    ('≈', "approximately "),
    ('≤', "less than or equal to "),
    ('≥', "greater than or equal to "),
    ('≠', "not equal to "),
    ('√', "square root of "),
    ('π', "pi"),
    ('²', " squared"),
    ('³', " cubed"),
    // Fractions
    ('½', "one half"),
    ('⅓', "one third"),
    ('⅔', "two thirds"),
    ('¼', "one quarter"),
    ('¾', "three quarters"),
    ('⅕', "one fifth"),
    ('⅖', "two fifths"),
    ('⅗', "three fifths"),
    ('⅘', "four fifths"),
    ('⅙', "one sixth"),
    ('⅚', "five sixths"),
    ('⅛', "one eighth"),
    ('⅜', "three eighths"),
    ('⅝', "five eighths"),
    ('⅞', "seven eighths"),
    // Legal / typographic
    ('©', "copyright"),
    ('®', "registered"),
    ('™', "trademark"),
    ('§', "section"),
    ('¶', "paragraph"),
    ('†', ""),
    ('‡', ""),
    ('•', ", "),
    ('—', ", "),
];

/// Convert symbols and abbreviations to spoken words so TTS pronounces them.
pub fn normalize_for_speech(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut out = String::with_capacity(len + len / 4);
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        // ── Time: 3:45pm / 15:30 / 9am ────────────────────────────────────────
        if ch.is_ascii_digit() && (i == 0 || !chars[i - 1].is_ascii_digit()) {
            if let Some((spoken, advance)) = try_read_time(&chars, i) {
                out.push_str(&spoken);
                i += advance;
                continue;
            }
        }

        // ── Unit suffix after number: 5kg → "5 kilograms" ─────────────────────
        if i > 0 && chars[i - 1].is_ascii_digit() && !ch.is_ascii_digit() {
            if let Some((spoken, advance)) = try_read_unit_suffix(&chars, i) {
                out.push_str(&spoken);
                i += advance;
                continue;
            }
        }

        // ── Degree symbol ──────────────────────────────────────────────────────
        if ch == '°' {
            match chars.get(i + 1) {
                Some('C') | Some('c') => {
                    out.push_str(" degrees Celsius");
                    i += 2;
                    continue;
                }
                Some('F') | Some('f') => {
                    out.push_str(" degrees Fahrenheit");
                    i += 2;
                    continue;
                }
                Some('K') | Some('k') => {
                    out.push_str(" kelvin");
                    i += 2;
                    continue;
                }
                _ => {
                    out.push_str(" degrees");
                    i += 1;
                    continue;
                }
            }
        }

        // ── Percent ────────────────────────────────────────────────────────────
        if ch == '%' {
            out.push_str(" percent");
            i += 1;
            continue;
        }

        // ── Abbreviations: e.g. → "for example", Dr. → "doctor" ──────────────
        if ch.is_alphabetic() {
            if let Some((expansion, advance)) = try_read_abbreviation(&chars, i) {
                out.push_str(&expansion);
                i += advance;
                continue;
            }
        }

        // ── Period / dot — context-dependent ────────────────────────────────────
        if ch == '.' {
            // "3.14" → "3 point 14", unless a unit follows: "3.5GHz" → "3.5 gigahertz".
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = chars.get(i + 1).map_or(false, |c| c.is_ascii_digit());
            if prev_digit && next_digit {
                let mut j = i + 1;
                while j < len && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let has_unit = try_read_unit_suffix(&chars, j).is_some();
                if has_unit {
                    out.push(ch);
                    i += 1;
                    continue;
                }
                out.push_str(" point ");
                i += 1;
                continue;
            }
            // Between letters (domain-like): "google.com" → "google dot com"
            let prev_alpha = i > 0 && chars[i - 1].is_alphabetic();
            let next_alpha = chars.get(i + 1).map_or(false, |c| c.is_alphabetic());
            if prev_alpha && next_alpha {
                out.push_str(" dot ");
                i += 1;
                continue;
            }
            // Sentence-ending / other punctuation: pass through for TTS
            out.push(ch);
            i += 1;
            continue;
        }

        // ── Currency symbols (table-driven) ────────────────────────────────────
        if let Some(&(_, singular, plural)) = CURRENCY_SYMBOLS.iter().find(|&&(c, _, _)| c == ch) {
            let (num_str, advance) = read_number(&chars, i + 1);
            if advance > 0 {
                let is_one = num_str == "1" || num_str == "1.0" || num_str == "1.00";
                let unit = if is_one { singular } else { plural };
                out.push_str(&num_str);
                out.push(' ');
                out.push_str(unit);
                i += 1 + advance;
                continue;
            }
        }

        // ── Kenyan Currency Prefix: KSh / KES ──────────────────────────────────
        if (ch == 'K' || ch == 'k') && (i == 0 || !chars[i - 1].is_alphabetic()) {
            let window: String = chars[i..len.min(i + 4)].iter().collect();
            let upper = window.to_ascii_uppercase();
            if upper.starts_with("KSH") || upper.starts_with("KES") {
                let code_len = 3;
                let after = i + code_len;
                let mut num_start = after;
                if num_start < len && chars[num_start] == '.' {
                    num_start += 1;
                }
                if num_start < len && chars[num_start] == ' ' {
                    num_start += 1;
                }
                let (num_str, advance) = read_number(&chars, num_start);
                if advance > 0 {
                    out.push_str(&num_str);
                    out.push_str(" Kenyan shillings");
                    i = num_start + advance;
                    continue;
                } else if after >= len || !chars[after].is_alphabetic() {
                    out.push_str("Kenyan shillings");
                    i = if after < len && chars[after] == '.' {
                        after + 1
                    } else {
                        after
                    };
                    continue;
                }
            }
        }

        // ── Ampersand ──────────────────────────────────────────────────────────
        if ch == '&' {
            let prev_space = i == 0 || chars[i - 1].is_whitespace();
            let next_space = chars.get(i + 1).map_or(true, |c| c.is_whitespace());
            if prev_space || next_space {
                out.push_str("and");
                i += 1;
                continue;
            }
        }

        // ── At-sign ────────────────────────────────────────────────────────────
        if ch == '@' {
            let prev_space = i == 0 || chars[i - 1].is_whitespace();
            let next_space = chars.get(i + 1).map_or(true, |c| c.is_whitespace());
            if prev_space || next_space {
                out.push_str("at");
                i += 1;
                continue;
            }
        }

        // ── Number sign: #5 → "number 5" ──────────────────────────────────────
        if ch == '#' && chars.get(i + 1).map_or(false, |c| c.is_ascii_digit()) {
            out.push_str("number ");
            i += 1;
            continue;
        }

        // ── En dash: 3–5 → "3 to 5", otherwise a pause ───────────────────────
        if ch == '–' {
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = chars.get(i + 1).map_or(false, |c| c.is_ascii_digit());
            if prev_digit && next_digit {
                out.push_str(" to ");
            } else {
                out.push_str(", ");
            }
            i += 1;
            continue;
        }

        // ── Standalone symbol table ────────────────────────────────────────────
        if let Some(&(_, spoken)) = STANDALONE_SYMBOLS.iter().find(|&&(c, _)| c == ch) {
            out.push_str(spoken);
            i += 1;
            continue;
        }

        out.push(ch);
        i += 1;
    }

    // Collapse runs of whitespace from symbol substitutions.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Abbreviations with periods, matched case-insensitively; keys must be lowercase.
pub const ABBREVIATIONS: &[(&str, &str)] = &[
    ("e.g.", "for example"),
    ("i.e.", "that is"),
    ("etc.", "etcetera"),
    ("vs.", "versus"),
    ("approx.", "approximately"),
    ("dept.", "department"),
    ("govt.", "government"),
    ("assn.", "association"),
    ("inc.", "incorporated"),
    ("corp.", "corporation"),
    ("ltd.", "limited"),
    ("prof.", "professor"),
    ("dr.", "doctor"),
    ("mr.", "mister"),
    ("mrs.", "missus"),
    ("ms.", "miss"),
    ("jr.", "junior"),
    ("sr.", "senior"),
    ("st.", "saint"),
    ("ave.", "avenue"),
    ("blvd.", "boulevard"),
    ("ft.", "fort"),
    ("mt.", "mount"),
    ("no.", "number"),
    ("vol.", "volume"),
    ("ch.", "chapter"),
    ("pg.", "page"),
    ("fig.", "figure"),
    ("approx.", "approximately"),
    ("max.", "maximum"),
    ("min.", "minimum"),
    ("temp.", "temperature"),
    ("est.", "established"),
    ("jan.", "January"),
    ("feb.", "February"),
    ("mar.", "March"),
    ("apr.", "April"),
    ("jun.", "June"),
    ("jul.", "July"),
    ("aug.", "August"),
    ("sep.", "September"),
    ("oct.", "October"),
    ("nov.", "November"),
    ("dec.", "December"),
];

/// Expand an abbreviation starting a word at `i`, returning `(expansion, chars_consumed)`.
pub fn try_read_abbreviation(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let at_boundary = i == 0
        || chars[i - 1].is_whitespace()
        || matches!(chars[i - 1], '(' | ',' | '"' | '\'' | '[');
    if !at_boundary {
        return None;
    }

    // 10 must cover the longest abbreviation.
    let window_end = (i + 10).min(len);
    let window: String = chars[i..window_end]
        .iter()
        .map(|c| c.to_ascii_lowercase())
        .collect();

    for &(abbrev, expansion) in ABBREVIATIONS {
        if window.starts_with(abbrev) {
            let consumed = abbrev.chars().count();
            return Some((expansion.to_string(), consumed));
        }
    }
    None
}

/// Match a unit suffix just after a number at `i`, allowing one space ("5 kg").
pub fn try_read_unit_suffix(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let (unit_start, space_consumed) = if i < len && chars[i] == ' ' {
        (i + 1, 1usize)
    } else {
        (i, 0usize)
    };
    if unit_start >= len {
        return None;
    }

    for &(suffix, spoken) in UNIT_SUFFIXES {
        let suffix_chars: Vec<char> = suffix.chars().collect();
        let slen = suffix_chars.len();
        if unit_start + slen > len {
            continue;
        }

        let matches = suffix_chars
            .iter()
            .enumerate()
            .all(|(j, &sc)| chars[unit_start + j] == sc);
        if !matches {
            continue;
        }

        // Alphabetic continuation guard
        let after = unit_start + slen;
        if after < len && chars[after].is_alphabetic() {
            continue;
        }

        return Some((spoken.to_string(), space_consumed + slen));
    }
    None
}

/// Scan a decimal number at `start`, returning `(text, chars_consumed)`; `("", 0)` if none.
pub fn read_number(chars: &[char], start: usize) -> (String, usize) {
    let mut j = start;
    let mut num_str = String::new();
    while j < chars.len() {
        if chars[j].is_ascii_digit() {
            num_str.push(chars[j]);
            j += 1;
        } else if chars[j] == ','
            && j > start
            && chars[j - 1].is_ascii_digit()
            && chars.get(j + 1).map_or(false, |c| c.is_ascii_digit())
            && chars.get(j + 2).map_or(false, |c| c.is_ascii_digit())
            && chars.get(j + 3).map_or(false, |c| c.is_ascii_digit())
            && (j + 4 >= chars.len()
                || !chars[j + 4].is_ascii_digit()
                || chars[j + 4] == '.'
                || chars[j + 4] == ',')
        {
            // Thousands separator comma between digits: e.g. 1,000 or 1,250,000
            j += 1;
        } else {
            break;
        }
    }
    if chars.get(j) == Some(&'.') && chars.get(j + 1).map_or(false, |c| c.is_ascii_digit()) {
        num_str.push('.');
        j += 1;
        while j < chars.len() && chars[j].is_ascii_digit() {
            num_str.push(chars[j]);
            j += 1;
        }
    }
    if j == start {
        return (String::new(), 0);
    }
    (num_str, j - start)
}

/// Parse a time like `3:45pm`, `15:30` or `9am` at `start`, returning `(spoken, consumed)`.
pub fn try_read_time(chars: &[char], start: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let mut j = start;

    // ── Hours: 1-2 digits, value 0-23 ────────────────────────────────────────
    let h_start = j;
    while j < len && chars[j].is_ascii_digit() && j - h_start < 2 {
        j += 1;
    }
    if j == h_start {
        return None;
    }
    let hour: u32 = chars[h_start..j].iter().collect::<String>().parse().ok()?;
    if hour > 23 {
        return None;
    }
    let hour_str: String = chars[h_start..j].iter().collect();

    // ── Optional :MM ─────────────────────────────────────────────────────────
    let mut minute_str: Option<String> = None;
    if chars.get(j) == Some(&':') {
        let d1 = chars.get(j + 1)?;
        let d2 = chars.get(j + 2)?;
        if d1.is_ascii_digit() && d2.is_ascii_digit() {
            let min: u32 = format!("{}{}", d1, d2).parse().ok()?;
            if min > 59 {
                return None;
            }
            minute_str = Some(format!("{}{}", d1, d2));
            j += 3; // consume :MM
        } else {
            return None;
        }
    }

    // ── Optional whitespace before am/pm ─────────────────────────────────────
    let ws_j = j;
    while j < len && chars[j] == ' ' {
        j += 1;
    }

    // ── Optional am/pm ───────────────────────────────────────────────────────
    let ampm = if j + 1 < len {
        let a = chars[j].to_ascii_lowercase();
        let b = chars[j + 1].to_ascii_lowercase();
        if (a == 'a' || a == 'p') && b == 'm' {
            // Must NOT be followed by another letter (avoids "amplitude" → "AM plitude")
            if chars.get(j + 2).map_or(true, |c| !c.is_alphabetic()) {
                j += 2;
                Some(if a == 'a' { "AM" } else { "PM" })
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // A bare number is not a time: require `:MM` or `am/pm`.
    if minute_str.is_none() && ampm.is_none() {
        return None;
    }

    // If we consumed whitespace but found no am/pm, roll it back.
    if ampm.is_none() {
        j = ws_j;
    }

    // ── Build spoken form ─────────────────────────────────────────────────────
    let mut spoken = hour_str;
    if let Some(ref m) = minute_str {
        // Skip "00" minutes when am/pm is present: "3:00 PM" → "3 PM"
        if m != "00" || ampm.is_none() {
            spoken.push(' ');
            spoken.push_str(m);
        }
    }
    if let Some(ap) = ampm {
        spoken.push(' ');
        spoken.push_str(ap);
    }

    Some((spoken, j - start))
}

/// Strip `<think>…</think>` blocks from a streaming chunk; `in_block` carries across chunks.
pub fn filter_thinking(chunk: &str, mut in_block: bool) -> (String, bool) {
    let mut visible = String::with_capacity(chunk.len());
    let mut rest = chunk;

    loop {
        if in_block {
            if let Some(end) = rest.find("</think>") {
                rest = &rest[end + "</think>".len()..];
                in_block = false;
            } else {
                break;
            }
        } else {
            if let Some(start) = rest.find("<think>") {
                visible.push_str(&rest[..start]);
                rest = &rest[start + "<think>".len()..];
                in_block = true;
            } else {
                visible.push_str(rest);
                break;
            }
        }
    }

    (visible, in_block)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_units_last_mark_names_its_boundary() {
        assert_eq!(
            Boundary::from_text_end("It is 23 degrees."),
            Boundary::Sentence
        );
        assert_eq!(Boundary::from_text_end("Is it? "), Boundary::Sentence);
        assert_eq!(
            Boundary::from_text_end("He said \"go.\""),
            Boundary::Sentence
        );
        assert_eq!(Boundary::from_text_end("a long time,"), Boundary::Clause);
        assert_eq!(Boundary::from_text_end("You will need:"), Boundary::Clause);
        assert_eq!(
            Boundary::from_text_end("The weather in Nairobi"),
            Boundary::Word
        );
        assert_eq!(Boundary::from_text_end(""), Boundary::Word);
    }

    #[test]
    fn pauses_rise_with_the_break_they_mark() {
        let order = [
            Boundary::End,
            Boundary::Word,
            Boundary::Clause,
            Boundary::ListLine,
            Boundary::Sentence,
            Boundary::Paragraph,
        ];
        for pair in order.windows(2) {
            assert!(pair[0].pause_secs() < pair[1].pause_secs(), "{pair:?}");
        }
    }

    fn texts(units: &[SpeechUnit]) -> Vec<&str> {
        units.iter().map(|u| u.text.as_str()).collect()
    }

    fn boundaries(units: &[SpeechUnit]) -> Vec<Boundary> {
        units.iter().map(|u| u.boundary).collect()
    }

    /// A reply as the model streams it: each word, then the space after it.
    fn tokens_of(reply: &str) -> Vec<String> {
        reply
            .split_inclusive(char::is_whitespace)
            .flat_map(|w| {
                let (word, space) = w.split_at(w.trim_end().len());
                [word.to_string(), space.to_string()]
            })
            .filter(|t| !t.is_empty())
            .collect()
    }

    fn stream(reply: &str, pace: Pace) -> Vec<SpeechUnit> {
        let mut splitter = ReplySplitter::new();
        let mut units = Vec::new();
        for token in tokens_of(reply) {
            units.extend(splitter.push(&token, pace));
        }
        units.extend(splitter.finish());
        units
    }

    #[test]
    fn a_whole_text_splits_at_sentences() {
        let units = split_text("Hello world. How are you? I'm fine!\nNew paragraph.");
        assert_eq!(
            texts(&units),
            [
                "Hello world.",
                "How are you?",
                "I'm fine!",
                "New paragraph."
            ]
        );
        assert_eq!(
            boundaries(&units),
            [
                Boundary::Sentence,
                Boundary::Sentence,
                Boundary::Sentence,
                Boundary::End
            ]
        );
    }

    #[test]
    fn the_first_unit_ends_at_its_first_clause_after_three_words() {
        let units = split_text("I would be glad to help with that, and here are the details.");
        assert_eq!(
            texts(&units),
            [
                "I would be glad to help with that,",
                "and here are the details."
            ]
        );
        assert_eq!(boundaries(&units), [Boundary::Clause, Boundary::End]);
        assert_eq!(
            texts(&split_text("Sure, let's start the project.")),
            ["Sure, let's start the project."],
            "a clause of two words stays with its sentence"
        );
    }

    #[test]
    fn later_sentences_stay_whole_while_there_is_time() {
        let reply = "Sure. It is sunny, and the high is 26 degrees, with wind from the east.";
        assert_eq!(
            texts(&stream(reply, Pace::UNHURRIED)),
            [
                "Sure.",
                "It is sunny, and the high is 26 degrees, with wind from the east."
            ]
        );
    }

    #[test]
    fn later_sentences_split_at_clauses_when_synthesis_is_behind() {
        let reply = "Sure. It is sunny, and the high is 26 degrees, with wind from the east.";
        let units = stream(reply, Pace::BEHIND);
        assert_eq!(
            texts(&units),
            [
                "Sure.",
                "It is sunny,",
                "and the high is 26 degrees,",
                "with wind from the east."
            ]
        );
        assert_eq!(units[1].boundary, Boundary::Clause);
    }

    #[test]
    fn a_clause_cut_waits_until_the_sentence_would_not_be_ready_in_time() {
        let pace = |slack_secs| Pace {
            slack_secs,
            synth_secs_per_token: 0.06,
        };
        // "It is sunny," is ~13 tokens; with the 40-token lookahead it needs ~3.2 s of slack.
        let reply = "Sure. It is sunny, and the high is 26 degrees.";
        assert_eq!(stream(reply, pace(2.0)).len(), 3);
        assert_eq!(stream(reply, pace(5.0)).len(), 2);
    }

    #[test]
    fn numbers_and_abbreviations_do_not_end_units() {
        for text in [
            "There are 1,000 units ready.",
            "The temperature is 25.3 degrees Celsius.",
            "Dr. Smith arrived at 7:30 a.m. today.",
        ] {
            assert_eq!(texts(&stream(text, Pace::BEHIND)), [text]);
        }
    }

    #[test]
    fn marks_at_the_end_of_the_buffer_wait_for_what_follows() {
        let mut s = ReplySplitter::new();
        assert!(
            s.push("The temperature is 25.", Pace::BEHIND).is_empty(),
            "25.3 may follow"
        );
        assert!(s.push("3 degrees", Pace::BEHIND).is_empty());
        let mut s = ReplySplitter::new();
        assert!(s
            .push("I would be glad to help with that,", Pace::BEHIND)
            .is_empty());
        let mut s = ReplySplitter::new();
        assert!(s.push("First one.", Pace::UNHURRIED).is_empty());
        assert!(
            s.push("\n\n", Pace::UNHURRIED).is_empty(),
            "the break is not over yet"
        );
        let units = s.push("Second", Pace::UNHURRIED);
        assert_eq!(units, [SpeechUnit::new("First one.", Boundary::Paragraph)]);
        assert_eq!(s.finish(), [SpeechUnit::new("Second", Boundary::End)]);
    }

    #[test]
    fn a_pause_in_the_stream_says_the_sentences_already_whole() {
        let mut s = ReplySplitter::new();
        assert!(s
            .push("Let me check the weather.", Pace::UNHURRIED)
            .is_empty());
        assert_eq!(
            s.pause(Pace::UNHURRIED),
            [SpeechUnit::new(
                "Let me check the weather.",
                Boundary::Sentence
            )]
        );
        assert!(s.push(" It is", Pace::UNHURRIED).is_empty());
        assert!(
            s.pause(Pace::UNHURRIED).is_empty(),
            "half a sentence waits for the rest"
        );
        assert_eq!(texts(&s.finish()), ["It is"]);
    }

    #[test]
    fn the_first_unit_keeps_whole_sentences_without_midphrase_cuts() {
        let reply =
            "The weather in Nairobi today is expected to be warm and mostly sunny all afternoon. Then";
        let units = stream(reply, Pace::UNHURRIED);
        assert_eq!(
            texts(&units),
            [
                "The weather in Nairobi today is expected to be warm and mostly sunny all afternoon.",
                "Then"
            ]
        );
        assert_eq!(units[0].boundary, Boundary::Sentence);
        assert_eq!(
            texts(&stream(
                "Here is a simple chapati recipe for about eight pieces. More.",
                Pace::UNHURRIED
            )),
            [
                "Here is a simple chapati recipe for about eight pieces.",
                "More."
            ],
            "a mark ends the first unit naturally"
        );
    }

    #[test]
    fn a_number_starting_a_line_is_a_list_marker_not_a_sentence() {
        let units = split_text("Here is how.\n1. Mix the flour, salt and sugar.\n2. Add the oil.");
        assert_eq!(
            texts(&units),
            [
                "Here is how.",
                "1. Mix the flour, salt and sugar.",
                "2. Add the oil."
            ]
        );
        assert_eq!(
            boundaries(&units),
            [Boundary::ListLine, Boundary::ListLine, Boundary::End]
        );
    }

    #[test]
    fn list_lines_end_units_and_get_a_comma_when_unpunctuated() {
        let units =
            split_text("You will need:\n- 3 cups of flour\n- 1 teaspoon of salt\n\nMix them well.");
        assert_eq!(
            texts(&units),
            [
                "You will need:",
                "- 3 cups of flour,",
                "- 1 teaspoon of salt",
                "Mix them well."
            ]
        );
        assert_eq!(
            boundaries(&units),
            [
                Boundary::ListLine,
                Boundary::ListLine,
                Boundary::Paragraph,
                Boundary::End
            ]
        );
    }

    #[test]
    fn a_wrapped_line_of_prose_is_not_a_break() {
        let text = "This is a long sentence that the model\nwrapped onto a second line.";
        assert_eq!(
            texts(&split_text(&format!("Okay. {text}"))),
            ["Okay.", text]
        );
        let header = "Weather Updates\nThe temperature is 25.3 degrees Celsius.";
        assert_eq!(texts(&split_text(header)), [header]);
    }

    #[test]
    fn a_blank_line_ends_a_unit_however_short_and_collapses_its_whitespace() {
        let units = split_text("Ingredients\n\nTwo cups.\t\t\n\n   Then knead.");
        assert_eq!(texts(&units), ["Ingredients", "Two cups.", "Then knead."]);
        assert_eq!(
            boundaries(&units),
            [Boundary::Paragraph, Boundary::Paragraph, Boundary::End]
        );
    }

    #[test]
    fn a_sentence_past_the_budget_is_cut_at_its_last_clause() {
        let clause = "and then the dough is folded over again";
        let long = format!("Okay. {}.", [clause; 8].join(", "));
        let units = stream(&long, Pace::UNHURRIED);
        assert!(units.len() >= 3, "{:?}", texts(&units));
        for unit in &units[1..units.len() - 1] {
            assert!(
                estimate_tokens(&unit.text) <= UNIT_TOKEN_BUDGET,
                "{}",
                unit.text
            );
            assert_eq!(unit.boundary, Boundary::Clause, "{}", unit.text);
        }
    }

    #[test]
    fn a_streamed_reply_is_cut_where_the_whole_of_it_is() {
        let reply = "Herons hunt by standing still for a long time, then striking fast. \
                     They eat fish, frogs and insects, and some even bait the water with bread.\n\n\
                     Dr. Smith counted 1,000 of them at 7:30 a.m. near the lake.";
        let streamed = stream(reply, Pace::UNHURRIED);
        assert_eq!(streamed, split_text(reply));
        assert_eq!(
            texts(&streamed),
            [
                "Herons hunt by standing still for a long time,",
                "then striking fast.",
                "They eat fish, frogs and insects, and some even bait the water with bread.",
                "Dr. Smith counted 1,000 of them at 7:30 a.m. near the lake."
            ]
        );
        assert_eq!(streamed[2].boundary, Boundary::Paragraph);
    }

    #[test]
    fn queued_units_are_packed_within_a_paragraph_and_the_budget() {
        let queue = || {
            std::collections::VecDeque::from(vec![
                SpeechUnit::new("Salt matters too.", Boundary::Sentence),
                SpeechUnit::new("A little slows the yeast.", Boundary::Sentence),
                SpeechUnit::new("Too much stops it.", Boundary::Paragraph),
                SpeechUnit::new("Shall I go on?", Boundary::End),
            ])
        };
        let mut q = queue();
        let first = pack_units(&mut q, Pace::UNHURRIED).unwrap();
        assert_eq!(
            first,
            SpeechUnit::new(
                "Salt matters too. A little slows the yeast. Too much stops it.",
                Boundary::Paragraph
            )
        );
        assert_eq!(
            pack_units(&mut q, Pace::UNHURRIED).unwrap().text,
            "Shall I go on?"
        );
        assert!(pack_units(&mut q, Pace::UNHURRIED).is_none());

        let mut q = queue();
        assert_eq!(
            pack_units(&mut q, Pace::BEHIND).unwrap().text,
            "Salt matters too.",
            "behind, the next unit goes alone so its audio comes sooner"
        );

        let long = "word ".repeat(UNIT_TOKEN_BUDGET / 6);
        let mut q = std::collections::VecDeque::from(vec![
            SpeechUnit::new(long.trim(), Boundary::Sentence),
            SpeechUnit::new(long.trim(), Boundary::Sentence),
        ]);
        assert_eq!(
            pack_units(&mut q, Pace::UNHURRIED).unwrap().text,
            long.trim()
        );
    }

    #[test]
    fn packed_list_lines_keep_their_own_lines() {
        let mut q = std::collections::VecDeque::from(vec![
            SpeechUnit::new("- 3 cups of flour,", Boundary::ListLine),
            SpeechUnit::new("- 1 teaspoon of salt,", Boundary::ListLine),
        ]);
        let unit = pack_units(&mut q, Pace::UNHURRIED).unwrap();
        assert_eq!(unit.text, "- 3 cups of flour,\n- 1 teaspoon of salt,");
        assert_eq!(
            strip_markdown_for_speech(&unit.text),
            "3 cups of flour, 1 teaspoon of salt,"
        );
    }

    #[test]
    fn unpunctuated_list_lines_are_joined_with_a_comma() {
        assert_eq!(
            strip_markdown_for_speech("- 3 cups of flour\n- 1 teaspoon of salt\n- water"),
            "3 cups of flour, 1 teaspoon of salt, water"
        );
        assert_eq!(
            strip_markdown_for_speech("A long line the model\nwrapped here."),
            "A long line the model wrapped here.",
            "prose lines join with a space"
        );
    }

    #[test]
    fn wake_triggers_put_the_phrase_first_and_reduce_calibrated_sentences_to_the_phrase() {
        let t = wake_triggers("Hey Goose", &["Hey goos, good morning".to_string()]);
        assert_eq!(t, vec!["hey goose", "goose"]);
        let custom = wake_triggers("Jarvis", &["Hey Jarv, good morning".to_string()]);
        assert_eq!(custom[0], "jarvis");
        assert!(custom.contains(&"jarv".to_string()), "{custom:?}");
    }

    #[test]
    fn heard_wake_matches_whole_words_in_any_punctuation() {
        let t = wake_triggers("goose", &[]);
        assert!(heard_wake("Goose, what's the time?", &t));
        assert!(heard_wake("um... hey Goose!", &t));
        assert!(!heard_wake("I saw a goose fly past", &t));
        assert!(!heard_wake("a mongoose ran past", &t));
        assert!(!heard_wake("", &t));
    }

    #[test]
    fn punctuation_survives_the_whole_speech_pass() {
        // Punctuation is prosody to Kokoro (pauses, pitch), so this pass must keep it.
        assert_eq!(
            strip_markdown_for_speech("**Hello**, world! Are you _sure_? Yes; really."),
            "Hello, world! Are you sure? Yes; really.",
        );
    }

    #[test]
    fn markdown_bold_stripped() {
        assert_eq!(
            strip_markdown_for_speech("The **quick** fox"),
            "The quick fox"
        );
    }

    #[test]
    fn echo_transcript_matches_speech() {
        let said = "The forecast in Nairobi today calls for warm weather and mostly sunny skies.";
        assert!(is_echo_transcript("warm weather", said));
        assert!(is_echo_transcript("mostly sunny skies", said));
        assert!(is_echo_transcript(
            "calls for warm weather and sunny skies",
            said
        ));
        assert!(!is_echo_transcript("what is the time", said));
        assert!(!is_echo_transcript("hey goose", said));
        assert!(!is_echo_transcript("weather", said));
    }

    #[test]
    fn markdown_italic_stripped() {
        assert_eq!(
            strip_markdown_for_speech("The _quick_ fox"),
            "The quick fox"
        );
        assert_eq!(
            strip_markdown_for_speech("The *quick* fox"),
            "The quick fox"
        );
    }

    #[test]
    fn markdown_header_stripped() {
        assert_eq!(strip_markdown_for_speech("## Hello"), "Hello");
        assert_eq!(
            strip_markdown_for_speech("# Title\nBody text"),
            "Title Body text"
        );
    }

    #[test]
    fn markdown_inline_code_stripped() {
        assert_eq!(
            strip_markdown_for_speech("Run `cargo build`"),
            "Run cargo build"
        );
    }

    #[test]
    fn markdown_code_block_skipped() {
        let md = "Here is code:\n```\nfn main() {}\n```\nDone.";
        assert_eq!(strip_markdown_for_speech(md), "Here is code: Done.");
    }

    #[test]
    fn markdown_link_keeps_text() {
        assert_eq!(
            strip_markdown_for_speech("See [the docs](https://example.com)"),
            "See the docs"
        );
    }

    #[test]
    fn markdown_image_dropped() {
        assert_eq!(strip_markdown_for_speech("![logo](logo.png) text"), "text");
    }

    #[test]
    fn markdown_list_items_stripped() {
        let md = "- item one\n- item two";
        assert_eq!(strip_markdown_for_speech(md), "item one, item two");
    }

    #[test]
    fn markdown_checkboxes_stripped() {
        let md = "- [ ] Unfinished task\n- [x] Finished task\n1. [ ] Ordered task";
        assert_eq!(
            strip_markdown_for_speech(md),
            "Unfinished task, Finished task, Ordered task"
        );
    }

    #[test]
    fn markdown_hr_dropped() {
        assert_eq!(
            strip_markdown_for_speech("before\n---\nafter"),
            "before after"
        );
    }

    #[test]
    fn markdown_blockquote_stripped() {
        assert_eq!(strip_markdown_for_speech("> quoted text"), "quoted text");
    }

    /// Replies a guest's voice turns began with on the nano (2026-11-02), verbatim.
    #[test]
    fn tool_calls_written_as_text_are_never_spoken() {
        for leaked in [
            r#"<giap-knowledge__search_web question="what to pack for a day hike" />"#,
            r#"<giap-knowledge__get_wikipedia_article topic="Why is the sky blue?"/>"#,
            r#"<knowledge__get_wikipedia_article topic="why is the sky blue?"/"#,
            r#"<call>giap-knowledge__get_wikipedia_article{topic:"crocodile vs alligator"}"#,
            r#"<giap-knowledge__get_wikipedia_article topic="Why is the sky blue?"#,
        ] {
            assert_eq!(strip_markdown_for_speech(leaked), "", "{leaked}");
        }
    }

    #[test]
    fn an_echoed_scaffold_block_is_dropped_with_its_body() {
        let leaked = "<answer-contract>\nReply with the result, not the route to it.\n\
                      </answer-contract>\nThe sky scatters blue light.";
        assert_eq!(
            strip_markdown_for_speech(leaked),
            "The sky scatters blue light."
        );
        let echoed = "<user-message>\nWhat is a crocodile?\n</user-message>\nA large reptile.";
        assert_eq!(strip_markdown_for_speech(echoed), "A large reptile.");
    }

    #[test]
    fn an_invented_snake_case_block_is_never_spoken() {
        assert_eq!(
            strip_markdown_for_speech(
                "<search_web>What is the story of how the Great Rift Valley formed</search_web>"
            ),
            ""
        );
        assert_eq!(
            strip_markdown_for_speech(
                "<turn_plan>Search, then answer.</turn_plan> It formed slowly."
            ),
            "It formed slowly."
        );
        assert_eq!(
            strip_markup_for_speech("Use snake_case names."),
            "Use snake_case names."
        );
    }

    #[test]
    fn comparisons_and_prose_survive_the_markup_strip() {
        assert_eq!(
            strip_markup_for_speech("3 < 5 and 7 > 2"),
            "3 < 5 and 7 > 2"
        );
        assert_eq!(
            strip_markup_for_speech("The __init__ method runs first."),
            "The __init__ method runs first."
        );
        assert_eq!(strip_markup_for_speech("I <3 tea."), "I <3 tea.");
    }

    #[test]
    fn filter_thinking_strips_complete_block() {
        let (text, in_block) = filter_thinking("<think>reasoning here</think>actual answer", false);
        assert_eq!(text, "actual answer");
        assert!(!in_block);
    }

    #[test]
    fn filter_thinking_no_block_passthrough() {
        let (text, in_block) = filter_thinking("just normal text", false);
        assert_eq!(text, "just normal text");
        assert!(!in_block);
    }

    #[test]
    fn filter_thinking_split_across_chunks() {
        let (text1, in_block) = filter_thinking("prefix<think>start of reasoning", false);
        assert_eq!(text1, "prefix");
        assert!(in_block);

        let (text2, in_block2) = filter_thinking("end of reasoning</think>real answer", in_block);
        assert_eq!(text2, "real answer");
        assert!(!in_block2);
    }

    #[test]
    fn filter_thinking_chunk_entirely_inside_block() {
        let (text, in_block) = filter_thinking("more reasoning tokens", true);
        assert_eq!(text, "");
        assert!(in_block);
    }

    #[test]
    fn filter_thinking_multiple_blocks() {
        let (text, in_block) =
            filter_thinking("<think>a</think>first<think>b</think>second", false);
        assert_eq!(text, "firstsecond");
        assert!(!in_block);
    }

    // ── normalize_for_speech tests ─────────────────────────────────────────────

    #[test]
    fn normalize_temperature_celsius() {
        assert_eq!(
            normalize_for_speech("It is 28°C today"),
            "It is 28 degrees Celsius today"
        );
    }

    #[test]
    fn normalize_temperature_fahrenheit() {
        assert_eq!(
            normalize_for_speech("It is 82°F"),
            "It is 82 degrees Fahrenheit"
        );
    }

    #[test]
    fn normalize_temperature_bare_degree() {
        assert_eq!(normalize_for_speech("Angle of 45°"), "Angle of 45 degrees");
    }

    #[test]
    fn normalize_percent() {
        assert_eq!(
            normalize_for_speech("Humidity is 72%"),
            "Humidity is 72 percent"
        );
    }

    #[test]
    fn normalize_dollars() {
        assert_eq!(
            normalize_for_speech("That costs $50"),
            "That costs 50 dollars"
        );
    }

    #[test]
    fn normalize_dollars_with_commas() {
        assert_eq!(
            normalize_for_speech("Price: $1,250.50"),
            "Price: 1250.50 dollars"
        );
    }

    #[test]
    fn normalize_kenyan_currency_with_commas() {
        assert_eq!(
            normalize_for_speech("around KES 150,000"),
            "around 150000 Kenyan shillings"
        );
    }

    #[test]
    fn normalize_dollars_singular() {
        assert_eq!(normalize_for_speech("Just $1"), "Just 1 dollar");
    }

    #[test]
    fn normalize_dollars_decimal() {
        assert_eq!(normalize_for_speech("Price: $9.99"), "Price: 9.99 dollars");
    }

    #[test]
    fn normalize_pounds() {
        assert_eq!(normalize_for_speech("Costs £30"), "Costs 30 pounds");
    }

    #[test]
    fn normalize_euros() {
        assert_eq!(normalize_for_speech("Costs €20"), "Costs 20 euros");
    }

    #[test]
    fn normalize_kenyan_currency() {
        assert_eq!(
            normalize_for_speech("dollar to KSH conversion"),
            "dollar to Kenyan shillings conversion"
        );
        assert_eq!(
            normalize_for_speech("around 129.83 KES"),
            "around 129.83 Kenyan shillings"
        );
        assert_eq!(
            normalize_for_speech("costs KSh 130 per unit"),
            "costs 130 Kenyan shillings per unit"
        );
    }

    #[test]
    fn normalize_ampersand_standalone() {
        assert_eq!(normalize_for_speech("fish & chips"), "fish and chips");
    }

    #[test]
    fn normalize_at_standalone() {
        assert_eq!(normalize_for_speech("meet @ 3pm"), "meet at 3 PM");
    }

    #[test]
    fn normalize_time_hhmm_ampm() {
        assert_eq!(normalize_for_speech("at 3:45pm"), "at 3 45 PM");
    }

    #[test]
    fn normalize_time_hhmm_ampm_uppercase() {
        assert_eq!(normalize_for_speech("at 3:45PM"), "at 3 45 PM");
    }

    #[test]
    fn normalize_time_24h() {
        assert_eq!(normalize_for_speech("at 15:30"), "at 15 30");
    }

    #[test]
    fn normalize_time_bare_ampm() {
        assert_eq!(normalize_for_speech("at 9am"), "at 9 AM");
    }

    #[test]
    fn normalize_time_zero_minutes_dropped() {
        assert_eq!(normalize_for_speech("at 3:00pm"), "at 3 PM");
    }

    #[test]
    fn normalize_time_not_a_time_bare_number() {
        assert_eq!(normalize_for_speech("I have 3 cats"), "I have 3 cats");
    }

    #[test]
    fn normalize_combined_with_markdown_strip() {
        // strip_markdown_for_speech runs normalize_for_speech at the end
        assert_eq!(
            strip_markdown_for_speech("Temperature: **28°C** and humidity **72%**"),
            "Temperature: 28 degrees Celsius and humidity 72 percent"
        );
    }

    // ── Unit suffix tests ──────────────────────────────────────────────────

    #[test]
    fn normalize_unit_kg() {
        assert_eq!(normalize_for_speech("5kg"), "5 kilograms");
    }
    #[test]
    fn normalize_unit_kg_space() {
        assert_eq!(normalize_for_speech("5 kg"), "5 kilograms");
    }
    #[test]
    fn normalize_unit_no_false_positive() {
        assert_eq!(normalize_for_speech("asking"), "asking");
    }
    #[test]
    fn normalize_unit_km_per_h() {
        assert_eq!(normalize_for_speech("100km/h"), "100 kilometers per hour");
    }
    #[test]
    fn normalize_unit_ghz() {
        assert_eq!(normalize_for_speech("3.5GHz"), "3.5 gigahertz");
    }
    #[test]
    fn normalize_unit_kwh() {
        assert_eq!(normalize_for_speech("2kWh"), "2 kilowatt hours");
    }
    #[test]
    fn normalize_unit_no_false_positive_mining() {
        assert_eq!(normalize_for_speech("5mining"), "5mining");
    }
    #[test]
    fn normalize_unit_mb() {
        assert_eq!(normalize_for_speech("10MB"), "10 megabytes");
    }
    #[test]
    fn normalize_unit_sq_meters() {
        assert_eq!(normalize_for_speech("5m²"), "5 square meters");
    }
    #[test]
    fn normalize_unit_db() {
        assert_eq!(normalize_for_speech("80dB"), "80 decibels");
    }
    #[test]
    fn normalize_unit_mph() {
        assert_eq!(normalize_for_speech("60mph"), "60 miles per hour");
    }

    // ── Expanded currency tests ────────────────────────────────────────────

    #[test]
    fn normalize_yen() {
        assert_eq!(normalize_for_speech("¥500"), "500 yen");
    }
    #[test]
    fn normalize_rupee_singular() {
        assert_eq!(normalize_for_speech("₹1"), "1 rupee");
    }
    #[test]
    fn normalize_rupee_plural() {
        assert_eq!(normalize_for_speech("₹100"), "100 rupees");
    }

    // ── Standalone symbol tests ────────────────────────────────────────────

    #[test]
    fn normalize_plus_minus() {
        assert_eq!(normalize_for_speech("±5"), "plus or minus 5");
    }
    #[test]
    fn normalize_pi() {
        assert_eq!(normalize_for_speech("π"), "pi");
    }
    #[test]
    fn normalize_fraction_half() {
        assert_eq!(normalize_for_speech("½"), "one half");
    }
    #[test]
    fn normalize_en_dash_range() {
        assert_eq!(normalize_for_speech("3–5"), "3 to 5");
    }
    #[test]
    fn normalize_copyright() {
        assert_eq!(normalize_for_speech("©"), "copyright");
    }
    #[test]
    fn normalize_squared() {
        assert_eq!(normalize_for_speech("5²"), "5 squared");
    }
    #[test]
    fn normalize_number_sign() {
        assert_eq!(normalize_for_speech("#5"), "number 5");
    }

    // ── Period / dot tests ─────────────────────────────────────────────────

    #[test]
    fn normalize_dot_between_digits() {
        assert_eq!(normalize_for_speech("3.14"), "3 point 14");
    }
    #[test]
    fn normalize_dot_domain() {
        assert_eq!(normalize_for_speech("google.com"), "google dot com");
    }
    #[test]
    fn normalize_dot_sentence_end() {
        assert_eq!(normalize_for_speech("Hello."), "Hello.");
    }
    #[test]
    fn normalize_dot_ip_address() {
        assert_eq!(
            normalize_for_speech("192.168.1.1"),
            "192 point 168 point 1 point 1"
        );
    }
    #[test]
    fn normalize_dot_version() {
        assert_eq!(normalize_for_speech("v3.2"), "v3 point 2");
    }

    // ── Abbreviation tests ─────────────────────────────────────────────────

    #[test]
    fn normalize_abbrev_eg() {
        assert_eq!(normalize_for_speech("e.g. cats"), "for example cats");
    }
    #[test]
    fn normalize_abbrev_ie() {
        assert_eq!(normalize_for_speech("i.e. dogs"), "that is dogs");
    }
    #[test]
    fn normalize_abbrev_etc() {
        assert_eq!(normalize_for_speech("cats, etc."), "cats, etcetera");
    }
    #[test]
    fn normalize_abbrev_dr() {
        assert_eq!(normalize_for_speech("Dr. Smith"), "doctor Smith");
    }
}

// -- Wake-word matching -----------------------------------------------------

/// Reduce a transcript to lowercase alphabetic words separated by single spaces.
///
/// Wake-word matching and stripping use this form on both sides; whisper punctuates freely.
pub fn normalize_transcript(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphabetic() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Position of `trigger` inside `haystack`, as a word range, or `None`.
///
/// Both must already be normalized. Whole words only, so "goose" never fires on "mongoose".
pub fn find_trigger_words(haystack: &str, trigger: &str) -> Option<(usize, usize)> {
    let hay: Vec<&str> = haystack.split_whitespace().collect();
    let needle: Vec<&str> = trigger.split_whitespace().collect();
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .find(|&i| hay[i..i + needle.len()] == needle[..])
        .map(|i| (i, i + needle.len()))
}

/// Whether `haystack` contains `trigger` as whole words.
pub fn contains_trigger(haystack: &str, trigger: &str) -> bool {
    find_trigger_words(haystack, trigger).is_some()
}

/// How many of `heard`'s words `said` holds in the same order, gaps allowed, as a share of `heard`.
pub fn in_order_share(heard: &[&str], said: &[&str]) -> f32 {
    if heard.is_empty() {
        return 0.0;
    }
    let mut row = vec![0usize; heard.len() + 1];
    for word in said {
        let mut diagonal = 0;
        for j in 1..=heard.len() {
            let above = row[j];
            row[j] = if heard[j - 1] == *word {
                diagonal + 1
            } else {
                row[j].max(row[j - 1])
            };
            diagonal = above;
        }
    }
    row[heard.len()] as f32 / heard.len() as f32
}

/// Whether `heard` is likely an echo of what the pond `said`.
pub fn is_echo_transcript(heard: &str, said: &str) -> bool {
    let norm_heard = normalize_transcript(heard);
    let norm_said = normalize_transcript(said);
    let heard_words: Vec<&str> = norm_heard.split_whitespace().collect();
    let said_words: Vec<&str> = norm_said.split_whitespace().collect();
    if heard_words.len() < 2 || said_words.is_empty() {
        return false;
    }
    in_order_share(&heard_words, &said_words) >= 0.6
}

/// Common Whisper mishearings of the shipped wake phrases (worst on the tiny and base models).
fn builtin_wake_variants(wake_word: &str) -> &'static [&'static str] {
    match wake_word {
        "goose" | "hey goose" => &["hey goose", "goose"],
        _ => &[],
    }
}

/// Everything a transcript may contain for the wake phrase to count as said, normalized.
/// Shipped phrases stay strictly ["hey goose", "goose"].
pub fn wake_triggers(wake_word: &str, calibrated: &[String]) -> Vec<String> {
    let wake_word = normalize_transcript(wake_word);
    if matches!(wake_word.as_str(), "hey goose" | "goose") {
        return vec!["hey goose".to_string(), "goose".to_string()];
    }
    let mut triggers = vec![wake_word.clone()];
    let heard = calibrated
        .iter()
        .filter_map(|v| extract_wake_variant(v, &wake_word, None));
    let builtin = builtin_wake_variants(&wake_word)
        .iter()
        .map(|v| v.to_string());
    for t in heard.chain(builtin) {
        if !t.is_empty() && !triggers.contains(&t) {
            triggers.push(t);
        }
    }
    triggers
}

/// Filler that may precede a leading wake word; a content word here could erase a command.
const RUN_UP_WORDS: &[&str] = &["um", "uh", "er", "hey", "hi", "hello", "ok", "okay", "so"];

/// Whether `transcript` opens with any of `triggers` as whole words, preceded only by filler.
pub fn heard_wake(transcript: &str, triggers: &[String]) -> bool {
    let heard = normalize_transcript(transcript);
    let words: Vec<&str> = heard.split_whitespace().collect();
    triggers.iter().any(|t| {
        find_trigger_words(&heard, t)
            .map(|(start, _)| words[..start].iter().all(|w| RUN_UP_WORDS.contains(w)))
            .unwrap_or(false)
    })
}

/// Strip one leading wake word, plus any filler before it; the rest is returned as heard.
pub fn strip_leading_wake_word(transcript: &str, triggers: &[String]) -> String {
    let normalized = normalize_transcript(transcript);
    let words: Vec<&str> = normalized.split_whitespace().collect();

    // Longest trigger wins ("hey goose" over "goose"), then the earliest.
    let best = triggers
        .iter()
        .filter_map(|t| find_trigger_words(&normalized, t))
        .filter(|&(start, _)| words[..start].iter().all(|w| RUN_UP_WORDS.contains(w)))
        .max_by_key(|&(start, end)| (end - start, std::cmp::Reverse(start)));

    let Some((_, end)) = best else {
        // No wake word: a follow-up, which must reach the model exactly as heard.
        return transcript.trim().to_string();
    };

    // Cut the original after `end` words: a normalized word is a maximal alphabetic run.
    let mut words_seen = 0usize;
    let mut in_word = false;
    let mut cut = transcript.len();
    for (i, c) in transcript.char_indices() {
        if c.is_alphabetic() {
            if !in_word {
                in_word = true;
                words_seen += 1;
            }
        } else if in_word {
            in_word = false;
            if words_seen == end {
                cut = i;
                break;
            }
        }
    }
    if in_word && words_seen == end {
        cut = transcript.len();
    }

    // Drop the greeting's punctuation, like the comma in "Goose, what's the weather".
    transcript[cut..]
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .to_string()
}

/// Whether `transcript` holds nothing but the wake phrase and filler, however many times over:
/// Whisper repeats a short wake ("Hey goose. Hey goose. Hey goose.") on a clip with no request.
pub fn only_wake_words(transcript: &str, triggers: &[String]) -> bool {
    let mut rest = transcript.trim().to_string();
    while heard_wake(&rest, triggers) {
        let next = strip_leading_wake_word(&rest, triggers);
        if next.len() >= rest.len() {
            break;
        }
        rest = next;
    }
    normalize_transcript(&rest)
        .split_whitespace()
        .all(|w| RUN_UP_WORDS.contains(&w))
}

/// Whether a reply ends by asking the person something. Only then is what the room says next
/// taken for an answer without the wake word; otherwise it is talk not meant for the pond.
pub fn asks_for_an_answer(reply: &str) -> bool {
    reply
        .trim_end_matches(|c: char| {
            c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ']' | '*' | '_' | '”' | '’' | '»')
        })
        .ends_with(['?', '？'])
}

/// The sentences the calibration screen asks for, each containing `phrase`.
///
/// Must produce the same list as `buildCalibrationPrompts` in
/// `pond-desktop/src/modes/voice/wakePhrase.ts`.
pub fn calibration_prompts(phrase: &str) -> Vec<String> {
    let p = phrase.trim();
    vec![
        p.to_string(),
        format!("Hey {p}, good morning"),
        format!("OK {p}, what time is it"),
        format!("{p}, tell me a joke"),
        format!("Thank you {p}"),
    ]
}

/// Character edit distance (Levenshtein).
fn char_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// How unalike two words are, in [0, 1]: edit distance over the longer length.
fn word_cost(a: &str, b: &str) -> f32 {
    let longest = a.chars().count().max(b.chars().count()).max(1);
    char_distance(a, b) as f32 / longest as f32
}

/// A heard phrase further than this from the real one is something else, not a mishearing
/// ("gus" for "goose" is 0.6 and kept; "bruce" is 0.8 and dropped).
const MAX_VARIANT_COST: f32 = 0.67;

/// The words of `transcript` that stand where `phrase` stands in `prompt`, and the cost of
/// the word alignment that put them there. All three must be normalized.
fn align_phrase(transcript: &str, prompt: &str, phrase: &str) -> Option<(String, f32)> {
    let t: Vec<&str> = transcript.split_whitespace().collect();
    let p: Vec<&str> = prompt.split_whitespace().collect();
    let (ps, pe) = find_trigger_words(prompt, phrase)?;
    let (m, k) = (t.len(), p.len());
    // cost[i][j]: aligning t[..i] with p[..j]; substitution costs word_cost, a gap costs 1.
    let mut cost = vec![vec![0f32; k + 1]; m + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i as f32;
    }
    for (j, c) in cost[0].iter_mut().enumerate() {
        *c = j as f32;
    }
    for i in 1..=m {
        for j in 1..=k {
            cost[i][j] = (cost[i - 1][j - 1] + word_cost(t[i - 1], p[j - 1]))
                .min(cost[i - 1][j] + 1.0)
                .min(cost[i][j - 1] + 1.0);
        }
    }
    // Walk back, noting which transcript words were paired with a word of the phrase.
    let mut in_span = vec![false; m];
    let (mut i, mut j) = (m, k);
    while i > 0 && j > 0 {
        if (cost[i][j] - (cost[i - 1][j - 1] + word_cost(t[i - 1], p[j - 1]))).abs() < 1e-6 {
            in_span[i - 1] = (ps..pe).contains(&(j - 1));
            i -= 1;
            j -= 1;
        } else if (cost[i][j] - (cost[i - 1][j] + 1.0)).abs() < 1e-6 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    let first = in_span.iter().position(|&b| b)?;
    let last = in_span.iter().rposition(|&b| b)?;
    // Unpaired words between two paired ones belong to the phrase too ("goo se").
    Some((t[first..=last].join(" "), cost[m][k]))
}

/// The wake phrase as heard in one calibration take: the transcript words standing where the
/// phrase stands in the sentence that was read out. Without `prompt`, every calibration
/// sentence is tried and the best-aligned one wins. `None` when the take does not contain the
/// phrase or anything like it, so a sentence is never stored as a trigger.
pub fn extract_wake_variant(
    transcript: &str,
    phrase: &str,
    prompt: Option<&str>,
) -> Option<String> {
    let t = normalize_transcript(transcript);
    let ph = normalize_transcript(phrase);
    if t.is_empty() || ph.is_empty() {
        return None;
    }
    let prompts: Vec<String> = match prompt {
        Some(p) => vec![normalize_transcript(p)],
        None => calibration_prompts(&ph)
            .iter()
            .map(|p| normalize_transcript(p))
            .collect(),
    };
    let (heard, _) = prompts
        .iter()
        .filter_map(|p| align_phrase(&t, p, &ph))
        .min_by(|a, b| a.1.total_cmp(&b.1))?;
    // Equal-cost alignments can pull a neighbouring word in ("hey" twice in "Hey hey goose");
    // drop edge words while that brings the span closer to the phrase.
    let mut words: Vec<&str> = heard.split(' ').collect();
    while words.len() > 1 {
        let now = word_cost(&words.join(" "), &ph);
        let head = word_cost(&words[1..].join(" "), &ph);
        let tail = word_cost(&words[..words.len() - 1].join(" "), &ph);
        if head < now && head <= tail {
            words.remove(0);
        } else if tail < now {
            words.pop();
        } else {
            break;
        }
    }
    let heard = words.join(" ");
    (word_cost(&heard, &ph) <= MAX_VARIANT_COST).then_some(heard)
}

#[cfg(test)]
mod wake_word_tests {
    use super::*;

    fn triggers(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| normalize_transcript(s)).collect()
    }

    #[test]
    fn normalization_survives_whispers_punctuation() {
        assert_eq!(normalize_transcript("Hey, Goose."), "hey goose");
        assert_eq!(normalize_transcript("  GOOSE!!  "), "goose");
        assert_eq!(normalize_transcript("goose123"), "goose");
    }

    // -- matching ----------------------------------------------------------

    #[test]
    fn a_trigger_matches_as_a_whole_word_anywhere_in_the_window() {
        assert!(contains_trigger("okay goose what time is it", "goose"));
        assert!(contains_trigger("goose", "goose"));
        assert!(contains_trigger("well hey goose there", "hey goose"));
    }

    #[test]
    fn a_trigger_never_matches_inside_a_longer_word() {
        assert!(!contains_trigger("i saw a mongoose today", "goose"));
        assert!(!contains_trigger("gooseberry jam", "goose"));
        assert!(!contains_trigger("the goosebumps were real", "goose"));
    }

    #[test]
    fn a_multi_word_trigger_needs_its_words_adjacent_and_in_order() {
        assert!(!contains_trigger("hey there goose", "hey goose"));
        assert!(!contains_trigger("goose hey", "hey goose"));
    }

    #[test]
    fn an_empty_or_oversized_trigger_matches_nothing() {
        assert!(!contains_trigger("goose", ""));
        assert!(!contains_trigger("goose", "hey goose now"));
        assert!(!contains_trigger("", "goose"));
    }

    // -- stripping ---------------------------------------------------------

    #[test]
    fn the_wake_word_comes_off_the_front_of_the_command() {
        let t = triggers(&["goose"]);
        assert_eq!(
            strip_leading_wake_word("Goose, what's the weather?", &t),
            "what's the weather?"
        );
    }

    #[test]
    fn the_surviving_command_keeps_its_original_punctuation() {
        let t = triggers(&["goose"]);
        assert_eq!(
            strip_leading_wake_word("goose, don't turn off the Kitchen lights!", &t),
            "don't turn off the Kitchen lights!"
        );
    }

    #[test]
    fn a_short_run_up_before_the_wake_word_is_dropped_too() {
        let t = triggers(&["goose"]);
        assert_eq!(
            strip_leading_wake_word("um goose turn on the lights", &t),
            "turn on the lights"
        );
    }

    #[test]
    fn a_later_mention_of_the_wake_word_is_kept() {
        let t = triggers(&["goose"]);
        assert_eq!(
            strip_leading_wake_word("Goose, remind me to feed the goose.", &t),
            "remind me to feed the goose."
        );
    }

    #[test]
    fn a_wake_word_past_the_run_up_window_is_left_alone() {
        let t = triggers(&["goose"]);
        let out = strip_leading_wake_word("tell me all about the goose please", &t);
        assert_eq!(out, "tell me all about the goose please");
    }

    #[test]
    fn a_short_command_about_the_wake_word_survives_intact() {
        let t = triggers(&["goose"]);
        for command in [
            "feed the goose",
            "the goose is loose",
            "my goose needs water",
        ] {
            assert_eq!(
                strip_leading_wake_word(command, &t),
                command,
                "{command:?} is a request, not a greeting"
            );
        }
    }

    #[test]
    fn only_filler_may_precede_the_greeting() {
        let t = triggers(&["goose"]);
        assert_eq!(
            strip_leading_wake_word("um okay goose what time is it", &t),
            "what time is it",
            "a run of filler is still a run-up"
        );
        assert_eq!(
            strip_leading_wake_word("cook goose tonight", &t),
            "cook goose tonight",
            "a content word before it means it is not a greeting"
        );
    }

    #[test]
    fn the_longest_matching_trigger_wins() {
        let t = triggers(&["goose", "hey goose"]);
        assert_eq!(
            strip_leading_wake_word("hey goose set a timer", &t),
            "set a timer"
        );
    }

    #[test]
    fn a_wake_whisper_repeated_is_still_only_the_wake() {
        let t = triggers(&["hey goose", "goose"]);
        assert!(only_wake_words("Hey goose. Hey goose. Hey goose.", &t));
        assert!(only_wake_words("Hey goose. Goose. Hey", &t));
        assert!(only_wake_words("", &t));
        assert!(!only_wake_words("Hey goose. What time is it?", &t));
        assert!(
            !only_wake_words("goose eggs for breakfast", &t),
            "a request that opens with the word is a request"
        );
    }

    #[test]
    fn only_a_reply_that_asks_waits_for_an_answer() {
        assert!(asks_for_an_answer("Which room do you mean?"));
        assert!(asks_for_an_answer("Shall I set it for **seven**?** \n"));
        assert!(asks_for_an_answer(
            "Do you want the long version (it takes a minute)?)"
        ));
        assert!(!asks_for_an_answer("It is sunny and 24 degrees."));
        assert!(!asks_for_an_answer("Did you know? Geese mate for life."));
        assert!(!asks_for_an_answer(""));
    }

    #[test]
    fn a_clip_holding_only_the_wake_word_yields_no_command() {
        let t = triggers(&["goose"]);
        assert_eq!(strip_leading_wake_word("Goose.", &t), "");
        assert_eq!(
            strip_leading_wake_word("hey goose", &triggers(&["hey goose"])),
            ""
        );
    }

    #[test]
    fn a_transcript_without_the_wake_word_passes_through() {
        let t = triggers(&["goose"]);
        assert_eq!(
            strip_leading_wake_word("what about tomorrow?", &t),
            "what about tomorrow?"
        );
    }

    #[test]
    fn stripping_respects_word_boundaries() {
        let t = triggers(&["goose"]);
        assert_eq!(
            strip_leading_wake_word("mongoose facts please", &t),
            "mongoose facts please"
        );
    }

    #[test]
    fn calibration_keeps_the_phrase_not_the_sentence() {
        let p = Some("OK goose, what time is it");
        assert_eq!(
            extract_wake_variant("OK goose, what time is it?", "goose", p).as_deref(),
            Some("goose")
        );
        assert_eq!(
            extract_wake_variant("Okay, goes. What time is it?", "goose", p).as_deref(),
            Some("goes")
        );
        assert_eq!(
            extract_wake_variant("OK, what time is it?", "goose", p),
            None
        );
    }

    #[test]
    fn a_mishearing_beside_a_lookalike_word_is_found_by_position() {
        // "good" spells closer to "goose" than "gus" does; alignment goes by place, not spelling.
        let v = extract_wake_variant(
            "Hey gus, good morning.",
            "goose",
            Some("Hey goose, good morning"),
        );
        assert_eq!(v.as_deref(), Some("gus"));
    }

    #[test]
    fn without_the_prompt_every_calibration_sentence_is_tried() {
        assert_eq!(
            extract_wake_variant("hey gus good morning", "goose", None).as_deref(),
            Some("gus")
        );
        assert_eq!(
            extract_wake_variant("thank you goose", "goose", None).as_deref(),
            Some("goose")
        );
        // An old calibration that stored a whole sentence reduces to the phrase.
        assert_eq!(
            extract_wake_variant("ok goose what time is it", "goose", None).as_deref(),
            Some("goose")
        );
    }

    #[test]
    fn multi_word_phrases_and_merged_words_survive() {
        let p = Some("Hey hey goose, good morning");
        assert_eq!(
            extract_wake_variant("Hey, hey goose. Good morning.", "hey goose", p).as_deref(),
            Some("hey goose")
        );
        assert_eq!(
            extract_wake_variant("Heygoose.", "hey goose", Some("hey goose")).as_deref(),
            Some("heygoose")
        );
        let p = Some("Hey hey goose, good morning");
        assert_eq!(
            extract_wake_variant("Hey heygoose, good morning.", "hey goose", p).as_deref(),
            Some("heygoose")
        );
    }

    #[test]
    fn something_unrelated_is_not_a_variant() {
        assert_eq!(extract_wake_variant("bruce", "goose", Some("goose")), None);
        assert_eq!(extract_wake_variant("", "goose", None), None);
    }

    #[test]
    fn calibration_prompts_match_the_desktop_list() {
        // The same literals are asserted in pond-desktop/src/modes/voice/wakePhrase.test.ts.
        assert_eq!(
            calibration_prompts("goose"),
            [
                "goose",
                "Hey goose, good morning",
                "OK goose, what time is it",
                "goose, tell me a joke",
                "Thank you goose"
            ]
        );
    }
}

// -- Whisper artifacts ------------------------------------------------------

/// Strip whisper's non-speech tags and known hallucinations; "" if nothing real remains.
pub fn strip_whisper_artifacts(text: &str) -> String {
    // Strip all [BRACKETED_TAGS] — Whisper uses these for non-speech events.
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        if let Some(close) = rest[open..].find(']') {
            rest = &rest[open + close + 1..];
        } else {
            rest = &rest[open..];
            break;
        }
    }
    out.push_str(rest);

    // Strip (PARENTHESIZED TAGS) — e.g. (inaudible), (music), (laughing)
    let mut cleaned = String::with_capacity(out.len());
    let mut prest = out.as_str();
    while let Some(open) = prest.find('(') {
        cleaned.push_str(&prest[..open]);
        if let Some(close) = prest[open..].find(')') {
            prest = &prest[open + close + 1..];
        } else {
            prest = &prest[open..];
            break;
        }
    }
    cleaned.push_str(prest);

    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return String::new();
    }

    // Reject common Whisper hallucinations on silence / noise.
    let lower = cleaned.to_lowercase();
    let trimmed_lower = lower.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    const EXACT_HALLUCINATIONS: &[&str] = &[
        "",
        ".",
        "..",
        "...",
        ",",
        "!",
        "?",
        "thank you",
        "thank you very much",
        "thanks for watching",
        "thanks for listening",
        "thanks",
        "you",
        "bye",
        "bye bye",
        "goodbye",
        "okay",
        "the end",
        "subtitles by",
        "subtitle",
        "until next time",
        "see you next time",
        "so",
        "um",
        "uh",
        "hmm",
        "huh",
        "ah",
        "oh",
        "i'm sorry",
        "i don't know",
        "please subscribe",
        "like and subscribe",
    ];
    if EXACT_HALLUCINATIONS.iter().any(|h| trimmed_lower == *h) {
        return String::new();
    }

    // Strip known hallucinated trailing tails on a real utterance.
    const TRAILING_TAILS: &[&str] = &[
        "until next time",
        "thanks for watching",
        "thank you for watching",
        "please subscribe",
        "like and subscribe",
    ];
    let mut result = cleaned;
    for tail in TRAILING_TAILS {
        let tail_with_space = format!(" {tail}");
        let lower_res = result.to_lowercase();
        let stripped_lower =
            lower_res.trim_end_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
        if let Some(pos) = stripped_lower.rfind(&tail_with_space) {
            result = result[..pos].trim_end_matches(|c: char| c.is_whitespace());
            break;
        }
    }

    // Reject very short transcripts (1-2 chars) — almost always noise artifacts.
    if result.len() <= 2 {
        return String::new();
    }

    // Reject if the transcript is just the same word/syllable repeated.
    let lower_result = result.to_lowercase();
    let words: Vec<&str> = lower_result.split_whitespace().collect();
    if words.len() >= 2 && words.iter().all(|w| *w == words[0]) {
        return String::new();
    }

    result.to_string()
}

#[cfg(test)]
mod artifact_tests {
    use super::*;

    #[test]
    fn whisper_artifacts_strip_hallucinations_and_tails() {
        assert_eq!(strip_whisper_artifacts("Thank you."), "");
        assert_eq!(strip_whisper_artifacts("  Thanks for watching!  "), "");
        assert_eq!(strip_whisper_artifacts("Until next time."), "");
        assert_eq!(
            strip_whisper_artifacts("Hey goose, what's on my camera? Until next time."),
            "Hey goose, what's on my camera?"
        );
        assert_eq!(
            strip_whisper_artifacts("Turn off the kitchen light. Thanks for watching!"),
            "Turn off the kitchen light."
        );
    }
}
