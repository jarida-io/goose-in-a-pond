//! Tells the model the engine's per-request step cap, which the engine never states. Rides the
//! user message's `<system-context>`, not the system prompt, so the KV prefix stays stable.

/// `None` means uncapped; the `<turn-budget>` tag sets the note apart from the request.
pub fn turn_budget_note(max_steps: Option<u32>) -> String {
    let body = match max_steps {
        // Guards against the model stopping early to ask permission to continue.
        None => "Your reasoning budget for this request is unbounded. Take as many \
                 tool-calling steps as the task genuinely needs and do not stop \
                 early to ask whether you should keep going — finish the task, \
                 then answer."
            .to_string(),
        // A CEILING, not a target: pacing language here licenses partial answers in every
        // default install (`agent_max_turns` = 50). A partial answer must say it is partial.
        Some(steps) => format!(
            "You may take up to {steps} tool-calling steps for this request. Use as \
             many as the task genuinely needs — do not stop early, and do not ask \
             whether you should keep going. If you actually reach the limit, answer \
             with what you have AND say plainly which parts you could not complete. \
             Never present a partial answer as a complete one."
        ),
    };
    format!("<turn-budget>\n{body}\n</turn-budget>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capped_note_states_the_step_count() {
        let note = turn_budget_note(Some(50));
        assert!(note.starts_with("<turn-budget>\n"));
        assert!(note.ends_with("\n</turn-budget>"));
        assert!(note.contains("up to 50 tool-calling steps"), "{note}");
        assert!(!note.contains("unbounded"));
    }

    #[test]
    fn uncapped_note_never_quotes_the_sentinel() {
        let note = turn_budget_note(None);
        assert!(note.contains("unbounded"), "{note}");
        // The sentinel (100_000) must never leak into the prompt as a number.
        assert!(!note.contains("100000"));
        assert!(!note.contains("100_000"));
        assert!(note.contains("do not stop"), "{note}");
    }

    #[test]
    fn no_budget_note_licenses_a_partial_answer() {
        for (label, note) in [
            ("uncapped", turn_budget_note(None)),
            ("capped", turn_budget_note(Some(50))),
            ("capped-small", turn_budget_note(Some(8))),
        ] {
            let lower = note.to_lowercase();
            for banned in ["stop gathering", "pace yourself"] {
                assert!(
                    !lower.contains(banned),
                    "the {label} turn-budget note tells the model to {banned:?}. The budget is a \
                     ceiling, not a target to economise against — this exact phrasing shipped in \
                     every default install and a 2B model answered a ten-item question with zero \
                     tool calls. Note: {note}"
                );
            }
            assert!(
                lower.contains("do not stop early"),
                "the {label} note does not push against stopping early. Note: {note}"
            );
        }
    }

    /// The danger is a partial answer that reads as a whole one, not partiality itself.
    #[test]
    fn the_capped_note_requires_an_incomplete_answer_to_say_so() {
        let note = turn_budget_note(Some(50)).to_lowercase();
        assert!(
            note.contains("could not complete"),
            "the capped note no longer asks the model to name the parts it could not finish; \
             an unlabelled partial answer is indistinguishable from a complete one. Note: {note}"
        );
        assert!(
            note.contains("never present a partial answer as a complete one"),
            "the capped note lost the instruction not to pass off a partial answer as whole. \
             Note: {note}"
        );
    }

    /// The adapter concatenates it into `<system-context>` beside `<memories>`.
    #[test]
    fn note_is_a_single_well_formed_element() {
        for note in [turn_budget_note(None), turn_budget_note(Some(8))] {
            assert_eq!(note.matches("<turn-budget>").count(), 1);
            assert_eq!(note.matches("</turn-budget>").count(), 1);
            assert!(!note.contains("<system-context>"));
            assert_eq!(note.trim(), note);
        }
    }
}
