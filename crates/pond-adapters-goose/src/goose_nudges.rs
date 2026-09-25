//! Pins the wording of the nudges goose appends last in context: a bolded `**Goal:**` there
//! leaked into small-model answers. Here, not in `pond-core`, which must not read the submodule.

#![cfg(test)]

/// The fork file that builds the engine's steering messages.
const AGENT_RS: &str = include_str!("../../../goose/crates/goose/src/agents/agent.rs");

/// Fewer means the extractor broke: three goal/grind/kickoff injections plus the stop hook.
const FLOOR_NUDGES: usize = 4;

/// Strip `//` comments (strings intact): `agent.rs` comments quote the banned wording.
fn strip_line_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let bytes = line.as_bytes();
        let mut in_string = false;
        let mut cut = line.len();
        let mut i = 0usize;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' if in_string => i += 1,
                b'"' => in_string = !in_string,
                b'/' if !in_string && bytes.get(i + 1) == Some(&b'/') => {
                    cut = i;
                    break;
                }
                _ => {}
            }
            i += 1;
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// `(anchor, literal_text)` for every message the engine injects, `{...}` removed. Anchored on
/// the bindings, not `Message::user()`, whose text is built a statement earlier.
fn injected_message_texts(src: &str) -> Vec<(String, String)> {
    // `goal_nudge` builds the completeness nudge in a single `format!`, so the walk captures it.
    const ANCHORS: &[&str] = &[
        "let nudge = format!(",
        "let kickoff = Message::user()",
        "fn goal_nudge",
    ];
    let code = strip_line_comments(src);
    let mut found = Vec::new();

    for anchor in ANCHORS {
        for (at, _) in code.match_indices(anchor) {
            // The statement ends at the first `;` outside a string literal.
            let tail = &code[at..];
            let mut in_string = false;
            let mut end = tail.len();
            let bytes = tail.as_bytes();
            let mut i = 0usize;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' if in_string => i += 1,
                    b'"' => in_string = !in_string,
                    b';' if !in_string => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
                i += 1;
            }
            let stmt = &tail[..end];

            // Join the string literals and drop `{...}`, so a variable named `goal` isn't the word.
            let mut text = String::new();
            for (n, piece) in stmt.split('"').enumerate() {
                if n % 2 == 1 {
                    text.push_str(piece);
                }
            }
            let mut cleaned = String::with_capacity(text.len());
            let mut depth = 0usize;
            for ch in text.chars() {
                match ch {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    _ if depth == 0 => cleaned.push(ch),
                    _ => {}
                }
            }
            found.push(((*anchor).to_string(), cleaned));
        }
    }
    found
}

#[test]
fn no_injected_nudge_carries_a_quotable_label() {
    let nudges = injected_message_texts(AGENT_RS);
    assert!(
        nudges.len() >= FLOOR_NUDGES,
        "found {} injected messages (floor {FLOOR_NUDGES}) — the extractor is \
         broken, so an empty result proves nothing",
        nudges.len()
    );

    for (anchor, text) in &nudges {
        let lower = text.to_lowercase();
        assert!(
            !lower.contains("goal"),
            "an injected message still says \"goal\":\n  anchor: {anchor}\n  text: {text:?}\n\
             This message is appended as an invisible USER message at the END of the \
             context. E2B and E4B read that noun straight back to the household — \
             \"I could not fully meet your goal\" — and the system prompt cannot \
             outrank it from thousands of tokens away. Say what was asked, not what \
             the harness calls it."
        );
        assert!(
            !text.contains("**"),
            "an injected message carries Markdown emphasis:\n  anchor: {anchor}\n  \
             text: {text:?}\nEvery GIAP prompt style forbids Markdown in output, so a \
             bolded label in the model's last input is both a quotable label and a \
             formatting instruction that contradicts the prompt."
        );
    }
}

#[test]
fn the_bolded_goal_label_is_gone_from_the_whole_file() {
    let code = strip_line_comments(AGENT_RS);
    assert!(
        !code.contains("**Goal:"),
        "`**Goal:` is back in agent.rs. This is the literal wording measured \
         leaking into household-facing answers on both gemma-4-E2B and E4B."
    );
}
