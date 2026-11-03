//! What the room's Whisper is told came before each stretch of speech: the wake phrase and the
//! household's names, spelled as the household spells them, so the decoder prefers them to near
//! homophones ("he goes", "Gus"). Never anything a person said: a prompt Whisper reads every time
//! must not carry one turn into the next.

/// Whisper reads at most 224 prompt tokens, and every token costs decode time on every segment.
pub const MAX_PROMPT_NAMES: usize = 8;
const MAX_NAME_CHARS: usize = 40;

/// Letters, spaces and the marks names are spelled with; anything else could read as an instruction
/// to Whisper's style ("[MUSIC]") or as a sentence of its own.
fn clean_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| c.is_alphabetic() || matches!(c, ' ' | '-' | '\'' | '.'))
        .take(MAX_NAME_CHARS)
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sentence_case(phrase: &str) -> String {
    let mut chars = phrase.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `Hey goose. Jerry, Amina.` for the phrase and names given; `None` when there is neither.
pub fn room_prompt(wake_phrase: &str, names: &[String]) -> Option<String> {
    let phrase = clean_name(wake_phrase);
    let mut seen: Vec<String> = Vec::new();
    for name in names.iter().map(|n| clean_name(n)) {
        let key = name.to_lowercase();
        if !name.is_empty() && !seen.iter().any(|s| s.to_lowercase() == key) {
            seen.push(name);
        }
        if seen.len() == MAX_PROMPT_NAMES {
            break;
        }
    }
    let mut parts = Vec::new();
    if !phrase.is_empty() {
        parts.push(format!("{}.", sentence_case(&phrase)));
    }
    if !seen.is_empty() {
        parts.push(format!("{}.", seen.join(", ")));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(n: &[&str]) -> Vec<String> {
        n.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_phrase_then_the_names_as_two_short_sentences() {
        assert_eq!(
            room_prompt("hey goose", &names(&["Jerry Anyumba", "Amina"])).as_deref(),
            Some("Hey goose. Jerry Anyumba, Amina.")
        );
        assert_eq!(room_prompt("hey goose", &[]).as_deref(), Some("Hey goose."));
        assert_eq!(
            room_prompt("", &names(&["Amina"])).as_deref(),
            Some("Amina.")
        );
        assert_eq!(room_prompt("  ", &names(&["", "  "])), None);
    }

    #[test]
    fn names_are_cleaned_deduplicated_and_capped() {
        let many: Vec<String> = (0..20)
            .map(|i| format!("Member{}", "x".repeat(i)))
            .collect();
        let prompt = room_prompt("hey goose", &many).unwrap();
        assert_eq!(prompt.matches(',').count(), MAX_PROMPT_NAMES - 1);

        let messy = names(&[
            "  Jerry\n Anyumba ",
            "jerry anyumba",
            "[MUSIC] O'Neil",
            "Zoë-Ann",
        ]);
        assert_eq!(
            room_prompt("hey goose", &messy).as_deref(),
            Some("Hey goose. Jerry Anyumba, MUSIC O'Neil, Zoë-Ann.")
        );
        let long = "A".repeat(100);
        assert!(room_prompt("", &[long]).unwrap().len() <= MAX_NAME_CHARS + 1);
    }

    #[test]
    fn a_prompt_never_holds_a_nul_byte() {
        let prompt = room_prompt("hey\0goose", &names(&["Ji\0m"])).unwrap();
        assert!(!prompt.contains('\0'));
    }
}
