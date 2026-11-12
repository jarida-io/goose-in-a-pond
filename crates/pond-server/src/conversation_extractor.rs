//! The live [`ConversationExtractor`]: read one window of a conversation with the local model.

use std::sync::Arc;

use async_trait::async_trait;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::domain::interest::{
    category_of, clean_facets, clean_topic, topic_key, ObservedInterest, Signal,
};
use pond_core::user_data::domain::member_profile::About;
use pond_core::user_data::ports::conversation_extractor::{
    estimated_tokens, ConversationExtractor, ExtractedMemory, ExtractedReminder, ExtractionError,
    ExtractionWindow, MemoryKind, WindowExtraction, EXTRACTION_PROMPT_BUDGET_TOKENS,
};
use tokio::sync::RwLock;

/// Per-window system prompt. Its opening must differ from every chat prompt's, or Goose's
/// `ReusePrefix` overwrites the chat's cached prefix. No worked example: small models copy it.
const EXTRACTION_PROMPT: &str = "\
You are reading one conversation between {subject} and {assistant}. Two
jobs: notice what {subject} cares about, and keep the rare fact about who
{subject} is that will still be true in six months. Keep your thinking
concise and focused on extracting exact facts from the conversation.

Reply with JSON and nothing else:
{skeleton}

interests: what this conversation shows {subject} caring about.
- topic: the specific thing by its own name, as {subject} would say it: a
  title, a team, a game, a product, a cause, a field. Never a general word.
- category: sport tech science business money politics culture film_tv
  music games books food travel health home cars shopping people places
  learning or other.
- facets: proper names and specific details from the conversation that
  narrow it down, as said there; never a general word. May be [].
- signal: \"asked\" if {subject} only asked about it, \"said\" if they said
  they like or follow it, \"does\" if they do it, own it or work on it.

memories: NEW facts about who {subject} is that outlast this conversation.
- One plain sentence naming {subject}, third person. Name every person and
  place; never open with \"He\", \"She\", \"It\", \"They\".
- A habit keeps its timing (\"every Saturday\"). NO one-off dates: no year,
  no \"on Tuesday\", no \"next week\". A date is a reminder. Only a yearly
  date (a birthday, an anniversary) may be a memory.
- about, exactly one of these words:
  name birthday work home language: who {subject} is
  partner child pet family friend: who that one is to {subject}
  diet allergy health: what {subject} eats, what their body needs
  routine: something {subject} does again and again
  likes dislikes: their tastes
  style: how they want to be answered (length, tone, format)
  goal place date: what they work towards, a place of theirs, a yearly date
  instruction: what {assistant} must always or never do for them
- Never a memory: a question {subject} asked, something they looked up or
  wanted explained (that is an interest, signal \"asked\"), anything
  {assistant} said, a guess. One question says nothing about who they are.
- Never copy a line of \"Already remembered\": it is known. Write one
  again only when this conversation says it more exactly.
{reminders}
Most conversations hold no memory, and \"memories\" is then []. At most
{n} memories; fewer is better.";

const SKELETON_WITH_REMINDERS: &str = "{\"interests\":[{\"topic\":\"...\",\"category\":\"...\",\
     \"facets\":[\"...\"],\"signal\":\"asked\"}],\"memories\":[{\"note\":\"...\",\
     \"about\":\"...\"}],\"reminders\":[{\"about\":\"...\",\"when\":\"...\"}]}";

/// The skeleton for a window too old to propose anything from.
const SKELETON_NO_REMINDERS: &str = "{\"interests\":[{\"topic\":\"...\",\"category\":\"...\",\
     \"facets\":[\"...\"],\"signal\":\"asked\"}],\"memories\":[{\"note\":\"...\",\
     \"about\":\"...\"}]}";

const REMINDERS_PARAGRAPH: &str = "\n\
reminders: anything that happens on one named day or at one named time. In\n\
\"when\", put {subject}'s own words about the timing, copied from the\n\
conversation -- do not work out the actual date, and do not invent one.\n";

/// Max rendered system-prompt length, in chars: the one fixed-size part of the prompt budget.
#[cfg(test)]
pub const EXTRACTION_PROMPT_CEILING: usize = 2_700;

/// Interests kept from one window; more is a list of everything mentioned, not what they care about.
const MAX_INTERESTS: usize = 5;

/// Max chars of an unparseable reply kept in `Unparseable::raw_head`.
const RAW_HEAD_CHARS: usize = 240;

pub struct LlmConversationExtractor {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
}

impl LlmConversationExtractor {
    pub fn new(live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>) -> Self {
        Self { live_provider }
    }
}

pub fn render_extraction_prompt(
    subject: &str,
    assistant: &str,
    max_memories: usize,
    allow_reminders: bool,
) -> String {
    let (skeleton, reminders) = if allow_reminders {
        (
            SKELETON_WITH_REMINDERS,
            REMINDERS_PARAGRAPH.replace("{subject}", subject),
        )
    } else {
        (SKELETON_NO_REMINDERS, String::new())
    };

    EXTRACTION_PROMPT
        .replace("{skeleton}", skeleton)
        .replace("{reminders}", &reminders)
        .replace("{subject}", subject)
        .replace("{assistant}", assistant)
        .replace("{n}", &max_memories.to_string())
}

/// Render the user message: known memories, then the conversation. To fit `budget_tokens`, drop
/// whole known rows from the end, never the conversation: a small model finishes half sentences.
pub fn render_window_message(window: &ExtractionWindow<'_>, budget_tokens: usize) -> String {
    let mut conversation = String::from("Conversation:\n");
    for message in window.messages {
        let speaker = if message.is_user() {
            window.subject.name.as_str()
        } else {
            window.assistant_name
        };
        conversation.push_str(&format!("{speaker}: {}\n", message.content.trim()));
    }

    let mut spent = estimated_tokens(&conversation);
    if spent > budget_tokens {
        // Unreachable via `carve_window`; if hit, its bound and this budget have drifted apart.
        tracing::warn!(
            tokens = spent,
            budget = budget_tokens,
            "[batch-extraction] one window's conversation alone overruns the prompt budget"
        );
    }

    let mut known_block = String::new();
    if !window.known.is_empty() {
        let header = format!("Already remembered about {}:\n", window.subject.name);
        // + 1 for the blank line closing the block.
        let header_cost = estimated_tokens(&header) + 1;
        let mut rows = String::new();
        let mut kept = 0usize;
        for known in window.known {
            let mark = if known.pattern { "*" } else { "" };
            let line = format!(
                "{}. [{}{}] {}\n",
                kept + 1,
                known.kind_label,
                mark,
                known.note
            );
            let cost = estimated_tokens(&line);
            if spent + header_cost + cost > budget_tokens {
                break;
            }
            spent += cost;
            rows.push_str(&line);
            kept += 1;
        }
        // An empty header invites a small model to fill it in.
        if kept > 0 {
            known_block.push_str(&header);
            known_block.push_str(&rows);
            known_block.push('\n');
        }
    }

    known_block + &conversation
}

#[async_trait]
impl ConversationExtractor for LlmConversationExtractor {
    async fn extract_window(
        &self,
        window: ExtractionWindow<'_>,
    ) -> Result<WindowExtraction, ExtractionError> {
        let provider = {
            let guard = self.live_provider.read().await;
            guard.as_ref().cloned().ok_or(ExtractionError::NoProvider)?
        };

        let system = render_extraction_prompt(
            &window.subject.name,
            window.assistant_name,
            window.max_memories,
            window.allow_reminders,
        );
        let user = render_window_message(
            &window,
            EXTRACTION_PROMPT_BUDGET_TOKENS.saturating_sub(estimated_tokens(&system)),
        );

        let response = provider
            .complete(&system, vec![ChatMessage::user(user)])
            .await
            .map_err(ExtractionError::Provider)?;

        parse_window_response(
            &response.content,
            window.max_memories,
            window.allow_reminders,
            &[window.subject.name.as_str(), window.assistant_name],
        )
    }
}

/// The same text with one level of quote-escaping removed, when it has any.
///
/// MEASURED, on gemma-4-E2B against a real eight-message conversation. The
/// model produced a COMPLETE, correct answer — ten good notes and two reminders,
/// properly nested, properly closed, inside a ```json fence — and wrote every
/// quote as `\"`:
///
/// ```text
/// {\"memories\": [{\"note\": \"Jerry drinks coffee only before noon.\", ...
/// ```
///
/// A backslash outside a string is not valid JSON, so every candidate above
/// failed and the window came back `parse_failures=1` with nothing written. Ten
/// notes the model got right were discarded over the escaping of a quote.
///
/// This is a model writing what it thinks a JSON string literal looks like —
/// the same confusion that puts a ```json fence around it — and on the model
/// this pond ships it is not rare.
///
/// `None` when there is nothing escaped, so the caller does not parse the same
/// bytes twice. Tried only AFTER every unmodified candidate has failed: a reply
/// that legitimately contains `\"` inside a note parses as itself, and
/// unescaping it first would break that note in half.
fn unescaped(slice: &str) -> Option<String> {
    slice.contains("\\\"").then(|| slice.replace("\\\"", "\""))
}

/// Recover the complete items from a reply the model ran out of room to finish.
///
/// MEASURED, on gemma-4-E2B against a real eight-message conversation. The
/// reply opened a ```json fence, wrote two entirely good notes, and stopped
/// mid-word inside the third:
///
/// ```text
/// {"memories":[
///   {"note":"... reachable before the 7:15 school run every morning.","kind":"context"},
///   {"note":"Jerry prefer
/// ```
///
/// Every candidate above needs VALID JSON, and a truncated reply closes
/// nothing — so `rfind('}')` lands on the end of the last complete item and the
/// slice is missing its `]` and its outer `}`. The whole window was discarded
/// as `parse_failures=1`, and two notes the model got right went with it. On a
/// pond whose model is small enough to truncate at all, that is most windows.
///
/// So: scan the items array, keep every object whose braces balance, discard
/// the partial one at the end. Nothing else is repaired — this does not close
/// quotes, guess at a missing field, or complete a word.
///
/// # It only fires on a reply that is actually truncated
///
/// One rule does that work: a bare array is only read when it OPENS the reply.
/// Otherwise this reaches into `{"facts":[...]}`, the old prompt's shape, and
/// hands its contents back as `memories` — which moves the watermark past every
/// window in the store, once, and never comes back.
///
/// # Why this returns `Ok` rather than `Unparseable`
///
/// It advances the cursor past a window whose tail was never mined, and that is
/// the deliberate trade. The alternative is re-reading the same window on every
/// pass, truncating at the same place (these calls run at temperature 0) and
/// writing nothing, forever. Keeping what the model finished and moving on is
/// progress; looping on it is not. An item that survives salvage still faces
/// every gate below — a partial object missing `note` is dropped there, not
/// admitted here.
fn salvage_truncated(cleaned: &str) -> Option<serde_json::Value> {
    // Both arrays: `reminders` follows `memories`, so a cut near the end loses reminders first.
    let memories = match array_after_key(cleaned, "memories") {
        Some(start) => complete_items(cleaned, start),
        // No "memories" key: a bare array counts only when it opens the reply (see above).
        None if !cleaned.contains("\"memories\"") => match cleaned.find('[') {
            Some(open) if !cleaned.find('{').is_some_and(|b| b < open) => {
                complete_items(cleaned, open + 1)
            }
            _ => Vec::new(),
        },
        // "memories" is not an array (`null`, say): the next `[` belongs to another key.
        None => Vec::new(),
    };
    let reminders = array_after_key(cleaned, "reminders")
        .map(|start| complete_items(cleaned, start))
        .unwrap_or_default();
    let interests = array_after_key(cleaned, "interests")
        .map(|start| complete_items(cleaned, start))
        .unwrap_or_default();

    if memories.is_empty() && reminders.is_empty() && interests.is_empty() {
        return None;
    }
    Some(serde_json::json!({
        "memories": memories,
        "reminders": reminders,
        "interests": interests,
    }))
}

/// Offset just inside the `[` of `key`'s array value. Only `:` and whitespace may come
/// between, or a `null` value would borrow the next key's array.
fn array_after_key(cleaned: &str, key: &str) -> Option<usize> {
    let quoted = format!("\"{key}\"");
    let at = cleaned.find(&quoted)? + quoted.len();
    let rest = &cleaned[at..];
    let after_colon = rest.trim_start().strip_prefix(':')?;
    let value = after_colon.trim_start();
    if !value.starts_with('[') {
        return None;
    }
    // Byte offset of the bracket in `cleaned`, plus one to step inside it.
    Some(cleaned.len() - value.len() + 1)
}

/// Every balanced object from `start` (just inside a `[`) until the array closes or text ends.
fn complete_items(cleaned: &str, start: usize) -> Vec<serde_json::Value> {
    let bytes = cleaned.as_bytes();
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut depth = 0usize;
    let mut item_start = None;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate().skip(start) {
        // Track string and escape state: braces inside a string are not structure.
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    item_start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    if let Some(from) = item_start.take() {
                        if let Ok(value) =
                            serde_json::from_str::<serde_json::Value>(&cleaned[from..=i])
                        {
                            items.push(value);
                        }
                    }
                }
            }
            // Array closed: keep the items even if a malformed tail (a trailing comma) follows.
            b']' if depth == 0 => break,
            _ => {}
        }
    }
    items
}

/// `None` for an object with neither key: an answer to another question, not an empty one.
fn usable_shape(value: serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Array(items) => Some(serde_json::json!({ "memories": items })),
        other
            if ["memories", "reminders", "interests"]
                .iter()
                .any(|key| other.get(key).is_some()) =>
        {
            Some(other)
        }
        _ => None,
    }
}

/// Parse a window reply, salvaging the measured failure shapes. Anything else is
/// [`ExtractionError::Unparseable`], never empty: empty moves the cursor past an unread window.
pub fn parse_window_response(
    raw: &str,
    max_memories: usize,
    allow_reminders: bool,
    subject_names: &[&str],
) -> Result<WindowExtraction, ExtractionError> {
    let cleaned = strip_thinking(raw.trim());

    let unparseable = || ExtractionError::Unparseable {
        raw_head: cleaned.chars().take(RAW_HEAD_CHARS).collect(),
    };

    let brace = cleaned
        .find('{')
        .zip(cleaned.rfind('}'))
        .filter(|(a, b)| a < b)
        .map(|(a, b)| &cleaned[a..=b]);

    // The old prompt's bare `[...]`, only if it opens the reply: every new-schema reply has a `[`.
    let bracket = match (cleaned.find('['), cleaned.find('{')) {
        (Some(open), brace_at) if brace_at.is_none_or(|b| open < b) => cleaned
            .rfind(']')
            .filter(|c| open < *c)
            .map(|c| &cleaned[open..=c]),
        _ => None,
    };

    let slices = [Some(cleaned.as_str()), brace, bracket];

    let value = slices
        .into_iter()
        .flatten()
        .filter_map(|slice| serde_json::from_str::<serde_json::Value>(slice).ok())
        .find_map(usable_shape)
        // Then unescaped, only after the plain pass: a note with a real `\"` must parse as itself.
        .or_else(|| {
            slices
                .into_iter()
                .flatten()
                .filter_map(unescaped)
                .filter_map(|slice| serde_json::from_str::<serde_json::Value>(&slice).ok())
                .find_map(usable_shape)
        })
        // Salvage last: it discards a trailing item. Both forms, for a reply over-escaped and cut.
        .or_else(|| salvage_truncated(cleaned.as_str()))
        .or_else(|| unescaped(cleaned.as_str()).and_then(|u| salvage_truncated(&u)))
        .ok_or_else(unparseable)?;

    let mut extraction = WindowExtraction::default();

    if let Some(items) = value.get("memories").and_then(|m| m.as_array()) {
        for item in items {
            if extraction.memories.len() >= max_memories {
                break;
            }
            let Some(note) = item
                .get("note")
                .or_else(|| item.get("content"))
                .and_then(|n| n.as_str())
                .map(str::trim)
                .filter(|n| !n.is_empty())
            else {
                continue;
            };
            // An unknown word is rejected and counted, never defaulted into a catch-all. A
            // reply in the older shape names a kind and no `about`.
            let about = item
                .get("about")
                .and_then(|a| a.as_str())
                .and_then(About::parse);
            let Some(kind) = about.map(MemoryKind::of).or_else(|| {
                item.get("kind")
                    .and_then(|k| k.as_str())
                    .and_then(MemoryKind::parse)
            }) else {
                extraction.rejected += 1;
                continue;
            };
            extraction.memories.push(ExtractedMemory {
                note: note.to_string(),
                kind,
                about,
            });
        }
    }

    if let Some(items) = value.get("interests").and_then(|i| i.as_array()) {
        extraction.interests = parse_interests(items, subject_names);
    }

    // A stale window was not asked for reminders; drop any the model volunteers.
    if allow_reminders {
        if let Some(items) = value.get("reminders").and_then(|r| r.as_array()) {
            for item in items {
                let about = item
                    .get("about")
                    .and_then(|a| a.as_str())
                    .map(str::trim)
                    .unwrap_or_default();
                let when = item
                    .get("when")
                    .and_then(|w| w.as_str())
                    .map(str::trim)
                    .unwrap_or_default();
                if about.is_empty() {
                    continue;
                }
                extraction.reminders.push(ExtractedReminder {
                    about: about.to_string(),
                    when_said: when.to_string(),
                });
            }
        }
    }

    Ok(extraction)
}

/// The interests a reply names, cleaned. A topic that is only the subject or the assistant is
/// dropped: neither is something the member follows.
fn parse_interests(items: &[serde_json::Value], subject_names: &[&str]) -> Vec<ObservedInterest> {
    let excluded: Vec<String> = subject_names.iter().map(|n| topic_key(n)).collect();
    let mut seen = std::collections::HashSet::new();
    items
        .iter()
        .filter_map(|item| {
            let topic = item
                .get("topic")
                .and_then(|t| t.as_str())
                .and_then(clean_topic)?;
            let key = topic_key(&topic);
            if topic == "..." || excluded.contains(&key) || !seen.insert(key.clone()) {
                return None;
            }
            // A facet that only repeats the topic narrows nothing.
            let facets = item
                .get("facets")
                .and_then(|f| f.as_array())
                .map(|f| {
                    clean_facets(
                        f.iter()
                            .filter_map(|v| v.as_str())
                            .filter(|v| *v != "..." && topic_key(v) != key),
                    )
                })
                .unwrap_or_default();
            Some(ObservedInterest {
                topic,
                category: category_of(
                    item.get("category")
                        .and_then(|c| c.as_str())
                        .unwrap_or("other"),
                ),
                facets,
                signal: Signal::parse(item.get("signal").and_then(|s| s.as_str()).unwrap_or("")),
            })
        })
        .take(MAX_INTERESTS)
        .collect()
}

/// Strip reasoning blocks, orphaned preambles, and standalone sentinels.
pub fn strip_thinking(text: &str) -> String {
    pond_core::models::services::thought_filter::strip_reasoning(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::ports::conversation_extractor::{
        KnownMemory, WindowMessage, WindowSubject,
    };

    fn rendered() -> String {
        render_extraction_prompt("Jerry", "Goose", 3, true)
    }

    // ── strip_thinking ───────────────────────────────────────────────────

    /// A regression here hangs rather than fails.
    #[test]
    fn a_close_before_an_open_terminates_and_keeps_the_answer() {
        assert_eq!(
            strip_thinking(r#"r1</think><think>r2</think>{"memories":[]}"#),
            r#"{"memories":[]}"#
        );
        assert_eq!(
            strip_thinking(r#"a<channel|><|channel>b<channel|>{"x":1}"#),
            r#"{"x":1}"#
        );
    }

    /// The template-inserted opening tag: reasoning, then a bare close.
    #[test]
    fn reasoning_that_lost_its_opening_tag_is_still_stripped() {
        assert_eq!(
            strip_thinking(r#"let me think</think>{"a":1}"#),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn well_formed_and_unclosed_blocks_strip_as_they_always_did() {
        assert_eq!(strip_thinking(r#"<think>hmm</think>{"a":1}"#), r#"{"a":1}"#);
        assert_eq!(
            strip_thinking(r#"{"a":1}<think>cut off mid-"#),
            r#"{"a":1}"#
        );
        assert_eq!(
            strip_thinking("<think>x</think>A<think>y</think>B"),
            "AB",
            "every block goes, not just the first"
        );
        assert_eq!(
            strip_thinking(r#"  {"a":1}  "#),
            r#"{"a":1}"#,
            "no tags: trimmed only"
        );
    }

    /// The fixture is a real gemma-4-E2B reply: complete, but with every quote written as `\"`.
    #[test]
    fn a_reply_whose_every_quote_is_escaped_still_yields_its_notes() {
        let over_escaped = concat!(
            "```json\n{\n  \\\"memories\\\": [\n",
            "    {\n      \\\"note\\\": \\\"Jerry drinks coffee only before noon.\\\",\n",
            "      \\\"kind\\\": \\\"preference\\\"\n    },\n",
            "    {\n      \\\"note\\\": \\\"Brother Manu lives in Kisumu and visits at Christmas.\\\",\n",
            "      \\\"kind\\\": \\\"relationship\\\"\n    }\n  ]\n}\n```"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(over_escaped).is_err(),
            "the fixture must be the malformed thing the model actually sent"
        );

        let out = parse_window_response(over_escaped, 5, false, &[])
            .expect("a correct answer must not be lost to the escaping of a quote");
        assert_eq!(out.memories.len(), 2);
        assert!(out.memories[0].note.contains("coffee only before noon"));
        assert!(out.memories[1].note.contains("Kisumu"));
    }

    #[test]
    fn a_note_containing_a_real_quotation_is_not_unescaped() {
        let raw = r#"{"memories":[{"note":"Jerry says \"no confirmations\" when asked twice.","kind":"preference"}]}"#;
        let out = parse_window_response(raw, 5, false, &[]).expect("valid JSON");
        assert_eq!(out.memories.len(), 1);
        assert!(
            out.memories[0].note.contains("\"no confirmations\""),
            "the quotation survives: {:?}",
            out.memories[0].note
        );
    }

    /// The fixture is a real gemma-4-E2B reply, verbatim, cut off mid-word.
    #[test]
    fn a_reply_the_model_ran_out_of_room_to_finish_keeps_what_it_finished() {
        let truncated = "```json\n{\n  \"memories\": [\n    {\n      \"note\": \"Jerry moves the espresso machine to the window shelf so it is reachable before the 7:15 school run every morning.\",\n      \"kind\": \"context\"\n    },\n    {\n      \"note\": \"Jerry buys beans from Kahawa on Ngong Road, a kilo at a time.\",\n      \"kind\": \"context\"\n    },\n    {\n      \"note\": \"Jerry prefer";

        let out = parse_window_response(truncated, 5, false, &[])
            .expect("two finished notes are worth more than nothing");
        assert_eq!(
            out.memories.len(),
            2,
            "both complete items, and not the partial one"
        );
        assert!(out.memories[0].note.contains("espresso machine"));
        assert!(out.memories[1].note.contains("Kahawa"));
        assert!(
            !out.memories
                .iter()
                .any(|m| m.note.starts_with("Jerry prefer")),
            "the half-written item is discarded, never completed or guessed at"
        );
    }

    #[test]
    fn salvage_refuses_anything_that_is_not_actually_truncated() {
        // Closed, so not truncated: discarding a trailing item cannot fix it.
        assert!(
            parse_window_response(
                r#"{"facts":[{"content":"Jerry prefers short answers.","segment":"preference"}]}"#,
                3,
                false,
                &[]
            )
            .is_err(),
            "the old prompt's shape must not be read as the new one"
        );

        // And the same wrong schema, truncated, is still not this schema.
        assert!(
            parse_window_response(
                r#"{"facts":[{"content":"Jerry prefers short answers.","segment":"preference"},{"content":"Jerry buys"#,
                3,
                false,
                &[]
            )
            .is_err(),
            "a bare array is only salvaged when it OPENS the reply"
        );
    }

    #[test]
    fn a_closed_array_with_a_malformed_tail_still_yields_its_notes() {
        let raw = "{\"memories\":[{\"note\":\"Jerry buys beans from Kahawa on Ngong Road.\",\"kind\":\"context\"}],}";
        let out = parse_window_response(raw, 5, false, &[]).expect("the notes inside are fine");
        assert_eq!(out.memories.len(), 1);
        assert!(out.memories[0].note.contains("Kahawa"));
    }

    #[test]
    fn a_reply_cut_off_inside_its_reminders_keeps_the_finished_one() {
        let raw = r#"{"memories":[{"note":"Jerry sees a dentist in Kisumu.","kind":"context"}],"reminders":[{"about":"the dentist","when":"next Tuesday"},{"about":"collect the tract"#;
        let out =
            parse_window_response(raw, 5, true, &[]).expect("finished items are worth keeping");
        assert_eq!(
            out.memories.len(),
            1,
            "the memory before it survives, as it always did"
        );
        assert_eq!(out.reminders.len(), 1, "the finished reminder survives too");
        assert_eq!(out.reminders[0].when_said, "next Tuesday");
    }

    #[test]
    fn a_null_memories_value_does_not_read_the_reminders_as_memories() {
        let raw = r#"{"memories":null,"reminders":[{"about":"the clinic","when":"on the 14th"},{"about":"the bi"#;
        let out = parse_window_response(raw, 5, true, &[])
            .expect("the finished reminder is worth keeping");
        assert!(
            out.memories.is_empty(),
            "no reminder was misread as a memory"
        );
        assert_eq!(out.reminders.len(), 1);
        assert_eq!(out.reminders[0].when_said, "on the 14th");
    }

    /// Unlike the test above, these items carry `note`, so only the key anchor keeps them out.
    #[test]
    fn a_null_memories_value_lends_no_other_array_to_the_memories_reader() {
        let raw = r#"{"memories":null,"example":[{"note":"Jerry likes his tea strong.","kind":"preference"},{"note":"Jerry wal"#;
        assert!(
            parse_window_response(raw, 5, true, &[]).is_err(),
            "an array that is not the memories value was admitted as memories"
        );
    }

    #[test]
    fn a_reply_that_parses_never_reaches_the_salvage() {
        let whole = r#"{"memories":[{"note":"one thing worth keeping about them","kind":"context"},{"note":"a second thing worth keeping too","kind":"context"}]}"#;
        let out = parse_window_response(whole, 5, false, &[]).expect("valid JSON");
        assert_eq!(
            out.memories.len(),
            2,
            "the last item survives a clean parse"
        );
    }

    #[test]
    fn rubbish_is_still_unparseable_after_the_salvage() {
        for raw in [
            "I'm sorry, I can't help with that.",
            "```json\n{\n  \"memories\": [\n    {\n      \"no",
            "{\"memories\": [",
            "",
        ] {
            assert!(
                parse_window_response(raw, 5, false, &[]).is_err(),
                "should stay unparseable: {raw:?}"
            );
        }
    }

    #[test]
    fn the_prompt_fits_its_stated_ceiling() {
        let prompt = rendered();
        assert!(
            prompt.len() <= EXTRACTION_PROMPT_CEILING,
            "the extraction prompt is {} chars against a ceiling of {}",
            prompt.len(),
            EXTRACTION_PROMPT_CEILING
        );
        // ~4 chars per token; under a third of the budget, so the window is not trimmed to nothing.
        assert!(prompt.len() / 4 < EXTRACTION_PROMPT_BUDGET_TOKENS / 3);
    }

    #[test]
    fn every_placeholder_is_filled() {
        for allow in [true, false] {
            let prompt = render_extraction_prompt("Jerry", "Goose", 3, allow);
            for placeholder in [
                "{subject}",
                "{assistant}",
                "{n}",
                "{skeleton}",
                "{reminders}",
            ] {
                assert!(
                    !prompt.contains(placeholder),
                    "{placeholder} survived rendering (allow_reminders={allow})"
                );
            }
        }
    }

    #[test]
    fn the_extraction_prompt_diverges_from_every_chat_prompt() {
        use pond_core::prompts::{
            PROMPT_BALANCED, PROMPT_CONCISE, PROMPT_TECHNICAL, PROMPT_WARM, SYSTEM_PROMPT,
        };

        let prompt = rendered();
        for chat in [
            SYSTEM_PROMPT,
            PROMPT_BALANCED,
            PROMPT_CONCISE,
            PROMPT_TECHNICAL,
            PROMPT_WARM,
        ] {
            let shared = prompt
                .as_bytes()
                .iter()
                .zip(chat.as_bytes())
                .take_while(|(a, b)| a == b)
                .count();
            assert!(
                shared < 16,
                "the extraction prompt shares {shared} leading bytes with a chat prompt. \
                 `prefill_plan` tests ReusePrefix before the sacrificial check, and \
                 ReusePrefix decodes into the LIVE session context -- a shared opening is \
                 how a background call comes to overwrite the household's retained prefix."
            );
        }
    }

    #[test]
    fn the_prompt_carries_no_worked_example() {
        let prompt = rendered();
        for leak in ["florence", "kisumu", "nairobi", "sourdough"] {
            assert!(
                !prompt.to_lowercase().contains(leak),
                "{leak:?} is content, and a model at this size copies content it is shown"
            );
        }
        // A sentence as the `when` placeholder would be copied verbatim into reminders.
        assert!(prompt.contains("\"when\":\"...\""));
    }

    #[test]
    fn the_prompt_and_the_echo_gate_agree_about_examples() {
        use pond_core::user_data::domain::memory::EXTRACTION_EXAMPLE_FACTS;

        let prompt = rendered();
        for example in EXTRACTION_EXAMPLE_FACTS {
            assert!(
                prompt.contains(example),
                "{example:?} is refused as an echo of the prompt's own example, but the \
                 prompt does not demonstrate it -- so this entry only loses a true memory"
            );
        }
        // A worked example added to the prompt must add its output to the list in the same change.
        assert!(
            EXTRACTION_EXAMPLE_FACTS.is_empty(),
            "the prompt carries no worked example, so nothing can be echoed from it"
        );
    }

    /// Fragments, not sentences: a match across one of the constant's line wraps breaks on reflow.
    #[test]
    fn the_prompt_states_both_halves_of_the_date_rule() {
        let prompt = rendered();
        assert!(prompt.contains("A habit keeps its timing"));
        assert!(prompt.contains("NO one-off dates"));
        assert!(prompt.contains("A date is a reminder."));
    }

    #[test]
    fn the_prompt_says_that_most_windows_hold_no_memory() {
        for allow in [true, false] {
            let prompt = render_extraction_prompt("Jerry", "Goose", 3, allow);
            assert!(prompt.contains("Most conversations hold no memory"));
            assert!(prompt.contains("\"memories\" is then []"));
        }
    }

    /// The member's complaint: one question became a trait. The prompt sends it to interests.
    #[test]
    fn the_prompt_files_a_question_as_an_interest_never_a_memory() {
        let prompt = rendered();
        assert!(prompt.contains("Never a memory: a question Jerry asked"));
        assert!(prompt.contains("signal \"asked\""));
        assert!(prompt.contains("One question says nothing about who they are."));
        for signal in ["\"asked\"", "\"said\"", "\"does\""] {
            assert!(prompt.contains(signal), "{signal} is read and never shown");
        }
    }

    #[test]
    fn the_prompt_and_the_parser_agree_on_the_interest_categories() {
        let prompt = rendered();
        for category in pond_core::user_data::domain::interest::CATEGORIES {
            assert!(
                prompt.contains(category),
                "{category} is stored but never offered to the model"
            );
        }
    }

    #[test]
    fn the_prompt_asks_for_interests_before_memories() {
        let prompt = rendered();
        let interests = prompt
            .find("{\"interests\":[")
            .expect("interests in the skeleton");
        let memories = prompt
            .find("\"memories\":[")
            .expect("memories in the skeleton");
        assert!(interests < memories);
    }

    #[test]
    fn the_prompt_and_the_parser_agree_on_the_catalogue() {
        let prompt = rendered();
        // `now` is not offered: extraction keeps what lasts.
        for about in About::ALL.into_iter().filter(|a| *a != About::Now) {
            assert!(
                prompt
                    .split_whitespace()
                    .any(|w| w.trim_end_matches(':') == about.as_str()),
                "{} is accepted by the parser and never shown to the model",
                about.as_str()
            );
        }
        for gone in ["identity", "project", "knowledge"] {
            assert!(
                !prompt.contains(gone),
                "{gone:?} is in the prompt but the parser rejects it"
            );
        }
    }

    #[test]
    fn a_stale_window_is_never_asked_for_a_reminder() {
        let prompt = render_extraction_prompt("Jerry", "Goose", 3, false);
        assert!(!prompt.contains("reminders"));
        assert!(!prompt.contains("\"when\""));
        assert!(prompt.contains("\"memories\":[{\"note\""));
        assert!(
            prompt.len() < rendered().len(),
            "dropping the paragraph is also worth about seventy tokens on every backlog \
             window, which is nearly all of a first run"
        );
    }

    // ── Parsing ──────────────────────────────────────────────────────────

    #[test]
    fn a_clean_reply_parses() {
        let raw = r#"{"memories":[{"note":"Jerry waters the greenhouse before work.","kind":"routine"}],"reminders":[{"about":"the dentist","when":"next Tuesday"}]}"#;
        let out = parse_window_response(raw, 3, true, &[]).unwrap();
        assert_eq!(out.memories.len(), 1);
        assert_eq!(out.memories[0].kind, MemoryKind::Routine);
        assert_eq!(out.reminders.len(), 1);
        assert_eq!(out.reminders[0].when_said, "next Tuesday");
    }

    #[test]
    fn a_preamble_and_a_thinking_block_are_salvaged() {
        let raw = "<think>hmm, what did they say</think>Here you go:\n\
                   {\"memories\":[{\"note\":\"Jerry's sister is Amara.\",\"kind\":\"relationship\"}]}\n\
                   Hope that helps!";
        let out = parse_window_response(raw, 3, true, &[]).unwrap();
        assert_eq!(out.memories.len(), 1);
    }

    /// A bare array is valid JSON, so it must be lifted on the first parse, not by a later salvage.
    #[test]
    fn a_bare_array_is_read_as_the_memories_list() {
        let raw = r#"[{"note":"Jerry prefers short answers.","kind":"preference"}]"#;
        let out = parse_window_response(raw, 3, true, &[]).unwrap();
        assert_eq!(out.memories.len(), 1);
        assert_eq!(out.memories[0].kind, MemoryKind::Preference);

        // With a preamble, as local models actually send it.
        let with_preamble = format!("Here is what I found:\n{raw}");
        assert_eq!(
            parse_window_response(&with_preamble, 3, true, &[])
                .unwrap()
                .memories
                .len(),
            1
        );
    }

    /// `{"facts":[..]}` is the old prompt's shape: read as empty, it would skip every window.
    #[test]
    fn an_object_with_neither_key_is_a_parse_failure() {
        let raw =
            r#"{"facts":[{"content":"Jerry prefers short answers.","segment":"preference"}]}"#;
        assert!(matches!(
            parse_window_response(raw, 3, true, &[]),
            Err(ExtractionError::Unparseable { .. })
        ));

        // Vacuity control: the same item under the right key parses.
        let right = r#"{"memories":[{"note":"Jerry prefers short answers.","kind":"preference"}]}"#;
        assert_eq!(
            parse_window_response(right, 3, true, &[])
                .unwrap()
                .memories
                .len(),
            1
        );
    }

    #[test]
    fn an_unreadable_reply_is_not_an_empty_one() {
        let empty =
            parse_window_response(r#"{"memories":[],"reminders":[]}"#, 3, true, &[]).unwrap();
        assert!(empty.is_empty());

        let err = parse_window_response("Sure! I had a look and nothing stood out.", 3, true, &[])
            .expect_err("no JSON is recoverable here");
        assert!(matches!(err, ExtractionError::Unparseable { .. }));
    }

    #[test]
    fn the_unparseable_error_carries_a_bounded_excerpt() {
        let long = "not json ".repeat(400);
        let err = parse_window_response(&long, 3, true, &[]).expect_err("unparseable");
        match err {
            ExtractionError::Unparseable { raw_head } => {
                assert!(raw_head.chars().count() <= RAW_HEAD_CHARS);
            }
            other => panic!("expected Unparseable, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_kind_is_counted_and_dropped() {
        let raw = r#"{"memories":[
            {"note":"William Ruto is the president of Kenya.","kind":"knowledge"},
            {"note":"Jerry prefers short answers.","kind":"preference"}
        ]}"#;
        let out = parse_window_response(raw, 3, true, &[]).unwrap();
        assert_eq!(out.memories.len(), 1);
        assert_eq!(out.rejected, 1);
    }

    #[test]
    fn reminders_from_a_stale_window_are_discarded() {
        let raw = r#"{"memories":[],"reminders":[{"about":"the dentist","when":"next Tuesday"}]}"#;
        let out = parse_window_response(raw, 3, false, &[]).unwrap();
        assert!(out.reminders.is_empty());

        // Vacuity control: the same reply yields a reminder when one was asked for.
        assert_eq!(
            parse_window_response(raw, 3, true, &[])
                .unwrap()
                .reminders
                .len(),
            1
        );
    }

    #[test]
    fn the_memory_cap_is_honoured() {
        let raw = r#"{"memories":[
            {"note":"Jerry prefers short answers.","kind":"preference"},
            {"note":"Jerry's sister is Amara.","kind":"relationship"},
            {"note":"Jerry waters the greenhouse before work.","kind":"routine"},
            {"note":"Jerry runs the standup.","kind":"routine"}
        ]}"#;
        assert_eq!(
            parse_window_response(raw, 2, true, &[])
                .unwrap()
                .memories
                .len(),
            2
        );
    }

    #[test]
    fn interests_are_read_beside_the_memories() {
        let raw = r#"{"interests":[
            {"topic":"Toyota Fielder","category":"cars","facets":["under KSh 1.2M","Kilimall"],"signal":"does"},
            {"topic":"Primetime","category":"Film/TV","facets":[],"signal":"asked"}
        ],"memories":[]}"#;
        let out = parse_window_response(raw, 3, true, &["Jerry", "Goose"]).unwrap();
        assert!(out.memories.is_empty());
        assert_eq!(out.interests.len(), 2);
        assert_eq!(out.interests[0].category, "cars");
        assert_eq!(out.interests[0].facets, vec!["under KSh 1.2M", "Kilimall"]);
        assert_eq!(out.interests[0].signal, Signal::Does);
        assert_eq!(out.interests[1].category, "film_tv");
        assert_eq!(out.interests[1].signal, Signal::Asked);
        assert!(
            !out.is_empty(),
            "a window with only interests still yielded something"
        );
    }

    #[test]
    fn the_subject_the_assistant_and_the_placeholder_are_never_interests() {
        let raw = r#"{"interests":[
            {"topic":"Goose","category":"tech","facets":[],"signal":"asked"},
            {"topic":"jerry","category":"people","facets":[],"signal":"said"},
            {"topic":"...","category":"...","facets":["..."],"signal":"asked"},
            {"topic":"Gen V","category":"film_tv","facets":["...","gen v"],"signal":"said"},
            {"topic":"gen v","category":"film_tv","facets":[],"signal":"said"}
        ],"memories":[]}"#;
        let out = parse_window_response(raw, 3, true, &["Jerry", "Goose"]).unwrap();
        assert_eq!(out.interests.len(), 1, "{:?}", out.interests);
        assert_eq!(out.interests[0].topic, "Gen V");
        assert!(out.interests[0].facets.is_empty());
    }

    #[test]
    fn a_reply_cut_off_inside_its_interests_keeps_the_finished_ones() {
        let raw = r#"{"interests":[{"topic":"The Boys","category":"film_tv","facets":["Homelander"],"signal":"said"},{"topic":"Gen"#;
        let out = parse_window_response(raw, 3, true, &[]).expect("a finished interest is kept");
        assert_eq!(out.interests.len(), 1);
        assert_eq!(out.interests[0].topic, "The Boys");
    }

    // ── The user message ─────────────────────────────────────────────────

    #[test]
    fn the_window_message_names_the_speakers_and_what_is_known() {
        let subject = WindowSubject::named("Jerry");
        let messages = vec![
            WindowMessage {
                id: "m1".to_string(),
                role: "user".to_string(),
                content: "the starter lives in the pantry".to_string(),
                created_at: chrono::Utc::now(),
            },
            WindowMessage {
                id: "m2".to_string(),
                role: "assistant".to_string(),
                content: "noted".to_string(),
                created_at: chrono::Utc::now(),
            },
        ];
        let known = vec![KnownMemory {
            note: "Jerry waters the greenhouse before work.".to_string(),
            kind_label: "routine".to_string(),
            pattern: false,
        }];
        let rendered = render_window_message(
            &ExtractionWindow {
                subject: &subject,
                assistant_name: "Goose",
                session_id: "sess-1",
                window_id: "m2",
                messages: &messages,
                known: &known,
                max_memories: 3,
                allow_reminders: true,
            },
            EXTRACTION_PROMPT_BUDGET_TOKENS,
        );

        assert!(rendered.contains("Already remembered about Jerry:"));
        assert!(rendered.contains("1. [routine] Jerry waters the greenhouse before work."));
        assert!(rendered.contains("Jerry: the starter lives in the pantry"));
        assert!(rendered.contains("Goose: noted"));
        // Nothing counts observations yet, so nothing is marked established.
        assert!(!rendered.contains("[routine*]"));
    }

    #[test]
    fn the_known_block_is_dropped_row_by_row_to_fit_the_budget() {
        let subject = WindowSubject::named("Jerry");
        let messages = vec![WindowMessage {
            id: "m1".to_string(),
            role: "user".to_string(),
            content: "the starter lives in the pantry".to_string(),
            created_at: chrono::Utc::now(),
        }];
        let known: Vec<KnownMemory> = (0..8)
            .map(|i| KnownMemory {
                note: format!("Jerry remembers thing {i}. {}", "long ".repeat(60)),
                kind_label: "routine".to_string(),
                pattern: false,
            })
            .collect();
        let window = ExtractionWindow {
            subject: &subject,
            assistant_name: "Goose",
            session_id: "sess-1",
            window_id: "m1",
            messages: &messages,
            known: &known,
            max_memories: 3,
            allow_reminders: true,
        };

        let budget = 200;
        let rendered = render_window_message(&window, budget);
        assert!(
            estimated_tokens(&rendered) <= budget,
            "the assembled user message is {} tokens against a budget of {budget}",
            estimated_tokens(&rendered)
        );
        // Whole rows only.
        assert!(rendered.contains("thing 0"));
        assert!(!rendered.contains("thing 7"));
        assert!(
            rendered.contains("the starter lives in the pantry"),
            "the conversation is never what gets dropped to fit"
        );

        // Vacuity control: with the real budget every row is shown.
        let full = render_window_message(&window, EXTRACTION_PROMPT_BUDGET_TOKENS);
        assert!(full.contains("thing 7"));
    }

    #[test]
    fn a_spent_budget_drops_the_header_with_the_rows() {
        let subject = WindowSubject::named("Jerry");
        let messages = vec![WindowMessage {
            id: "m1".to_string(),
            role: "user".to_string(),
            content: "hello".to_string(),
            created_at: chrono::Utc::now(),
        }];
        let known = vec![KnownMemory {
            note: "Jerry waters the greenhouse before work.".to_string(),
            kind_label: "routine".to_string(),
            pattern: false,
        }];
        let rendered = render_window_message(
            &ExtractionWindow {
                subject: &subject,
                assistant_name: "Goose",
                session_id: "sess-1",
                window_id: "m1",
                messages: &messages,
                known: &known,
                max_memories: 3,
                allow_reminders: true,
            },
            // Enough for the conversation and nothing else.
            5,
        );
        assert!(!rendered.contains("Already remembered"));
        assert!(rendered.starts_with("Conversation:"));
    }

    #[test]
    fn an_empty_store_renders_no_already_remembered_header() {
        let subject = WindowSubject::anonymous();
        let messages = vec![WindowMessage {
            id: "m1".to_string(),
            role: "user".to_string(),
            content: "hello".to_string(),
            created_at: chrono::Utc::now(),
        }];
        let rendered = render_window_message(
            &ExtractionWindow {
                subject: &subject,
                assistant_name: "Goose",
                session_id: "sess-1",
                window_id: "m1",
                messages: &messages,
                known: &[],
                max_memories: 3,
                allow_reminders: true,
            },
            EXTRACTION_PROMPT_BUDGET_TOKENS,
        );
        assert!(!rendered.contains("Already remembered"));
        assert!(rendered.starts_with("Conversation:"));
    }
}
