//! The `<memories>` block a turn's `<system-context>` carries, and which memories a conversation
//! already holds.
//!
//! On the live path a turn's envelope stays in the history (`turn_trimmer` only runs on the hybrid
//! path), so a memory shown once is still in front of the model on every later turn. Shown again,
//! it costs each turn's prefill and grows the window: over ten days on the household Orin, 186 of
//! 335 injected memory lines (56%) were already in their conversation.

use crate::user_data::domain::memory::MemoryFragment;

/// One memory as the block shows it.
pub fn line(segment: Option<&str>, content: &str) -> String {
    match segment {
        Some(segment) if !segment.is_empty() => format!("- [{segment}] {content}"),
        _ => format!("- {content}"),
    }
}

/// A memory entry for structured presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryView<'a> {
    pub segment: Option<&'a str>,
    pub content: &'a str,
    pub corrects: Option<&'a str>,
}

impl<'a> MemoryView<'a> {
    pub fn new(segment: Option<&'a str>, content: &'a str) -> Self {
        Self {
            segment,
            content,
            corrects: None,
        }
    }

    pub fn with_corrects(mut self, corrects: Option<&'a str>) -> Self {
        self.corrects = corrects;
        self
    }
}

/// Strip bullet prefix ("- [segment] " or "- ") to get clean content for matching.
fn strip_bullet_prefix(s: &str) -> &str {
    let t = s.trim();
    if let Some(rest) = t.strip_prefix("- [") {
        if let Some(idx) = rest.find("] ") {
            return rest[idx + 2..].trim();
        }
    }
    if let Some(rest) = t.strip_prefix("- ") {
        return rest.trim();
    }
    t
}

/// Whether `rendered` (a [`line`]) is a whole line of some `<memories>` block in `earlier`, the
/// text of an earlier user message. Lines inside a block always have a newline on each side.
pub fn shown_in(rendered: &str, earlier: &str) -> bool {
    let target_content = strip_bullet_prefix(rendered);
    let mut rest = earlier;
    while let Some(start) = rest.find("<memories>\n") {
        let body = &rest[start + "<memories>".len()..];
        let Some(end) = body.find("\n</memories>") else {
            return false;
        };
        let block = &body[..=end];
        if block.contains(&format!("\n{rendered}\n")) {
            return true;
        }
        if !target_content.is_empty() {
            for block_line in block.lines() {
                if strip_bullet_prefix(block_line) == target_content {
                    return true;
                }
            }
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

/// Render structured categorized memories to be placed inside `<memories>`.
///
/// Categorization groups:
/// 1. `<corrections>`: Critical user corrections and negative constraints (highest priority)
/// 2. `<identity>`: Core user identity, bio and household relationships
/// 3. `<preferences>`: User preferences and recurring routines/habits
/// 4. `<projects>`: Active projects, tasks and temporary context
/// 5. `<knowledge>`: General reference facts and domain knowledge
pub fn render_structured(items: &[MemoryView<'_>]) -> String {
    let mut corrections = Vec::new();
    let mut identity = Vec::new();
    let mut preferences = Vec::new();
    let mut projects = Vec::new();
    let mut knowledge = Vec::new();

    for item in items {
        let seg = item.segment.unwrap_or("").to_lowercase();
        if seg == "correction" || item.corrects.is_some() {
            if let Some(corrects) = item.corrects {
                corrections.push(format!("- {} (Replaces: {})", item.content, corrects));
            } else {
                corrections.push(format!("- {}", item.content));
            }
        } else if seg == "identity" || seg == "relationship" {
            identity.push(format!("- {}", item.content));
        } else if seg == "preference" || seg == "routine" {
            preferences.push(format!("- {}", item.content));
        } else if seg == "project" || seg == "context" {
            projects.push(format!("- {}", item.content));
        } else {
            knowledge.push(format!("- {}", item.content));
        }
    }

    let mut sections = Vec::new();
    if !corrections.is_empty() {
        sections.push(format!(
            "<corrections>\n{}\n</corrections>",
            corrections.join("\n")
        ));
    }
    if !identity.is_empty() {
        sections.push(format!("<identity>\n{}\n</identity>", identity.join("\n")));
    }
    if !preferences.is_empty() {
        sections.push(format!(
            "<preferences>\n{}\n</preferences>",
            preferences.join("\n")
        ));
    }
    if !projects.is_empty() {
        sections.push(format!("<projects>\n{}\n</projects>", projects.join("\n")));
    }
    if !knowledge.is_empty() {
        sections.push(format!(
            "<knowledge>\n{}\n</knowledge>",
            knowledge.join("\n")
        ));
    }

    sections.join("\n")
}

/// Convenience helper to render a slice of [`MemoryFragment`] into structured XML sections.
pub fn render_fragments<'a>(fragments: impl IntoIterator<Item = &'a MemoryFragment>) -> String {
    let items: Vec<MemoryView<'_>> = fragments
        .into_iter()
        .map(|f| {
            let seg = f.segment.as_ref().map(|s| s.as_str());
            MemoryView::new(seg, &f.content).with_corrects(f.corrects.as_deref())
        })
        .collect();
    render_structured(&items)
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
    fn render_structured_prioritizes_corrections_and_groups_sections() {
        let items = vec![
            MemoryView::new(Some("knowledge"), "Rust compiles to native."),
            MemoryView::new(Some("preference"), "Prefers dark mode."),
            MemoryView::new(Some("correction"), "User is vegan, no dairy.")
                .with_corrects(Some("likes cheese")),
            MemoryView::new(Some("identity"), "Jerry lives in Ndenderu."),
            MemoryView::new(Some("project"), "Building Goose home assistant."),
        ];
        let rendered = render_structured(&items);
        assert!(rendered.starts_with(
            "<corrections>\n- User is vegan, no dairy. (Replaces: likes cheese)\n</corrections>"
        ));
        assert!(rendered.contains("<identity>\n- Jerry lives in Ndenderu.\n</identity>"));
        assert!(rendered.contains("<preferences>\n- Prefers dark mode.\n</preferences>"));
        assert!(rendered.contains("<projects>\n- Building Goose home assistant.\n</projects>"));
        assert!(rendered.contains("<knowledge>\n- Rust compiles to native.\n</knowledge>"));
    }

    #[test]
    fn shown_in_matches_across_structured_sections() {
        let items = vec![
            MemoryView::new(Some("identity"), "Jerry lives in Ndenderu."),
            MemoryView::new(Some("preference"), "Prefers dark mode."),
        ];
        let structured = render_structured(&items);
        let earlier = format!(
            "<user-message>\nhi\n</user-message>\n<system-context>\n<memories>\n{}\n</memories>\n</system-context>",
            structured
        );

        // Matches both plain bullet and bracketed segment line
        assert!(shown_in("- Jerry lives in Ndenderu.", &earlier));
        assert!(shown_in("- [identity] Jerry lives in Ndenderu.", &earlier));
        assert!(shown_in("- Prefers dark mode.", &earlier));
        assert!(shown_in("- [preference] Prefers dark mode.", &earlier));
        assert!(!shown_in("- Jerry lives in Mombasa.", &earlier));
    }

    #[test]
    fn render_structured_empty_when_no_memories() {
        assert!(render_structured(&[]).is_empty());
    }
}
