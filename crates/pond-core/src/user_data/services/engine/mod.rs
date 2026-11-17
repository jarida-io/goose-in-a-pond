//! The idea engine's pure core: how its jobs queue, what a critic and a reviser are asked, what
//! follows a job's steps, the deliverable recipes and briefs, settling, and the tick.

pub mod brief;
pub mod critic;
pub mod questions;
pub mod queue;
pub mod recipes;
pub mod refresh;
pub mod revise;
pub mod settle;
pub mod steps;
pub mod task;
pub mod tick;

/// The roles of the steps that are one direct model call, with no agent session and no tools.
pub const ROLE_MAKE: &str = "engine-make";
pub const ROLE_CRITIC: &str = "engine-critic";
pub const ROLE_REVISE: &str = "engine-revise";

/// Minutes a deliverable job has, and a job made of inline steps.
pub const DELIVERABLE_MINUTES: i64 = 60;
pub const INLINE_MINUTES: i64 = 15;

/// The heading a refreshed document opens with, and what stands under it when nothing changed.
pub const CHANGED_HEADING: &str = "What changed";
pub const NO_CHANGE: &str = "No change.";

pub fn is_inline_role(role: &str) -> bool {
    matches!(role.trim(), ROLE_MAKE | ROLE_CRITIC | ROLE_REVISE)
}

const CUT_MARK: &str = " [cut]";
/// A cut part shorter than this says nothing, so it is left out whole.
const MIN_PART_CHARS: usize = 40;

fn chars(text: &str) -> usize {
    text.chars().count()
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `text` cut to `max` characters at a word, marked as cut.
fn clip(text: &str, max: usize) -> String {
    let text = text.trim();
    if chars(text) <= max {
        return text.to_string();
    }
    let room = max.saturating_sub(chars(CUT_MARK));
    let cut: String = text.chars().take(room).collect();
    let kept = match cut.rfind(char::is_whitespace) {
        Some(at) => cut.split_at(at).0,
        None => cut.as_str(),
    };
    if kept.trim().is_empty() {
        return String::new();
    }
    format!("{}{CUT_MARK}", kept.trim_end())
}

/// The whole lines of `text`, from the top, that fit in `max` characters.
fn fit_lines(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for line in text.trim().lines() {
        let cost = chars(line) + usize::from(!out.is_empty());
        if used + cost > max {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        used += cost;
    }
    out.trim_end().to_string()
}

fn bullets<'a>(items: impl IntoIterator<Item = &'a str>) -> String {
    items
        .into_iter()
        .map(one_line)
        .filter(|item| !item.is_empty())
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Questions a member answered, one to a line, as a prompt reads them.
fn answer_lines(answered: &[(String, String)]) -> String {
    answered
        .iter()
        .map(|(question, answer)| (one_line(question), one_line(answer)))
        .filter(|(question, answer)| !question.is_empty() && !answer.is_empty())
        .map(|(question, answer)| format!("- {question} Answer: {answer}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Appends one headed part of a prompt; a part with nothing in it is left out.
fn section(out: &mut String, heading: &str, body: &str) {
    let body = body.trim();
    if body.is_empty() {
        return;
    }
    out.push_str(heading);
    out.push('\n');
    out.push_str(body);
    out.push_str("\n\n");
}

fn section_cost(heading: &str) -> usize {
    chars(heading) + 3
}

/// A prompt's character budget, handed out to its parts in the order they are asked for.
struct Room(usize);

impl Room {
    fn new(budget_chars: usize) -> Self {
        Self(budget_chars)
    }

    /// Counts a part that is never cut.
    fn keep(&mut self, heading: &str, body: &str) {
        let body = body.trim();
        if !body.is_empty() {
            self.0 = self.0.saturating_sub(section_cost(heading) + chars(body));
        }
    }

    fn for_body(&self, heading: &str, body: &str) -> Option<usize> {
        let left = self.0.checked_sub(section_cost(heading))?;
        (!body.trim().is_empty() && left >= MIN_PART_CHARS.min(chars(body.trim()))).then_some(left)
    }

    fn take(&mut self, heading: &str, fitted: String) -> String {
        if !fitted.is_empty() {
            self.0 = self
                .0
                .saturating_sub(section_cost(heading) + chars(&fitted));
        }
        fitted
    }

    /// As many whole lines of `body`, from the top, as the budget still holds.
    fn lines(&mut self, heading: &str, body: &str) -> String {
        match self.for_body(heading, body) {
            Some(left) => self.take(heading, fit_lines(body, left)),
            None => String::new(),
        }
    }

    /// `body`, cut at its end to what the budget still holds.
    fn text(&mut self, heading: &str, body: &str) -> String {
        match self.for_body(heading, body) {
            Some(left) => self.take(heading, clip(body, left)),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::engine_job::JobKind;

    #[test]
    fn the_inline_roles_are_the_three_the_engine_runs_itself() {
        for role in [ROLE_MAKE, ROLE_CRITIC, ROLE_REVISE] {
            assert!(is_inline_role(role), "{role}");
        }
        assert!(is_inline_role(" engine-critic "));
        assert!(!is_inline_role("long-task-worker"));
        assert!(!is_inline_role("engine"));
    }

    #[test]
    fn the_time_caps_are_the_ones_a_job_is_judged_by() {
        assert_eq!(
            JobKind::Deliverable.time_cap().num_minutes(),
            DELIVERABLE_MINUTES
        );
        for kind in [JobKind::FactSheet, JobKind::Questions, JobKind::Capture] {
            assert_eq!(kind.time_cap().num_minutes(), INLINE_MINUTES);
        }
    }

    #[test]
    fn text_is_cut_at_a_word_and_says_so() {
        assert_eq!(clip("  short  ", 20), "short");
        let cut = clip("one two three four five six seven", 20);
        assert_eq!(cut, "one two three [cut]");
        assert!(chars(&cut) <= 20);
        assert_eq!(clip("a long sentence", 3), "");
    }

    #[test]
    fn lines_are_kept_whole_from_the_top() {
        let sheet = "first line\nsecond line\nthird line";
        assert_eq!(fit_lines(sheet, 22), "first line\nsecond line");
        assert_eq!(fit_lines(sheet, 21), "first line");
        assert_eq!(fit_lines(sheet, 5), "");
        assert_eq!(fit_lines(sheet, 500), sheet);
    }

    #[test]
    fn a_budget_gives_the_first_asker_what_it_needs_and_the_last_what_is_left() {
        let mut room = Room::new(100);
        room.keep("Draft:", &"d".repeat(30));
        assert_eq!(room.0, 100 - 30 - 9);
        let first = room.lines("Facts:", "a fact that is known\nanother fact that is known");
        assert_eq!(first, "a fact that is known\nanother fact that is known");
        let second = room.text("Brief:", &"word ".repeat(40));
        assert!(second.is_empty(), "nothing worth showing is left: {second}");
        assert_eq!(room.lines("Empty:", "  "), "");
    }
}
