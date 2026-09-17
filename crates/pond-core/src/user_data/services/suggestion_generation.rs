//! Composing a question out of one of the household's own memories.
//!
//! # Why this exists
//!
//! `suggestion.rs` picks among seven fixed prompt strings and attaches a
//! measured count to whichever it picks. That tier is correct, costs no
//! inference, and answers on a pond with no model at all — but its questions
//! never vary, so a household with mail, memories and devices reads the same
//! three sentences every day. They call that a placeholder and they are right.
//!
//! This module composes the other kind. It is the pure half: building the
//! prompt, and reading the answer back. The model call, the queue and the
//! schedule all live outside it, which is what lets every rule below be tested
//! in microseconds against exact strings rather than against a model.
//!
//! # The model writes the question and NOTHING else
//!
//! [`GeneratedSuggestion::reason`] is composed by [`reason_for`] from the
//! source memory's own columns. The model never writes it and is never asked
//! to. That is the difference between this and a generated blurb: the sentence
//! under the question is a fact about the store, so the offer stays falsifiable
//! (DESIGN.md §3). A model allowed to write its own justification will write a
//! true-sounding one about a memory that does not exist.
//!
//! # The answer space is a list of numbers, not a vocabulary
//!
//! The model is handed numbered memories and answers with the NUMBER plus a
//! question. It never echoes a category, an id, or a date.
//!
//! That is not a style preference; it is the lesson PAI-7 paid for twice on the
//! Orin. The proactive reviewer's first run died on `unknown field "type"` and
//! its third on `missing field "trigger_kind"` — a 2B model omitting a required
//! field of a taxonomy it was asked to reproduce. The checklist's own
//! conclusion was to make the brief's numbered list be the answer space instead
//! of asking a small model to echo a string. This schema is that conclusion: two
//! fields, one of which is an integer the pond supplied.
//!
//! A number is also CHECKABLE in a way a string is not. `parse_response` can
//! prove `3` names a memory that was in the prompt; it could never prove that
//! about a model-written id.

use crate::user_data::domain::memory::{MemoryFragment, MemorySegment};
use chrono::{DateTime, Datelike, Utc};

/// How many memories one pass shows the model.
///
/// Small on purpose. These run on a 2B–4B model on a six-core board that the
/// chat path also wants, and the prompt is the expensive half — twelve memories
/// is roughly 400 tokens of user content, which leaves the whole answer inside
/// any output budget. It is also about QUALITY: asked to pick from twelve a
/// model picks; asked to pick from two hundred it summarises.
pub const MEMORIES_PER_PASS: usize = 12;

/// How many questions one pass may queue.
///
/// Fewer than it is shown, so choosing is part of the job. A model told to
/// write one per memory writes twelve mediocre questions; told to pick the best
/// three it picks.
pub const MAX_PER_PASS: usize = 3;

/// The shortest a question may be, in characters.
///
/// "Why?" parses, is a question, and is worth nothing. The floor is what stops
/// a model that has run out of ideas from filling the quota.
const MIN_QUESTION_CHARS: usize = 16;

/// The longest. A question that does not fit the card is a question the
/// household reads half of — the lead offer's own measure is about 30ch a line
/// over three lines at 800x480.
const MAX_QUESTION_CHARS: usize = 96;

/// A question the pond composed, with the memory it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSuggestion {
    /// The memory this was composed from. The read path re-checks it still
    /// exists before offering, so a deleted memory takes its question with it.
    pub source_memory_id: String,
    /// Whose memory it was. `None` means unattributed, so the suggestion
    /// belongs to the household rather than to a member.
    pub profile_id: Option<String>,
    /// The sentence the household reads AND the prompt sent when they tap it.
    pub prompt: String,
    /// The fact underneath, written by the pond. Never by the model.
    pub reason: String,
}

/// Why a candidate was refused.
///
/// Counted and logged rather than silently dropped: a pass that queues nothing
/// must be able to say whether the model wrote nothing or wrote three things
/// this module threw away. The two are very different problems and they look
/// identical from a row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The number did not name a memory in the prompt.
    UnknownMemory,
    /// Below [`MIN_QUESTION_CHARS`] or above [`MAX_QUESTION_CHARS`].
    Length,
    /// Not a question. The card's whole shape is "something you could ask".
    NotAQuestion,
    /// The question is the memory with a question mark on it.
    EchoesTheMemory,
    /// Another accepted candidate already names this memory.
    DuplicateMemory,
    /// Written in the pond's voice, addressed to the household.
    ///
    /// The card's contract is that the question shown IS the message sent when
    /// it is tapped. So "Jerry, what other movies do you want to see?" is not
    /// a suggestion — tapped, it sends Jerry a question addressed to Jerry,
    /// about Jerry's own preferences, to a pond that cannot answer it. Eleven
    /// of eleven queued cards on a real pond had this shape.
    WrongVoice,
}

impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Refusal::UnknownMemory => "names no memory from the prompt",
            Refusal::Length => "wrong length for the card",
            Refusal::NotAQuestion => "is not a question",
            Refusal::EchoesTheMemory => "restates the memory",
            Refusal::DuplicateMemory => "a second question about one memory",
            Refusal::WrongVoice => "addressed to the household, not to the pond",
        }
    }
}

/// What one pass produced.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GenerationOutcome {
    pub accepted: Vec<GeneratedSuggestion>,
    /// One entry per refused candidate, in the order the model wrote them.
    pub refused: Vec<Refusal>,
    /// True when the response could not be read as the schema at all.
    ///
    /// Distinct from "the model proposed nothing", exactly as
    /// `ExtractionError::Unparseable` is: one is a model that cannot write the
    /// shape, the other is a model with nothing to say, and a pass that
    /// conflates them cannot tell a broken prompt from a quiet week.
    pub unparseable: bool,
}

/// The system prompt. Fixed, so it stays in the KV prefix across passes.
///
/// # Who is speaking, and who is being asked
///
/// The card's contract is that the question shown IS the message sent when it
/// is tapped. The first version of this prompt never said so, and the model
/// filled the gap the way a model does: all ELEVEN cards queued on a real pond
/// came out addressed to the household — "Jerry, what other movies do you want
/// to see?" Tapped, that sends Jerry a question about Jerry's own preferences,
/// to a pond that cannot possibly answer it.
///
/// Two lines of the old prompt caused it and both are gone:
///
///   * "Your job is the question the note leaves open" named a job with no
///     author and no reader. A model filling in a missing author defaults to
///     itself, and the only other party named is the person — so it asked them.
///   * "Take only names from a note: the person, the place, the thing" actively
///     licensed pulling "Jerry" forward, and a carried-forward name lands in
///     vocative position. The person is no longer on that list.
///
/// So sentence one now names BOTH ends of the message before anything else, and
/// the answerability test follows immediately: it is sent to the pond, so the
/// pond must be able to answer it. That second half is what rules out the
/// residual the guard cannot see — "What other movies do I want to see?" is in
/// the household's voice and still useless, because only they know.
///
/// NO WORKED EXAMPLE, for the reason the extractor carries `EchoedExample`:
/// models return the prompt's own example as a finding. The copyable literals
/// here fail closed — `<question>` dies on `Refusal::Length`.
pub const SYSTEM: &str = "\
Each note below is something this household has told the pond, or something it \
learned about how they live. You are writing the message they would send the \
pond next, in their own words.

It is sent TO the pond, so write only what the pond could answer for them: \
something to look up, check, work out or remember. Two kinds are wrong - one \
only they could answer, and one the note already answers.

Never open by addressing anyone. The person in a note is \"I\" and their things \
are \"mine\"; carry the place and the thing forward, never the name.

You are given numbered notes. Reply with JSON and nothing else:

{\"suggestions\": [{\"memory\": <number>, \"question\": \"<question>\"}]}

- At most 3. Pick the notes the pond could be most useful about; ignore the rest.
- \"memory\": one of the numbers you were given.
- \"question\": what they would type, at least five words, under 90 characters, \
ending in \"?\".
- No explanation, no preamble.";

/// Which notes are worth spending a pass on, best first.
///
/// The pass sees twelve notes and the store holds hundreds, so WHICH twelve is
/// most of the quality. Recency alone was the first rule and it is not enough:
/// on a real pond it put "Alan Michael Ritchson was born on November 28, 1982,
/// in Grand Forks, North Dakota" in front of the model, which faithfully
/// produced "What interesting facts do you know about Grand Forks?" — a quiz
/// question, not a thing the pond could usefully do for this household.
///
/// The ordering is by what the note is ABOUT:
///
///   * A **routine** is a habit. It is the whole reason this feature reads
///     memories at all — "swims Saturday mornings" is what lets the pond be
///     useful about a Saturday.
///   * **Preferences, projects and relationships** are the household's own
///     shape, and questions built on them are about their life.
///   * **Knowledge is last**, and that is the load-bearing end. It is usually
///     something the POND said in a conversation and the extractor kept: a
///     birth date, a capital city. A question built on one is trivia the
///     household could have asked anybody.
///
/// A rank, not a filter. On a pond whose notes are all one kind, ordering them
/// changes nothing and refusing them would leave the column empty — and the
/// template tier, which is what answers when this yields nothing, is the same
/// generic questions the composed tier exists to replace.
pub fn worth_asking_about(memory: &MemoryFragment) -> u8 {
    match memory.segment {
        Some(MemorySegment::Routine) => 0,
        Some(MemorySegment::Preference) => 1,
        Some(MemorySegment::Project) => 2,
        Some(MemorySegment::Relationship) => 3,
        Some(MemorySegment::Correction) => 4,
        Some(MemorySegment::Identity) => 5,
        Some(MemorySegment::Context) | None => 6,
        Some(MemorySegment::Knowledge) => 7,
    }
}

/// Order a pass's candidates: what they are about first, recency within that.
///
/// A STABLE sort, so the caller's own order — `search_recent` hands them over
/// newest first — survives inside each rank. Newest-first still decides between
/// two habits; it just no longer decides between a habit and a capital city.
pub fn order_candidates(memories: &mut [MemoryFragment]) {
    memories.sort_by_key(worth_asking_about);
}

/// Number the memories for the model.
///
/// The index is 1-based and positional: it means "the nth line of this prompt",
/// never a database id. `parse_response` maps it back against the same slice,
/// so a model that answers `4` cannot reach a memory the pond did not show it.
pub fn build_user_prompt(memories: &[MemoryFragment]) -> String {
    let mut out = String::from("Notes:\n");
    for (i, m) in memories.iter().enumerate() {
        // The content only. Not the id, not the segment, not the score: every
        // extra field is a token spent and a thing a small model may echo back
        // instead of the number it was asked for.
        out.push_str(&format!("{}. {}\n", i + 1, m.content.trim()));
    }
    out.push_str("\nJSON:");
    out
}

/// The sentence under the question, from the memory's own columns.
///
/// Deliberately does NOT quote the note. The question already carries whatever
/// the note was about, so quoting it puts the same private sentence on a wall
/// panel twice — and this card is read from across a room. What is left is
/// still falsifiable against the row: when it was saved, and what kind of thing
/// the pond filed it as.
pub fn reason_for(memory: &MemoryFragment, now: DateTime<Utc>) -> String {
    let kind = match memory.segment {
        Some(MemorySegment::Routine) => "something you do regularly",
        Some(MemorySegment::Preference) => "how you like things",
        Some(MemorySegment::Relationship) => "someone you mentioned",
        Some(MemorySegment::Project) => "something you were working on",
        Some(MemorySegment::Identity) => "something about you",
        Some(MemorySegment::Correction) => "a correction you made",
        Some(MemorySegment::Knowledge) => "something you told me",
        Some(MemorySegment::Context) | None => "something you told me",
    };
    format!(
        "From {}, saved {}.",
        kind,
        when_said(memory.created_at, now)
    )
}

/// A date a person would say out loud.
///
/// Relative inside a fortnight because "saved 3 days ago" is what makes the
/// offer feel current; absolute beyond it because "saved 97 days ago" is
/// arithmetic nobody does. Never a clock time: the card is glanced at, and the
/// hour a note was saved has never been the interesting part.
fn when_said(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let days = (now - at).num_days();
    match days {
        d if d <= 0 => "today".to_string(),
        1 => "yesterday".to_string(),
        2..=13 => format!("{days} days ago"),
        _ => format!("on {} {}", at.day(), month_name(at.month())),
    }
}

fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        _ => "December",
    }
}

/// Read the model's answer back against the memories it was shown.
///
/// `memories` must be the same slice `build_user_prompt` was given, in the same
/// order. That is what makes the number checkable.
///
/// `subjects` are the names the household goes by — `settings.user_name` and
/// every profile's display name. They are here for one reason: a question that
/// opens by addressing somebody by name is the assistant talking TO the
/// household, and this card's contract is that the question shown IS the
/// message sent. See [`Refusal::WrongVoice`].
pub fn parse_response(
    raw: &str,
    memories: &[MemoryFragment],
    subjects: &[String],
    now: DateTime<Utc>,
) -> GenerationOutcome {
    let Some(items) = extract_items(raw) else {
        return GenerationOutcome {
            unparseable: true,
            ..Default::default()
        };
    };

    let mut outcome = GenerationOutcome::default();
    for item in items {
        if outcome.accepted.len() >= MAX_PER_PASS {
            break;
        }
        let number = item.get("memory").and_then(serde_json::Value::as_u64);
        let question = item
            .get("question")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim();

        // The number first: a candidate naming no memory has no reason to be
        // written and is refused before its text is even looked at.
        let Some(memory) = number
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n >= 1)
            .and_then(|n| memories.get(n - 1))
        else {
            outcome.refused.push(Refusal::UnknownMemory);
            continue;
        };

        if let Some(refusal) = judge(question, &memory.content, subjects) {
            outcome.refused.push(refusal);
            continue;
        }

        if outcome
            .accepted
            .iter()
            .any(|a| a.source_memory_id == memory.id)
        {
            outcome.refused.push(Refusal::DuplicateMemory);
            continue;
        }

        outcome.accepted.push(GeneratedSuggestion {
            source_memory_id: memory.id.clone(),
            profile_id: memory.profile_id.clone(),
            prompt: question.to_string(),
            reason: reason_for(memory, now),
        });
    }
    outcome
}

/// Everything wrong with one question, in the order worth reporting.
fn judge(question: &str, memory: &str, subjects: &[String]) -> Option<Refusal> {
    let chars = question.chars().count();
    if !(MIN_QUESTION_CHARS..=MAX_QUESTION_CHARS).contains(&chars) {
        return Some(Refusal::Length);
    }
    if !question.ends_with('?') {
        return Some(Refusal::NotAQuestion);
    }
    if addresses_the_household(question, subjects) {
        return Some(Refusal::WrongVoice);
    }
    if echoes(question, memory) {
        return Some(Refusal::EchoesTheMemory);
    }
    None
}

/// Does this question open by addressing somebody who lives here?
///
/// A LEADING VOCATIVE and nothing else, deliberately narrow. All eleven real
/// failures had exactly this shape — "Jerry, what other movies do you want to
/// see?" — and it is the one signal that cannot be anything else: a person
/// typing a message to their pond does not begin it with their own name.
///
/// # What this refuses to guess at
///
/// The tempting wider rules all have victims, and they were found by trying
/// them rather than by reasoning:
///
///   * **Refuse "you"** — but "you" is the POND in every legitimate question of
///     this kind. "What do you remember about me?" and "What can you see in this
///     house?" are the two best cards the template tier has.
///   * **Require "my" / "I" / "me"** — but "Did the Cinema routine run last
///     night?" is a correct card with no first-person token in it, and so is
///     "What is the weather doing today?".
///
/// The residual this does NOT catch is the pronoun swap: "What other movies do
/// I want to see?" is in the household's voice, passes every rung here, and is
/// still useless because only they could answer it. Nothing in a string can see
/// that — it is a question about whether the POND can answer, which is why the
/// instruction carries it and this function does not pretend to.
fn addresses_the_household(question: &str, subjects: &[String]) -> bool {
    // Everything before the first vocative separator, and only that. A name
    // LATER in the sentence is the household talking ABOUT somebody -- "When
    // does Manu arrive?" -- which is exactly what this feature exists to
    // produce, so the head is where the test has to stop.
    //
    // A spaced dash counts and a bare hyphen does not: "Jerry - did it run?" is
    // the same defect wearing different punctuation, while "Jerry-Ann" is a
    // name. The spaces are what tell them apart.
    let Some(end) = question.char_indices().find_map(|(i, c)| match c {
        ',' | ':' | ';' => Some(i),
        '-' | '\u{2013}' | '\u{2014}' => {
            let spaced_before = question[..i].ends_with(' ');
            let spaced_after = question[i + c.len_utf8()..].starts_with(' ');
            (spaced_before && spaced_after).then_some(i)
        }
        _ => None,
    }) else {
        return false; // no separator, so no vocative
    };
    let head = question[..end].trim().to_lowercase();
    subjects.iter().any(|name| {
        let name = name.trim().to_lowercase();
        // A blank or one-letter name matches half the language; `user_name` is
        // empty on a pond nobody has named, and this must not then refuse every
        // question with a comma in it.
        name.chars().count() >= 2 && head == name
    })
}

/// Crudely strip an English inflection.
///
/// Measured, not theoretical. The first end-to-end run against gemma-4-E2B
/// produced "Does brother Manu live in Kisumu and visit at Christmas?" from the
/// note "brother Manu lives in Kisumu and visits at Christmas" — the note with
/// a question mark on it, and it PASSED, because `lives`/`live` and
/// `visits`/`visit` counted as different words and dropped the overlap to four
/// of six. "Should I keep the oven at 180 degrees for sourdough?" got through
/// the same way on `keeps`/`keep`.
///
/// Turning a statement into a question is exactly what changes a verb's
/// inflection, so comparing inflected forms is comparing the one thing this
/// guard should be blind to. No stemmer library for four suffixes: the cost of
/// a dependency here is a dependency in `pond-core`, which imports no
/// framework by design.
fn stem(word: &str) -> String {
    for suffix in ["ing", "es", "ed", "s"] {
        if let Some(root) = word.strip_suffix(suffix) {
            // Not below four characters: "goes" -> "go" and "does" -> "do"
            // collapse toward words that share a stem with half the language.
            if root.len() >= 4 {
                return root.to_string();
            }
        }
    }
    word.to_string()
}

/// Is this the note with a question mark on it?
///
/// Word overlap rather than a substring test, because the failure is not
/// literal: a model that has nothing to add reorders the note's words rather
/// than copying the string. Judged on the NOTE's words — how many of them the
/// question reuses — so a long question built around the note still fails and a
/// short question that happens to share a common word does not.
fn echoes(question: &str, memory: &str) -> bool {
    let words = |s: &str| -> Vec<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 3)
            .map(stem)
            .collect()
    };
    let note = words(memory);
    if note.is_empty() {
        return false;
    }
    let asked = words(question);
    let shared = note.iter().filter(|w| asked.contains(w)).count();
    // Four fifths of the note's own words, reused. Below that a question is
    // building on the note, which is what it is for.
    shared * 5 >= note.len() * 4
}

/// Find the `suggestions` array, however the model wrapped it.
///
/// Three shapes are accepted and the reason is measured rather than defensive:
/// 61 of 72 gemma-4-E2B replies in the extraction bake-off arrived inside
/// ```json fences, so fence salvage is load-bearing on the model this pond
/// actually runs. A bare array is accepted because a model told to answer with
/// a list often answers with a list.
fn extract_items(raw: &str) -> Option<Vec<serde_json::Value>> {
    let candidates = [
        raw.trim(),
        strip_fence(raw),
        slice_braces(raw),
        slice_array(raw),
    ];
    for candidate in candidates.iter().filter(|c| !c.is_empty()) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) else {
            continue;
        };
        if let Some(items) = value.get("suggestions").and_then(|s| s.as_array()) {
            return Some(items.clone());
        }
        if let Some(items) = value.as_array() {
            return Some(items.clone());
        }
    }
    None
}

fn strip_fence(raw: &str) -> &str {
    let Some(start) = raw.find("```") else {
        return "";
    };
    let after = &raw[start + 3..];
    let after = after.strip_prefix("json").unwrap_or(after);
    match after.find("```") {
        Some(end) => after[..end].trim(),
        None => after.trim(),
    }
}

fn slice_braces(raw: &str) -> &str {
    match (raw.find('{'), raw.rfind('}')) {
        (Some(a), Some(b)) if b > a => &raw[a..=b],
        _ => "",
    }
}

fn slice_array(raw: &str) -> &str {
    match (raw.find('['), raw.rfind(']')) {
        (Some(a), Some(b)) if b > a => &raw[a..=b],
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 17, 12, 0, 0).unwrap()
    }

    fn memory(id: &str, content: &str) -> MemoryFragment {
        MemoryFragment {
            id: id.to_string(),
            profile_id: None,
            session_id: None,
            content: content.to_string(),
            embedding: None,
            source: "extraction".to_string(),
            tags: vec![],
            created_at: now() - chrono::Duration::days(3),
            segment: None,
            importance: None,
            tier: None,
            decay_rate: None,
            access_count: 0,
            last_accessed_at: None,
            lifecycle: None,
            superseded_by: None,
            corrects: None,
        }
    }

    fn segmented(id: &str, content: &str, segment: MemorySegment) -> MemoryFragment {
        let mut m = memory(id, content);
        m.segment = Some(segment);
        m
    }

    fn three() -> Vec<MemoryFragment> {
        vec![
            memory("m1", "swims each Saturday morning at the club"),
            memory("m2", "keeps the oven at 180 for bread"),
            memory("m3", "brother Manu lives in Kisumu"),
        ]
    }

    // ── The prompt ───────────────────────────────────────────────────────

    /// The number is positional and means "the nth line of this prompt". If it
    /// ever became a database id, a model could name a memory it was not shown.
    #[test]
    fn the_prompt_numbers_from_one_and_carries_only_the_note() {
        let prompt = build_user_prompt(&three());
        assert!(prompt.contains("1. swims each Saturday morning at the club"));
        assert!(prompt.contains("3. brother Manu lives in Kisumu"));
        // No ids, no segments, no scores: every extra field is a token spent
        // and a thing a small model may echo instead of the number.
        assert!(!prompt.contains("m1"), "the prompt must not carry ids");
        assert!(!prompt.contains("extraction"), "nor the source");
    }

    // ── Which notes a pass spends itself on ──────────────────────────────

    /// The measured failure this ordering exists for. On a real pond the twelve
    /// most RECENT notes put a stored birth date in front of the model, which
    /// faithfully produced "What interesting facts do you know about Grand
    /// Forks?" — trivia the household could have asked anybody.
    #[test]
    fn a_habit_is_asked_about_before_a_stored_fact() {
        let mut notes = vec![
            segmented(
                "k1",
                "Alan Ritchson was born in Grand Forks",
                MemorySegment::Knowledge,
            ),
            segmented(
                "r1",
                "swims at the club on Saturday mornings",
                MemorySegment::Routine,
            ),
            segmented(
                "p1",
                "drinks coffee only before noon",
                MemorySegment::Preference,
            ),
        ];
        order_candidates(&mut notes);
        assert_eq!(
            notes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["r1", "p1", "k1"],
            "a habit first, a stored fact last"
        );
    }

    /// Recency still decides between two notes of the same kind — the sort is
    /// stable, so the caller's newest-first order survives inside each rank.
    #[test]
    fn recency_still_decides_between_two_habits() {
        let mut notes = vec![
            segmented(
                "newer",
                "swims on Saturday mornings",
                MemorySegment::Routine,
            ),
            segmented("older", "bakes bread on Sundays", MemorySegment::Routine),
        ];
        order_candidates(&mut notes);
        assert_eq!(
            notes.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["newer", "older"],
            "the caller's order survives inside a rank"
        );
    }

    /// A rank, not a filter. A pond whose notes are ALL stored facts still gets
    /// a pass — refusing them would leave the column on the template tier,
    /// which is the generic questions this whole feature exists to replace.
    #[test]
    fn a_pond_of_nothing_but_stored_facts_still_gets_a_pass() {
        let mut notes = vec![
            segmented(
                "k1",
                "Nairobi is the capital of Kenya",
                MemorySegment::Knowledge,
            ),
            segmented(
                "k2",
                "the Orin Nano has six cores",
                MemorySegment::Knowledge,
            ),
        ];
        order_candidates(&mut notes);
        assert_eq!(notes.len(), 2, "nothing is dropped, only ordered");
    }

    // ── Reading the answer ───────────────────────────────────────────────

    #[test]
    fn a_clean_answer_becomes_suggestions_the_pond_can_stand_behind() {
        let mems = three();
        let raw = r#"{"suggestions":[
            {"memory":1,"question":"Am I still making it to the club on Saturdays?"},
            {"memory":3,"question":"How long since I called Manu?"}
        ]}"#;
        let out = parse_response(raw, &mems, &[], now());

        assert!(!out.unparseable);
        assert!(out.refused.is_empty());
        assert_eq!(out.accepted.len(), 2);
        assert_eq!(out.accepted[0].source_memory_id, "m1");
        assert_eq!(out.accepted[1].source_memory_id, "m3");
        // The reason is the pond's, not the model's.
        assert_eq!(
            out.accepted[0].reason,
            "From something you told me, saved 3 days ago."
        );
    }

    /// The whole point of a numbered answer space: a number the pond did not
    /// supply cannot reach a memory. A model-written id could not be checked
    /// this way at all.
    #[test]
    fn a_number_the_pond_never_showed_reaches_no_memory() {
        let mems = three();
        for bogus in ["0", "4", "99"] {
            let raw = format!(
                r#"{{"suggestions":[{{"memory":{bogus},"question":"Is this reachable at all?"}}]}}"#
            );
            let out = parse_response(&raw, &mems, &[], now());
            assert!(out.accepted.is_empty(), "{bogus} must reach nothing");
            assert_eq!(out.refused, vec![Refusal::UnknownMemory]);
        }

        // Vacuity control: the same question under a number that WAS shown is
        // accepted, so the refusals above are the range check and not the text.
        let ok = parse_response(
            r#"{"suggestions":[{"memory":2,"question":"Is this reachable at all?"}]}"#,
            &mems,
            &[],
            now(),
        );
        assert_eq!(ok.accepted.len(), 1);
    }

    /// A question that is the note reordered adds nothing, and it is what a
    /// model with nothing to say produces. Judged on word overlap rather than
    /// substring, because the failure is not literal.
    #[test]
    fn a_question_that_is_just_the_note_back_is_refused() {
        let mems = three();
        let out = parse_response(
            r#"{"suggestions":[{"memory":1,"question":"Do I swim each Saturday morning at the club?"}]}"#,
            &mems,
            &[],
            now(),
        );
        assert_eq!(out.refused, vec![Refusal::EchoesTheMemory]);

        // And the control: a question BUILT on the note, sharing some of its
        // words, is exactly what this is for and must survive.
        let good = parse_response(
            r#"{"suggestions":[{"memory":1,"question":"What time should I leave for the club?"}]}"#,
            &mems,
            &[],
            now(),
        );
        assert_eq!(good.accepted.len(), 1, "building on a note is the job");
    }

    /// The eleven queued on a real pond, all of one shape: the pond addressing
    /// the household. Tapped, each sends the household member a question about
    /// their own preferences, to a pond that cannot answer it.
    #[test]
    fn a_question_addressed_to_the_household_is_not_a_suggestion() {
        let subjects = vec!["Jerry".to_string()];
        let mems = vec![memory(
            "m1",
            "has a playlist named Ye and listens to Playboi Carti",
        )];

        for wrong in [
            "Jerry, what other movies do you want to see?",
            "Jerry, did the Cinema routine run successfully?",
            "jerry, what kind of music are you listening to?",
            // The same defect wearing different punctuation.
            "Jerry - did the Cinema routine run last night?",
            "Jerry: what should we watch tonight?",
        ] {
            let raw = format!(
                r#"{{"suggestions":[{{"memory":1,"question":{}}}]}}"#,
                serde_json::to_string(wrong).unwrap()
            );
            let out = parse_response(&raw, &mems, &subjects, now());
            assert_eq!(out.refused, vec![Refusal::WrongVoice], "for {wrong:?}");
        }
    }

    /// The vocative is the ONLY signal this rung reads, and the tempting wider
    /// rules all have victims. Each of these is a card worth showing.
    #[test]
    fn the_voice_rung_refuses_none_of_the_questions_worth_showing() {
        let subjects = vec!["Jerry".to_string(), "Manu".to_string()];
        let mems = vec![memory(
            "m1",
            "brother Manu lives in Kisumu and visits at Christmas",
        )];

        for right in [
            // "you" is the POND here, which is why refusing "you" is wrong.
            "What do you remember about me?",
            // A name LATER in the sentence is the household talking ABOUT
            // somebody -- the exact thing this feature exists to produce.
            "When does Manu arrive this year?",
            // No first-person token at all, and still a correct card, which is
            // why requiring "my"/"I"/"me" is wrong.
            "Did the Cinema routine run last night?",
            // A comma, but no vocative.
            "Before Christmas, what should I book?",
            // A name in the middle, set off by a comma -- the household talking
            // ABOUT somebody, which is the whole point of the feature.
            "Should I invite Manu, or keep it small?",
            // A hyphen with no spaces is part of a word, not a vocative.
            "Is the Jerry-Ann recipe the one with cardamom?",
        ] {
            let raw = format!(
                r#"{{"suggestions":[{{"memory":1,"question":{}}}]}}"#,
                serde_json::to_string(right).unwrap()
            );
            let out = parse_response(&raw, &mems, &subjects, now());
            assert!(
                !out.refused.contains(&Refusal::WrongVoice),
                "{right:?} must not be refused for voice, got {:?}",
                out.refused
            );
        }
    }

    /// `user_name` is empty on a pond nobody has named. A blank subject that
    /// matched would refuse every question containing a comma.
    #[test]
    fn a_pond_with_no_name_for_anybody_refuses_nothing_for_voice() {
        let mems = vec![memory("m1", "keeps the oven at 180 degrees for sourdough")];
        for subjects in [
            vec![],
            vec![String::new()],
            vec![" ".to_string()],
            vec!["J".to_string()],
        ] {
            let out = parse_response(
                r#"{"suggestions":[{"memory":1,"question":"Tomorrow, how long should I proof it?"}]}"#,
                &mems,
                &subjects,
                now(),
            );
            assert!(
                !out.refused.contains(&Refusal::WrongVoice),
                "empty or one-letter names must match nothing: {subjects:?}"
            );
        }
    }

    /// The two that a real gemma-4-E2B pass produced and the guard let through,
    /// verbatim. Both are the note with a question mark on it; both survived
    /// because turning a statement into a question changes a verb's inflection,
    /// which was the one thing the comparison was blind to.
    #[test]
    fn the_restatements_a_real_model_actually_produced_are_refused() {
        let cases = [
            (
                "brother Manu lives in Kisumu and visits at Christmas",
                "Does brother Manu live in Kisumu and visit at Christmas?",
            ),
            (
                "keeps the oven at 180 degrees for sourdough",
                "Should I keep the oven at 180 degrees for sourdough?",
            ),
        ];
        for (note, question) in cases {
            let mems = vec![memory("m1", note)];
            let raw = format!(
                r#"{{"suggestions":[{{"memory":1,"question":{}}}]}}"#,
                serde_json::to_string(question).unwrap()
            );
            let out = parse_response(&raw, &mems, &[], now());
            assert_eq!(
                out.refused,
                vec![Refusal::EchoesTheMemory],
                "{question:?} restates {note:?}"
            );
        }
    }

    /// The control for the stemmer: it must not become so blunt that a question
    /// BUILT on a note is refused for sharing its subject. That is the job.
    #[test]
    fn stemming_does_not_start_refusing_useful_questions() {
        let mems = vec![
            memory("m1", "swims at the Aga Khan pool on Saturday mornings"),
            memory(
                "m2",
                "is rebuilding the Jetson Orin kiosk for the kitchen shelf",
            ),
        ];
        for (n, question) in [
            (1, "What time should I leave on Saturday?"),
            (2, "How far did I get with the kiosk last week?"),
        ] {
            let raw = format!(
                r#"{{"suggestions":[{{"memory":{n},"question":{}}}]}}"#,
                serde_json::to_string(question).unwrap()
            );
            let out = parse_response(&raw, &mems, &[], now());
            assert_eq!(out.accepted.len(), 1, "{question:?} builds on its note");
        }
    }

    /// The stemmer refuses to cut below four characters, so short words that
    /// merely END in a suffix letter keep their meaning.
    #[test]
    fn stemming_leaves_short_words_alone() {
        assert_eq!(stem("does"), "does", "not 'do'");
        assert_eq!(stem("goes"), "goes", "not 'go'");
        assert_eq!(stem("keeps"), "keep");
        assert_eq!(stem("visits"), "visit");
        assert_eq!(stem("rebuilding"), "rebuild");
    }

    #[test]
    fn a_question_that_does_not_fit_the_card_is_refused() {
        let mems = three();
        let long = format!("Could you tell me {}?", "a".repeat(MAX_QUESTION_CHARS));
        for (q, want) in [
            ("Why?", Refusal::Length),
            (long.as_str(), Refusal::Length),
            ("Tell me about the club.", Refusal::NotAQuestion),
        ] {
            let raw = format!(
                r#"{{"suggestions":[{{"memory":2,"question":{}}}]}}"#,
                serde_json::to_string(q).unwrap()
            );
            let out = parse_response(&raw, &mems, &[], now());
            assert_eq!(out.refused, vec![want], "for {q:?}");
        }
    }

    /// Two questions about one note is one note's worth of value taking two of
    /// three slots on a screen that shows one at 800x480.
    #[test]
    fn one_memory_yields_at_most_one_question() {
        let mems = three();
        let raw = r#"{"suggestions":[
            {"memory":2,"question":"What temperature do I use for bread?"},
            {"memory":2,"question":"Should I preheat for longer than usual?"}
        ]}"#;
        let out = parse_response(raw, &mems, &[], now());
        assert_eq!(out.accepted.len(), 1);
        assert_eq!(out.refused, vec![Refusal::DuplicateMemory]);
    }

    #[test]
    fn a_pass_queues_no_more_than_it_is_allowed() {
        let mems = three();
        let raw = r#"{"suggestions":[
            {"memory":1,"question":"What time should I leave for the club?"},
            {"memory":2,"question":"What temperature do I use for bread?"},
            {"memory":3,"question":"How long since I called him?"},
            {"memory":1,"question":"Should I book a lane in advance?"}
        ]}"#;
        assert_eq!(
            parse_response(raw, &mems, &[], now()).accepted.len(),
            MAX_PER_PASS
        );
    }

    /// 61 of 72 gemma-4-E2B replies in the extraction bake-off arrived inside
    /// ```json fences, so this is the shape the model this pond runs actually
    /// produces — not a defensive nicety.
    #[test]
    fn the_shapes_a_small_model_really_answers_with_all_parse() {
        let mems = three();
        let q = r#"{"memory":1,"question":"What time should I leave for the club?"}"#;
        for raw in [
            format!("{{\"suggestions\":[{q}]}}"),
            format!("```json\n{{\"suggestions\":[{q}]}}\n```"),
            format!("```\n{{\"suggestions\":[{q}]}}\n```"),
            format!("Here you go:\n{{\"suggestions\":[{q}]}}\nHope that helps."),
            format!("[{q}]"),
        ] {
            let out = parse_response(&raw, &mems, &[], now());
            assert!(!out.unparseable, "should parse: {raw}");
            assert_eq!(out.accepted.len(), 1, "for {raw}");
        }
    }

    /// The distinction `ExtractionError::Unparseable` was added for: a model
    /// that cannot write the shape and a model with nothing to say look
    /// identical from a row count, and they are very different problems.
    #[test]
    fn a_model_that_wrote_nothing_is_not_a_model_that_wrote_rubbish() {
        let mems = three();

        let empty = parse_response(r#"{"suggestions":[]}"#, &mems, &[], now());
        assert!(!empty.unparseable, "an empty list is an answer");
        assert!(empty.accepted.is_empty());

        let rubbish = parse_response("I'm sorry, I can't help with that.", &mems, &[], now());
        assert!(rubbish.unparseable, "prose is not an answer");
    }

    // ── The reason ───────────────────────────────────────────────────────

    /// The card is read from across a room and the question already carries
    /// whatever the note was about. Quoting it underneath puts the same private
    /// sentence on a wall panel twice.
    #[test]
    fn the_reason_never_quotes_the_note() {
        let mems = three();
        let out = parse_response(
            r#"{"suggestions":[{"memory":1,"question":"What time should I leave for the club?"}]}"#,
            &mems,
            &[],
            now(),
        );
        let reason = &out.accepted[0].reason;
        for word in ["swims", "Saturday", "club"] {
            assert!(!reason.contains(word), "{reason:?} leaks {word:?}");
        }
    }

    #[test]
    fn the_reason_says_when_the_way_a_person_would() {
        let mut m = memory("m1", "swims each Saturday");
        for (days, want) in [
            (0_i64, "saved today."),
            (1, "saved yesterday."),
            (5, "saved 5 days ago."),
            (40, "saved on 8 August."),
        ] {
            m.created_at = now() - chrono::Duration::days(days);
            assert!(
                reason_for(&m, now()).ends_with(want),
                "{days} days -> {:?}, wanted {want:?}",
                reason_for(&m, now())
            );
        }
    }

    /// A segment the pond actually filed changes the sentence, so the reason is
    /// falsifiable against the row rather than being one string for everything.
    #[test]
    fn the_reason_names_what_the_pond_filed_it_as() {
        let mut m = memory("m1", "swims each Saturday");
        m.segment = Some(MemorySegment::Routine);
        assert!(reason_for(&m, now()).starts_with("From something you do regularly,"));

        m.segment = Some(MemorySegment::Relationship);
        assert!(reason_for(&m, now()).starts_with("From someone you mentioned,"));

        m.segment = None;
        assert!(reason_for(&m, now()).starts_with("From something you told me,"));
    }

    /// Whose memory it was travels with the suggestion, because the read path
    /// is what keeps a personal note off a shared screen and this column is
    /// what it reads.
    #[test]
    fn the_suggestion_carries_whose_memory_it_came_from() {
        let mut mems = three();
        mems[0].profile_id = Some("jerry".to_string());
        let out = parse_response(
            r#"{"suggestions":[{"memory":1,"question":"What time should I leave for the club?"}]}"#,
            &mems,
            &[],
            now(),
        );
        assert_eq!(out.accepted[0].profile_id.as_deref(), Some("jerry"));
    }
}
