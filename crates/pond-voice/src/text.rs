//! Turning model output into speakable text.
//!
//! TypeScript ports of these functions live under `pond-desktop/src/`; keep them in step.

/// Split completed sentences out of a buffer, returning `(sentences, remainder)`.
pub fn split_sentences(text: &str) -> (Vec<String>, String) {
    const MAX_BUF: usize = 250;
    let mut sentences: Vec<String> = Vec::with_capacity(8);
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
                    remainder = after
                        .trim_start_matches(|c: char| c == ' ' || c == '\n')
                        .to_string();
                    found = true;
                    break;
                }
            } else if *ch == '\n' {
                let chunk = remainder[..*i].trim().to_string();
                if !chunk.is_empty() {
                    sentences.push(chunk);
                }
                let next = i + 1;
                remainder = remainder[next..].to_string();
                found = true;
                let _ = idx;
                break;
            }
        }

        if !found {
            break;
        }
    }

    (sentences, remainder)
}

/// Convert markdown to plain text for TTS, then apply `normalize_for_speech`.
///
/// Drops code blocks, images, link URLs and horizontal rules; other markup keeps its text.
pub fn strip_markdown_for_speech(text: &str) -> String {
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
    // Unordered lists: - / * / +
    if let Some(rest) = line
        .strip_prefix("- ")
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
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    if chars.get(j) == Some(&'.') && chars.get(j + 1).map_or(false, |c| c.is_ascii_digit()) {
        j += 1;
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
        assert_eq!(strip_markdown_for_speech(md), "item one item two");
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

/// Filler that may precede a leading wake word; a content word here could erase a command.
const RUN_UP_WORDS: &[&str] = &["um", "uh", "er", "hey", "hi", "hello", "ok", "okay", "so"];

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
    const EXACT_HALLUCINATIONS: &[&str] = &[
        ".",
        "..",
        "...",
        ",",
        "!",
        "?",
        "thank you",
        "thanks for watching",
        "thanks for listening",
        "thanks",
        "you",
        "bye",
        "bye bye",
        "okay",
        "the end",
        "subtitles by",
        "subtitle",
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
    if EXACT_HALLUCINATIONS.iter().any(|h| lower == *h) {
        return String::new();
    }

    // Reject very short transcripts (1-2 chars) — almost always noise artifacts.
    if cleaned.len() <= 2 {
        return String::new();
    }

    // Reject if the transcript is just the same word/syllable repeated.
    let words: Vec<&str> = lower.split_whitespace().collect();
    if words.len() >= 2 && words.iter().all(|w| *w == words[0]) {
        return String::new();
    }

    cleaned.to_string()
}
