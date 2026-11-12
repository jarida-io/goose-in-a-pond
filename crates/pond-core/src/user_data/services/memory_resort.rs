//! Re-sorting memories filed before the profile: the prompt that asks what each note is about,
//! reading the reply, and what code then does with each note. Nothing here calls a model.

use crate::user_data::domain::member_profile::{admit, About, NotKept};
use crate::user_data::domain::memory::MemoryFragment;
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
  likes dislikes style: their tastes, how they like to be answered
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

/// The older rows a newly filed single-valued fact leaves closed: every live row of the same
/// member and `about` but the newest. `rows` are that member's live rows about it.
pub fn closed_by_the_newest(rows: &[MemoryFragment]) -> Vec<(&MemoryFragment, &MemoryFragment)> {
    let Some(newest) = rows.iter().max_by_key(|row| row.created_at) else {
        return Vec::new();
    };
    rows.iter()
        .filter(|row| row.id != newest.id)
        .map(|row| (row, newest))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::member_profile::file_under;

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

    #[test]
    fn of_several_rows_about_one_single_valued_thing_only_the_newest_stays() {
        let mut rows: Vec<MemoryFragment> = ["Jerry is vegetarian.", "Jerry eats fish."]
            .iter()
            .enumerate()
            .map(|(i, content)| {
                let mut row = note(content);
                row.id = format!("row-{i}");
                row.created_at += chrono::Duration::days(i as i64);
                file_under(&mut row, About::Diet);
                row
            })
            .collect();
        let closed = closed_by_the_newest(&rows);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].0.id, "row-0");
        assert_eq!(closed[0].1.id, "row-1");
        rows.truncate(1);
        assert!(closed_by_the_newest(&rows).is_empty());
    }
}
