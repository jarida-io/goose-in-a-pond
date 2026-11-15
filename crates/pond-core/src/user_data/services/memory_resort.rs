//! Re-sorting memories filed before the profile: the prompt that asks what each note is about,
//! reading the reply, and what code then does with each note. Nothing here calls a model.
//!
//! A re-sort files and archives; it never closes one row in favour of another. Older rows were
//! written with no notion of one-at-a-time facts, and a model that files three different
//! preferences under one such word would otherwise leave two of them closed.

use crate::user_data::domain::member_profile::{admit, About, NotKept};
use crate::user_data::domain::memory::{
    MemoryFragment, MAX_FACT_CONTENT_LEN, MIN_FACT_CONTENT_LEN,
};
use crate::user_data::services::model_json::{object_in, text_of};

/// Notes one model call files: few enough that a small model answers for every one.
pub const NOTES_PER_CALL: usize = 10;
/// Calls a note may go unanswered in before the re-sort leaves it as it is.
pub const MAX_ATTEMPTS: u32 = 2;
/// How often the job looks for notes still to file.
pub const RESORT_POLL: std::time::Duration = std::time::Duration::from_secs(15 * 60);

const SYSTEM: &str = "\
You are filing notes the Pond keeps about {name} into {name}'s profile. For \
each numbered note below, say what it is about, or that it is not a lasting \
fact about {name}.

Reply with JSON only, exactly this shape:
{\"notes\":[{\"n\":1,\"about\":\"...\"}]}

- n: the note's number.
- about: exactly one of these words, or \"none\":
  name birthday work home language: who {name} is
  partner child pet family friend: who that one is to {name}
  diet allergy health: what {name} eats, what their body needs
  routine: something {name} does again and again
  likes dislikes: their tastes
  style: how they want to be answered (length, tone, format)
  goal place date: what they work towards, a place of theirs, a yearly date
  instruction: what the assistant must always or never do for them
- \"none\" when the note is not a lasting fact about {name}: a question they \
asked, something they looked up, a plan or event on one day, general \
knowledge, something the assistant said, or a guess.
One entry for every note, in order. No other keys.";

pub fn render_system(name: &str) -> String {
    SYSTEM.replace("{name}", name)
}

pub fn build_user_message(notes: &[MemoryFragment]) -> String {
    let mut out = String::from("Notes:\n");
    for (i, note) in notes.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, note.content.trim()));
    }
    out.push_str("\nJSON:");
    out
}

/// What the model said of one note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    About(About),
    /// Not a lasting fact about the member.
    NotAFact,
}

/// What the reply says of each of `count` notes, by position; `None` where it says nothing
/// usable, so that note is asked about again.
pub fn parse_reply(raw: &str, count: usize) -> Vec<Option<Said>> {
    let mut said = vec![None; count];
    let Some(map) = object_in(raw) else {
        return said;
    };
    let Some(entries) = map.get("notes").and_then(|n| n.as_array()) else {
        return said;
    };
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(n) = entry
            .get("n")
            .and_then(|n| n.as_u64().or_else(|| n.as_str()?.trim().parse().ok()))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| (1..=count).contains(n))
        else {
            continue;
        };
        let Some(word) = text_of(entry, "about") else {
            continue;
        };
        let word = word.trim().to_ascii_lowercase();
        said[n - 1] = if matches!(word.as_str(), "none" | "null" | "no" | "n/a") {
            Some(Said::NotAFact)
        } else {
            About::parse(&word).map(Said::About)
        };
    }
    said
}

/// What becomes of a note once the model has spoken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Keep it, filed under this.
    File(About),
    /// Archive it, for this reason. Never delete: a person can bring it back.
    Archive(&'static str),
    /// The model said nothing usable of it; ask again another time.
    Unanswered,
}

/// The write policy applied to a note the model has named. `aliases` are its member's names;
/// empty for a household note.
pub fn verdict(note: &MemoryFragment, said: Option<Said>, aliases: &[String]) -> Verdict {
    match said {
        None => Verdict::Unanswered,
        Some(Said::NotAFact) => Verdict::Archive("not a lasting fact about a member"),
        Some(Said::About(about)) => match admit(&note.content, Some(about), aliases) {
            Ok(_) => Verdict::File(about),
            Err(why) => Verdict::Archive(reason(why)),
        },
    }
}

fn reason(why: NotKept) -> &'static str {
    why.as_str()
}

/// Whether a note runs several facts together, so it is unpacked before anything is filed.
pub fn is_a_paragraph(note: &str) -> bool {
    note.trim().chars().count() > MAX_FACT_CONTENT_LEN
}

/// Words a sentence does not end on, though they end in a stop.
const ABBREVIATIONS: [&str; 12] = [
    "dr", "mr", "mrs", "ms", "prof", "st", "vs", "etc", "no", "ltd", "inc", "jr",
];

/// The single sentences a paragraph breaks into, for the re-sort to file one at a time. One
/// that opens on "he" or "his" names `name` instead, since alone it would say nothing of whom.
/// Sentences too long to be one fact are left out: the paragraph itself is archived, not lost.
pub fn unpacked(note: &str, name: Option<&str>) -> Vec<String> {
    let note = note.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut sentences: Vec<String> = Vec::new();
    let mut from = 0;
    let stops: Vec<(usize, char)> = note.char_indices().collect();
    for (i, &(at, c)) in stops.iter().enumerate() {
        let next = stops.get(i + 1).map(|&(_, n)| n);
        let after = stops.get(i + 2).map(|&(_, n)| n);
        let ends = match c {
            ';' => next == Some(' '),
            '.' | '!' | '?' => {
                let word = note[from..at]
                    .rsplit(' ')
                    .next()
                    .unwrap_or("")
                    .trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase();
                next == Some(' ')
                    && after.is_some_and(|a| a.is_uppercase() || a == '\'' || a == '"')
                    && word.chars().count() > 1
                    && !ABBREVIATIONS.contains(&word.as_str())
            }
            _ => false,
        };
        if ends {
            sentences.push(note[from..at].to_string());
            from = at + c.len_utf8();
        }
    }
    sentences.push(note[from..].to_string());

    let mut out: Vec<String> = Vec::new();
    for sentence in sentences {
        let sentence = sentence.trim().trim_end_matches(['.', ';']).trim();
        let sentence = match name {
            Some(name) => named(sentence, name),
            None => sentence.to_string(),
        };
        let chars = sentence.chars().count();
        if !(MIN_FACT_CONTENT_LEN..MAX_FACT_CONTENT_LEN).contains(&chars) {
            continue;
        }
        let sentence = format!("{sentence}.");
        if !out.iter().any(|kept| kept.eq_ignore_ascii_case(&sentence)) {
            out.push(sentence);
        }
    }
    out
}

/// `sentence` with a leading "he", "she", "his" or "her" replaced by `name`.
fn named(sentence: &str, name: &str) -> String {
    let (first, rest) = sentence.split_once(' ').unwrap_or((sentence, ""));
    match first.to_lowercase().as_str() {
        "he" | "she" => format!("{name} {rest}"),
        "his" | "her" => format!("{name}'s {rest}"),
        _ => {
            let mut chars = sentence.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(content: &str) -> MemoryFragment {
        MemoryFragment::from_chat("id".into(), Some("m1".into()), None, content.to_string())
    }

    fn jerry() -> Vec<String> {
        vec!["Jerry".to_string()]
    }

    #[test]
    fn the_prompt_offers_every_word_the_reply_may_use_but_now() {
        let prompt = render_system("Jerry");
        for about in About::ALL.into_iter().filter(|a| *a != About::Now) {
            assert!(
                prompt
                    .split_whitespace()
                    .any(|w| w.trim_end_matches(':') == about.as_str()),
                "{} is never offered",
                about.as_str()
            );
        }
        assert!(prompt.contains("\"none\""));
    }

    #[test]
    fn a_paragraph_is_unpacked_into_sentences_that_each_name_the_member() {
        let blob = "Jerry Anyumba is a backend engineer and CEO of Jarida One Ltd. and leads \
            Jarida, based in Nairobi. He speaks English and Swahili. His projects include \
            'Goose In A Pond'; he monitors his room with a camera. He is mentored by Dr. Anya. \
            He speaks English and Swahili.";
        assert_eq!(
            unpacked(blob, Some("Jerry Anyumba")),
            vec![
                "Jerry Anyumba is a backend engineer and CEO of Jarida One Ltd. and leads \
                 Jarida, based in Nairobi.",
                "Jerry Anyumba speaks English and Swahili.",
                "Jerry Anyumba's projects include 'Goose In A Pond'.",
                "Jerry Anyumba monitors his room with a camera.",
                "Jerry Anyumba is mentored by Dr. Anya.",
            ]
        );
        // A household note names nobody, and a run-on too long to be a fact is left out.
        assert_eq!(
            unpacked("The house has two cats. They eat at six.", None),
            vec!["The house has two cats.", "They eat at six."]
        );
        let run_on = format!("Jerry likes {}.", "tea and ".repeat(60));
        assert!(is_a_paragraph(&run_on));
        assert!(unpacked(&run_on, Some("Jerry")).is_empty());
        assert!(!is_a_paragraph("Jerry likes tea."));
    }

    #[test]
    fn the_notes_are_numbered_from_one() {
        let message = build_user_message(&[note("Jerry likes tea."), note("Jerry runs.")]);
        assert_eq!(
            message,
            "Notes:\n1. Jerry likes tea.\n2. Jerry runs.\n\nJSON:"
        );
    }

    #[test]
    fn a_reply_is_read_by_number_and_what_it_leaves_out_stays_unanswered() {
        let raw =
            "sure: {\"notes\":[{\"n\":2,\"about\":\"None\"},{\"n\":\"1\",\"about\":\"Likes\"},\
                   {\"n\":9,\"about\":\"diet\"},{\"n\":3,\"about\":\"sport\"}]}";
        assert_eq!(
            parse_reply(raw, 4),
            vec![
                Some(Said::About(About::Likes)),
                Some(Said::NotAFact),
                None,
                None
            ]
        );
        assert_eq!(parse_reply("no json here", 2), vec![None, None]);
    }

    #[test]
    fn a_named_note_is_filed_only_if_the_write_policy_keeps_it() {
        let aliases = jerry();
        assert_eq!(
            verdict(
                &note("Jerry is allergic to peanuts."),
                Some(Said::About(About::Allergy)),
                &aliases
            ),
            Verdict::File(About::Allergy)
        );
        assert_eq!(
            verdict(
                &note("The user prefers short answers."),
                Some(Said::About(About::Style)),
                &aliases
            ),
            Verdict::File(About::Style),
            "an older note calls its member the user"
        );
        assert_eq!(
            verdict(
                &note("I like my tea strong."),
                Some(Said::About(About::Likes)),
                &aliases
            ),
            Verdict::Archive("first person")
        );
        assert_eq!(
            verdict(
                &note("Jerry asked about the weather in Kisumu."),
                Some(Said::About(About::Likes)),
                &aliases
            ),
            Verdict::Archive("a question asked once")
        );
        assert_eq!(
            verdict(
                &note("The capital of France is Paris."),
                Some(Said::NotAFact),
                &aliases
            ),
            Verdict::Archive("not a lasting fact about a member")
        );
        assert_eq!(
            verdict(&note("Jerry likes tea."), None, &aliases),
            Verdict::Unanswered
        );
    }
}
