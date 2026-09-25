//! Restates the answer rule last in the turn message; stated once ~6k tokens up, it decays.
//! Keep the rules in the system prefix too: subagents never see `<system-context>`.

/// Answer-shape note for `<system-context>`, after [`super::turn_budget::turn_budget_note`].
/// Its example must stay in a domain the pond has no tool for: small models copy it verbatim.
pub fn answer_contract() -> String {
    format!(
        "<answer-contract>\n{ANSWER_RULE} Shape: \"The parcel arrives on Thursday, \
         and someone needs to sign for it.\" — the answer, nothing about looking it \
         up.\n</answer-contract>"
    )
}

/// The bare rule, shared with `orchestrator.rs :: subagent_envelope` so the two can't drift.
/// "The route to it" means narrating retrieval, not reasoning (`<thinking>` gates that).
pub const ANSWER_RULE: &str = "Reply with the result, not the route to it. If part of it \
                               failed, name that part in ordinary words.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_is_a_balanced_element() {
        let note = answer_contract();
        assert!(note.starts_with("<answer-contract>\n"), "{note}");
        assert!(note.ends_with("\n</answer-contract>"), "{note}");
        assert_eq!(note.matches("<answer-contract>").count(), 1);
        assert_eq!(note.matches("</answer-contract>").count(), 1);
    }

    /// Rule and example must agree: on a conflict the example wins.
    #[test]
    fn it_states_the_rule_and_shows_it() {
        let note = answer_contract();
        let lower = note.to_lowercase();
        assert!(
            lower.contains("not the route"),
            "the rule itself is missing: {note}"
        );
        assert!(
            note.contains('"'),
            "no worked example -- on a 2B model the demonstration is what carries \
             format compliance, not the sentence: {note}"
        );
        assert!(
            lower.contains("nothing about looking it up"),
            "the example does not say what it is an example OF: {note}"
        );
    }

    #[test]
    fn the_example_cannot_be_mistaken_for_a_real_result() {
        let note = answer_contract().to_lowercase();

        // Domains the pond has tools for.
        for word in [
            "degrees",
            "celsius",
            "fahrenheit",
            "cloudy",
            "overcast",
            "sunny",
            "raining",
            "weather",
            "temperature",
            "stock",
            "usd",
            "bitcoin",
        ] {
            assert!(
                !note.contains(word),
                "the answer-contract example mentions '{word}', a domain this pond \
                 serves with a real tool — a model that copies the example then \
                 fabricates a plausible reading instead of using the tool result: \
                 {note}"
            );
        }

        assert!(
            !note.contains("nairobi"),
            "the example names this pond's own city: {note}"
        );
    }

    /// The example is what carries format compliance on a small model.
    #[test]
    fn it_is_still_a_worked_example_and_not_just_a_rule() {
        let note = answer_contract();
        let quoted: Vec<&str> = note.split('"').collect();
        assert!(
            quoted.len() >= 3 && quoted[1].split_whitespace().count() >= 5,
            "the demonstration must remain a real sentence, not a stub: {note}"
        );
        assert!(
            quoted[1].ends_with('.'),
            "the example should model a complete declarative answer: {note}"
        );
    }

    /// "Be terse" alone teaches dropping the failed part, the one part the user can't reconstruct.
    #[test]
    fn a_shortfall_is_still_reportable() {
        let lower = answer_contract().to_lowercase();
        assert!(
            lower.contains("failed") && lower.contains("ordinary words"),
            "the contract suppresses the answer's shape without preserving the \
             admission beside it"
        );
    }

    #[test]
    fn it_stays_small_enough_to_repeat_every_turn() {
        let chars = answer_contract().chars().count();
        assert!(
            chars <= 320,
            "the answer contract is {chars} chars (~{} tokens), re-prefilled on \
             every turn. Past ~320 it stops being a restatement and becomes a \
             second copy of the rules, which belongs in the cached prefix.",
            chars / 4
        );
    }
}
