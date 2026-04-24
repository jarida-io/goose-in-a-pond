use crate::domain::agent::{AgentRequest, AgentStreamEvent, WorkflowEvent, WorkflowState};
use crate::domain::message::ChatMessage;
use crate::domain::session::SessionMessage;
use crate::ports::agent::Agent;
use crate::ports::profile::ProfileRepository;
use crate::ports::provider::LlmProvider;
use crate::ports::session_storage::SessionStorage;
use crate::ports::speaker_id::SpeakerIdentification;
use crate::ports::voice_input::VoiceInput;
use crate::ports::voice_output::VoiceOutput;
use crate::ports::wake_word::StreamingWakeWordDetector;
use crate::services::instant_activation::InstantActivation;
use crate::services::print_output::PrintOutput;
use crate::prompts::{SYSTEM_PROMPT, TITLE_GENERATION_PROMPT};
use crate::services::context_compactor::ContextCompactor;
use crate::services::stdin_input::StdinInput;
use anyhow::Result;
use futures::StreamExt as _;
use std::io::{self, Write};
use std::sync::Arc;
use uuid::Uuid;

// ── Voice helpers ─────────────────────────────────────────────────────────────

/// Classify a voice message into a model role string.
///
/// Voice mode defaults Chat-classified messages to "chat" as well; the role
/// is metadata for the GooseAdapter's model selection.
fn resolve_voice_role(message: &str) -> String {
    use crate::domain::model_role::ModelRole;
    use crate::services::request_classifier::classify_request;
    match classify_request(message) {
        ModelRole::Think => "think".to_string(),
        ModelRole::Task  => "task".to_string(),
        ModelRole::Chat  => "chat".to_string(),
    }
}

/// Human-readable announcement spoken while an MCP tool is executing.
fn tool_announcement(tool: &str) -> String {
    let name = tool.split("__").last().unwrap_or(tool);
    match name {
        "get_current_weather" | "get_weather" => "Let me check the weather.".to_string(),
        "get_devices" | "list_devices"        => "Checking your devices.".to_string(),
        "set_schedule" | "create_schedule"    => "Setting that up.".to_string(),
        "save_memory"                         => "Got it, I'll remember that.".to_string(),
        other => format!("Let me {}.", other.replace('_', " ")),
    }
}

// Quips replaced by thinking tone (VoiceOutput::start_thinking_tone).
// Kept for potential future use (e.g. text-only fallback).
#[allow(dead_code)]
const THINKING_QUIPS: &[&str] = &[
    "Let me think.",
    "Ruffling through possibilities.",
    "One moment.",
    "Consulting the pond elders.",
    "Processing.",
    "Wading in.",
    "Let me check.",
    "Thinking that through.",
    "Allow me a moment.",
    "Right, let me look at that.",
];

#[allow(dead_code)]
fn pick_quip() -> &'static str {
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        % THINKING_QUIPS.len();
    THINKING_QUIPS[idx]
}

/// Split completed sentences out of a text buffer.
///
/// Sentence boundaries: `.`, `?`, `!` followed by whitespace or end-of-string,
/// and bare newlines. Forces a flush at 250 characters to handle code blocks
/// or long lists without sentence punctuation.
///
/// Returns `(sentences_to_speak, remaining_buffer)`.
fn split_sentences(text: &str) -> (Vec<String>, String) {
    const MAX_BUF: usize = 250;
    let mut sentences: Vec<String> = Vec::new();
    let mut remainder = text.to_string();

    loop {
        // Force-flush at max buffer: break at last space within the limit
        if remainder.len() > MAX_BUF {
            if let Some(split_at) = remainder[..MAX_BUF].rfind(' ') {
                sentences.push(remainder[..split_at].to_string());
                remainder = remainder[split_at + 1..].to_string();
                continue;
            }
        }

        let mut found = false;
        let chars: Vec<(usize, char)> = remainder.char_indices().collect();
        for (idx, (i, ch)) in chars.iter().enumerate() {
            if matches!(ch, '.' | '?' | '!') {
                let next = i + ch.len_utf8();
                let after = &remainder[next..];
                if after.is_empty() || after.starts_with(' ') || after.starts_with('\n') {
                    sentences.push(remainder[..next].to_string());
                    remainder = after.trim_start_matches(|c: char| c == ' ' || c == '\n').to_string();
                    found = true;
                    break;
                }
            } else if *ch == '\n' {
                // Newline is its own boundary
                let chunk = remainder[..*i].trim().to_string();
                if !chunk.is_empty() {
                    sentences.push(chunk);
                }
                let next = i + 1;
                remainder = remainder[next..].to_string();
                found = true;
                let _ = idx; // suppress unused warning
                break;
            }
        }

        if !found {
            break;
        }
    }

    (sentences, remainder)
}

/// Convert a markdown string to plain text suitable for TTS.
///
/// Handles:
/// - Code fences (``` / ~~~) — block skipped entirely
/// - Inline code (`…`) — backticks removed, content kept
/// - Bold / italic (`**`, `__`, `*`, `_`) — markers removed
/// - Headers (`#`, `##`, …) — `#` stripped, text kept
/// - Blockquotes (`> `) — `>` stripped, text kept
/// - Unordered lists (`- `, `* `, `+ `) — marker stripped, text kept
/// - Ordered lists (`1. `, `2. `, …) — marker stripped, text kept
/// - Horizontal rules (`---`, `***`, `___`) — line dropped
/// - Links (`[text](url)`) — url dropped, text kept
/// - Images (`![alt](url)`) — dropped entirely
/// - Strikethrough (`~~…~~`) — markers removed
fn strip_markdown_for_speech(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code_fence = false;

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

        // Strip structural prefix then inline markers
        let content = strip_line_prefix(trimmed);
        let content = strip_inline_md(content);
        let content = content.trim().to_string();
        if !content.is_empty() {
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(&content);
        }
    }

    normalize_for_speech(out.trim())
}

fn is_hr(s: &str) -> bool {
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
fn strip_line_prefix(line: &str) -> &str {
    // Headers: ### text → text
    if line.starts_with('#') {
        return line.trim_start_matches('#').trim_start();
    }
    // Blockquotes: > text
    if let Some(rest) = line.strip_prefix("> ").or_else(|| line.strip_prefix('>')) {
        return rest.trim_start();
    }
    // Unordered lists: - / * / +
    if let Some(rest) = line.strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
    {
        return rest;
    }
    // Ordered lists: 1. 2. 10. etc.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && bytes.get(i) == Some(&b'.') && bytes.get(i + 1) == Some(&b' ') {
        return &line[i + 2..];
    }
    line
}

/// Strip inline markdown markers from a string, handling bold, italic, code, links, images.
fn strip_inline_md(s: &str) -> String {
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
        if chars.get(i..i + 2) == Some(&['*', '*'])
            || chars.get(i..i + 2) == Some(&['_', '_'])
        {
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

/// Find a `[text](url)` link starting at `start` (which points to `[`).
/// Returns `(end_index, link_text)` where `end_index` is one past the closing `)`.
fn find_link(chars: &[char], start: usize) -> Option<(usize, String)> {
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
    // Must be followed by `(`
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

/// Find the closing occurrence of `marker` in `chars` starting at `start`.
/// Returns the index where the marker begins (not one-past-end).
fn find_marker_close(chars: &[char], start: usize, marker: &[char]) -> Option<usize> {
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

/// Unit suffixes matched after a number. Sorted longest-first for greedy matching.
const UNIT_SUFFIXES: &[(&str, &str)] = &[
    // ── Compound / slash units ──
    ("km/h",  " kilometers per hour"),
    ("mi/h",  " miles per hour"),
    ("KB/s",  " kilobytes per second"),
    ("MB/s",  " megabytes per second"),
    ("m/s",   " meters per second"),
    ("ft/s",  " feet per second"),
    ("fl oz", " fluid ounces"),
    // ── Data (IEC binary) ──
    ("KiB", " kibibytes"), ("MiB", " mebibytes"), ("GiB", " gibibytes"), ("TiB", " tebibytes"),
    // ── Data speed ──
    ("kbps", " kilobits per second"), ("Mbps", " megabits per second"), ("Gbps", " gigabits per second"),
    // ── Energy (long) ──
    ("kWh", " kilowatt hours"), ("kcal", " kilocalories"), ("BTU", " B T U"),
    // ── Frequency ──
    ("THz", " terahertz"), ("GHz", " gigahertz"), ("MHz", " megahertz"), ("kHz", " kilohertz"),
    // ── Power ──
    ("GW", " gigawatts"), ("MW", " megawatts"), ("kW", " kilowatts"), ("mW", " milliwatts"),
    // ── Voltage ──
    ("kV", " kilovolts"), ("mV", " millivolts"),
    // ── Current ──
    ("mA", " milliamps"), ("μA", " microamps"),
    // ── Resistance ──
    ("MΩ", " megaohms"), ("kΩ", " kilohms"),
    // ── Pressure ──
    ("MPa", " megapascals"), ("kPa", " kilopascals"),
    ("mmHg", " millimeters of mercury"),
    ("atm", " atmospheres"), ("bar", " bar"), ("psi", " P S I"),
    // ── Energy ──
    ("MJ", " megajoules"), ("kJ", " kilojoules"),
    ("cal", " calories"), ("eV", " electron volts"), ("Wh", " watt hours"),
    // ── Sound ──
    ("dBA", " D B A"), ("dB", " decibels"),
    // ── Duration ──
    ("hrs", " hours"), ("sec", " seconds"), ("min", " minutes"),
    ("ms", " milliseconds"), ("ns", " nanoseconds"), ("μs", " microseconds"),
    ("hr", " hours"),
    // ── Data storage ──
    ("KB", " kilobytes"), ("MB", " megabytes"), ("GB", " gigabytes"),
    ("TB", " terabytes"), ("PB", " petabytes"), ("EB", " exabytes"),
    // ── Speed ──
    ("mph", " miles per hour"), ("bps", " bits per second"),
    // ── Area (with superscript) ──
    ("km²", " square kilometers"), ("cm²", " square centimeters"),
    ("m²", " square meters"), ("ft²", " square feet"), ("in²", " square inches"),
    ("cm³", " cubic centimeters"), ("m³", " cubic meters"),
    ("ha", " hectares"),
    // ── Length ──
    ("km", " kilometers"), ("cm", " centimeters"), ("mm", " millimeters"),
    ("nm", " nanometers"), ("μm", " micrometers"),
    ("mi", " miles"), ("ft", " feet"), ("yd", " yards"),
    // ── Weight ──
    ("kg", " kilograms"), ("mg", " milligrams"), ("μg", " micrograms"),
    ("lbs", " pounds"), ("lb", " pounds"), ("oz", " ounces"), ("st", " stone"),
    // ── Volume ──
    ("mL", " milliliters"), ("dL", " deciliters"), ("kL", " kiloliters"),
    ("gal", " gallons"), ("qt", " quarts"), ("pt", " pints"),
    // ── Single-char units (last — shortest match) ──
    ("Hz", " hertz"), ("Pa", " pascals"),
    ("W", " watts"), ("V", " volts"), ("A", " amps"),
    ("J", " joules"), ("Ω", " ohms"), ("L", " liters"),
    ("m", " meters"), ("g", " grams"),
];

/// Currency symbols: (char, singular, plural).
const CURRENCY_SYMBOLS: &[(char, &str, &str)] = &[
    ('$', "dollar",   "dollars"),
    ('£', "pound",    "pounds"),
    ('€', "euro",     "euros"),
    ('¥', "yen",      "yen"),
    ('₹', "rupee",    "rupees"),
    ('₽', "ruble",    "rubles"),
    ('₩', "won",      "won"),
    ('₪', "shekel",   "shekels"),
    ('₦', "naira",    "naira"),
    ('₱', "peso",     "pesos"),
    ('₺', "lira",     "lira"),
    ('₴', "hryvnia",  "hryvnias"),
    ('₵', "cedi",     "cedis"),
    ('₡', "colon",    "colones"),
    ('₫', "dong",     "dong"),
    ('₭', "kip",      "kip"),
    ('₮', "tugrik",   "tugriks"),
    ('₧', "peseta",   "pesetas"),
    ('₣', "franc",    "francs"),
];

/// Standalone single-character symbols.
const STANDALONE_SYMBOLS: &[(char, &str)] = &[
    // Math
    ('±', "plus or minus "), ('×', " times "), ('÷', " divided by "),
    ('∞', "infinity"), ('≈', "approximately "), ('≤', "less than or equal to "),
    ('≥', "greater than or equal to "), ('≠', "not equal to "),
    ('√', "square root of "), ('π', "pi"),
    ('²', " squared"), ('³', " cubed"),
    // Fractions
    ('½', "one half"), ('⅓', "one third"), ('⅔', "two thirds"),
    ('¼', "one quarter"), ('¾', "three quarters"),
    ('⅕', "one fifth"), ('⅖', "two fifths"), ('⅗', "three fifths"), ('⅘', "four fifths"),
    ('⅙', "one sixth"), ('⅚', "five sixths"),
    ('⅛', "one eighth"), ('⅜', "three eighths"), ('⅝', "five eighths"), ('⅞', "seven eighths"),
    // Legal / typographic
    ('©', "copyright"), ('®', "registered"), ('™', "trademark"),
    ('§', "section"), ('¶', "paragraph"),
    ('†', ""), ('‡', ""),
    ('•', ", "),
    ('—', ", "),
];

/// Convert symbols and abbreviations to their spoken equivalents so that
/// TTS engines (Piper, etc.) pronounce them correctly.
fn normalize_for_speech(text: &str) -> String {
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
                Some('C') | Some('c') => { out.push_str(" degrees Celsius");    i += 2; continue; }
                Some('F') | Some('f') => { out.push_str(" degrees Fahrenheit"); i += 2; continue; }
                Some('K') | Some('k') => { out.push_str(" kelvin");             i += 2; continue; }
                _                     => { out.push_str(" degrees");            i += 1; continue; }
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
            // Between digits: "3.14" → "3 point 14"
            // BUT keep as decimal when followed by a unit suffix (3.5GHz → "3.5 gigahertz")
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = chars.get(i + 1).map_or(false, |c| c.is_ascii_digit());
            if prev_digit && next_digit {
                // Peek ahead: find the end of the digit run after the dot
                let mut j = i + 1;
                while j < len && chars[j].is_ascii_digit() { j += 1; }
                // If a unit suffix follows the digits, keep the dot as-is (decimal)
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

/// Try to match a unit suffix at position `i` (immediately after a number ended).
/// Allows one optional space between number and unit: "5kg" and "5 kg" both match.
/// Rejects if the char after the suffix is alphabetic (prevents "5mining" → "5 minutes").
/// Common abbreviations with periods — matched case-insensitively.
/// (abbreviation_lowercase, expansion, char_length_including_dots)
const ABBREVIATIONS: &[(&str, &str)] = &[
    ("e.g.",  "for example"),
    ("i.e.",  "that is"),
    ("etc.",  "etcetera"),
    ("vs.",   "versus"),
    ("approx.", "approximately"),
    ("dept.", "department"),
    ("govt.", "government"),
    ("assn.", "association"),
    ("inc.",  "incorporated"),
    ("corp.", "corporation"),
    ("ltd.",  "limited"),
    ("prof.", "professor"),
    ("dr.",   "doctor"),
    ("mr.",   "mister"),
    ("mrs.",  "missus"),
    ("ms.",   "miss"),
    ("jr.",   "junior"),
    ("sr.",   "senior"),
    ("st.",   "saint"),
    ("ave.",  "avenue"),
    ("blvd.", "boulevard"),
    ("ft.",   "fort"),
    ("mt.",   "mount"),
    ("no.",   "number"),
    ("vol.",  "volume"),
    ("ch.",   "chapter"),
    ("pg.",   "page"),
    ("fig.",  "figure"),
    ("approx.", "approximately"),
    ("max.",  "maximum"),
    ("min.",  "minimum"),
    ("temp.", "temperature"),
    ("est.",  "established"),
    ("jan.",  "January"), ("feb.", "February"), ("mar.", "March"),
    ("apr.",  "April"), ("jun.", "June"), ("jul.", "July"),
    ("aug.",  "August"), ("sep.", "September"), ("oct.", "October"),
    ("nov.",  "November"), ("dec.", "December"),
];

/// Try to match a common abbreviation starting at position `i`.
/// `i` must be at a word boundary (start-of-string or after whitespace/punctuation).
/// Returns `(expansion, total_chars_consumed)`.
fn try_read_abbreviation(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars.len();
    // Must be at a word boundary
    let at_boundary = i == 0
        || chars[i - 1].is_whitespace()
        || matches!(chars[i - 1], '(' | ',' | '"' | '\'' | '[');
    if !at_boundary { return None; }

    // Build a lowercase window from position i (up to 10 chars)
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

fn try_read_unit_suffix(chars: &[char], i: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let (unit_start, space_consumed) = if i < len && chars[i] == ' ' {
        (i + 1, 1usize)
    } else {
        (i, 0usize)
    };
    if unit_start >= len { return None; }

    for &(suffix, spoken) in UNIT_SUFFIXES {
        let suffix_chars: Vec<char> = suffix.chars().collect();
        let slen = suffix_chars.len();
        if unit_start + slen > len { continue; }

        let matches = suffix_chars.iter().enumerate().all(|(j, &sc)| chars[unit_start + j] == sc);
        if !matches { continue; }

        // Alphabetic continuation guard
        let after = unit_start + slen;
        if after < len && chars[after].is_alphabetic() { continue; }

        return Some((spoken.to_string(), space_consumed + slen));
    }
    None
}

/// Scan a number (digits, optional single `.` for decimals) starting at `start`.
/// Returns `(number_string, chars_consumed)`.  Returns `("", 0)` if no digit found.
fn read_number(chars: &[char], start: usize) -> (String, usize) {
    let mut j = start;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    // Optional decimal part
    if chars.get(j) == Some(&'.') && chars.get(j + 1).map_or(false, |c| c.is_ascii_digit()) {
        j += 1; // consume '.'
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
    }
    if j == start {
        return (String::new(), 0);
    }
    let s: String = chars[start..j].iter().collect();
    (s, j - start)
}

/// Try to parse a time expression at position `start` in `chars`.
///
/// Recognises:
/// - `H:MM am/pm`  e.g. `3:45pm`  → "3 45 PM"
/// - `HH:MM`       e.g. `15:30`   → "15 30"
/// - `H am/pm`     e.g. `9am`     → "9 AM"
///
/// Returns `Some((spoken, chars_consumed))` on success, `None` otherwise.
fn try_read_time(chars: &[char], start: usize) -> Option<(String, usize)> {
    let len = chars.len();
    let mut j = start;

    // ── Hours: 1-2 digits, value 0-23 ────────────────────────────────────────
    let h_start = j;
    while j < len && chars[j].is_ascii_digit() && j - h_start < 2 {
        j += 1;
    }
    if j == h_start { return None; }
    let hour: u32 = chars[h_start..j].iter().collect::<String>().parse().ok()?;
    if hour > 23 { return None; }
    let hour_str: String = chars[h_start..j].iter().collect();

    // ── Optional :MM ─────────────────────────────────────────────────────────
    let mut minute_str: Option<String> = None;
    if chars.get(j) == Some(&':') {
        let d1 = chars.get(j + 1)?;
        let d2 = chars.get(j + 2)?;
        if d1.is_ascii_digit() && d2.is_ascii_digit() {
            let min: u32 = format!("{}{}", d1, d2).parse().ok()?;
            if min > 59 { return None; }
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

    // Require at least one of: `:MM` or `am/pm`.
    // A bare `3` with nothing after is not a time.
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

/// Strip `<think>…</think>` reasoning blocks from a streaming text chunk.
///
/// Models like Qwen3/QwQ/DeepSeek-R1 emit reasoning inside `<think>` tags before
/// their actual answer.  TTS should skip that content; only the visible answer
/// should be spoken.
///
/// `in_block` is the carry-over state from the previous chunk (we may be in the
/// middle of a block that started in an earlier event).
///
/// Returns `(visible_text, updated_in_block)`.
pub fn filter_thinking(chunk: &str, mut in_block: bool) -> (String, bool) {
    let mut visible = String::with_capacity(chunk.len());
    let mut rest = chunk;

    loop {
        if in_block {
            // Inside a think block — look for the closing tag.
            if let Some(end) = rest.find("</think>") {
                rest = &rest[end + "</think>".len()..];
                in_block = false;
            } else {
                // Entire remaining chunk is still inside the block — skip it all.
                break;
            }
        } else {
            // Outside a think block — look for the opening tag.
            if let Some(start) = rest.find("<think>") {
                visible.push_str(&rest[..start]);
                rest = &rest[start + "<think>".len()..];
                in_block = true;
            } else {
                // No more think blocks — everything remaining is visible.
                visible.push_str(rest);
                break;
            }
        }
    }

    (visible, in_block)
}

/// Domain Service: ChatService
///
/// Orchestrates the Wait → Listen → Thinking → Speak workflow loop.
/// Also persists messages to session storage for conversation history.
///
/// All inference is routed through the `Agent` port (GooseAdapter in production).
/// An optional `LlmProvider` may be attached solely for session title generation.
///
/// Input is abstracted via the `VoiceInput` port.  The default is
/// `StdinInput` (reads from stdin).  Override with `with_voice_input()`.
pub struct ChatService {
    agent: Arc<dyn Agent>,
    provider: Option<Arc<dyn LlmProvider>>,
    voice_input: Arc<dyn VoiceInput>,
    voice_output: Arc<dyn VoiceOutput>,
    /// Wake-word detector.  Defaults to `InstantActivation` (keyboard / stdin mode).
    /// All detectors implement `StreamingWakeWordDetector`; `run_loop` always calls
    /// `wait_for_activation_with_audio()` so captured command audio is available for
    /// the one-breath flow when the detector supports it.
    wake_word_detector: Arc<dyn StreamingWakeWordDetector>,
    session_id: String,
    session_storage: Arc<dyn SessionStorage>,
    /// System prompt sent to the LLM on every completion call.
    /// Defaults to `SYSTEM_PROMPT`; override with `with_system_prompt()`.
    system_prompt: String,
    /// System prompt used when the speaker is not recognised (guest turns).
    guest_system_prompt: String,
    /// Optional LLM-based context compactor.  When set, triggers at 80% of
    /// the context budget instead of falling straight to trim_to_budget.
    compactor: Option<ContextCompactor>,
    speaker_id: Option<Arc<dyn SpeakerIdentification>>,
    profile_repo: Option<Arc<dyn ProfileRepository>>,
}

impl ChatService {
    pub fn new(
        agent: Arc<dyn Agent>,
        session_id: String,
        session_storage: Arc<dyn SessionStorage>,
    ) -> Self {
        Self {
            agent,
            provider: None,
            voice_input: Arc::new(StdinInput::new()),
            voice_output: Arc::new(PrintOutput),
            wake_word_detector: Arc::new(InstantActivation),
            session_id,
            session_storage,
            system_prompt: SYSTEM_PROMPT.to_string(),
            guest_system_prompt: SYSTEM_PROMPT.to_string(),
            compactor: None,
            speaker_id: None,
            profile_repo: None,
        }
    }

    /// Attach a real LLM provider. When set, `chat_once` calls the provider
    /// with the full conversation history instead of the echo agent.
    pub fn with_provider(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Override the input source.  Defaults to `StdinInput`.
    pub fn with_voice_input(mut self, input: Arc<dyn VoiceInput>) -> Self {
        self.voice_input = input;
        self
    }

    /// Override the voice output.  Defaults to `PrintOutput` (stdout).
    pub fn with_voice_output(mut self, output: Arc<dyn VoiceOutput>) -> Self {
        self.voice_output = output;
        self
    }

    /// Set the wake-word detector.  Defaults to `InstantActivation` (no wait).
    ///
    /// All detectors implement `StreamingWakeWordDetector`.  `run_loop` always calls
    /// `wait_for_activation_with_audio()`, so detectors that capture command audio
    /// (e.g. `WhisperKeywordDetector`) enable the one-breath flow automatically.
    pub fn with_wake_word_detector(mut self, detector: Arc<dyn StreamingWakeWordDetector>) -> Self {
        self.wake_word_detector = detector;
        self
    }

    /// Override the system prompt sent to the LLM.
    ///
    /// Use `pond_core::prompts::build_system_prompt()` to build a personalised
    /// prompt from `Settings`.  The default is the static `SYSTEM_PROMPT` constant.
    pub fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = prompt;
        self
    }

    pub fn with_guest_system_prompt(mut self, prompt: String) -> Self {
        self.guest_system_prompt = prompt;
        self
    }

    pub fn with_speaker_id(mut self, speaker_id: Arc<dyn SpeakerIdentification>) -> Self {
        self.speaker_id = Some(speaker_id);
        self
    }

    pub fn with_profile_repo(mut self, repo: Arc<dyn ProfileRepository>) -> Self {
        self.profile_repo = Some(repo);
        self
    }

    /// Enable LLM-based context compaction.
    ///
    /// When set, `chat_once` will summarise older history while preserving the
    /// most recent turns whenever the conversation exceeds 80% of the context
    /// limit, instead of simply dropping old messages via `trim_to_budget`.
    pub fn with_context_compactor(mut self, compactor: ContextCompactor) -> Self {
        self.compactor = Some(compactor);
        self
    }

    /// Single-shot chat (useful for tests and non-interactive callers).
    ///
    /// All inference is routed through the `Agent` port (GooseAdapter in production).
    /// Goose manages conversation history and context compaction internally.
    /// Our `SessionStorage` is used only for the REST API's history/listing endpoints.
    pub async fn chat_once(&self, message: String) -> Result<String> {
        // Persist the user message first
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Always route through the Agent port (GooseAdapter in production), which manages
        // its own history, system prompt, and MCP tools internally.
        // The optional `self.provider` is kept solely for session title generation.
        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: resolve_voice_role(&message),
        };
        let response_text = self.agent.chat(request).await?.text;

        // Persist the assistant response
        let assistant_msg = ChatMessage::assistant(response_text.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        // Auto-generate a session title after the first exchange
        self.maybe_generate_title(&message, &response_text).await;

        Ok(response_text)
    }

    /// Auto-generate a title for the session after the very first exchange.
    ///
    /// Only fires when:
    ///   1. An `LlmProvider` is available (title generation needs an LLM)
    ///   2. The session has no title yet
    ///   3. This is the first user+assistant pair (2 messages total)
    ///
    /// The title is generated by sending the user message and assistant
    /// response to the LLM with `TITLE_GENERATION_PROMPT`, then storing
    /// the result via `session_storage.update_title()`.
    ///
    /// Failures are logged but never bubble up — title generation is
    /// best-effort and must never break the chat flow.
    async fn maybe_generate_title(&self, user_text: &str, assistant_text: &str) {
        // Only generate if we have an LLM provider
        let provider = match &self.provider {
            Some(p) => p,
            None => return,
        };

        // Check if session already has a title
        if let Ok(session) = self.session_storage.get_session(&self.session_id).await {
            if session.title.is_some() {
                return;
            }
        }

        // Check if this is the first exchange (exactly 2 messages: user + assistant)
        if let Ok(msgs) = self.session_storage.get_messages(&self.session_id).await {
            if msgs.len() != 2 {
                return;
            }
        }

        // Build context for the title generation LLM call
        let context = format!(
            "User: {}\nAssistant: {}",
            user_text, assistant_text
        );
        let messages = vec![ChatMessage::user(&context)];

        match provider.complete(TITLE_GENERATION_PROMPT, messages).await {
            Ok(response) => {
                // Clean up: trim whitespace, remove quotes, limit length
                let title = response
                    .content
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .chars()
                    .take(80)
                    .collect::<String>();

                if !title.is_empty() {
                    if let Err(e) = self
                        .session_storage
                        .update_title(&self.session_id, title.clone())
                        .await
                    {
                        tracing::warn!("Failed to save session title: {}", e);
                    } else {
                        tracing::debug!("Auto-generated session title: {}", title);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("Title generation failed (non-fatal): {}", e);
            }
        }
    }

    /// Streaming chat — routes through the Agent, chunks TTS by sentence.
    ///
    /// Differences from `chat_once`:
    /// - Calls `agent.chat_stream()` so text arrives token-by-token.
    /// - Speaks each completed sentence immediately (low-latency TTS).
    /// - Announces MCP tool calls with a short spoken phrase before execution.
    /// - Speaking happens *inside* this method; callers must NOT call
    ///   `voice_output.speak()` on the returned text.
    pub async fn chat_stream_once(&self, message: String) -> Result<String> {
        // Persist user message
        let user_msg = ChatMessage::user(message.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            user_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        let request = AgentRequest {
            message: message.clone(),
            session_id: self.session_id.clone(),
            model_role: resolve_voice_role(&message),
        };

        // Start a soft ambient thinking tone while the LLM infers.
        // Stopped as soon as the first speakable content arrives.
        self.voice_output.start_thinking_tone();
        let mut tone_stopped = false;

        macro_rules! stop_tone {
            () => {
                if !tone_stopped {
                    self.voice_output.stop_thinking_tone();
                    tone_stopped = true;
                }
            };
        }

        let mut stream = self.agent.chat_stream(request).await?;
        let mut full_text = String::new();
        let mut sentence_buf = String::new();
        let mut spoken_first = false;
        let mut in_think_block = false;

        while let Some(event_result) = stream.next().await {
            match event_result? {
                AgentStreamEvent::ToolCall { tool, .. } => {
                    // Flush any buffered text before announcing the tool
                    if !sentence_buf.trim().is_empty() {
                        let chunk = sentence_buf.trim().to_string();
                        sentence_buf.clear();
                        stop_tone!();
                        if let Err(e) = self.voice_output.speak(&chunk).await {
                            tracing::warn!("TTS failed: {}", e);
                        }
                    }
                    let announcement = tool_announcement(&tool);
                    stop_tone!();
                    if let Err(e) = self.voice_output.speak(&announcement).await {
                        tracing::warn!("Tool announcement TTS failed: {}", e);
                    }
                }
                AgentStreamEvent::Text { content } => {
                    // Strip <think>…</think> reasoning blocks — not meant for TTS or transcript.
                    let (visible, new_in_think) = filter_thinking(&content, in_think_block);
                    in_think_block = new_in_think;
                    let content = visible;
                    if content.is_empty() {
                        continue;
                    }

                    if !spoken_first {
                        self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Speak));
                        spoken_first = true;
                    }
                    full_text.push_str(&content);
                    sentence_buf.push_str(&content);

                    let (sentences, remainder) = split_sentences(&sentence_buf);
                    sentence_buf = remainder;
                    for sentence in sentences {
                        let spoken = strip_markdown_for_speech(&sentence);
                        if spoken.is_empty() {
                            continue;
                        }
                        stop_tone!();
                        if let Err(e) = self.voice_output.speak(&spoken).await {
                            tracing::warn!("TTS failed: {}", e);
                        }
                    }
                }
                AgentStreamEvent::Done { .. } => {
                    // Flush any remaining buffer
                    let remainder = sentence_buf.trim().to_string();
                    if !remainder.is_empty() {
                        let spoken = strip_markdown_for_speech(&remainder);
                        if !spoken.is_empty() {
                            stop_tone!();
                            if let Err(e) = self.voice_output.speak(&spoken).await {
                                tracing::warn!("TTS flush failed: {}", e);
                            }
                        }
                    }
                    sentence_buf.clear();
                    break;
                }
                AgentStreamEvent::Error { content } => {
                    return Err(anyhow::anyhow!("Agent stream error: {}", content));
                }
                AgentStreamEvent::Status { .. } | AgentStreamEvent::ToolResult { .. } => {
                    // Not spoken — status/tool results are informational only
                }
            }
        }

        // Flush anything left if stream ended without Done
        let remainder = sentence_buf.trim().to_string();
        if !remainder.is_empty() {
            let spoken = strip_markdown_for_speech(&remainder);
            if !spoken.is_empty() {
                stop_tone!();
                if let Err(e) = self.voice_output.speak(&spoken).await {
                    tracing::warn!("TTS final flush failed: {}", e);
                }
            }
        }

        // Ensure the thinking tone is stopped even if no speakable text was produced.
        stop_tone!();

        // Persist the assistant response
        let assistant_msg = ChatMessage::assistant(full_text.clone());
        let session_msg = SessionMessage::new(
            Uuid::new_v4().to_string(),
            self.session_id.clone(),
            assistant_msg,
        );
        self.session_storage
            .add_message(self.session_id.clone(), session_msg)
            .await?;

        self.maybe_generate_title(&message, &full_text).await;
        Ok(full_text)
    }

    /// Run the interactive workflow loop.
    ///
    /// State machine:
    ///   Wait → Listen → Thinking → Speak → (back to Wait)
    ///
    /// Input is obtained via the `VoiceInput` port (stdin by default).
    async fn resolve_speaker(&self, audio_bytes: &[u8]) -> (String, String) {
        let speaker_id = match &self.speaker_id {
            Some(s) => s,
            None => return ("Guest".to_string(), self.guest_system_prompt.clone()),
        };
        if audio_bytes.is_empty() {
            return ("Guest".to_string(), self.guest_system_prompt.clone());
        }
        match speaker_id.identify_speaker(audio_bytes).await {
            Ok(Some((profile_id, confidence))) => {
                let name = if let Some(repo) = &self.profile_repo {
                    match repo.get(&profile_id).await {
                        Ok(Some(profile)) => profile.display_name,
                        _ => profile_id,
                    }
                } else {
                    profile_id
                };
                println!("  👤 {} (confidence: {:.0}%)", name, confidence * 100.0);
                (name, self.system_prompt.clone())
            }
            Ok(None) => {
                println!("  👤 Guest");
                ("Guest".to_string(), self.guest_system_prompt.clone())
            }
            Err(e) => {
                tracing::warn!("Speaker identification failed (non-fatal): {}", e);
                ("Guest".to_string(), self.guest_system_prompt.clone())
            }
        }
    }

    pub async fn run_loop(&self) -> Result<()> {
        // First interaction always requires the wake word.
        // After that, conversational turn-taking: Goose listens for the user's
        // next turn directly after speaking, no wake word needed.
        // If the user doesn't speak (empty transcription), fall back to wake word.
        let mut first_turn = true;

        loop {
            let (input, audio_bytes) = if first_turn {
                // ── Wait for wake word ──
                self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Wait));
                println!("\n  🟢 {} (type \"exit\" to quit)", self.wake_word_detector.activation_prompt());
                io::stdout().flush()?;

                let activation = self.wake_word_detector.wait_for_activation_with_audio().await?;

                // ── Listen (one-breath or fresh recording) ──
                self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Listen));
                print!("  {}", self.voice_input.prompt());
                io::stdout().flush()?;

                if let Some(wav) = activation.captured_audio {
                    self.voice_input.prime_with_captured(wav);
                }

                match self.voice_input.listen_with_audio().await? {
                    None => {
                        self.emit_event(WorkflowEvent::Exit);
                        println!("\n  ⏹ End of input.");
                        break;
                    }
                    Some((text, _)) if text.is_empty() => {
                        first_turn = true;
                        continue;
                    }
                    Some(pair) => pair,
                }
            } else {
                // ── Conversational turn — listen without wake word ──
                self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Listen));
                println!("\n  🎧 Listening for your reply...");
                io::stdout().flush()?;

                match self.voice_input.listen_with_audio().await? {
                    None => {
                        self.emit_event(WorkflowEvent::Exit);
                        println!("\n  ⏹ End of input.");
                        break;
                    }
                    Some((text, _)) if text.is_empty() => {
                        println!("  💤 No speech detected, returning to wake word mode.");
                        first_turn = true;
                        continue;
                    }
                    Some(pair) => pair,
                }
            };

            // ── Dismissal / sleep commands → speak farewell, return to wake word ──
            let lower = input.trim().to_lowercase();
            let lower = lower.trim_end_matches(|c: char| c == '.' || c == '!');
            if matches!(
                lower,
                "bye" | "goodbye" | "good bye" | "dismissed" | "go to sleep"
                    | "that's all" | "thats all" | "never mind" | "nevermind"
                    | "stop" | "stop listening"
            ) {
                let farewell = "Until next time. Just say my name when you need me.";
                println!("  🫡 {}", farewell);
                if let Err(e) = self.voice_output.speak(farewell).await {
                    tracing::warn!("TTS farewell failed: {}", e);
                }
                first_turn = true;
                continue;
            }

            // ── Hard exit (terminates the voice loop entirely) ──
            if matches!(lower, "exit" | "quit") {
                let farewell = "Goodbye! I'll be here whenever you need me.";
                println!("  👋 {}", farewell);
                if let Err(e) = self.voice_output.speak(farewell).await {
                    tracing::warn!("TTS farewell failed: {}", e);
                }
                self.emit_event(WorkflowEvent::Exit);
                break;
            }

            // Conversation is active — subsequent turns skip the wake word
            first_turn = false;

            self.emit_event(WorkflowEvent::UserInput(input.clone()));

            // ── Identify speaker ──
            let (_, prompt) = self.resolve_speaker(&audio_bytes).await;
            let _ = prompt; // system_prompt used inside chat_stream_once for now

            // ── Thinking → Speak (streaming) ──
            self.emit_event(WorkflowEvent::StateChanged(WorkflowState::Thinking));

            match self.chat_stream_once(input).await {
                Ok(response_text) => {
                    self.emit_event(WorkflowEvent::AgentOutput(response_text));
                }
                Err(e) => {
                    eprintln!("  ❌ Error: {}", e);
                    // On error, fall back to wake word mode
                    first_turn = true;
                }
            }
        }

        Ok(())
    }

    /// Hook point for future event subscribers (logging, UI, etc.).
    fn emit_event(&self, event: WorkflowEvent) {
        match &event {
            WorkflowEvent::StateChanged(state) => {
                tracing::debug!("Workflow state: {}", state);
            }
            WorkflowEvent::UserInput(text) => {
                tracing::debug!("User input: {}", text);
            }
            WorkflowEvent::AgentOutput(text) => {
                tracing::debug!("Agent output: {}", text);
            }
            WorkflowEvent::Exit => {
                tracing::debug!("Workflow exit requested");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::mock_agent::MockAgent;
    use crate::services::mock_provider::MockProvider;
    use crate::services::mock_session::InMemorySessionStorage;

    #[tokio::test]
    async fn chat_once_returns_echo() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        let result = service.chat_once("Hello!".to_string()).await.unwrap();
        assert_eq!(result, "Echo: Hello!");
    }

    #[tokio::test]
    async fn chat_persists_messages_to_storage() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.chat_once("First message".to_string()).await.unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 2); // User message + Assistant response
        assert_eq!(messages[0].message.content, "First message");
        assert!(messages[1].message.content.contains("First message"));
    }

    #[tokio::test]
    async fn chat_messages_persist_across_iterations() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone());

        // First iteration
        service.chat_once("Message 1".to_string()).await.unwrap();

        // Second iteration
        service.chat_once("Message 2".to_string()).await.unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 4); // 2 iterations × 2 messages each
        assert_eq!(messages[0].message.content, "Message 1");
        assert_eq!(messages[2].message.content, "Message 2");
    }

    #[tokio::test]
    async fn chat_with_provider_auto_generates_title() {
        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-test".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_provider(provider);

        // First message → triggers title generation
        service.chat_once("What is the weather?".to_string()).await.unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        // MockProvider returns "Mock response to: ..." which becomes the title
        assert!(session.title.is_some(), "Title should be auto-generated after first exchange");
        let title = session.title.unwrap();
        assert!(!title.is_empty(), "Title should not be empty");
    }

    #[tokio::test]
    async fn title_not_regenerated_on_second_message() {
        let agent = Arc::new(MockAgent::new());
        let provider = Arc::new(MockProvider::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "title-stable".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let service = ChatService::new(agent, session_id.clone(), storage.clone())
            .with_provider(provider);

        // First message → generates title
        service.chat_once("Hello".to_string()).await.unwrap();
        let first_title = storage.get_session(&session_id).await.unwrap().title.clone();

        // Second message → should NOT overwrite title
        service.chat_once("How are you?".to_string()).await.unwrap();
        let second_title = storage.get_session(&session_id).await.unwrap().title.clone();

        assert_eq!(first_title, second_title, "Title should not change after first generation");
    }

    #[tokio::test]
    async fn no_title_without_provider() {
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "no-provider".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        // No provider → title stays None
        let service = ChatService::new(agent, session_id.clone(), storage.clone());
        service.chat_once("Hello".to_string()).await.unwrap();

        let session = storage.get_session(&session_id).await.unwrap();
        assert!(session.title.is_none(), "No title should be set without a provider");
    }

    #[tokio::test]
    async fn with_voice_input_builder_compiles() {
        use crate::services::stdin_input::StdinInput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service = ChatService::new(agent, session_id, storage)
            .with_voice_input(Arc::new(StdinInput::new()));
    }

    #[test]
    fn markdown_bold_stripped() {
        assert_eq!(strip_markdown_for_speech("The **quick** fox"), "The quick fox");
    }

    #[test]
    fn markdown_italic_stripped() {
        assert_eq!(strip_markdown_for_speech("The _quick_ fox"), "The quick fox");
        assert_eq!(strip_markdown_for_speech("The *quick* fox"), "The quick fox");
    }

    #[test]
    fn markdown_header_stripped() {
        assert_eq!(strip_markdown_for_speech("## Hello"), "Hello");
        assert_eq!(strip_markdown_for_speech("# Title\nBody text"), "Title Body text");
    }

    #[test]
    fn markdown_inline_code_stripped() {
        assert_eq!(strip_markdown_for_speech("Run `cargo build`"), "Run cargo build");
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
        assert_eq!(strip_markdown_for_speech(md), "item one item two");
    }

    #[test]
    fn markdown_hr_dropped() {
        assert_eq!(strip_markdown_for_speech("before\n---\nafter"), "before after");
    }

    #[test]
    fn markdown_blockquote_stripped() {
        assert_eq!(strip_markdown_for_speech("> quoted text"), "quoted text");
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
        // First chunk opens the block but doesn't close it
        let (text1, in_block) = filter_thinking("prefix<think>start of reasoning", false);
        assert_eq!(text1, "prefix");
        assert!(in_block);

        // Second chunk closes it and continues with real content
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
        let (text, in_block) = filter_thinking(
            "<think>a</think>first<think>b</think>second", false
        );
        assert_eq!(text, "firstsecond");
        assert!(!in_block);
    }

    // ── normalize_for_speech tests ─────────────────────────────────────────────

    #[test]
    fn normalize_temperature_celsius() {
        assert_eq!(normalize_for_speech("It is 28°C today"), "It is 28 degrees Celsius today");
    }

    #[test]
    fn normalize_temperature_fahrenheit() {
        assert_eq!(normalize_for_speech("It is 82°F"), "It is 82 degrees Fahrenheit");
    }

    #[test]
    fn normalize_temperature_bare_degree() {
        assert_eq!(normalize_for_speech("Angle of 45°"), "Angle of 45 degrees");
    }

    #[test]
    fn normalize_percent() {
        assert_eq!(normalize_for_speech("Humidity is 72%"), "Humidity is 72 percent");
    }

    #[test]
    fn normalize_dollars() {
        assert_eq!(normalize_for_speech("That costs $50"), "That costs 50 dollars");
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
        // 3:00 PM → "3 PM" (the :00 is silent when am/pm present)
        assert_eq!(normalize_for_speech("at 3:00pm"), "at 3 PM");
    }

    #[test]
    fn normalize_time_not_a_time_bare_number() {
        // A lone digit with nothing after it must NOT be consumed as a time
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
        assert_eq!(normalize_for_speech("192.168.1.1"), "192 point 168 point 1 point 1");
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

    #[tokio::test]
    async fn with_voice_output_builder_compiles() {
        use crate::services::print_output::PrintOutput;
        let agent = Arc::new(MockAgent::new());
        let storage = Arc::new(InMemorySessionStorage::new());
        let session_id = "test-session".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let _service = ChatService::new(agent, session_id, storage)
            .with_voice_output(Arc::new(PrintOutput));
    }

}
