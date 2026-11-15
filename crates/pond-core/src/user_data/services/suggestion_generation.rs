//! Composing a question out of one of the household's own memories: the prompt and the parse.
//!
//! The model writes the question and the line under it, a specific "because" about the member;
//! the line falls back to one built from the memory's own columns ([`reason_for`]) when the model
//! writes none worth showing. The model answers with a memory NUMBER, never an echoed id or
//! category: small models mangle echoes, and a number is checkable.

use crate::user_data::domain::memory::{MemoryFragment, MemorySegment};
use crate::user_data::domain::suggestion::SuggestionContext;
use chrono::{DateTime, Datelike, Utc};

/// Memories shown per pass; small for cost (~400 prompt tokens on a shared 2B-4B board) and
/// quality (given twelve a model picks, given hundreds it summarises).
pub const MEMORIES_PER_PASS: usize = 12;

/// Questions one pass may queue; fewer than it is shown, so the model has to choose.
pub const MAX_PER_PASS: usize = 3;

/// Shortest question, in chars; stops a model out of ideas filling the quota with "Why?".
const MIN_QUESTION_CHARS: usize = 16;

/// Longest question: the card fits about three 30-char lines at 800x480.
const MAX_QUESTION_CHARS: usize = 96;

/// Shortest and longest "because" worth showing; outside them the pond's own line is shown.
const MIN_BECAUSE_CHARS: usize = 12;
const MAX_BECAUSE_CHARS: usize = 110;

/// A question the pond composed, with the memory it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedSuggestion {
    /// Source memory; the read path re-checks it exists, so deleting it drops the question.
    pub source_memory_id: String,
    /// Whose memory it was; `None` = unattributed, so it belongs to the household.
    pub profile_id: Option<String>,
    /// The sentence the household reads AND the prompt sent when they tap it.
    pub prompt: String,
    /// The line under the question: the model's specific "because", else [`reason_for`]'s.
    pub reason: String,
    /// The memory's own text, which grounds the chat model's answer when the card is tapped.
    pub fact: String,
}

impl GeneratedSuggestion {
    /// Grounding context that originated this composed suggestion.
    pub fn context(&self) -> SuggestionContext {
        SuggestionContext::PersonalMemory {
            source_memory_id: self.source_memory_id.clone(),
            fact: self.fact.clone(),
        }
    }
}

/// Why a candidate was refused; logged so an empty pass can tell silence from refusals.
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
    /// Written in the pond's voice to the household, yet a tap sends it to the pond as theirs.
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
    /// The response did not fit the schema at all; distinct from a model that proposed nothing.
    pub unparseable: bool,
}

/// The system prompt, fixed so it stays in the KV prefix. It names both ends of the message
/// first (the member is asking the pond, which must be able to answer) and has no worked
/// example, which models echo back; its copyable placeholders fail on `Refusal::Length`.
pub const SYSTEM: &str = "\
You suggest what one member of this household could ask the pond next: a \
question the pond can usefully look up, check or work out FOR them, which they \
tap to send as their own message.

Use these sources: what is known about them, under \"About\" (their portrait \
and confirmed interests), and the numbered notes. Build each question on one \
note, aimed at what they are into or working on: a price to check, news to \
look up, a plan to work out, options to compare. Prioritise what \"About\" \
says they are working on now, then what they follow closely.

Never:
- trivia about the note itself, or a question the note already answers;
- anything about the pond, the assistant or how it works;
- the life story or career of a famous person, or of anyone else;
- a question only they could answer.

The question is in their own words: \"I\", \"my\", never their name, never \
opening by addressing anyone. Keep your thinking concise and focused on high practical utility.

Reply with JSON and nothing else:

{\"suggestions\": [{\"memory\": <number>, \"question\": \"<question>\", \"because\": \"<line>\", \"score\": <0 to 1>}]}

- At most 3. Pick the notes the pond could be most useful about; ignore the rest.
- \"memory\": one of the numbers you were given.
- \"question\": what they would type, at least five words, under 90 characters, \
ending in \"?\".
- \"because\": one line to them, starting \"You\", saying the specific thing \
about them this builds on, under 90 characters.
- \"score\": 0 to 1, how useful the answer would be to them now. Sort by \
score, highest first.
- No explanation, no preamble.";

/// Which notes are worth a pass, lowest first: habits lead, knowledge (usually trivia the pond
/// itself said) trails. A rank, not a filter, so a pond of one kind still gets a pass.
pub fn worth_asking_about(memory: &MemoryFragment) -> u8 {
    match memory.segment {
        Some(MemorySegment::Routine) => 0,
        Some(MemorySegment::Preference | MemorySegment::FoodHealth) => 1,
        Some(MemorySegment::Project) => 2,
        Some(MemorySegment::Relationship | MemorySegment::Place | MemorySegment::Date) => 3,
        Some(MemorySegment::Correction | MemorySegment::Instruction) => 4,
        Some(MemorySegment::Identity) => 5,
        Some(MemorySegment::Context) | None => 6,
        Some(MemorySegment::Knowledge) => 7,
    }
}

/// Order by rank, keeping the caller's newest-first order within each (the sort is stable).
pub fn order_candidates(memories: &mut [MemoryFragment]) {
    memories.sort_by_key(worth_asking_about);
}

/// The top note's owner's notes, in order, up to [`MEMORIES_PER_PASS`] (unattributed is an
/// owner too). Privacy: one owner per prompt, so any number the model writes names that owner.
pub fn one_owners_candidates(ordered: Vec<MemoryFragment>) -> Vec<MemoryFragment> {
    let Some(owner) = ordered.first().map(|m| m.profile_id.clone()) else {
        return Vec::new();
    };
    ordered
        .into_iter()
        .filter(|m| m.profile_id == owner)
        .take(MEMORIES_PER_PASS)
        .collect()
}

/// Number the memories by position (1-based, never an id), so an answer can reach only these.
/// `about` is what the pond knows of the notes' owner: their portrait and confirmed interests.
pub fn build_user_prompt(memories: &[MemoryFragment], about: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(about) = about.map(str::trim).filter(|a| !a.is_empty()) {
        out.push_str(&format!("About:\n{about}\n\n"));
    }
    out.push_str("Notes:\n");
    for (i, m) in memories.iter().enumerate() {
        // Content only: any extra field costs tokens and may be echoed instead of the number.
        out.push_str(&format!("{}. {}\n", i + 1, m.content.trim()));
    }
    out.push_str("\nJSON:");
    out
}

/// The line under the question, from the memory's columns; never quotes the (private) note.
pub fn reason_for(memory: &MemoryFragment, now: DateTime<Utc>) -> String {
    let kind = match memory.segment {
        Some(MemorySegment::Routine) => "something you do regularly",
        Some(MemorySegment::Preference) => "how you like things",
        Some(MemorySegment::Relationship) => "someone you mentioned",
        Some(MemorySegment::Project) => "something you were working on",
        Some(MemorySegment::Identity) => "something about you",
        Some(MemorySegment::Correction) => "a correction you made",
        Some(MemorySegment::FoodHealth) => "what you eat and what you need",
        Some(MemorySegment::Place) => "a place you mentioned",
        Some(MemorySegment::Date) => "a date you keep",
        Some(MemorySegment::Instruction) => "something you asked me to always do",
        Some(MemorySegment::Knowledge) => "something you told me",
        Some(MemorySegment::Context) | None => "something you told me",
    };
    format!(
        "From {}, saved {}.",
        kind,
        when_said(memory.created_at, now)
    )
}

/// A date a person would say: relative within a fortnight, absolute beyond, never a clock time.
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

/// Read the model's answer back; `memories` must be the slice `build_user_prompt` got, in order.
/// `subjects`: `settings.user_name` and every profile name, used for [`Refusal::WrongVoice`].
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

    // Highest score first, whatever order the model wrote them in; unscored keep their place.
    let mut items = items;
    items.sort_by(|a, b| {
        let score = |v: &serde_json::Value| v.get("score").and_then(|s| s.as_f64()).unwrap_or(0.5);
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

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

        // The number first: a candidate naming no memory is refused before its text is read.
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

        let because = item
            .get("because")
            .and_then(serde_json::Value::as_str)
            .and_then(usable_because)
            .unwrap_or_else(|| reason_for(memory, now));
        outcome.accepted.push(GeneratedSuggestion {
            source_memory_id: memory.id.clone(),
            profile_id: memory.profile_id.clone(),
            prompt: question.to_string(),
            reason: because,
            fact: memory.content.trim().to_string(),
        });
    }
    outcome
}

/// The model's "because", when it is a line worth showing: one line, the right length, a
/// statement, and not the placeholder or the pond's own canned sentence.
fn usable_because(raw: &str) -> Option<String> {
    let line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_matches('"').trim();
    let chars = line.chars().count();
    let usable = (MIN_BECAUSE_CHARS..=MAX_BECAUSE_CHARS).contains(&chars)
        && !line.ends_with('?')
        && !line.contains("<line>")
        && !line.starts_with("From something");
    usable.then(|| line.to_string())
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

/// Does this question open by addressing somebody who lives here? Only a leading vocative, on
/// purpose: "you" is usually the pond, and good cards often lack "I"/"my"/"me".
fn addresses_the_household(question: &str, subjects: &[String]) -> bool {
    // Only the head before the first separator: a later name is talk ABOUT somebody. A spaced
    // dash separates ("Jerry - did it run?"); a bare hyphen is part of a name ("Jerry-Ann").
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
        // Skip blank or one-letter names: `user_name` is empty on an unnamed pond.
        name.chars().count() >= 2 && head == name
    })
}

/// Crudely strip an English inflection, since questioning a statement inflects its verbs
/// ("lives" -> "live"). Hand-rolled: a stemmer crate would be a `pond-core` dependency.
fn stem(word: &str) -> String {
    for suffix in ["ing", "es", "ed", "s"] {
        if let Some(root) = word.strip_suffix(suffix) {
            // Keep roots of four or more: "does" -> "do" would match half the language.
            if root.len() >= 4 {
                return root.to_string();
            }
        }
    }
    word.to_string()
}

/// Is this the note with a question mark on it? By word overlap (echoes reorder the words),
/// measured as the share of the NOTE's words the question reuses.
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
    // Four fifths reused; below that the question builds on the note, as intended.
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
    let stripped = crate::models::services::thought_filter::strip_reasoning(raw);
    let candidates = [
        stripped.trim(),
        strip_fence(&stripped),
        slice_braces(&stripped),
        slice_array(&stripped),
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
            about: None,
            valid_until: None,
        }
    }

    fn owned(id: &str, content: &str, owner: Option<&str>) -> MemoryFragment {
        let mut m = memory(id, content);
        m.profile_id = owner.map(str::to_string);
        m
    }

    /// A composing prompt holds one owner's notes, never a mix.
    #[test]
    fn one_composing_call_holds_one_owners_notes() {
        let ordered = vec![
            owned(
                "m1",
                "Liz sees Dr. Otieno at Aga Khan on Friday.",
                Some("liz"),
            ),
            owned(
                "m2",
                "The household does the grocery run at Carrefour.",
                None,
            ),
            owned(
                "m3",
                "Jerry waters the greenhouse before work.",
                Some("jerry"),
            ),
            owned(
                "m4",
                "Liz takes her tea without sugar these days.",
                Some("liz"),
            ),
        ];
        let ids: Vec<String> = one_owners_candidates(ordered)
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec!["m1", "m4"], "only the top note's owner, in order");
    }

    #[test]
    fn unattributed_notes_are_an_owner_of_their_own() {
        let ordered = vec![
            owned(
                "m2",
                "The household does the grocery run at Carrefour.",
                None,
            ),
            owned(
                "m1",
                "Liz sees Dr. Otieno at Aga Khan on Friday.",
                Some("liz"),
            ),
            owned("m5", "The bins go out on Thursday night.", None),
        ];
        let ids: Vec<String> = one_owners_candidates(ordered)
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids, vec!["m2", "m5"]);
        assert!(one_owners_candidates(Vec::new()).is_empty());
    }

    /// Deliberately misnumbered: a question about the first note, reported as the other.
    #[test]
    fn a_misnumbered_question_cannot_leave_its_owner() {
        let candidates = one_owners_candidates(vec![
            owned(
                "m1",
                "Liz sees Dr. Otieno at Aga Khan on Friday about her blood pressure.",
                Some("liz"),
            ),
            owned(
                "m2",
                "The household does the grocery run at Carrefour.",
                None,
            ),
            owned(
                "m4",
                "Liz takes her tea without sugar these days.",
                Some("liz"),
            ),
        ]);
        let reply = r#"[{"memory":2,"question":"Should I move the grocery run around the Aga Khan visit?"}]"#;
        let out = parse_response(reply, &candidates, &[], now());
        assert_eq!(
            out.accepted.len(),
            1,
            "the control: the question was accepted at all"
        );
        assert_eq!(
            out.accepted[0].profile_id.as_deref(),
            Some("liz"),
            "a question built from Liz's note was attributed to somebody else"
        );
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

    #[test]
    fn the_prompt_numbers_from_one_and_carries_only_the_note() {
        let prompt = build_user_prompt(&three(), None);
        assert!(prompt.starts_with("Notes:"), "no empty About block");
        assert!(prompt.contains("1. swims each Saturday morning at the club"));
        assert!(prompt.contains("3. brother Manu lives in Kisumu"));
        assert!(!prompt.contains("m1"), "the prompt must not carry ids");
        assert!(!prompt.contains("extraction"), "nor the source");
    }

    #[test]
    fn the_prompt_leads_with_what_is_known_about_the_member() {
        let prompt = build_user_prompt(
            &three(),
            Some("Runs a small software company.\nInto: Formula 1 (Ferrari)"),
        );
        assert!(prompt.starts_with("About:\nRuns a small software company."));
        assert!(prompt.find("About:").unwrap() < prompt.find("Notes:").unwrap());
    }

    #[test]
    fn the_rules_forbid_trivia_the_assistant_and_biographies() {
        assert!(SYSTEM.contains("trivia about the note itself"));
        assert!(SYSTEM.contains("anything about the pond, the assistant"));
        assert!(SYSTEM.contains("life story or career of a famous person"));
        assert!(SYSTEM.contains("\"because\""));
    }

    // ── Which notes a pass spends itself on ──────────────────────────────

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
        // No "because" from the model: the pond's own line is shown.
        assert_eq!(
            out.accepted[0].reason,
            "From something you told me, saved 3 days ago."
        );
    }

    #[test]
    fn the_highest_scored_suggestions_take_the_slots() {
        let mems = three();
        let raw = r#"{"suggestions":[
            {"memory":1,"question":"What time does the club pool open on Saturday?","score":0.2},
            {"memory":2,"question":"What temperature should I bake rye bread at?","score":0.9},
            {"memory":3,"question":"What is the fastest bus to Kisumu?","score":0.7},
            {"memory":1,"question":"Is the club pool heated in winter?","score":0.8}
        ]}"#;
        let out = parse_response(raw, &mems, &[], now());
        let ids: Vec<&str> = out
            .accepted
            .iter()
            .map(|a| a.source_memory_id.as_str())
            .collect();
        assert_eq!(ids, vec!["m2", "m1", "m3"]);
        assert_eq!(out.accepted[1].prompt, "Is the club pool heated in winter?");
    }

    #[test]
    fn the_models_because_is_shown_when_it_is_a_line_worth_showing() {
        let mems = three();
        let raw = r#"{"suggestions":[
            {"memory":1,"question":"What time does the club pool open on Saturday?","because":"You swim at the club every Saturday morning"},
            {"memory":2,"question":"What temperature should I bake rye bread at?","because":"<line>"},
            {"memory":3,"question":"What is the fastest bus to Kisumu?","because":"Is Manu in Kisumu?"}
        ]}"#;
        let out = parse_response(raw, &mems, &[], now());
        assert_eq!(out.accepted.len(), 3);
        assert_eq!(
            out.accepted[0].reason,
            "You swim at the club every Saturday morning"
        );
        assert!(
            out.accepted[1].reason.starts_with("From "),
            "the placeholder falls back to the pond's line"
        );
        assert!(
            out.accepted[2].reason.starts_with("From "),
            "a question is not a reason"
        );
    }

    /// Tapping the card grounds the answer in what the member said, not in the line under it.
    #[test]
    fn the_grounding_fact_is_the_memorys_own_text() {
        let mems = three();
        let out = parse_response(
            r#"{"suggestions":[{"memory":3,"question":"What is the fastest bus to Kisumu?","because":"You have a brother in Kisumu"}]}"#,
            &mems,
            &[],
            now(),
        );
        match out.accepted[0].context() {
            SuggestionContext::PersonalMemory { fact, .. } => {
                assert_eq!(fact, "brother Manu lives in Kisumu")
            }
            other => panic!("expected a memory context, got {other:?}"),
        }
    }

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

        // Control: under a shown number it is accepted, so the refusals above are the range check.
        let ok = parse_response(
            r#"{"suggestions":[{"memory":2,"question":"Is this reachable at all?"}]}"#,
            &mems,
            &[],
            now(),
        );
        assert_eq!(ok.accepted.len(), 1);
    }

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

        // Control: a question built on the note, sharing some words, must survive.
        let good = parse_response(
            r#"{"suggestions":[{"memory":1,"question":"What time should I leave for the club?"}]}"#,
            &mems,
            &[],
            now(),
        );
        assert_eq!(good.accepted.len(), 1, "building on a note is the job");
    }

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
            // A name later in the sentence is the household talking ABOUT somebody.
            "When does Manu arrive this year?",
            // No first-person token, still a correct card: why "my"/"I"/"me" is not required.
            "Did the Cinema routine run last night?",
            // A comma, but no vocative.
            "Before Christmas, what should I book?",
            // A mid-sentence name set off by a comma: still talk ABOUT somebody.
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

    /// Verbatim gemma-4-E2B output that differs from its note only in verb inflection.
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

    /// Two questions about one note would spend two of three slots on one note's worth of value.
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

    /// The pond's fallback line, which is all a card shows when the model wrote no "because".
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

    /// The read path keeps personal notes off shared screens by reading this column.
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
