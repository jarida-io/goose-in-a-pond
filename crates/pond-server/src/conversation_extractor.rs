//! The live [`ConversationExtractor`]: read one window of a conversation with
//! the local model.
//!
//! # What this prompt is for, and what the old one could not ask
//!
//! The per-turn prompt this replaced asked "extract durable facts about the
//! USER from this conversation turn". It was a good prompt for the question it
//! asked, and the question was the wrong one twice over. A turn cannot show a
//! habit, so "is this a pattern, a standing way they do things?" -- a third of
//! what a household actually wants remembered -- was unanswerable by
//! construction. And a turn has no room for what the pond already knows, so a
//! model restating something better than it was first written had no way to say
//! so: the restatement came back as a near-duplicate and was thrown away.
//!
//! This prompt names the subject, asks what was memorable, asks the habit
//! question outright, draws the line between a habit's timing and a one-off
//! date, and shows what is already known so new evidence can build on it.
//!
//! # Why the date rule reads the way it does
//!
//! It was measured, not imagined. 432 live replies through this prompt and this
//! parser, across six local models. Every date that turned up inside a `note`
//! was in a class that carries one: a recurring habit, a recurring clock time,
//! a version year, an appointment, a biographical year, a date mid-sentence.
//! The classes that carry no date -- preference, relationship, correction,
//! context, a number that is not a date, a window with nothing in it -- were
//! 0 for 207. The models do not need telling about those, and telling them
//! would only spend budget.
//!
//! The recurring class is the one the old wording got wrong. "NO dates" asked
//! the models to write "swims each Saturday" without the Saturday, and they
//! would not: the shipped model and the larger one kept the weekday in all 12
//! recurring windows between them, at greedy and at both seeds, and granite did
//! the same. They were right to. A recurrence names no day on any calendar,
//! nothing in it can become false, and it is the pattern the `routine` kind
//! exists to capture -- so the rule now carves it out, and the write gate
//! agrees with it (`memory.rs`'s `recurrence_positions`).
//!
//! What is left forbidden is the one-off, which is what a reminder is for. On
//! the shipped model the split works: 31 reminders across its 36 dated windows,
//! not one date lost entirely, and every `when` was the household's own words
//! rather than a date the model worked out.
//!
//! The closing paragraph earns its line the same way. The shipped model
//! invented a memory on 5 of 6 windows that held nothing -- "Jerry checked if
//! Goose was awake" -- which fills a store with noise no date rule ever
//! touches. The larger model was 6 of 6 correct on the same windows, so this is
//! a prompt problem rather than a gate problem, and it is addressed where the
//! problem is.
//!
//! # The two design rules encoded here rather than remembered
//!
//! 1. **The first line must diverge from the chat system prompt's first line.**
//!    `prefill_plan` tests `ReusePrefix` BEFORE the sacrificial check
//!    (`inference_engine.rs:514-536`, `REUSE_MIN_TOKENS = 256`), and
//!    `ReusePrefix` decodes into the LIVE session context -- so an extraction
//!    call that shared 256 leading tokens with the chat prompt would overwrite
//!    the household's retained prefix and the next person to speak would pay a
//!    cold prefill. The chat prompt opens `<identity>`; this one opens `You are
//!    reading`. `the_extraction_prompt_diverges_from_every_chat_prompt` makes
//!    that a property rather than a coincidence.
//!
//! 2. **No worked example.** `project_functiongemma_behaviour` records that
//!    models at this size copy parameter descriptions into values verbatim, and
//!    `memory.rs`'s `EchoedExample` gate exists because the old prompt's own
//!    Florence-in-Kisumu example reached the live store as a fact about a family
//!    that does not exist. The schema skeleton shows shape, never content, and
//!    the `"when"` placeholder is `"..."` with the instruction in prose for
//!    exactly the same reason.
//!
//! # Why the parse salvage moved here
//!
//! `strip_thinking` and the first-`{`-to-last-`}` recovery are measured
//! behaviour against real local models, not guesses, so they were carried over
//! verbatim. What changed is the disposition of a total failure: the per-turn
//! path returned `Ok(vec![])`, indistinguishable from "nothing worth keeping".
//! In a batch design that silently advances a cursor past a window nobody read,
//! so it is [`ExtractionError::Unparseable`] here.
//!
//! This module is also the only remaining definition of `strip_thinking`: both
//! consolidators call it here, where they used to call a copy in the extractor
//! that has since been deleted.

use std::sync::Arc;

use async_trait::async_trait;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::user_data::ports::conversation_extractor::{
    estimated_tokens, ConversationExtractor, ExtractedMemory, ExtractedReminder, ExtractionError,
    ExtractionWindow, MemoryKind, WindowExtraction, EXTRACTION_PROMPT_BUDGET_TOKENS,
};
use tokio::sync::RwLock;

/// The system prompt, rendered per window.
///
/// `{subject}` is the resolved window subject -- a member's display name,
/// `settings.user_name`, or literally `the user` on a pond where nobody has
/// given a name. `{assistant}` is `settings.assistant_name`. `{n}` is
/// `memory_extraction_max_facts`.
///
/// The reminders paragraph and the `"reminders"` key are dropped whole for a
/// window older than the staleness cutoff, which saves about seventy tokens on
/// the backlog windows that make up nearly all of a first run.
const EXTRACTION_PROMPT: &str = "\
You are reading one conversation between {subject} and {assistant}, to
decide what is worth remembering about {subject}.

For each thing, ask: would this still be useful in six months, with the
conversation gone? And: is it a one-off, or is it a habit, a pattern, a
standing way {subject} does things? If it is a standing way, its kind is
\"routine\".

Reply with JSON and nothing else:
{skeleton}

note:
- Third person. Never \"I\", \"me\", \"my\", \"we\". Say \"{subject}\" by name.
- Stands alone: name every person and place. Never \"there\", \"that place\",
  \"the latter\", or an opening \"He\", \"She\", \"It\", \"They\".
- One plain sentence. No label prefix.
- A habit keeps its timing: \"every Saturday\", \"each morning at six\" are
  part of the habit. Keep them.
- NO one-off dates: no year, no \"on Tuesday\", no \"next week\", no
  \"tomorrow\", no \"at six\" for something that happens once. Write the
  memory without it, or leave the memory out. A date is a reminder.

kind, one of exactly these five:
- relationship  a person or pet {subject} knows, and who they are to them
- preference    how {subject} likes things: style, defaults, likes, dislikes
- routine       something {subject} does again and again: a habit, a pattern
- correction    {subject} fixed something that was wrong
- context       who {subject} is: role, home, the work they are living through

Some of this may already be known. If the conversation adds weight to
something in \"Already remembered\", write that memory again as the BETTER
version of itself: same subject, said more exactly. Do not repeat one
unchanged.
{reminders}
Say nothing about {assistant}'s replies, nothing {subject} asked for only
once, nothing you are guessing at. Most conversations hold nothing worth
keeping: a greeting, a question answered, a sum worked out -- for those,
answer {empty}. At most {n} memories; fewer is better.";

/// The schema skeleton when reminders are wanted.
const SKELETON_WITH_REMINDERS: &str =
    "{\"memories\":[{\"note\":\"...\",\"kind\":\"relationship\"}],\
     \"reminders\":[{\"about\":\"...\",\"when\":\"...\"}]}";

/// The skeleton for a window too old to propose anything from.
const SKELETON_MEMORIES_ONLY: &str =
    "{\"memories\":[{\"note\":\"...\",\"kind\":\"relationship\"}]}";

/// The reminders paragraph, dropped entirely alongside the `"reminders"` key.
const REMINDERS_PARAGRAPH: &str = "\n\
reminders: anything that happens on one named day or at one named time. In\n\
\"when\", put {subject}'s own words about the timing, copied from the\n\
conversation -- do not work out the actual date, and do not invent one.\n";

/// The hard ceiling on the rendered system prompt, in characters.
///
/// A budget rather than a hope: the window, the known-memories block and this
/// prompt share one prompt-side clamp, and the two that grow with the
/// conversation are the ones that must be trimmed when something has to give.
/// This one is authored, so it is the one that can be asserted.
pub const EXTRACTION_PROMPT_CEILING: usize = 2_300;

/// How much of an unparseable reply is carried into the error.
///
/// Bounded because this reaches a log file, and an unbounded model reply in a
/// log is a household's conversation written to a second place with none of the
/// store's scoping, retention or redaction.
const RAW_HEAD_CHARS: usize = 240;

pub struct LlmConversationExtractor {
    live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>,
}

impl LlmConversationExtractor {
    pub fn new(live_provider: Arc<RwLock<Option<Arc<dyn LlmProvider>>>>) -> Self {
        Self { live_provider }
    }
}

/// Render the system prompt for one window.
pub fn render_extraction_prompt(
    subject: &str,
    assistant: &str,
    max_memories: usize,
    allow_reminders: bool,
) -> String {
    let (skeleton, reminders, empty) = if allow_reminders {
        (
            SKELETON_WITH_REMINDERS,
            REMINDERS_PARAGRAPH.replace("{subject}", subject),
            "{\"memories\":[],\"reminders\":[]}",
        )
    } else {
        (SKELETON_MEMORIES_ONLY, String::new(), "{\"memories\":[]}")
    };

    EXTRACTION_PROMPT
        .replace("{skeleton}", skeleton)
        .replace("{reminders}", &reminders)
        .replace("{empty}", empty)
        .replace("{subject}", subject)
        .replace("{assistant}", assistant)
        .replace("{n}", &max_memories.to_string())
}

/// Render the one user message: what is already known, then the conversation.
///
/// # The budget is spent here, and it is spent in one direction
///
/// `budget_tokens` is what is left of [`EXTRACTION_PROMPT_BUDGET_TOKENS`] after
/// the system prompt, and this is the last place anything can be dropped before
/// the call. The conversation is never what gets dropped: it is what the call
/// exists to read, and `carve_window` has already bounded it. The
/// known-memories block is, one whole row at a time from the least relevant end
/// -- never truncated mid-sentence, because half a remembered sentence shown to
/// a model at this size is one it will finish in its own words.
///
/// A budget that was declared and never measured is what let a 40 KB pasted log
/// reach a provider with an 8192-token clamp.
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
        // Not reachable through `carve_window`, which refuses an exchange
        // larger than the window budget outright. It is a warning rather than a
        // silent overrun because reaching it means the carve bound and this
        // budget have come apart, and the symptom at the other end is a
        // truncated prompt that loses the schema and comes back unparseable.
        tracing::warn!(
            tokens = spent,
            budget = budget_tokens,
            "[batch-extraction] one window's conversation alone overruns the prompt budget"
        );
    }

    let mut known_block = String::new();
    if !window.known.is_empty() {
        let header = format!("Already remembered about {}:\n", window.subject.name);
        // The header plus the blank line that closes the block. Both are paid
        // for before the first row is admitted, so the block cannot fit itself
        // and then overrun on its own punctuation.
        let header_cost = estimated_tokens(&header) + 1;
        let mut rows = String::new();
        let mut kept = 0usize;
        for known in window.known {
            // The asterisk marks an established memory so the model can tell
            // what it is adding weight to from what it is seeing once. Nothing
            // counts observations yet, so nothing carries one.
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
        // The header is written only if something ended up under it. A header
        // with nothing beneath it is an invitation to a small model to fill it
        // in, which is the same failure an empty store already guards against.
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
        // What is left of the budget once the authored half is paid for. The
        // system prompt is the one component whose size is known in advance, so
        // it is the one that is never trimmed and always subtracted.
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
    // Both arrays, not just the first. This used to rescue `memories` alone and
    // stop at its closing `]` -- and the schema puts `reminders` AFTER it, so a
    // reply truncated near its end, the likeliest place, lost exactly the
    // reminders. The result was still `Ok`, the window counted as examined, and
    // the cursor moved past it: a complete "next Tuesday" gone, with
    // `reminders_lost` and `dates_lost` both reading zero.
    let memories = match array_after_key(cleaned, "memories") {
        Some(start) => complete_items(cleaned, start),
        // No `"memories"` key at all. A bare `[` counts only when the array
        // OPENS the reply, exactly as the `bracket` candidate above requires.
        // Without that restriction this reaches inside `{"facts":[...]}` -- a
        // well-formed answer to a schema nobody asked for -- pulls out its
        // array and returns it as `memories`. That is the failure
        // `an_object_with_neither_key_is_a_parse_failure` exists for, and the
        // worst one available here: it moves the watermark past every window
        // in the store, once, and never comes back.
        None if !cleaned.contains("\"memories\"") => match cleaned.find('[') {
            Some(open) if !cleaned.find('{').is_some_and(|b| b < open) => {
                complete_items(cleaned, open + 1)
            }
            _ => Vec::new(),
        },
        // The key is there but its value is not an array (`null`, say). Its
        // items are none -- and the next `[` in the reply belongs to some
        // OTHER key. Reading it here is how a reminders array used to be
        // misread as memories, fail the note gate one by one, and vanish.
        None => Vec::new(),
    };
    let reminders = array_after_key(cleaned, "reminders")
        .map(|start| complete_items(cleaned, start))
        .unwrap_or_default();

    // One complete item of either kind is worth keeping; none means there was
    // nothing to salvage and the reply really is unparseable.
    if memories.is_empty() && reminders.is_empty() {
        return None;
    }
    Some(serde_json::json!({ "memories": memories, "reminders": reminders }))
}

/// Where the array that is `key`'s VALUE begins, just past its `[`.
///
/// Only whitespace and the `:` may stand between the key and the bracket. A
/// looser "first `[` after the key" is what let a key with a `null` value lend
/// the next key's array to the wrong reader.
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

/// Every complete, balanced object in an array, scanning from just inside its
/// opening bracket until the array closes or the text runs out.
///
/// An item that survives this still faces every gate below -- a partial object
/// missing `note` is dropped there, not admitted here.
fn complete_items(cleaned: &str, start: usize) -> Vec<serde_json::Value> {
    let bytes = cleaned.as_bytes();
    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut depth = 0usize;
    let mut item_start = None;
    let mut in_string = false;
    let mut escaped = false;

    for (i, &b) in bytes.iter().enumerate().skip(start) {
        // Braces inside a string are not structure. A note containing "{" is
        // unusual and a note containing an apostrophe-escaped quote is not, so
        // the string state has to be tracked properly rather than counted.
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
            // The array closed. Stop scanning -- but KEEP what was collected.
            // A closed array with a malformed tail (a trailing comma is the
            // common one) holds perfectly good items, and refusing it was
            // measured to cost exactly those.
            b']' if depth == 0 => break,
            _ => {}
        }
    }
    items
}

/// Whether a parsed value is an answer to the question that was asked.
///
/// An array is the old schema's memories list and is lifted into the new shape.
/// An object is accepted only if it carries at least one of the two keys --
/// otherwise it is a reply to some other question, and reading it as "nothing
/// was worth keeping" would advance a cursor past a window nobody read.
fn usable_shape(value: serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::Array(items) => Some(serde_json::json!({ "memories": items })),
        other if other.get("memories").is_some() || other.get("reminders").is_some() => Some(other),
        _ => None,
    }
}

/// Parse what came back, salvaging what the measured failures call for.
///
/// Three shapes are accepted, in this order: the object as written, the object
/// recovered from between the first `{` and the last `}` (a model that added a
/// preamble), and a bare `[...]` read as the `memories` array (a model that
/// answered the shape of the old prompt), each of those again with one level of
/// quote-escaping removed (see `unescaped`), and finally the complete items of
/// a reply the model ran out of room to finish (see `salvage_truncated`, which
/// is tried LAST because it repairs by discarding). Anything else is
/// [`ExtractionError::Unparseable`] -- NOT an empty result, which in a batch
/// design would advance a cursor past a window nobody read.
pub fn parse_window_response(
    raw: &str,
    max_memories: usize,
    allow_reminders: bool,
) -> Result<WindowExtraction, ExtractionError> {
    let cleaned = strip_thinking(raw.trim());

    let unparseable = || ExtractionError::Unparseable {
        raw_head: cleaned.chars().take(RAW_HEAD_CHARS).collect(),
    };

    // Three candidate slices, tried in order, and the first that yields a
    // USABLE shape wins. "Usable" is doing the load-bearing work: a slice that
    // parses as JSON but carries neither key is a model answering a schema
    // nobody asked for, and accepting it would produce an `Ok` with nothing in
    // it -- which moves the watermark past a window that was perfectly
    // readable. That is the failure `Unparseable` exists to prevent, and it
    // arrives through the parser rather than through the model.
    let brace = cleaned
        .find('{')
        .zip(cleaned.rfind('}'))
        .filter(|(a, b)| a < b)
        .map(|(a, b)| &cleaned[a..=b]);

    // A bare `[...]` is the shape the per-turn prompt asked for, and a local
    // model that has seen both will sometimes answer in the older one.
    //
    // Only when the text OPENS with the array, though. Every well-formed reply
    // in the new schema also contains `[` and `]`, so an unconditional bracket
    // salvage would reach inside `{"facts":[...]}` -- the old schema's wrapper
    // -- pull out its array, and read a list of objects that carry none of the
    // fields this parser wants as though the model had answered correctly.
    let bracket = match (cleaned.find('['), cleaned.find('{')) {
        (Some(open), brace_at) if brace_at.is_none_or(|b| open < b) => cleaned
            .rfind(']')
            .filter(|c| open < *c)
            .map(|c| &cleaned[open..=c]),
        _ => None,
    };

    let slices = [Some(cleaned.as_str()), brace, bracket];

    // Every candidate as written, first. Nothing below may run while a slice
    // still parses on its own terms.
    let value = slices
        .into_iter()
        .flatten()
        .filter_map(|slice| serde_json::from_str::<serde_json::Value>(slice).ok())
        .find_map(usable_shape)
        // Then the same candidates with one level of quote-escaping removed —
        // a complete, correct answer whose every quote is `\"`. See
        // `unescaped`. After the plain pass, because a note that really does
        // contain an escaped quote parses as itself and must not be unescaped.
        .or_else(|| {
            slices
                .into_iter()
                .flatten()
                .filter_map(unescaped)
                .filter_map(|slice| serde_json::from_str::<serde_json::Value>(&slice).ok())
                .find_map(usable_shape)
        })
        // LAST, and only after every valid-JSON reading has failed. The salvage
        // repairs truncation by DISCARDING a trailing item, which on a
        // well-formed reply would throw away a memory the model finished. Both
        // forms, because a reply can be over-escaped AND truncated.
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
            // An unknown kind is REJECTED, never defaulted. Defaulting is
            // measurably how five third-party biography facts entered the live
            // store: the old parser mapped everything it did not recognise onto
            // `knowledge`, so a model answering the wrong question still got a
            // row. A rejection is counted, so a prompt the model is
            // systematically misreading shows up as a number.
            let Some(kind) = item
                .get("kind")
                .and_then(|k| k.as_str())
                .and_then(MemoryKind::parse)
            else {
                extraction.rejected += 1;
                continue;
            };
            extraction.memories.push(ExtractedMemory {
                note: note.to_string(),
                kind,
            });
        }
    }

    // A window too old to propose anything from was not shown the reminders
    // half of the schema. A model that produced them anyway is answering from
    // its own idea of the task, and honouring that would put a proposal in
    // front of somebody about a conversation from months ago.
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

/// Strip `<think>…</think>` and `<|channel>…<channel|>` tokens.
///
/// Carried verbatim from the per-turn extractor that was deleted with the
/// per-turn path. It is measured behaviour against the local families, not a
/// guess, so it moved rather than being rewritten -- and it is the ONE
/// definition now: both consolidators call this, where they used to call a
/// copy.
pub fn strip_thinking(text: &str) -> String {
    let result = strip_blocks(text, "<think>", "</think>");
    strip_blocks(&result, "<|channel>", "<channel|>")
        .trim()
        .to_string()
}

/// Remove every `open .. close` block, and any reasoning that has lost its
/// opening tag.
///
/// Always terminates, and that is the point of it being written this way. The
/// previous loop searched for the close tag from the START of the string rather
/// than after the open tag it had just found, so a close that came first --
/// `r1</think><think>r2</think>{..}`, the shape a chat template that inserts the
/// opening tag itself produces -- rebuilt a string still holding the same open
/// tag, byte-identical or longer, forever. That was a synchronous spin inside
/// an async fn, called from the extraction pass and both consolidators while
/// they held the lane's only inference slot: one reply of that shape would have
/// starved every other background job for the life of the process.
///
/// Here every iteration removes at least the tag it found, so the string
/// strictly shrinks.
fn strip_blocks(text: &str, open: &str, close: &str) -> String {
    let mut s = text.to_string();

    // A close with no open before it ends a reasoning block whose opening tag
    // the template emitted on the model's behalf: everything up to and
    // including it is reasoning.
    while let Some(c) = s.find(close) {
        if s.find(open).is_some_and(|o| o < c) {
            break;
        }
        s.replace_range(..c + close.len(), "");
    }

    while let Some(start) = s.find(open) {
        let body = start + open.len();
        match s[body..].find(close) {
            Some(rel) => s.replace_range(start..body + rel + close.len(), ""),
            None => {
                // Unclosed -- a reply cut off mid-thought. Strip to the end.
                s.truncate(start);
                break;
            }
        }
    }
    s
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

    /// THE DEFECT: a close tag before an open one used to spin forever,
    /// holding the lane slot. This test would hang, not fail, on the old loop
    /// -- which is exactly how the defect presents on a pond.
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

    /// The ordinary shapes are unchanged -- the fix must not cost the case
    /// every model produces.
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

    /// The prompt fits the budget it claims.
    ///
    /// The window and the known-memories block both grow with the
    /// conversation; this is the one component that is authored, so it is the
    /// one whose size can be asserted rather than hoped for.
    /// The reply a real gemma-4-E2B gave on this pond, and what it used to cost.
    ///
    /// A complete, correct answer — ten notes and two reminders, properly
    /// nested and closed, inside a ```json fence — with every quote written as
    /// `\"`. A backslash outside a string is not valid JSON, so every candidate
    /// failed and the window came back `parse_failures=1` with nothing written.
    #[test]
    fn a_reply_whose_every_quote_is_escaped_still_yields_its_notes() {
        let over_escaped = concat!(
            "```json\n{\n  \\\"memories\\\": [\n",
            "    {\n      \\\"note\\\": \\\"Jerry drinks coffee only before noon.\\\",\n",
            "      \\\"kind\\\": \\\"preference\\\"\n    },\n",
            "    {\n      \\\"note\\\": \\\"Brother Manu lives in Kisumu and visits at Christmas.\\\",\n",
            "      \\\"kind\\\": \\\"relationship\\\"\n    }\n  ]\n}\n```"
        );
        // It really is invalid JSON as written -- the control for everything
        // below, and the reason the pond lost ten notes.
        assert!(
            serde_json::from_str::<serde_json::Value>(over_escaped).is_err(),
            "the fixture must be the malformed thing the model actually sent"
        );

        let out = parse_window_response(over_escaped, 5, false)
            .expect("a correct answer must not be lost to the escaping of a quote");
        assert_eq!(out.memories.len(), 2);
        assert!(out.memories[0].note.contains("coffee only before noon"));
        assert!(out.memories[1].note.contains("Kisumu"));
    }

    /// The unescape runs only after every unmodified candidate has failed, so a
    /// note that legitimately contains an escaped quote parses as itself. Run
    /// the other way round, this fixture's note would be broken in half.
    #[test]
    fn a_note_containing_a_real_quotation_is_not_unescaped() {
        let raw = r#"{"memories":[{"note":"Jerry says \"no confirmations\" when asked twice.","kind":"preference"}]}"#;
        let out = parse_window_response(raw, 5, false).expect("valid JSON");
        assert_eq!(out.memories.len(), 1);
        assert!(
            out.memories[0].note.contains("\"no confirmations\""),
            "the quotation survives: {:?}",
            out.memories[0].note
        );
    }

    /// The reply a real gemma-4-E2B gave, verbatim, and what it used to cost.
    ///
    /// An eight-message conversation, temperature 0. The model opened a ```json
    /// fence, wrote two entirely good notes and stopped mid-word inside the
    /// third. Every candidate above needs valid JSON, so the window came back
    /// `parse_failures=1`, nothing was written, and the cursor did not advance
    /// — meaning the next pass would read the same window, truncate at the same
    /// place, and write nothing again.
    #[test]
    fn a_reply_the_model_ran_out_of_room_to_finish_keeps_what_it_finished() {
        let truncated = "```json\n{\n  \"memories\": [\n    {\n      \"note\": \"Jerry moves the espresso machine to the window shelf so it is reachable before the 7:15 school run every morning.\",\n      \"kind\": \"context\"\n    },\n    {\n      \"note\": \"Jerry buys beans from Kahawa on Ngong Road, a kilo at a time.\",\n      \"kind\": \"context\"\n    },\n    {\n      \"note\": \"Jerry prefer";

        let out = parse_window_response(truncated, 5, false)
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

    /// The two ways salvage could launder a wrong answer into a right one, both
    /// refused. This is the regression guard for
    /// `an_object_with_neither_key_is_a_parse_failure`, which this salvage
    /// broke the first time it was written.
    #[test]
    fn salvage_refuses_anything_that_is_not_actually_truncated() {
        // A CLOSED array is not truncated; whatever is wrong with it cannot be
        // fixed by discarding a trailing item.
        assert!(
            parse_window_response(
                r#"{"facts":[{"content":"Jerry prefers short answers.","segment":"preference"}]}"#,
                3,
                false
            )
            .is_err(),
            "the old prompt's shape must not be read as the new one"
        );

        // And the same wrong schema, truncated, is still not this schema.
        assert!(
            parse_window_response(
                r#"{"facts":[{"content":"Jerry prefers short answers.","segment":"preference"},{"content":"Jerry buys"#,
                3,
                false
            )
            .is_err(),
            "a bare array is only salvaged when it OPENS the reply"
        );
    }

    /// The recall the closed-array refusal would have cost. A trailing comma
    /// after a closed array is the commonest malformed tail these models write,
    /// and the notes inside it are perfectly good.
    #[test]
    fn a_closed_array_with_a_malformed_tail_still_yields_its_notes() {
        let raw = "{\"memories\":[{\"note\":\"Jerry buys beans from Kahawa on Ngong Road.\",\"kind\":\"context\"}],}";
        let out = parse_window_response(raw, 5, false).expect("the notes inside are fine");
        assert_eq!(out.memories.len(), 1);
        assert!(out.memories[0].note.contains("Kahawa"));
    }

    /// A reply truncated inside `reminders` keeps the reminder the model
    /// finished, not just the memories before it.
    ///
    /// THE DEFECT: salvage rescued `memories` and stopped at its closing `]`,
    /// and the schema puts `reminders` after it. So the tail of the reply --
    /// the likeliest place for a cut -- held exactly what was thrown away, the
    /// window was still counted as examined, and the date was gone with every
    /// counter reading zero.
    #[test]
    fn a_reply_cut_off_inside_its_reminders_keeps_the_finished_one() {
        let raw = r#"{"memories":[{"note":"Jerry sees a dentist in Kisumu.","kind":"context"}],"reminders":[{"about":"the dentist","when":"next Tuesday"},{"about":"collect the tract"#;
        let out = parse_window_response(raw, 5, true).expect("finished items are worth keeping");
        assert_eq!(
            out.memories.len(),
            1,
            "the memory before it survives, as it always did"
        );
        assert_eq!(out.reminders.len(), 1, "the finished reminder survives too");
        assert_eq!(out.reminders[0].when_said, "next Tuesday");
    }

    /// A key whose value is not an array lends nobody its neighbour's.
    ///
    /// "First `[` after the key" read a `null` memories value's NEXT array --
    /// the reminders -- as memories. None has a `note`, so every one failed
    /// the gate and vanished: the same loss, by a different road.
    #[test]
    fn a_null_memories_value_does_not_read_the_reminders_as_memories() {
        let raw = r#"{"memories":null,"reminders":[{"about":"the clinic","when":"on the 14th"},{"about":"the bi"#;
        let out =
            parse_window_response(raw, 5, true).expect("the finished reminder is worth keeping");
        assert!(
            out.memories.is_empty(),
            "no reminder was misread as a memory"
        );
        assert_eq!(out.reminders.len(), 1);
        assert_eq!(out.reminders[0].when_said, "on the 14th");
    }

    /// The key anchor, on the case where it is load-bearing: another key's
    /// array whose items DO carry a `note`.
    ///
    /// With a looser "first `[` after the key", a `null` memories value lends
    /// the next array to the memories reader -- and where that array's items
    /// have notes (an echoed example, an invented field), they pass the gate
    /// and are admitted as things the household said. The same laundering
    /// `an_object_with_neither_key_is_a_parse_failure` guards, reached through
    /// the salvage. Here nothing was asked for and nothing is kept: the reply
    /// is unparseable, which is the honest answer.
    #[test]
    fn a_null_memories_value_lends_no_other_array_to_the_memories_reader() {
        let raw = r#"{"memories":null,"example":[{"note":"Jerry likes his tea strong.","kind":"preference"},{"note":"Jerry wal"#;
        assert!(
            parse_window_response(raw, 5, true).is_err(),
            "an array that is not the memories value was admitted as memories"
        );
    }

    /// The salvage is tried LAST and must never touch a reply that parses. It
    /// repairs by DISCARDING, so on a well-formed reply it would throw away the
    /// final memory the model actually finished.
    #[test]
    fn a_reply_that_parses_never_reaches_the_salvage() {
        let whole = r#"{"memories":[{"note":"one thing worth keeping about them","kind":"context"},{"note":"a second thing worth keeping too","kind":"context"}]}"#;
        let out = parse_window_response(whole, 5, false).expect("valid JSON");
        assert_eq!(
            out.memories.len(),
            2,
            "the last item survives a clean parse"
        );
    }

    /// Salvage must not turn genuine rubbish into a false success. A cursor
    /// advanced past a window nobody read is the failure `Unparseable` exists
    /// for, and this is the path that could quietly reintroduce it.
    #[test]
    fn rubbish_is_still_unparseable_after_the_salvage() {
        for raw in [
            "I'm sorry, I can't help with that.",
            "```json\n{\n  \"memories\": [\n    {\n      \"no",
            "{\"memories\": [",
            "",
        ] {
            assert!(
                parse_window_response(raw, 5, false).is_err(),
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
        // Roughly four characters per token, and the whole call is budgeted at
        // EXTRACTION_PROMPT_BUDGET_TOKENS. If the system half alone approached
        // that, the window would be trimmed to nothing to fit.
        assert!(prompt.len() / 4 < EXTRACTION_PROMPT_BUDGET_TOKENS / 3);
    }

    /// Nothing is left unrendered.
    ///
    /// A placeholder that survives rendering is shown to the model as literal
    /// text, and a model at this size copies what it is shown -- so `{subject}`
    /// in the output would become `{subject}` in a stored memory.
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
                "{empty}",
            ] {
                assert!(
                    !prompt.contains(placeholder),
                    "{placeholder} survived rendering (allow_reminders={allow})"
                );
            }
        }
    }

    /// The first line must diverge from every chat system prompt's first line.
    ///
    /// This is the `ReusePrefix` rule, and it is the difference between an
    /// extraction call that costs one window and one that clobbers the
    /// household's retained KV prefix so the next person to speak pays a cold
    /// prefill. `prefill_plan` compares from token zero and fires at 256
    /// shared tokens; a shared opening is the only way a call this short could
    /// get near that.
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

    /// The prompt shows a SHAPE, never a fact.
    ///
    /// The old prompt's worked example ("my mom florence lives in kisumu")
    /// reached the live store as two facts about a family that does not exist,
    /// which is why `memory.rs` carries an `EchoedExample` gate naming those
    /// exact sentences. A schema skeleton with `...` for every value cannot be
    /// copied into a memory, because there is nothing there to copy.
    #[test]
    fn the_prompt_carries_no_worked_example() {
        let prompt = rendered();
        for leak in ["florence", "kisumu", "nairobi", "sourdough"] {
            assert!(
                !prompt.to_lowercase().contains(leak),
                "{leak:?} is content, and a model at this size copies content it is shown"
            );
        }
        // The `when` placeholder is the sharpest case: a sentence there
        // ("what {user} said about timing") would appear verbatim as a
        // reminder. `project_functiongemma_behaviour` records exactly that.
        assert!(prompt.contains("\"when\":\"...\""));
    }

    /// The prompt and the echo gate agree, in both directions.
    ///
    /// `memory.rs` refuses a fact verbatim-equal to anything in
    /// `EXTRACTION_EXAMPLE_FACTS`, because a model at this size copies what it
    /// is shown: the old prompt's Florence-in-Kisumu demonstration reached the
    /// live store as two facts about a family that does not exist.
    ///
    /// The two have to be coupled both ways, and this is the only place that
    /// can see both. A demonstration with no entry is that same failure
    /// returning. An entry with no demonstration is worse in the quiet
    /// direction: it refuses a true memory forever, for a prompt that no longer
    /// exists, and nothing would ever say so.
    ///
    /// Today the list is EMPTY and the prompt shows no worked example, which is
    /// the pairing this asserts.
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
        // And the other way: the prompt demonstrates nothing, so the list is
        // empty. A worked example added above must add its output to the list
        // in the same change.
        assert!(
            EXTRACTION_EXAMPLE_FACTS.is_empty(),
            "the prompt carries no worked example, so nothing can be echoed from it"
        );
    }

    /// The date rule is stated in the prompt the gate enforces, BOTH halves of
    /// it.
    ///
    /// The halves are a pair and neither survives alone. Forbidding dates
    /// without carving out the habit asks for "swims each Saturday" with the
    /// Saturday taken out, which all six measured models refuse to write and
    /// the gate no longer wants; carving out the habit without forbidding the
    /// one-off gives the store "the dentist is on Tuesday" forever.
    ///
    /// Matched on fragments rather than whole sentences: the lines wrap in the
    /// constant, and an assertion across a wrap fails on a reflow that changed
    /// nothing.
    #[test]
    fn the_prompt_states_both_halves_of_the_date_rule() {
        let prompt = rendered();
        assert!(prompt.contains("A habit keeps its timing"));
        assert!(prompt.contains("NO one-off dates"));
        assert!(prompt.contains("A date is a reminder."));
    }

    /// The prompt says out loud that most windows hold nothing.
    ///
    /// Measured: the shipped model wrote a memory on 5 of 6 windows that held
    /// none -- a greeting, a unit conversion -- and every one of those would be
    /// injected into later turns for the life of the pond. No date rule and no
    /// write gate touches that failure; only this line does.
    #[test]
    fn the_prompt_says_that_most_windows_hold_nothing() {
        for allow in [true, false] {
            let prompt = render_extraction_prompt("Jerry", "Goose", 3, allow);
            assert!(prompt.contains("Most conversations hold nothing worth"));
            // And the empty answer is shown in the same breath, in the shape
            // the parser wants for this window.
            assert!(prompt.contains("answer {\"memories\":[]"));
        }
    }

    /// Every kind the parser accepts is named in the prompt, and nothing else
    /// is.
    ///
    /// A catalogue the model is not shown is a catalogue it cannot choose from;
    /// a label in the prompt the parser rejects is a memory the pond throws
    /// away for a wording it asked for.
    #[test]
    fn the_prompt_and_the_parser_agree_on_the_catalogue() {
        let prompt = rendered();
        for kind in MemoryKind::ALL {
            assert!(
                prompt.contains(kind.as_str()),
                "{} is accepted by the parser and never shown to the model",
                kind.as_str()
            );
        }
        for gone in ["identity", "project", "knowledge"] {
            assert!(
                !prompt.contains(gone),
                "{gone:?} is in the prompt but the parser rejects it"
            );
        }
    }

    /// A stale window is not shown the reminders half at all.
    #[test]
    fn a_stale_window_is_never_asked_for_a_reminder() {
        let prompt = render_extraction_prompt("Jerry", "Goose", 3, false);
        assert!(!prompt.contains("reminders"));
        assert!(!prompt.contains("\"when\""));
        assert!(prompt.contains("{\"memories\":[]}"));
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
        let out = parse_window_response(raw, 3, true).unwrap();
        assert_eq!(out.memories.len(), 1);
        assert_eq!(out.memories[0].kind, MemoryKind::Routine);
        assert_eq!(out.reminders.len(), 1);
        assert_eq!(out.reminders[0].when_said, "next Tuesday");
    }

    /// The measured salvages, kept because they were measured.
    #[test]
    fn a_preamble_and_a_thinking_block_are_salvaged() {
        let raw = "<think>hmm, what did they say</think>Here you go:\n\
                   {\"memories\":[{\"note\":\"Jerry's sister is Amara.\",\"kind\":\"relationship\"}]}\n\
                   Hope that helps!";
        let out = parse_window_response(raw, 3, true).unwrap();
        assert_eq!(out.memories.len(), 1);
    }

    /// A model answering the shape of the old prompt still gets read.
    /// A model answering the shape of the old prompt still gets read.
    ///
    /// The subtle half: a bare array IS valid JSON, so it parses on the first
    /// attempt and lands as an array `Value`. Treating the array-recovery as a
    /// later salvage branch means it is never reached, and every array reply
    /// comes back as an `Ok` with no memories in it -- which advances the
    /// cursor past a window that was perfectly readable.
    #[test]
    fn a_bare_array_is_read_as_the_memories_list() {
        let raw = r#"[{"note":"Jerry prefers short answers.","kind":"preference"}]"#;
        let out = parse_window_response(raw, 3, true).unwrap();
        assert_eq!(out.memories.len(), 1);
        assert_eq!(out.memories[0].kind, MemoryKind::Preference);

        // With a preamble in front of it, which is the form that actually
        // arrives from a local model.
        let with_preamble = format!("Here is what I found:\n{raw}");
        assert_eq!(
            parse_window_response(&with_preamble, 3, true)
                .unwrap()
                .memories
                .len(),
            1
        );
    }

    /// An object answering a schema nobody asked for is a parse failure, not an
    /// empty answer.
    ///
    /// `{"facts":[...]}` is the OLD prompt's shape, and the pond will be
    /// running both prompts against the same model for the length of the
    /// cutover. Reading it as "nothing worth keeping" would move the watermark
    /// past every window in the store, once, and never come back.
    #[test]
    fn an_object_with_neither_key_is_a_parse_failure() {
        let raw =
            r#"{"facts":[{"content":"Jerry prefers short answers.","segment":"preference"}]}"#;
        assert!(matches!(
            parse_window_response(raw, 3, true),
            Err(ExtractionError::Unparseable { .. })
        ));

        // Vacuity control: the same object under the right key parses, so the
        // refusal above is about the SCHEMA and not about the parser having
        // stopped working.
        let right = r#"{"memories":[{"note":"Jerry prefers short answers.","kind":"preference"}]}"#;
        assert_eq!(
            parse_window_response(right, 3, true)
                .unwrap()
                .memories
                .len(),
            1
        );
    }

    /// The type change this port exists for.
    ///
    /// An unreadable reply and an empty one must not be the same value. In the
    /// per-turn path both are `Ok(vec![])` and the cost is one turn's facts; in
    /// a batch design the cursor advances past a window nobody read and the
    /// conversation is never revisited.
    #[test]
    fn an_unreadable_reply_is_not_an_empty_one() {
        let empty = parse_window_response(r#"{"memories":[],"reminders":[]}"#, 3, true).unwrap();
        assert!(empty.is_empty());

        let err = parse_window_response("Sure! I had a look and nothing stood out.", 3, true)
            .expect_err("no JSON is recoverable here");
        assert!(matches!(err, ExtractionError::Unparseable { .. }));
    }

    /// The raw head in the error is bounded.
    ///
    /// It reaches a log file, and an unbounded model reply in a log is a
    /// household's conversation written somewhere with none of the store's
    /// scoping, retention or redaction.
    #[test]
    fn the_unparseable_error_carries_a_bounded_excerpt() {
        let long = "not json ".repeat(400);
        let err = parse_window_response(&long, 3, true).expect_err("unparseable");
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
        let out = parse_window_response(raw, 3, true).unwrap();
        assert_eq!(out.memories.len(), 1);
        assert_eq!(out.rejected, 1);
    }

    /// A reminder from a window that was never asked for one is discarded.
    #[test]
    fn reminders_from_a_stale_window_are_discarded() {
        let raw = r#"{"memories":[],"reminders":[{"about":"the dentist","when":"next Tuesday"}]}"#;
        let out = parse_window_response(raw, 3, false).unwrap();
        assert!(out.reminders.is_empty());

        // Vacuity control: the same reply DOES produce a reminder when the
        // window was asked for one.
        assert_eq!(
            parse_window_response(raw, 3, true).unwrap().reminders.len(),
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
            parse_window_response(raw, 2, true).unwrap().memories.len(),
            2
        );
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

    /// The known block is dropped row by row to fit the budget.
    ///
    /// `EXTRACTION_PROMPT_BUDGET_TOKENS` had exactly one reader before this:
    /// its own test, asserting the SYSTEM half. Nothing measured the assembled
    /// prompt, and the two components that grow with the conversation were the
    /// two nobody bounded. This is the last place anything can be dropped
    /// before the call, and what gets dropped is never the conversation -- that
    /// is what the call exists to read.
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
        // Whole rows only. A truncated remembered sentence is one a model at
        // this size finishes in its own words.
        assert!(rendered.contains("thing 0"));
        assert!(!rendered.contains("thing 7"));
        assert!(
            rendered.contains("the starter lives in the pantry"),
            "the conversation is never what gets dropped to fit"
        );

        // Vacuity control: with the real budget every row is shown, so the
        // drops above are about the BUDGET and not about the renderer having
        // stopped rendering.
        let full = render_window_message(&window, EXTRACTION_PROMPT_BUDGET_TOKENS);
        assert!(full.contains("thing 7"));
    }

    /// A budget that leaves no room for the block leaves no header either.
    ///
    /// A header with nothing under it is an invitation to a small model to fill
    /// it in -- the same failure `an_empty_store_renders_no_already_remembered_header`
    /// guards against, arriving by a different route.
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

    /// An empty store does not produce a header with nothing under it.
    ///
    /// "Already remembered about Jerry:" followed by a blank is an invitation
    /// to a small model to fill it in.
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
