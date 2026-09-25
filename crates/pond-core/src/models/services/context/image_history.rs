//! Caps the history images kept as pixels; older ones degrade to a text placeholder.
//! An image anywhere makes the turn multimodal, which bypasses the engine's KV prompt cache.

use std::borrow::Cow;

/// One serves "a photo, then questions about it"; raising it inflates every later prefill.
/// The CURRENT turn's image is not history, so it is outside this budget.
pub const MAX_HISTORY_REPLAY_IMAGES: usize = 1;

/// Prefix of every placeholder wording; matching on it also catches older builds' wordings.
pub const HISTORY_IMAGE_PLACEHOLDER_MARKER: &str = "[image attachment removed";

/// Stand-in for a message whose images were ALL dropped.
pub const HISTORY_IMAGE_PLACEHOLDER: &str =
    "[image attachment removed: an image attached here is no longer available in this context]";

/// Stand-in for a message that lost SOME images. Never says "above" (the kept image may sit on
/// either side) or gives a count (a message can be capped again on a later turn).
pub const HISTORY_IMAGE_PLACEHOLDER_PARTIAL: &str =
    "[image attachment removed: at least one other image attached here is no longer available in \
     this context; the image still shown in this message is unaffected]";

/// The placeholder that is true for a message left holding `kept` images.
/// `kept` is the count ACTUALLY attached, not the plan's: a planned image's bytes may not load.
#[must_use]
pub fn history_image_placeholder(kept: usize) -> &'static str {
    if kept == 0 {
        HISTORY_IMAGE_PLACEHOLDER
    } else {
        HISTORY_IMAGE_PLACEHOLDER_PARTIAL
    }
}

/// Whether `text` already carries a placeholder from an earlier capping pass.
#[must_use]
pub fn contains_history_image_placeholder(text: &str) -> bool {
    text.contains(HISTORY_IMAGE_PLACEHOLDER_MARKER)
}

/// Remove every placeholder so exactly one is re-emitted, making repeated capping converge.
/// Removes marker through `]` (or to the end) for any wording, so older builds' are caught.
#[must_use]
pub fn strip_history_image_placeholders(text: &str) -> Cow<'_, str> {
    if !contains_history_image_placeholder(text) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(HISTORY_IMAGE_PLACEHOLDER_MARKER) {
        out.push_str(&rest[..start]);
        let after = &rest[start + HISTORY_IMAGE_PLACEHOLDER_MARKER.len()..];
        match after.find(']') {
            Some(end) => rest = &after[end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out.trim().to_string())
}

/// Images to replay per message, given per-message counts in CHRONOLOGICAL order. Budget is
/// spent newest-first; within a message the LEADING images stay (users refer to the first).
#[must_use]
pub fn plan_image_replay(image_counts: &[usize], budget: usize) -> Vec<usize> {
    let mut plan = vec![0usize; image_counts.len()];
    let mut remaining = budget;
    for (slot, count) in plan.iter_mut().zip(image_counts.iter()).rev() {
        if remaining == 0 {
            break;
        }
        let take = (*count).min(remaining);
        *slot = take;
        remaining -= take;
    }
    plan
}

/// Hydration replay and the live trimmer must both use this, or a restart gains or loses images.
#[must_use]
pub fn plan_history_images(image_counts: &[usize]) -> Vec<usize> {
    plan_image_replay(image_counts, MAX_HISTORY_REPLAY_IMAGES)
}

/// Images a plan drops; zero means leave the conversation byte-identical.
#[must_use]
pub fn dropped_image_count(image_counts: &[usize], plan: &[usize]) -> usize {
    image_counts
        .iter()
        .zip(plan.iter())
        .map(|(had, keep)| had.saturating_sub(*keep))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_history_plans_nothing() {
        assert!(plan_image_replay(&[], MAX_HISTORY_REPLAY_IMAGES).is_empty());
    }

    #[test]
    fn text_only_history_replays_nothing() {
        assert_eq!(plan_image_replay(&[0, 0, 0], 2), vec![0, 0, 0]);
    }

    #[test]
    fn budget_is_spent_on_the_newest_images_first() {
        assert_eq!(plan_image_replay(&[1, 0, 1, 0, 1], 1), vec![0, 0, 0, 0, 1]);
    }

    #[test]
    fn budget_spills_backwards_when_the_newest_turn_is_smaller() {
        assert_eq!(plan_image_replay(&[2, 0, 1], 3), vec![2, 0, 1]);
        assert_eq!(plan_image_replay(&[2, 0, 1], 2), vec![1, 0, 1]);
    }

    #[test]
    fn a_message_is_replayed_partially_rather_than_not_at_all() {
        assert_eq!(plan_image_replay(&[4], 2), vec![2]);
    }

    #[test]
    fn zero_budget_drops_everything() {
        assert_eq!(plan_image_replay(&[3, 2, 1], 0), vec![0, 0, 0]);
    }

    #[test]
    fn plan_never_exceeds_what_a_message_actually_has() {
        let counts = [1usize, 2, 0, 1];
        let plan = plan_image_replay(&counts, 100);
        assert_eq!(plan, counts);
    }

    #[test]
    fn default_budget_keeps_only_the_latest_image() {
        let plan = plan_image_replay(&[1, 1], MAX_HISTORY_REPLAY_IMAGES);
        assert_eq!(plan, vec![0, 1]);
    }

    #[test]
    fn plan_history_images_uses_the_shared_budget() {
        assert_eq!(
            plan_history_images(&[1, 0, 1, 0, 1]),
            plan_image_replay(&[1, 0, 1, 0, 1], MAX_HISTORY_REPLAY_IMAGES)
        );
    }

    #[test]
    fn nothing_is_dropped_when_history_already_fits() {
        for counts in [vec![], vec![0, 0, 0], vec![0, 1, 0]] {
            let plan = plan_history_images(&counts);
            assert_eq!(dropped_image_count(&counts, &plan), 0, "counts={counts:?}");
            assert_eq!(plan, counts, "counts={counts:?}");
        }
    }

    #[test]
    fn dropped_count_is_every_image_beyond_the_budget() {
        let counts = vec![2, 0, 3, 1];
        let plan = plan_history_images(&counts);
        assert_eq!(plan, vec![0, 0, 0, 1]);
        assert_eq!(dropped_image_count(&counts, &plan), 5);
    }

    #[test]
    fn dropped_count_tolerates_a_short_plan() {
        assert_eq!(dropped_image_count(&[1, 1, 1], &[0]), 1);
    }

    // ── Placeholder wording and convergence ───────────────────────────────

    #[test]
    fn every_wording_carries_the_marker() {
        for text in [HISTORY_IMAGE_PLACEHOLDER, HISTORY_IMAGE_PLACEHOLDER_PARTIAL] {
            assert!(text.starts_with(HISTORY_IMAGE_PLACEHOLDER_MARKER), "{text}");
            assert!(text.ends_with(']'), "{text}");
            assert!(contains_history_image_placeholder(text), "{text}");
        }
        assert_eq!(history_image_placeholder(0), HISTORY_IMAGE_PLACEHOLDER);
        for kept in 1..4 {
            assert_eq!(
                history_image_placeholder(kept),
                HISTORY_IMAGE_PLACEHOLDER_PARTIAL
            );
        }
    }

    #[test]
    fn the_partial_wording_does_not_contradict_the_surviving_image() {
        let partial = HISTORY_IMAGE_PLACEHOLDER_PARTIAL.to_lowercase();
        assert!(
            partial.contains("other image"),
            "must describe the DROPPED images, not the kept one"
        );
        assert!(
            partial.contains("still shown"),
            "must acknowledge the image that is still attached"
        );
        for positional in ["above", "below", "preceding", "following"] {
            assert!(
                !partial.contains(positional),
                "'{positional}' is wrong on one of the two callers"
            );
        }
    }

    #[test]
    fn text_without_a_placeholder_is_borrowed_unchanged() {
        let text = "what colour is this?";
        let stripped = strip_history_image_placeholders(text);
        assert!(matches!(stripped, Cow::Borrowed(_)));
        assert_eq!(stripped, text);
    }

    #[test]
    fn stripping_removes_every_placeholder_and_leaves_the_real_text() {
        let text = format!("look at this\n{HISTORY_IMAGE_PLACEHOLDER}");
        assert_eq!(strip_history_image_placeholders(&text), "look at this");

        // Both wordings, in either order, and more than one of them.
        let doubled = format!(
            "{HISTORY_IMAGE_PLACEHOLDER_PARTIAL}\nwhat colour is this?\n{HISTORY_IMAGE_PLACEHOLDER}"
        );
        let stripped = strip_history_image_placeholders(&doubled);
        assert_eq!(stripped, "what colour is this?");
        assert!(!contains_history_image_placeholder(&stripped));
    }

    #[test]
    fn stripping_a_message_that_was_only_a_placeholder_leaves_nothing() {
        assert_eq!(
            strip_history_image_placeholders(HISTORY_IMAGE_PLACEHOLDER),
            ""
        );
    }

    #[test]
    fn an_unterminated_placeholder_is_dropped_whole() {
        let text = format!("look\n{HISTORY_IMAGE_PLACEHOLDER_MARKER}: an image was att");
        assert_eq!(strip_history_image_placeholders(&text), "look");
    }

    #[test]
    fn repeated_strip_and_emit_converges_to_one_placeholder() {
        let mut text = "what colour is this?".to_string();
        for kept in [1usize, 0, 0] {
            text = format!(
                "{}\n{}",
                strip_history_image_placeholders(&text),
                history_image_placeholder(kept)
            );
            assert_eq!(
                text.matches(HISTORY_IMAGE_PLACEHOLDER_MARKER).count(),
                1,
                "one placeholder per state, never one per pass: {text}"
            );
        }
        assert!(text.starts_with("what colour is this?"));
        assert!(text.ends_with(HISTORY_IMAGE_PLACEHOLDER));
    }
}
