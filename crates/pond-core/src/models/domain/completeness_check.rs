//! When goose's completeness check ("Check the question was answered", `goal_check_enabled`) may
//! run its round. The adapter asks this the moment goose arms the check.
//!
//! The check appends a hidden nudge after the model's last reply. A nudge round without tool
//! calls is hidden by goose; one WITH tool calls stays visible, and the pond's stream then moves
//! the answer already shown into working notes (text before a tool call is the model at work) and
//! shows whatever the model writes next. So after a real answer, the check can only replace it,
//! and on the Jetson it replaced "Your girlfriend is Arlene" with a refusal. The check is kept
//! for what it is for: a turn that ended without an answer, or one in which a tool failed and
//! the model may still put that right.

/// Whether the completeness check should run its round. `answered`: the model wrote text after
/// its last tool call. `tool_failed`: any tool call this turn failed.
pub fn completeness_check_needed(answered: bool, tool_failed: bool) -> bool {
    !answered || tool_failed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_with_nothing_failed_is_left_alone() {
        assert!(!completeness_check_needed(true, false));
    }

    #[test]
    fn a_turn_without_an_answer_is_checked() {
        assert!(completeness_check_needed(false, false));
    }

    #[test]
    fn a_turn_with_a_failed_tool_is_checked() {
        assert!(completeness_check_needed(true, true));
    }
}
