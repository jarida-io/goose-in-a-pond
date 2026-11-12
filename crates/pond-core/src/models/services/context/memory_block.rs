//! The `<memories>` block a turn's `<system-context>` carries, and which memories a conversation
//! already holds.
//!
//! On the live path a turn's envelope stays in the history (`turn_trimmer` only runs on the hybrid
//! path), so a memory shown once is still in front of the model on every later turn. Shown again,
//! it costs each turn's prefill and grows the window: over ten days on the household Orin, 186 of
//! 335 injected memory lines (56%) were already in their conversation.

/// One memory as the block shows it.
pub fn line(segment: Option<&str>, content: &str) -> String {
    match segment {
        Some(segment) if !segment.is_empty() => format!("- [{segment}] {content}"),
        _ => format!("- {content}"),
    }
}

/// The fact a block line states: without its bullet, an older `[segment]` tag, or the claim a
/// correction says it replaces. `None` for a heading, which is no fact.
fn stated_fact(s: &str) -> Option<&str> {
    let rest = s.trim().strip_prefix("- ")?;
    let rest = match rest.strip_prefix('[').and_then(|r| r.split_once("] ")) {
        Some((_, after)) => after,
        None => rest,
    };
    let rest = match rest.rfind(" (replaces: ") {
        Some(at) if rest.ends_with(')') => &rest[..at],
        _ => rest,
    };
    Some(rest.trim())
}

/// Whether the fact `rendered` (a [`line`]) states is a line of some `<memories>` block in
/// `earlier`, the text of an earlier user message, under whatever heading it stood there.
pub fn shown_in(rendered: &str, earlier: &str) -> bool {
    let Some(fact) = stated_fact(rendered).filter(|f| !f.is_empty()) else {
        return false;
    };
    let mut rest = earlier;
    while let Some(start) = rest.find("<memories>\n") {
        let body = &rest[start + "<memories>".len()..];
        let Some(end) = body.find("\n</memories>") else {
            return false;
        };
        if body[..=end].lines().any(|l| stated_fact(l) == Some(fact)) {
            return true;
        }
        rest = &body[end..];
    }
    false
}

/// The memories in `rendered` that no earlier user message of the conversation shows, in order.
pub fn not_yet_shown<'a>(rendered: &'a [String], earlier: &[String]) -> Vec<&'a String> {
    rendered
        .iter()
        .filter(|line| !earlier.iter().any(|text| shown_in(line, text)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(memories: &[&str]) -> String {
        format!(
            "<user-message>\nhi\n</user-message>\n<system-context>\nCURRENT CONTEXT: Today.\n\
             <memories>\n{}\n</memories>\n<answer-contract>\nx\n</answer-contract>\n\
             </system-context>",
            memories.join("\n")
        )
    }

    #[test]
    fn a_line_names_its_segment_when_it_has_one() {
        assert_eq!(
            line(Some("identity"), "Jerry lives in Ndenderu."),
            "- [identity] Jerry lives in Ndenderu."
        );
        assert_eq!(line(None, "Likes chapati."), "- Likes chapati.");
        assert_eq!(line(Some(""), "Likes chapati."), "- Likes chapati.");
    }

    #[test]
    fn first_middle_and_last_lines_of_a_block_count_as_shown() {
        let earlier = envelope(&["- [identity] A.", "- [knowledge] B.", "- C."]);
        for rendered in ["- [identity] A.", "- [knowledge] B.", "- C."] {
            assert!(shown_in(rendered, &earlier), "{rendered}");
        }
    }

    #[test]
    fn a_line_that_only_starts_another_is_not_shown() {
        let earlier = envelope(&["- [identity] Jerry lives in Ndenderu."]);
        assert!(!shown_in("- [identity] Jerry lives", &earlier));
        assert!(!shown_in("- [identity] Jerry lives in Ndenderu", &earlier));
    }

    #[test]
    fn text_outside_a_memories_block_does_not_count() {
        let earlier = "<user-message>\n- [identity] A.\n</user-message>".to_string();
        assert!(!shown_in("- [identity] A.", &earlier));
    }

    #[test]
    fn every_block_in_a_message_is_searched() {
        let earlier = format!("{}\n{}", envelope(&["- A."]), envelope(&["- B."]));
        assert!(shown_in("- A.", &earlier));
        assert!(shown_in("- B.", &earlier));
    }

    #[test]
    fn only_memories_no_earlier_turn_shows_are_kept_in_their_order() {
        let earlier = vec![envelope(&["- [identity] A.", "- B."])];
        let rendered = vec![
            "- C.".to_string(),
            "- [identity] A.".to_string(),
            "- D.".to_string(),
            "- B.".to_string(),
        ];
        assert_eq!(
            not_yet_shown(&rendered, &earlier),
            vec![&rendered[0], &rendered[2]]
        );
        assert_eq!(
            not_yet_shown(&rendered, &[]).len(),
            4,
            "a first turn shows everything"
        );
    }

    #[test]
    fn a_fact_shown_under_a_heading_or_as_a_correction_counts_as_shown() {
        let earlier = "<system-context>\n<memories>\nPreferences\n- Jerry likes dark mode. \
                       (replaces: Jerry likes light mode.)\nWho they are\n- Jerry lives in \
                       Kisumu.\n</memories>\n</system-context>";
        assert!(shown_in(
            &line(Some("preference"), "Jerry likes dark mode."),
            earlier
        ));
        assert!(shown_in(&line(None, "Jerry lives in Kisumu."), earlier));
        assert!(!shown_in(&line(None, "Jerry likes light mode."), earlier));
        assert!(!shown_in("Preferences", earlier), "a heading is not a fact");
    }
}
