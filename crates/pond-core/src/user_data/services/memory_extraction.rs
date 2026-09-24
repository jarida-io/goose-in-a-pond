//! The batch memory-extraction engine: one window of one conversation per
//! lane slot, in the pond's idle time.
//!
//! # The change of unit, which is the whole design
//!
//! What stood here was a per-TURN service: after every single exchange it asked
//! the model "given this turn, what facts are in it?", on the same single-slot
//! local model that was serving chat, paying a fresh prefill each time. That
//! question cannot answer the third of what a household wants remembered --
//! "is this a one-off, or a habit, a standing way they do things?" -- because a
//! habit is not visible inside one exchange. It also paid for twenty prefills
//! per twenty messages where this engine pays one.
//!
//! The per-turn path is gone. Every turn any surface persists is now read here,
//! which closes most of a gap nobody was counting: `/chat` and the voice loop
//! persisted their turns and never extracted from them at all, so whole
//! conversations held there contributed nothing to memory.
//!
//! Most, and not all. On a pond where more than one person lives, a
//! conversation nothing has identified is deliberately never mined -- and the
//! voice child cannot be identified at all. It is spawned as its own process
//! with no HTTP request behind it, so none of the three things that bind a
//! session to a member reach it: a paired device's bearer token, a face match,
//! or the member picking themselves. Everything spoken to such a pond is
//! therefore skipped, indefinitely, and the fix is upstream of this engine --
//! something on the voice path has to learn who is speaking. What this engine
//! owes in the meantime is to count them all and say so loudly, which is
//! [`PassReport::sessions_unnameable`] and the `unattributed_sessions` field of
//! the status endpoint. Attributing one member's speech to another would be
//! worse than not remembering it.
//!
//! # This is the write gate
//!
//! Whatever the extractor adapter hands over, nothing reaches the store without
//! passing [`fact_defect`], the date stripper, the subject gate and the dedup
//! pass. Adapters may prompt their model as well as they like, but they are not
//! trusted to be the only enforcement -- a 4B model reliably ignores part of
//! any instruction it is given, and the measured consequences are in the live
//! store: five William Ruto biography facts filed as the household's identity,
//! and the old prompt's own worked example stored as a fact about a family that
//! does not exist.
//!
//! # What this phase does NOT do
//!
//! It does not reinforce. A candidate that matches something already stored is
//! DROPPED, exactly as the per-turn path dropped it -- and the drop is recorded
//! with its band and with the id it lost to, so the phase that builds on a
//! match instead of discarding it can be judged against a number rather than
//! against a story.
use crate::models::domain::message::Role;
use crate::models::ports::embedding::EmbeddingProvider;
use crate::shared::domain::session_activity::SessionOrigin;
use crate::user_data::domain::memory::{
    cosine_similarity, fact_defect, names_subject, normalise_fact_content, reminder_covers_note,
    FactDefect, MemoryEventKind, MemoryFragment, MemorySegment,
};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::proposal::PROPOSAL_SESSION_ID;
use crate::user_data::domain::reminder::{
    window_is_fresh_enough, CapturedReminder, ReminderCandidate,
};
use crate::user_data::domain::session::{Session, SessionIdentity};
use crate::user_data::ports::conversation_extractor::{
    ConversationExtractor, ExtractionError, ExtractionWindow, KnownMemory, WindowExtraction,
    WindowMessage, WindowSubject,
};
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::ports::reminder_repository::ReminderRepository;
use crate::user_data::ports::session_storage::SessionStorage;
use crate::user_data::services::memory_relevance::{
    is_duplicate_content, rank_by_relevance, DEDUP_RECENT_WINDOW,
};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Characters in one window, across all its messages.
///
/// The second of the three bounds, and usually the binding one: twenty messages
/// of a real conversation overrun the prompt budget long before they overrun
/// the message count. Trimmed from the OLDEST end, so the newest exchange in a
/// window is never the one that gets dropped to make it fit.
pub const EXTRACTION_WINDOW_CHARS: usize = 6_000;

/// Fewest new messages before a conversation is worth a window.
///
/// Two, because one is never a complete exchange -- a lone user message has no
/// reply yet, and a lone assistant message is the pond talking to itself.
pub const MIN_NEW_MESSAGES: u64 = 2;

/// Consecutive unparseable replies against one watermark before the walk moves
/// past that window.
///
/// The give-up rung. Without it a model that cannot emit the schema re-reads
/// one window forever and the backlog never moves; with it, three failures cost
/// one window and are recorded as having cost it.
pub const MAX_PARSE_ATTEMPTS: u32 = 3;

/// Known memories shown to the model, so new evidence can build on what is
/// already there rather than restate it.
pub const KNOWN_MEMORIES_SHOWN: usize = 8;

/// Characters the "already remembered" block may spend, across all its rows.
///
/// The count bound above is not a size bound: eight rows of arbitrary-length
/// `content` is an unbounded block, and the budget it shares with the window is
/// not. Rows are added most-relevant-first until this is spent and the rest are
/// dropped whole -- never truncated, because half a remembered sentence shown to
/// a model at this size is a sentence it will finish in its own words.
///
/// ~200 tokens, which is the share the budget arithmetic gives this block.
pub const KNOWN_MEMORIES_CHARS: usize = 800;

/// The wall clock one pass may spend, however its windows turn out.
///
/// The second half of the bound in [`BatchExtractionConfig::model_call_budget`],
/// and the one that survives a model whose calls are individually slow rather
/// than individually many. Five minutes: a pass of three windows is budgeted at
/// 25-45 s of inference on the Orin, so this is roughly six times the intended
/// cost and can only be reached by a provider that is in trouble.
///
/// It is checked BETWEEN windows, so one in-flight call can still overrun it.
/// Cancelling a call the provider is already running is not something this port
/// can express, and claiming the deadline is hard would be asserting a
/// guarantee the pond does not have.
pub const EXTRACTION_PASS_MAX_SECS: u64 = 300;

/// How many neighbours to score a candidate against when banding it.
pub const BAND_NEIGHBOURS: usize = 5;

/// How long an embed failure stands the engine down.
///
/// Banding without an embedder is not wrong, it is BLIND: every candidate comes
/// back unscored and the histogram the whole shadow phase exists to produce is
/// empty. Better to stop and say so than to spend the inference slot producing
/// nothing measurable.
pub const EMBED_FAILURE_COOLDOWN_SECS: u64 = 300;

/// What the engine is allowed to do.
///
/// Read from `settings.memory_extraction_mode`, and an unrecognised value reads
/// as [`Shadow`](ExtractionMode::Shadow). That is the narrowing direction and
/// the reason this is parsed rather than matched inline: a typo in a settings
/// row must not be able to start writing to the household's memory store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionMode {
    /// Read, band, log. Write no memory. This phase.
    Shadow,
    /// Write new memories; dedup drops a match exactly as it does today.
    Write,
    /// Write, and build on a match rather than dropping it.
    Reinforce,
}

impl ExtractionMode {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "write" => ExtractionMode::Write,
            "reinforce" => ExtractionMode::Reinforce,
            _ => ExtractionMode::Shadow,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ExtractionMode::Shadow => "shadow",
            ExtractionMode::Write => "write",
            ExtractionMode::Reinforce => "reinforce",
        }
    }

    /// Whether this mode may write to the memory store at all.
    pub fn writes(self) -> bool {
        !matches!(self, ExtractionMode::Shadow)
    }
}

/// Where a conversation's walk currently stands.
///
/// Spelled out as a type because the `Ok(None)` case -- the anchor message has
/// been deleted -- is the one the design originally had no answer for, and an
/// unnamed third state is how a walk ends up either frozen on a dead watermark
/// or silently advancing past a conversation nobody read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorState {
    /// Never examined. Walk from the first message.
    Unstarted { total: u64 },
    /// Examined up to a message that still exists, with this many after it.
    InProgress { remaining: u64, total: u64 },
    /// The watermark names a message that is gone -- a truncated history, an
    /// edited turn. Clear the cursor and re-walk from the start.
    Reset,
}

impl CursorState {
    /// How many messages are still unread, and where they start.
    ///
    /// `None` for [`Reset`](CursorState::Reset), which has no offset to give:
    /// the caller clears the cursor and comes back next pass rather than
    /// guessing.
    pub fn unread(self) -> Option<(u64, usize)> {
        match self {
            CursorState::Unstarted { total } => Some((total, 0)),
            CursorState::InProgress { remaining, total } => {
                Some((remaining, total.saturating_sub(remaining) as usize))
            }
            CursorState::Reset => None,
        }
    }
}

/// A conversation the pass could examine, projected onto what ordering needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCandidate {
    pub id: String,
    pub updated_at: DateTime<Utc>,
    /// When the walk last looked at it; `None` means never.
    pub extracted_at: Option<DateTime<Utc>>,
}

/// One conversation a pass has decided to read, and who it is about.
///
/// The pair travels together because they are decided together: the subject is
/// resolved in the same loop that reads the cursor, before the ordering, and
/// carrying it as a parameter is what stops the window from re-resolving it and
/// reaching a different answer.
struct WindowTarget<'a> {
    session_id: &'a str,
    subject: WindowSubject,
}

/// Whether a conversation is one a person had.
///
/// The pond opens sessions for its own background work -- a cron line firing at
/// 3am mints one, and the proactive reviewer owns a permanent one. Mining those
/// for "what is worth remembering about the user" reads the pond's own output
/// back to itself as though a person had said it, which is the mechanism that
/// put the assistant's self-description into the live store as the user's
/// identity.
pub fn is_eligible_session(session_id: &str) -> bool {
    SessionOrigin::of(session_id).is_human() && session_id != PROPOSAL_SESSION_ID
}

/// Order the conversations a pass will consider.
///
/// Slot one goes to the most recently active eligible conversation, so today's
/// chat never waits behind a backlog of three hundred. Every slot after it
/// drains the backlog oldest-first, with never-examined conversations at the
/// very front -- `(extracted_at IS NOT NULL, extracted_at ASC, updated_at
/// DESC)`.
///
/// The split matters because the two jobs pull in opposite directions. Pure
/// recency never finishes the history; pure backlog order means a conversation
/// somebody is having right now is read for the first time in a fortnight.
pub fn order_pass(mut candidates: Vec<SessionCandidate>) -> Vec<SessionCandidate> {
    candidates.retain(|c| is_eligible_session(&c.id));
    if candidates.is_empty() {
        return candidates;
    }

    // Most recently active first, ties broken by id so a pass is reproducible.
    let newest = candidates
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            a.updated_at
                .cmp(&b.updated_at)
                .then_with(|| b.id.cmp(&a.id))
        })
        .map(|(i, _)| i)
        .expect("non-empty");
    let first = candidates.remove(newest);

    candidates.sort_by(|a, b| {
        a.extracted_at
            .is_some()
            .cmp(&b.extracted_at.is_some())
            .then_with(|| a.extracted_at.cmp(&b.extracted_at))
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut ordered = vec![first];
    ordered.append(&mut candidates);
    ordered
}

/// What cutting a window out of a page of messages came to.
///
/// Three outcomes rather than `Option`, because the two failures call for
/// opposite things from the caller and an `Option` collapsed them: a page with
/// no reply in it yet must be left alone until the reply arrives, and a page
/// whose newest exchange cannot fit the prompt budget must be stepped PAST or
/// the walk stalls on it for the life of the pond.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowCarve<'a> {
    /// Read these.
    Ready(&'a [WindowMessage]),
    /// No complete exchange here yet -- a run of user messages nobody has
    /// answered, or a stretch that was all system plumbing.
    NoExchange,
    /// The newest complete exchange ALONE overruns the character budget.
    ///
    /// Carries the cost and the last message of that exchange, so the caller
    /// can say what was skipped and advance exactly past it.
    Oversized { chars: usize, through: &'a str },
}

/// Cut a window out of the messages that follow the cursor.
///
/// Three bounds, and the smallest wins: the message count, the character
/// budget, and never ending mid-exchange. The third is what the other two are
/// in service of -- a window that ends on a user's question, with the answer in
/// the next window, asks the model to read half a conversation and then moves
/// the watermark past the half it did not show.
///
/// # The budget is enforced, and that is a change
///
/// This used to fall back to the FULL untrimmed page when no complete pair fit
/// the budget, on the reasoning that "the prompt builder clamps it". No prompt
/// builder clamped anything: a 40 KB pasted log went to a provider with an 8192
/// prompt ceiling, which either truncates from the front -- losing the system
/// prompt's rules and the schema, so the reply is unparseable and takes the
/// give-up path -- or fails outright. Skipping that exchange and saying so is a
/// loss the household can see; sending it was a loss dressed as a cost.
pub fn carve_window(messages: &[WindowMessage], max_chars: usize) -> WindowCarve<'_> {
    // Trim to the character budget first, from the OLDEST end, so a long window
    // keeps its most recent exchange rather than its oldest.
    let mut used = 0usize;
    let mut start = messages.len();
    for (i, m) in messages.iter().enumerate().rev() {
        let cost = m.content.chars().count();
        if used + cost > max_chars && start < messages.len() {
            break;
        }
        used += cost;
        start = i;
    }

    if let Some(carved) = last_complete_pair(&messages[start..]) {
        return WindowCarve::Ready(carved);
    }

    // Nothing fit. Which of the two failures it is decides what the caller does,
    // so it is answered here rather than guessed there: if the page HAS a
    // complete exchange and it simply does not fit, that is an oversize.
    match last_complete_pair(messages) {
        Some(whole) => WindowCarve::Oversized {
            chars: whole.iter().map(|m| m.content.chars().count()).sum(),
            through: whole
                .last()
                .map(|m| m.id.as_str())
                .expect("last_complete_pair never returns an empty slice"),
        },
        None => WindowCarve::NoExchange,
    }
}

/// The prefix ending at the last assistant reply that has a user message ahead
/// of it, or `None` when there is no such point.
fn last_complete_pair(slice: &[WindowMessage]) -> Option<&[WindowMessage]> {
    let mut seen_user = false;
    let mut end = None;
    for (i, m) in slice.iter().enumerate() {
        if m.is_user() {
            seen_user = true;
        } else if seen_user {
            end = Some(i + 1);
        }
    }
    end.map(|e| &slice[..e])
}

/// The shape of the shadow pass's whole output: how close each candidate was to
/// something the store already holds.
///
/// Measured at OFFER time -- candidate against the store as it stands when the
/// candidate is produced -- and not over pairs of stored rows. Over stored rows
/// the histogram reads "everything is new", because dedup has already dropped
/// exactly the pairs the bands are about. The loss is the measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BandHistogram {
    /// At or above the reinforce threshold, or a lexical duplicate.
    pub same: usize,
    /// Between the relate and reinforce thresholds.
    pub related: usize,
    /// Below the relate threshold.
    pub fresh: usize,
    /// Could not be scored: no embedder, an embed failure, or a store with no
    /// comparable vectors. Counted rather than folded into `fresh`, because a
    /// candidate nobody could score is not evidence that it is new.
    pub unscored: usize,
}

/// How close one candidate came to something the store already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// At or above the reinforce threshold, or a lexical duplicate.
    Same,
    /// Between the relate and reinforce thresholds.
    Related,
    /// Below the relate threshold: nothing in the store is about this.
    Fresh,
    /// Nobody could score it.
    Unscored,
}

impl Band {
    pub fn as_str(self) -> &'static str {
        match self {
            Band::Same => "same",
            Band::Related => "related",
            Band::Fresh => "new",
            Band::Unscored => "unscored",
        }
    }
}

impl BandHistogram {
    pub fn total(&self) -> usize {
        self.same + self.related + self.fresh + self.unscored
    }

    fn count(&mut self, band: Band) {
        match band {
            Band::Same => self.same += 1,
            Band::Related => self.related += 1,
            Band::Fresh => self.fresh += 1,
            Band::Unscored => self.unscored += 1,
        }
    }

    fn add(&mut self, other: BandHistogram) {
        self.same += other.same;
        self.related += other.related;
        self.fresh += other.fresh;
        self.unscored += other.unscored;
    }
}

/// What one window's candidates came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WindowOutcome {
    bands: BandHistogram,
    written: usize,
    refused: usize,
    /// Of `refused`, the ones refused for carrying a calendar date.
    dated: usize,
    /// Of `dated`, the ones no stored reminder could be matched to.
    ///
    /// Per NOTE, not per window: this said "whose window filed no reminder at
    /// all" while the window was the unit, and one reminder then marked every
    /// dated note beside it as kept. A window with two dated notes and one
    /// reminder reported nothing lost and discarded the other date in silence.
    /// Each note is now asked separately, by [`reminder_covers_note`].
    dates_lost: usize,
    demoted: usize,
    reminders: usize,
    /// Of `reminders`, the ones that became a new row.
    reminders_stored: usize,
    /// Of `reminders`, the ones whose row this window had already written. Not
    /// a loss: the date is in the store, put there by the earlier walk.
    reminders_deduped: usize,
    /// Of `reminders`, the ones that reached no store at all -- the write
    /// failed, or nothing was wired to write to. This is the date being lost in
    /// the one way the counter alone could never show.
    reminders_lost: usize,
    /// The `about` text of every reminder this window actually got into the
    /// store, stored or already-there.
    ///
    /// Kept as text rather than as a count because `dates_lost` is a question
    /// about ONE note -- is THIS date anywhere -- and a count can only answer
    /// it for the window. See [`reminder_covers_note`].
    reminders_kept_about: Vec<String>,
    /// For each candidate that was dropped, the id of the stored row it lost
    /// to -- or `lexical` when the lexical rule fired and no vector was
    /// involved. This is the record of what the build-on phase has to recover.
    dropped_onto: Vec<String>,
}

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Windows read by the model and banded.
    pub windows_examined: usize,
    /// Model calls this pass made, whatever they came to.
    ///
    /// The number `sessions_per_pass` actually bounds. `windows_examined`
    /// counts SUCCESSES -- it is incremented after the cursor advance -- so
    /// bounding the pass by it meant every failure path was free: a model that
    /// could not emit the schema, which is the exact case `MAX_PARSE_ATTEMPTS`
    /// exists for, walked the entire store at one call per conversation.
    /// Measured at 40 calls where 3 were configured. This is the counter the
    /// loop stops on.
    pub model_calls: usize,
    /// Conversations considered and passed over -- nothing new, no complete
    /// exchange, or out of attempts.
    pub windows_skipped: usize,
    /// Watermarks that named a deleted message and were cleared.
    pub cursor_resets: usize,
    /// Replies with no recoverable JSON.
    pub parse_failures: usize,
    /// Windows the walk moved past after `MAX_PARSE_ATTEMPTS` failures.
    pub gave_up: usize,
    pub memories_offered: usize,
    pub reminders_offered: usize,
    /// Items carrying a label outside the five-value catalogue.
    pub rejected_kinds: usize,
    pub bands: BandHistogram,
    /// Memories actually written. Zero in shadow mode, asserted rather than
    /// assumed -- this field exists so "wrote nothing" is a number a test can
    /// read rather than a claim about a code path.
    pub memories_written: usize,
    /// Candidates the write gate refused, for any defect.
    pub memories_refused: usize,
    /// Of those, the ones refused for carrying a calendar date.
    ///
    /// The cost of the date rule, stated as a number rather than assumed to be
    /// small. Nothing edits a dated note any more, so this is the whole of what
    /// the rule throws away, and it is expected to be rare: 19 of 85 notes on
    /// the shipped model, 9 of 91 on the larger one, and zero across 207
    /// windows of preferences, relationships, corrections, context, plain
    /// numbers and small talk.
    pub memories_dated: usize,
    /// Of those, the ones no stored reminder could be matched to.
    ///
    /// Counted per NOTE, not per window: each refused note is asked separately
    /// whether a reminder this window got into the store is about it, by the
    /// shared-content-word rule in [`reminder_covers_note`]. Where that rule
    /// cannot match, the note counts here -- so this number over-reports loss
    /// rather than under-reporting it.
    ///
    /// The date is gone from the pond entirely for a note that is genuinely
    /// here: it was refused and nothing was filed. That is usually the model
    /// answering half of a prompt that asks for both halves -- the shipped model
    /// filed a reminder on 31 of its 36 dated windows and lost no date at all;
    /// one of the others filed none on any of 36. A pond whose model behaves
    /// like the second one should be told, not quietly served.
    pub memories_dates_lost: usize,
    /// Candidates filed as `Knowledge` because they never named the window's
    /// subject, whatever label the model put on them.
    pub memories_demoted: usize,
    /// Candidates dropped because the store already holds something close
    /// enough. This is the loss the build-on path exists to recover, counted so
    /// that recovery can be judged against a number.
    pub memories_dropped: usize,
    /// Dated items lifted out as reminder candidates, whatever became of them.
    ///
    /// How the pass reports its own reading of the window. It is deliberately
    /// NOT evidence that anything was kept -- that is `reminders_written` and
    /// `reminders_lost` below, and the three used to be one number that said
    /// "captured" while the row was dropped on the floor.
    pub reminders_captured: usize,
    /// Reminder rows this pass actually wrote.
    ///
    /// Of `reminders_captured`, the ones that reached the `reminders` table as
    /// a new row. A re-walk that recognised its own earlier row counts in
    /// neither this nor `reminders_lost`: nothing was written and nothing was
    /// lost.
    pub reminders_written: usize,
    /// Reminder candidates that reached no store at all.
    ///
    /// The write failed, or no reminder store was wired into this process.
    /// Surfaced rather than swallowed for the same reason `provider_failures`
    /// is: the household is owed the difference between a pond that is keeping
    /// its dates and one that is dropping every one of them, and the failure is
    /// silent from every other angle -- the memory was refused, the counter went
    /// up, and nothing anywhere holds the date.
    pub reminders_lost: usize,
    /// Eligible CONVERSATIONS nobody could say whose they were.
    ///
    /// The containment for the multi-member case, and it is a loss: on a pond
    /// with several members, a conversation nothing has identified is never
    /// mined at all rather than mined under one global name.
    ///
    /// Counted over every eligible conversation in the store, not over the
    /// windows a pass reached. Skipping one is free, so the window loop walked
    /// past however many of them the model call budget left room for and
    /// stopped -- which made this a sample of a prefix. The pond it matters
    /// most on is the one where it read lowest: a household whose typed chats
    /// are identified and whose spoken ones are not spends its whole budget on
    /// the typed ones and reports that nothing is being skipped.
    pub sessions_unnameable: usize,
    /// Windows stepped past because their newest exchange alone overruns the
    /// prompt budget.
    ///
    /// A real loss, and the honest disposition of one: a 40 KB pasted log
    /// cannot be read within a budget the provider enforces, so it is skipped
    /// and counted rather than sent and silently truncated.
    pub windows_oversized: usize,
    /// Windows skipped because this exact stretch has already been mined.
    ///
    /// The idempotence guard, read rather than merely written. A watermark
    /// naming a deleted message resets a conversation and re-walks it from the
    /// first message; without this, every window of a 200-message chat is read
    /// again, at one model call each, for memories the store already holds.
    pub windows_already_mined: usize,
    /// Windows where the provider was called and failed.
    pub provider_failures: usize,
    /// Whether any window found no provider at all.
    pub no_provider: bool,
    /// Whether the pass stopped because its wall clock ran out.
    pub deadline_reached: bool,
    /// Why the pass could do nothing, when it could do nothing.
    ///
    /// Decided ONCE, at the end of the pass, from what the whole pass came to.
    /// It used to be a plain assignment inside the per-window loop and was
    /// never cleared, so one transient provider timeout in window 1 told the
    /// household extraction was stopped -- in the same pass that wrote four
    /// memories through windows 2 and 3. A banner exists to say a true thing
    /// about the pond; this one described the worst moment of a pass rather
    /// than its outcome.
    pub blocked_on: Option<String>,
}

/// What the engine has done lately, for `GET /api/v1/memories/extraction-status`.
///
/// Process state rather than a database read, because the two questions a
/// person asks of a background job -- "is it running?" and "why is it not?" --
/// are both about this process. A restart resets it, which is honest: a pass
/// that ran before the restart is not evidence about the engine running now.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ExtractionEngineStatus {
    /// What the engine is allowed to do, as the last pass read it.
    pub mode: String,
    /// When the last pass finished, whether or not it examined anything.
    pub last_pass_at: Option<DateTime<Utc>>,
    /// Windows that pass read.
    pub last_pass_windows: usize,
    /// Memories the last pass wrote.
    pub last_pass_written: usize,
    /// Memories the last pass refused for carrying a date, and how many of
    /// those left no reminder behind.
    ///
    /// The cost of the date rule, on the surface rather than in a log file.
    /// Nothing edits a note any more, so a dated note is refused whole -- which
    /// is affordable exactly as long as the model files the date as a reminder
    /// instead. `last_pass_dates_lost` counts the refused NOTES that no stored
    /// reminder could be matched to: that date is gone from the pond entirely,
    /// and it is usually a property of the MODEL rather than of the pond. A
    /// household running a model that answers half the schema should be able to
    /// see that without reading a trace.
    pub last_pass_dated: usize,
    pub last_pass_dates_lost: usize,
    /// Reminder rows the last pass wrote, and candidates it could not store.
    ///
    /// `last_pass_reminders_lost` above zero is a property of the POND, not of
    /// the model: the model did what it was asked and the store would not take
    /// it. It is the failure mode this surface exists for, because every other
    /// symptom of it looks like success -- the note is refused, the reminder is
    /// counted, and the date is gone.
    pub last_pass_reminders_written: usize,
    pub last_pass_reminders_lost: usize,
    /// Conversations in the store that nobody can say whose they are, as the
    /// last pass counted them.
    ///
    /// Reported separately from `blocked_on`, which only speaks when the whole
    /// pass did nothing. A pond can be extracting happily and still be quietly
    /// skipping one member's chats forever, and that is a thing a household
    /// should be able to see without reading a log file.
    ///
    /// It is a total over every eligible conversation, not a count of what one
    /// pass looked at, because the surface that produces most of them cannot be
    /// fixed from here: the voice child runs as its own process with no HTTP
    /// request, so none of the three things that identify a session -- a paired
    /// device's token, a face match, a member picking themselves -- ever
    /// reaches it. Until one of them does, this number is what the household is
    /// owed.
    pub unattributed_sessions: usize,
    /// Why the last pass could do nothing, when it could do nothing.
    ///
    /// This is a surface, not a field nobody reads: a pond whose embedder never
    /// loaded looks identical from the outside to one with nothing left to
    /// extract, and the difference is months of history.
    pub blocked_on: Option<String>,
}

impl ExtractionEngineStatus {
    /// Fold a finished pass into the record.
    pub fn record(&mut self, mode: ExtractionMode, report: &PassReport) {
        self.mode = mode.as_str().to_string();
        self.last_pass_at = Some(Utc::now());
        self.last_pass_windows = report.windows_examined;
        self.last_pass_written = report.memories_written;
        self.last_pass_dated = report.memories_dated;
        self.last_pass_dates_lost = report.memories_dates_lost;
        self.last_pass_reminders_written = report.reminders_written;
        self.last_pass_reminders_lost = report.reminders_lost;
        self.unattributed_sessions = report.sessions_unnameable;
        self.blocked_on = report.blocked_on.clone();
    }
}

/// Whether the embedder is worth asking.
///
/// The lane's enable predicate, and deliberately NOT
/// `search_unembedded(1).is_empty()`. That check is a latch that can never
/// re-open inside a process: three ordinary paths mint rows with no vector, and
/// the only thing that fills them is a one-shot startup backfill. The design's
/// own restate path would have disabled the engine permanently.
///
/// Health instead of emptiness: an embedder that is wired and has not failed
/// recently. Rows without vectors no longer stand the engine down; they only
/// mean those particular rows cannot match semantically this pass.
#[derive(Debug, Default)]
pub struct EmbedderHealth {
    /// When the embedder last failed, as seconds since this process started.
    /// A `Mutex<Option<Instant>>` so the lane's predicate can read it without
    /// an await -- the lane loop asks before it takes the slot.
    last_failure: std::sync::Mutex<Option<std::time::Instant>>,
}

impl EmbedderHealth {
    pub fn record_failure(&self) {
        if let Ok(mut slot) = self.last_failure.lock() {
            *slot = Some(std::time::Instant::now());
        }
    }

    pub fn record_success(&self) {
        if let Ok(mut slot) = self.last_failure.lock() {
            *slot = None;
        }
    }

    /// True when no failure is recent enough to stand the engine down.
    pub fn is_stale(&self) -> bool {
        match self.last_failure.lock() {
            Ok(slot) => slot.is_none_or(|at| {
                at.elapsed() >= std::time::Duration::from_secs(EMBED_FAILURE_COOLDOWN_SECS)
            }),
            // A poisoned lock means a writer panicked. Reporting "healthy" here
            // would let the engine spend the slot producing an empty histogram;
            // reporting unhealthy costs one cooldown and nothing else.
            Err(_) => false,
        }
    }
}

/// Everything the pass reads from settings, resolved once per pass.
#[derive(Debug, Clone)]
pub struct BatchExtractionConfig {
    pub mode: ExtractionMode,
    pub sessions_per_pass: usize,
    pub window_messages: usize,
    pub window_chars: usize,
    pub max_memories: usize,
    pub relate_threshold: f32,
    pub reinforce_threshold: f32,
    pub assistant_name: String,
    /// Who a window is about when the conversation itself does not say.
    ///
    /// The pond-wide fallback, from `settings.user_name`. A window whose
    /// session HAS an identity is about that member instead, and on a pond with
    /// several members a window with no identity falls back to nobody at all --
    /// see [`resolve_window_subject`].
    pub fallback_subject: WindowSubject,
    /// The household, as this pass sees it.
    pub roster: HouseholdRoster,
    /// Whether reminders are wanted at all. A window still has to be recent
    /// enough on top of this; see [`window_is_fresh_enough`].
    pub allow_reminders: bool,
    /// The wall clock one pass may spend, in seconds.
    ///
    /// A constant rather than a settings row, deliberately: it is a safety
    /// bound on the inference slot, not a dial a household has any way to
    /// choose, and a settings key that can be set to a day would put the
    /// failure it prevents back within reach of a typo. It is a field rather
    /// than a bare constant only so a test can spend it.
    pub max_pass_secs: u64,
}

impl BatchExtractionConfig {
    /// Read the dials off a settings snapshot.
    pub fn from_settings(settings: &crate::user_data::domain::settings::Settings) -> Self {
        // `user_name` ships as "Friend", a placeholder rather than a name.
        // Writing it into permanent storage would produce memories about
        // somebody who does not exist, so an unconfigured pond says literally
        // "the user" -- which is also the wording the write gate already
        // accepts.
        let fallback_subject = match settings.user_name.trim() {
            "" | "Friend" => WindowSubject::anonymous(),
            name => WindowSubject::named(name),
        };
        Self {
            mode: ExtractionMode::parse(&settings.memory_extraction_mode),
            sessions_per_pass: settings.memory_extraction_sessions_per_pass.max(1) as usize,
            window_messages: settings.memory_extraction_window_messages.max(2) as usize,
            window_chars: EXTRACTION_WINDOW_CHARS,
            max_memories: settings.memory_extraction_max_facts.max(1) as usize,
            relate_threshold: settings.memory_relate_threshold,
            reinforce_threshold: settings.memory_reinforce_threshold,
            assistant_name: settings.assistant_name.clone(),
            fallback_subject,
            // Empty until the caller says otherwise. A settings row cannot
            // know who lives here, and defaulting to "assume one member" would
            // make a multi-member pond silently stamp one name on everybody --
            // which is the failure this whole resolution exists to prevent.
            roster: HouseholdRoster::default(),
            allow_reminders: settings.memory_date_proposals_enabled,
            max_pass_secs: EXTRACTION_PASS_MAX_SECS,
        }
    }

    /// The hard ceiling on MODEL CALLS in one pass.
    ///
    /// The same number as `sessions_per_pass`, which is what that dial was
    /// always meant to mean: a window costs at most one call, so a pass costs
    /// at most this many calls whether or not any of them produce anything. The
    /// dial bounded successes before, and every failure path -- unparseable,
    /// provider error, no provider, a failed cursor advance -- returned without
    /// counting, so a pond of 300 conversations facing a model that could not
    /// emit the schema spent 50-75 minutes holding the single inference slot
    /// and writing nothing.
    ///
    /// A pass that spends its budget on failures therefore examines fewer
    /// windows than the dial says, and that is the intended reading: the dial
    /// buys inference, not outcomes.
    pub fn model_call_budget(&self) -> usize {
        self.sessions_per_pass
    }

    /// Tell the pass who lives here.
    ///
    /// Read once per pass from the profile store, rather than per window: the
    /// roster changes when somebody is added to the household, which is not
    /// something that happens between two windows of one pass.
    pub fn with_household(mut self, roster: HouseholdRoster) -> Self {
        self.roster = roster;
        self
    }
}

/// The household members a pass can name.
///
/// A projection of the profile store onto the two things resolution needs: how
/// many people live here, and what each of them is called. Held as data rather
/// than as a port so the resolution below is a pure function -- the rule about
/// whose memory a conversation becomes is exactly the kind of decision that
/// should be testable without a database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HouseholdRoster {
    members: Vec<(String, String)>,
}

impl HouseholdRoster {
    /// `(profile_id, display_name)` for every member.
    pub fn new(members: Vec<(String, String)>) -> Self {
        Self { members }
    }

    pub fn display_name(&self, profile_id: &str) -> Option<&str> {
        self.members
            .iter()
            .find(|(id, _)| id == profile_id)
            .map(|(_, name)| name.as_str())
    }

    /// Whether the pond has to tell people apart at all.
    ///
    /// One member is not a household in the sense that matters here: there is
    /// nobody an unattributed conversation could be confused WITH, so it is
    /// safe to fall back to the pond-wide name.
    pub fn has_several_members(&self) -> bool {
        self.members.len() > 1
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

/// Who a window is about, or the refusal to guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectResolution {
    /// Extract, about this person, under this name.
    Named(WindowSubject),
    /// Skip the window, do not advance the cursor, and count it.
    ///
    /// Several people live here and nothing has said which of them this
    /// conversation is. Mining it anyway would write one member's habits under
    /// another member's name, into a store that is injected into every later
    /// turn -- and unlike a missed window, that is not a loss the household can
    /// see or correct.
    Unnameable,
}

/// Decide whose conversation this is.
///
/// A batch job has no request, so it cannot reconstruct the per-request device
/// rung that `resolve_turn_scope` fuses at turn time. It reads back what the
/// chat path persisted instead: `SessionIdentity`, which the paired-device rung
/// now writes through `set_session_identity_if_stronger`.
///
/// The four cases, in the order they are tried:
///
/// | Session state | Name | Owner |
/// |---|---|---|
/// | identity names a member the roster knows | that display name | that member |
/// | one member or none, and `user_name` is configured | `user_name` | nobody |
/// | one member or none, and `user_name` is the shipped placeholder | `the user` | nobody |
/// | several members, session unattributed | -- | skipped |
///
/// The `Friend` placeholder never reaches the store: it is the shipped default
/// of `settings.user_name`, and writing memories about "Friend" would be the
/// pond inventing a person. `the user` is the wording the write gate has always
/// accepted and what all 379 existing rows are written in.
pub fn resolve_window_subject(
    identity: &SessionIdentity,
    roster: &HouseholdRoster,
    fallback: &WindowSubject,
) -> SubjectResolution {
    if let Some(profile_id) = identity.profile_id.as_deref() {
        if let Some(display) = roster.display_name(profile_id) {
            if !display.trim().is_empty() {
                return SubjectResolution::Named(WindowSubject::member(profile_id, display.trim()));
            }
        }
        // An identity naming a member the roster does not have is a deleted
        // profile or a stale row. It is evidence that somebody specific was
        // here, which is exactly what makes the pond-wide fallback wrong.
        return SubjectResolution::Unnameable;
    }
    if roster.has_several_members() {
        return SubjectResolution::Unnameable;
    }
    SubjectResolution::Named(fallback.clone())
}

/// The batch engine.
///
/// Holds the embedder and its health; everything else is passed per pass, so a
/// settings change takes effect on the next tick rather than at the next
/// restart.
pub struct BatchExtractionService {
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    /// Where a dated utterance goes once the date rule has refused it as a
    /// memory. `None` is a pond with nothing wired, and it is not treated as a
    /// quiet no-op: every candidate that finds it missing is counted as lost,
    /// because from the household's side that is exactly what it is.
    reminder_repository: Option<Arc<dyn ReminderRepository>>,
    health: EmbedderHealth,
}

impl Default for BatchExtractionService {
    fn default() -> Self {
        Self::new()
    }
}

impl BatchExtractionService {
    pub fn new() -> Self {
        Self {
            embedding_provider: None,
            reminder_repository: None,
            health: EmbedderHealth::default(),
        }
    }

    pub fn with_embedding_provider(mut self, provider: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    pub fn with_reminder_repository(mut self, repository: Arc<dyn ReminderRepository>) -> Self {
        self.reminder_repository = Some(repository);
        self
    }

    /// The lane's enable predicate: is there an embedder, and is it working?
    ///
    /// Asked before the slot is taken, so a pond with no embedder never holds
    /// the inference lane to produce an unscorable histogram.
    pub fn embedder_is_usable(&self) -> bool {
        self.embedding_provider.is_some() && self.health.is_stale()
    }

    /// Run one pass: up to `sessions_per_pass` windows, one per conversation.
    ///
    /// `cancel` is checked before each window rather than only between passes.
    /// A pass that loses the household mid-window then loses at most that one
    /// window's inference, instead of spending three windows' worth on work
    /// whose results are discarded.
    pub async fn run_pass(
        &self,
        storage: &dyn SessionStorage,
        repo: &dyn MemoryRepository,
        extractor: &dyn ConversationExtractor,
        config: &BatchExtractionConfig,
        cancel: &CancellationToken,
    ) -> PassReport {
        let mut report = PassReport::default();

        if !self.embedder_is_usable() {
            // Not a refusal to extract -- a refusal to extract BLIND. Without an
            // embedder every candidate lands in `unscored` and the histogram
            // this phase exists to produce says nothing.
            report.blocked_on = Some("no_embedder".to_string());
            return report;
        }

        let sessions: Vec<Session> = match storage.list_sessions().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("[batch-extraction] session list failed: {e}");
                report.blocked_on = Some("session_list_failed".to_string());
                return report;
            }
        };

        let candidates: Vec<SessionCandidate> = sessions
            .iter()
            .filter(|s| is_eligible_session(&s.id))
            .map(|s| SessionCandidate {
                id: s.id.clone(),
                updated_at: s.updated_at,
                extracted_at: None,
            })
            .collect();

        // The cursor stamp is what orders the backlog, so it has to be read
        // before ordering -- for EVERY eligible conversation, not a prefix of
        // them. `list_sessions()` returns newest-first, so bounding this loop
        // by a count would mean the oldest conversations are never candidates
        // at all, and "read the whole history" would quietly become "read the
        // most recent fifty". One indexed single-row read each, once a minute,
        // against a model call that costs ten seconds: the scan is not where
        // this job is expensive.
        //
        // What is genuinely unfixed here is `list_sessions()` itself -- no
        // LIMIT, no cursor, no `since` -- which this job inherits exactly as
        // titling does. Past a few thousand conversations that wants a
        // paginated read, and it will present as a slow tick rather than as a
        // failure.
        let mut with_cursors: Vec<(SessionCandidate, WindowSubject)> = Vec::new();
        for mut candidate in candidates {
            match storage.extraction_cursor(&candidate.id).await {
                Ok(cursor) => {
                    // Out of attempts against the current watermark: the walk
                    // has already given up on this window and moved on, or is
                    // about to. Not counted as a skipped window -- this loop
                    // sees every conversation in the store, and counting here
                    // would report the same disqualification again on every
                    // pass forever.
                    if cursor.attempts >= MAX_PARSE_ATTEMPTS {
                        continue;
                    }
                    // Whose conversation this is, resolved HERE rather than
                    // inside the window loop, for two reasons that are both
                    // about honesty rather than cost.
                    //
                    // The count. A conversation nobody can name is skipped for
                    // free, so the window loop reaches it only if the model
                    // call budget has not run out first -- which made
                    // `unnameable` a sample of the prefix a pass happened to
                    // walk, not a count of what the pond is failing to
                    // remember. A panel household whose spoken conversations
                    // are all unattributed could read zero while every one of
                    // them was being skipped. Resolved here, it is the number
                    // over every eligible conversation in the store.
                    //
                    // And the read. This is one indexed single-row lookup per
                    // conversation, in the same loop that already pays one for
                    // the cursor, instead of a second one per examined window.
                    let identity = match storage.get_session_identity(&candidate.id).await {
                        Ok(identity) => identity,
                        Err(e) => {
                            // Narrowing, as everywhere else on this path: an
                            // unreadable identity is treated as no identity,
                            // which on a multi-member pond means the window is
                            // left alone rather than mined under a guess.
                            tracing::debug!(
                                "[batch-extraction] identity read failed for {}: {e}",
                                candidate.id
                            );
                            SessionIdentity::unknown()
                        }
                    };
                    match resolve_window_subject(
                        &identity,
                        &config.roster,
                        &config.fallback_subject,
                    ) {
                        SubjectResolution::Named(subject) => {
                            candidate.extracted_at = cursor.extracted_at;
                            with_cursors.push((candidate, subject));
                        }
                        SubjectResolution::Unnameable => {
                            // The cursor does NOT move. This is a window the
                            // pond still owes itself: the day somebody
                            // identifies the conversation -- a paired device, a
                            // face, picking themselves in the UI -- the walk
                            // picks it up from where it stopped. Advancing here
                            // would make the loss permanent and silent.
                            report.sessions_unnameable += 1;
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        "[batch-extraction] cursor read failed for {}: {e}",
                        candidate.id
                    );
                }
            }
        }

        // Said once per pass, at WARN, and not once per window at DEBUG. The
        // condition is not transient and it is not the engine's to fix: a
        // conversation reaches this state because nothing on the path that
        // wrote it could say who was speaking, and the surface that writes the
        // most of them -- the voice child, which runs as its own process with
        // no request and no route -- has no way to be told. It is named in the
        // status endpoint and in the Memory section too; this line is for the
        // pond that has no screen.
        if report.sessions_unnameable > 0 {
            tracing::warn!(
                conversations = report.sessions_unnameable,
                "[batch-extraction] several people live here and nothing says whose these \
                 conversations are, so none of them is being remembered. Identifying one -- a \
                 paired device, a face match, or picking the member in the session -- is what \
                 releases it; filing them under one member's name is the one thing this will \
                 not do."
            );
        }

        let started = std::time::Instant::now();
        let deadline = std::time::Duration::from_secs(config.max_pass_secs);

        // Ordering reads the candidates; the subject rides along beside each
        // one so the resolution cannot be redone -- and disagree -- inside the
        // window.
        let ordered = order_pass(with_cursors.iter().map(|(c, _)| c.clone()).collect());
        let mut subjects: std::collections::HashMap<String, WindowSubject> = with_cursors
            .into_iter()
            .map(|(candidate, subject)| (candidate.id, subject))
            .collect();

        for candidate in ordered {
            // The bound that actually bounds inference. It is asked before
            // anything else because everything else in this loop is cheap and
            // this is the one thing that is not.
            if report.model_calls >= config.model_call_budget() {
                break;
            }
            // And the bound on outcomes, which is the same number and can only
            // be reached first by a pass in which every call succeeded.
            if report.windows_examined >= config.sessions_per_pass {
                break;
            }
            // The wall clock, which catches the failure the call budget cannot:
            // calls that are individually slow rather than individually many.
            // Only once a call has been paid for, so a pass always attempts at
            // least one window and the walk cannot be frozen by a deadline.
            if report.model_calls > 0 && started.elapsed() >= deadline {
                report.deadline_reached = true;
                tracing::debug!(
                    calls = report.model_calls,
                    secs = started.elapsed().as_secs(),
                    "[batch-extraction] pass wall clock spent; stopping"
                );
                break;
            }
            // Before the window, not after: the expensive thing is the model
            // call, and a pass that checks only between windows pays for one it
            // already knew it did not want.
            if cancel.is_cancelled() {
                break;
            }
            let Some(subject) = subjects.remove(&candidate.id) else {
                // Unreachable: every ordered candidate was put in the map above.
                continue;
            };
            self.examine_session(
                storage,
                repo,
                extractor,
                config,
                WindowTarget {
                    session_id: &candidate.id,
                    subject,
                },
                &mut report,
            )
            .await;
        }

        report.blocked_on = pass_blocker(&report);
        report
    }

    /// Read one window of one conversation, or say why not.
    async fn examine_session(
        &self,
        storage: &dyn SessionStorage,
        repo: &dyn MemoryRepository,
        extractor: &dyn ConversationExtractor,
        config: &BatchExtractionConfig,
        target: WindowTarget<'_>,
        report: &mut PassReport,
    ) {
        // The subject was resolved before this conversation was ordered, not
        // here: a window nobody can name is never a candidate at all, so it
        // costs neither a history page nor a model call, and the count of them
        // is a count over the whole store rather than over the prefix a pass
        // had budget to reach. It travels WITH the conversation rather than
        // being recomputed here, so the name in the prompt, the owner stamped
        // on the row and the rows a candidate is banded against cannot come
        // apart.
        let WindowTarget {
            session_id,
            subject,
        } = target;
        let Some(state) = self.cursor_state(storage, session_id).await else {
            return;
        };

        let (remaining, offset) = match state {
            CursorState::Reset => {
                // The anchor is gone -- a truncated history or an edited turn.
                // Clearing and re-walking re-offers memories this conversation
                // already produced; they come back through the same dedup that
                // drops any other restatement, so the cost is one window of
                // inference rather than a duplicate row.
                if let Err(e) = storage.set_extraction_cursor(session_id, None).await {
                    tracing::debug!("[batch-extraction] cursor reset failed for {session_id}: {e}");
                    return;
                }
                report.cursor_resets += 1;
                tracing::debug!(
                    session_id = %session_id,
                    "[batch-extraction] watermark named a deleted message; re-walking"
                );
                return;
            }
            other => match other.unread() {
                Some(pair) => pair,
                None => return,
            },
        };

        if remaining < MIN_NEW_MESSAGES {
            report.windows_skipped += 1;
            return;
        }

        let messages = match storage
            .get_messages_paginated(session_id, config.window_messages, offset)
            .await
        {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("[batch-extraction] history read failed for {session_id}: {e}");
                return;
            }
        };

        let window: Vec<WindowMessage> = messages
            .iter()
            .filter_map(|m| {
                let role = match m.message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    // A system message in the middle of a conversation is
                    // plumbing, not something anybody said.
                    _ => return None,
                };
                Some(WindowMessage {
                    id: m.id.clone(),
                    role: role.to_string(),
                    content: m.message.content.clone(),
                    created_at: m.created_at,
                })
            })
            .collect();

        let carved = match carve_window(&window, config.window_chars) {
            WindowCarve::Ready(carved) => carved,
            WindowCarve::Oversized { chars, through } => {
                // One exchange that cannot fit the prompt budget. Sending it
                // anyway is what the fallback here used to do, and the provider
                // clamp at the other end either truncates from the front --
                // taking the system prompt's rules and the schema with it -- or
                // refuses the call. Both of those cost a model call and produce
                // nothing; this costs none and says what was lost.
                //
                // The cursor MOVES past it. A window that will never fit does
                // not get smaller by being read again, and leaving the
                // watermark here stalls the conversation for the life of the
                // pond.
                report.windows_oversized += 1;
                let _ = storage
                    .set_extraction_cursor(session_id, Some(through))
                    .await;
                tracing::debug!(
                    session_id = %session_id,
                    chars = chars,
                    budget = config.window_chars,
                    "[batch-extraction] one exchange overruns the prompt budget; stepping past \
                     it unread"
                );
                return;
            }
            WindowCarve::NoExchange => {
                report.windows_skipped += 1;
                // A full window with no complete exchange in it -- a run of user
                // messages nobody answered, or a stretch that is all system
                // plumbing. Leaving the cursor would re-read the same messages on
                // every pass forever, so the walk steps past them and records that
                // it did. Only when the window is FULL: a short run may still get
                // its reply.
                //
                // Measured against the RAW page rather than the filtered one, and
                // advanced to the raw page's last id. A window of twenty system
                // messages filters down to nothing, so a check against the filtered
                // count would never find the window full and the walk would stall
                // on that conversation for the life of the pond.
                if messages.len() >= config.window_messages {
                    if let Some(last) = messages.last() {
                        let _ = storage
                            .set_extraction_cursor(session_id, Some(&last.id))
                            .await;
                        tracing::debug!(
                            session_id = %session_id,
                            "[batch-extraction] no complete exchange in a full window; stepping past"
                        );
                    }
                }
                return;
            }
        };

        let window_id = carved
            .last()
            .map(|m| m.id.clone())
            .expect("carve_window never returns an empty slice");

        // ── The idempotence guard, READ ──────────────────────────────────
        // Every window this engine finishes writes an audit row keyed to the
        // window. This is the read of it, and it is the read that makes the
        // cursor advance a commit point rather than nearly one: a watermark
        // naming a deleted message clears the cursor and re-walks the
        // conversation from its first message, which is a routine consequence of
        // the ordinary edit primitive (`DELETE /sessions/{id}/messages/{mid}`
        // deletes that message and everything after it). Without this, a
        // 200-message chat is re-read in full, ten model calls, for memories
        // that are already in the store.
        //
        // Keyed on rows PRODUCED, not on the window having been looked at: a
        // window that a shadow pass read and wrote nothing from must still be
        // mined when the pond starts writing, and a window that genuinely held
        // nothing costs one call to re-confirm and writes nothing either way.
        if let Some(kept) = self.already_mined(repo, &window_id).await {
            report.windows_already_mined += 1;
            let _ = storage
                .set_extraction_cursor(session_id, Some(&window_id))
                .await;
            tracing::debug!(
                session_id = %session_id,
                kept = kept,
                "[batch-extraction] this stretch has already been mined; stepping past it \
                 without a model call"
            );
            return;
        }

        let scope = subject.scope();
        let known = self.known_memories(repo, &scope, carved).await;

        // A window is asked for reminders only while a reminder from it could
        // still be about something that has not happened. A backfill reads
        // months of history in one night; a reminder out of last March is about
        // an appointment already kept or already missed, and the design
        // deliberately cannot parse the date to tell which. Older windows are
        // still mined for memories -- a habit does not go stale -- and dropping
        // the paragraph also saves about seventy tokens on nearly every window
        // of a first run.
        let last_said_at = carved.last().map(|m| m.created_at).unwrap_or_else(Utc::now);
        let allow_reminders =
            config.allow_reminders && window_is_fresh_enough(last_said_at, Utc::now());

        // Counted BEFORE the call, and on every path out of it. This is the
        // counter the pass is bounded by, so anything that increments it after
        // the outcome is known puts the failure paths back outside the bound --
        // which is exactly how a three-window pass became a three-hundred-call
        // one.
        report.model_calls += 1;

        let extraction = extractor
            .extract_window(ExtractionWindow {
                subject: &subject,
                assistant_name: &config.assistant_name,
                session_id,
                window_id: &window_id,
                messages: carved,
                known: &known,
                max_memories: config.max_memories,
                allow_reminders,
            })
            .await;

        let extraction = match extraction {
            Ok(e) => e,
            Err(ExtractionError::NoProvider) => {
                // Nothing was wrong with the window. No attempt, no advance.
                // Recorded as a count rather than as a verdict: what the pass
                // as a whole came to is decided once, at the end, by
                // `pass_blocker`.
                report.no_provider = true;
                return;
            }
            Err(ExtractionError::Provider(e)) => {
                tracing::debug!("[batch-extraction] provider failed on {session_id}: {e}");
                report.provider_failures += 1;
                return;
            }
            Err(ExtractionError::Unparseable { raw_head }) => {
                report.parse_failures += 1;
                let attempts = storage
                    .note_extraction_attempt(session_id)
                    .await
                    .unwrap_or(MAX_PARSE_ATTEMPTS);
                tracing::debug!(
                    session_id = %session_id,
                    attempt = attempts,
                    "[batch-extraction] no JSON recoverable: {raw_head}"
                );
                if attempts >= MAX_PARSE_ATTEMPTS {
                    // The give-up rung. Advancing past a window nobody could
                    // read is a loss; re-reading it forever is a bigger one,
                    // and this way the loss is in the audit log.
                    report.gave_up += 1;
                    let _ = storage
                        .set_extraction_cursor(session_id, Some(&window_id))
                        .await;
                    let _ = repo
                        .log_event(
                            MemoryEventKind::Extracted,
                            &window_event_id(&window_id),
                            Some(session_id),
                            Some(&format!(
                                "{{\"window\":\"{}\",\"parse_failed\":true,\"skipped\":true}}",
                                escape_json(&window_id)
                            )),
                        )
                        .await;
                }
                return;
            }
        };

        report.memories_offered += extraction.memories.len();
        report.reminders_offered += extraction.reminders.len();
        report.rejected_kinds += extraction.rejected;

        let outcome = self
            .dispose_candidates(
                repo,
                &extraction,
                config,
                &subject,
                &scope,
                session_id,
                &window_id,
                last_said_at,
                allow_reminders,
            )
            .await;

        report.bands.add(outcome.bands);
        report.memories_written += outcome.written;
        report.memories_refused += outcome.refused;
        report.memories_dated += outcome.dated;
        report.memories_dates_lost += outcome.dates_lost;
        report.memories_demoted += outcome.demoted;
        report.memories_dropped += outcome.dropped_onto.len();
        report.reminders_captured += outcome.reminders;
        report.reminders_written += outcome.reminders_stored;
        report.reminders_lost += outcome.reminders_lost;

        let _ = repo
            .log_event(
                MemoryEventKind::Extracted,
                &window_event_id(&window_id),
                Some(session_id),
                Some(&format!(
                    "{{\"window\":\"{}\",\"mode\":\"{}\",\"offered\":{},\"kept\":{},\
                     \"refused\":{},\"dated\":{},\"dates_lost\":{},\"demoted\":{},\
                     \"reminders\":{},\"reminders_stored\":{},\"reminders_lost\":{},\
                     \"rejected\":{},\
                     \"same\":{},\"related\":{},\"new\":{},\"unscored\":{},\"dropped_onto\":[{}]}}",
                    escape_json(&window_id),
                    config.mode.as_str(),
                    extraction.memories.len(),
                    outcome.written,
                    outcome.refused,
                    outcome.dated,
                    outcome.dates_lost,
                    outcome.demoted,
                    outcome.reminders,
                    outcome.reminders_stored,
                    outcome.reminders_lost,
                    extraction.rejected,
                    outcome.bands.same,
                    outcome.bands.related,
                    outcome.bands.fresh,
                    outcome.bands.unscored,
                    // The ids a candidate lost to. This is what makes the
                    // build-on phase judgeable rather than assertable: the rows
                    // it would have improved are named here, before it exists,
                    // by the change that throws them away.
                    outcome
                        .dropped_onto
                        .iter()
                        .map(|id| format!("\"{}\"", escape_json(id)))
                        .collect::<Vec<_>>()
                        .join(","),
                )),
            )
            .await;

        if let Err(e) = storage
            .set_extraction_cursor(session_id, Some(&window_id))
            .await
        {
            tracing::debug!("[batch-extraction] cursor advance failed for {session_id}: {e}");
            return;
        }
        report.windows_examined += 1;
    }

    /// Where the walk stands in one conversation.
    ///
    /// `None` on a read error: skip this tick, count no attempt. A transient
    /// SQL failure is not the model failing to answer, and charging it to the
    /// give-up rung would let three unlucky reads disqualify a conversation.
    async fn cursor_state(
        &self,
        storage: &dyn SessionStorage,
        session_id: &str,
    ) -> Option<CursorState> {
        let cursor = storage.extraction_cursor(session_id).await.ok()?;
        let total = storage.count_messages(session_id).await.ok()?;

        match cursor.through_message_id.as_deref() {
            None => Some(CursorState::Unstarted { total }),
            Some(anchor) => match storage.messages_after(session_id, anchor).await {
                Ok(Some(remaining)) => Some(CursorState::InProgress { remaining, total }),
                Ok(None) => Some(CursorState::Reset),
                Err(e) => {
                    tracing::debug!(
                        "[batch-extraction] messages_after failed for {session_id}: {e}"
                    );
                    None
                }
            },
        }
    }

    /// Whether this exact stretch has already produced memories.
    ///
    /// `Some(kept)` when a finished window carrying this key wrote `kept > 0`
    /// rows. `None` when the window is unread, when it was read and produced
    /// nothing, or when the read itself failed -- all three of which mean "read
    /// it", because the cost of being wrong here is one model call and a dedup
    /// pass, while the cost of the opposite mistake is a window of the
    /// household's life silently never mined.
    ///
    /// Keyed through `memory_id`, which is indexed
    /// (`idx_memory_events_memory_id`), rather than through a `data LIKE` scan
    /// over `session_id` -- same key, no new index, no migration.
    async fn already_mined(&self, repo: &dyn MemoryRepository, window_id: &str) -> Option<usize> {
        let events = repo
            .get_events(Some(&window_event_id(window_id)), 8)
            .await
            .ok()?;
        events.iter().find_map(|event| {
            let data = event.data.as_deref()?;
            let parsed: serde_json::Value = serde_json::from_str(data).ok()?;
            let kept = parsed.get("kept")?.as_u64()? as usize;
            (kept > 0).then_some(kept)
        })
    }

    /// The memories most worth showing the model alongside this window.
    ///
    /// A recency pool unioned with a semantic pool, ranked by the same blend
    /// retrieval uses, truncated to [`KNOWN_MEMORIES_SHOWN`] and then to
    /// [`KNOWN_MEMORIES_CHARS`]. Deliberately the same shape as what the agent
    /// injects at turn time: if the model is shown a different eight than the
    /// household would be, "already known" means something different in the
    /// prompt than it does in the pond.
    ///
    /// Read under the WINDOW'S scope. Under the household's, this block is how
    /// one member's memories come to be printed to the model under another
    /// member's name, with the prompt inviting it to "write that memory again
    /// as the BETTER version of itself".
    async fn known_memories(
        &self,
        repo: &dyn MemoryRepository,
        scope: &ProfileScope,
        window: &[WindowMessage],
    ) -> Vec<KnownMemory> {
        let query: String = window
            .iter()
            .filter(|m| m.is_user())
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join(" ");

        let mut pool: Vec<(MemoryFragment, Option<f32>)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

        if let Some(vector) = self.embed(&query).await {
            if let Ok(similar) = repo
                .search_similar(&vector, scope, KNOWN_MEMORIES_SHOWN * 2)
                .await
            {
                for fragment in similar {
                    let score = fragment
                        .embedding
                        .as_deref()
                        .map(|e| cosine_similarity(&vector, e));
                    if seen.insert(fragment.id.clone()) {
                        pool.push((fragment, score));
                    }
                }
            }
        }

        if let Ok(recent) = repo.search_recent(scope, KNOWN_MEMORIES_SHOWN * 2).await {
            for fragment in recent {
                if seen.insert(fragment.id.clone()) {
                    pool.push((fragment, None));
                }
            }
        }

        rank_by_relevance(&mut pool, Utc::now());

        // Two bounds, and the character one is the one that was missing. Eight
        // rows of arbitrary-length content is an unbounded block sharing a
        // budget with the window, and a single long stored memory could spend
        // the whole of it. Rows are dropped whole once the budget is gone --
        // never truncated, because a half sentence shown to a model at this
        // size is one it finishes in its own words.
        let mut used = 0usize;
        let mut known = Vec::new();
        for (fragment, _) in pool.into_iter().take(KNOWN_MEMORIES_SHOWN) {
            let cost = fragment.content.chars().count();
            if used + cost > KNOWN_MEMORIES_CHARS && !known.is_empty() {
                break;
            }
            // The first row is kept whatever it costs -- an empty block is a
            // worse prompt than an overlong one, and the window carve has
            // already left room. It cannot overrun the budget on its own:
            // nothing writes a memory longer than this block's whole share.
            used += cost;
            known.push(KnownMemory {
                kind_label: fragment
                    .segment
                    .as_ref()
                    .map(|s| segment_label(s).to_string())
                    .unwrap_or_else(|| "context".to_string()),
                note: fragment.content,
                // No column counts observations yet, so nothing may be shown to
                // the model as established. Rendering `true` here would be the
                // pond asserting evidence it does not have.
                pattern: false,
            });
        }
        known
    }

    /// Put every candidate through the write gate: date, defect, subject,
    /// band, and then either the store or the record of having dropped it.
    ///
    /// One pass rather than a banding pass and a writing pass, because the
    /// embedding is the expensive part and both need the same vector.
    #[allow(clippy::too_many_arguments)]
    async fn dispose_candidates(
        &self,
        repo: &dyn MemoryRepository,
        extraction: &WindowExtraction,
        config: &BatchExtractionConfig,
        subject: &WindowSubject,
        // The subject's own scope, resolved once by the caller. Passed rather
        // than recomputed so the name in the prompt, the owner stamped on the
        // row and the rows this candidate is compared against cannot come apart.
        scope: &ProfileScope,
        session_id: &str,
        window_id: &str,
        said_at: DateTime<Utc>,
        allow_reminders: bool,
    ) -> WindowOutcome {
        let mut outcome = WindowOutcome::default();

        // The model's own reminders first, so a window that produced nothing
        // else still accounts for them.
        //
        // Gated here as well as in the parser. A window too old to propose
        // anything from is not shown the reminders half of the schema at all,
        // and this is the second place that holds: an extractor that answers
        // with reminders regardless -- a recorded one, a future one -- must not
        // put a proposal in front of somebody about a conversation from months
        // ago just because it was not asked.
        if allow_reminders {
            for reminder in &extraction.reminders {
                self.capture_reminder(
                    ReminderCandidate {
                        about: reminder.about.clone(),
                        when_said: reminder.when_said.clone(),
                        session_id: session_id.to_string(),
                        window_id: window_id.to_string(),
                        subject: subject.name.clone(),
                        profile_id: subject.profile_id.clone(),
                        said_at,
                    },
                    &mut outcome,
                )
                .await;
            }
        }

        if extraction.memories.is_empty() {
            return outcome;
        }

        // The lexical half of the Same band. Read once per window rather than
        // once per candidate: it is the same query every time. Re-read as the
        // window writes, so two wordings of one thing inside a single window
        // cannot both land.
        //
        // Under the subject's scope, like every other read here: a lexical
        // duplicate of ANOTHER member's row is not a duplicate, it is the same
        // true thing about a different person, and dropping it is how one
        // member never gets a memory the other already has.
        let mut recent: Vec<String> = repo
            .search_recent(scope, DEDUP_RECENT_WINDOW)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.content.to_lowercase())
            .collect();

        for memory in &extraction.memories {
            // ── The quality gate ─────────────────────────────────────────
            // Which now includes the date rule, because there is no longer a
            // step in front of it that edits the note. Four passes tried to
            // rewrite a dated sentence into an undated one and each stored its
            // own wreckage -- "The user swims morning.", "The user prefers
            // model of the tractor.", "The user keeps the oven." -- so the
            // whole note is refused instead, and what reaches the store is the
            // sentence the model wrote, word for word.
            //
            // A defective fact is dropped, never repaired: the rewrite a
            // mechanical fix would need -- conjugating "I like" into "Jerry
            // likes", inventing the referent of "the latter", deciding whether
            // a year is a date or a model number -- is exactly the judgement
            // that is not available here, and a wrong repair outlives the
            // conversation that could have corrected it.
            let content = normalise_fact_content(&memory.note);
            if let Some(defect) = fact_defect(&content) {
                outcome.refused += 1;
                if defect == FactDefect::CalendarDate {
                    outcome.dated += 1;
                    // The date is not lost by refusing the note -- IF the model
                    // put it where it was asked to. It is measured doing that
                    // on 31 of its 36 dated windows on the shipped model, and
                    // on none of 36 on one of the others.
                    //
                    // Asked per NOTE, and it has to be. This was a count over
                    // the window -- "some reminder landed" -- applied to every
                    // dated note in it, so a window with two dated notes and one
                    // reminder reported both as kept: measured on "dentist next
                    // Tuesday" plus "collecting the tractor on 3 March" with one
                    // reminder about the dentist, which gave dated 2, dates_lost
                    // 0, and the tractor gone from the pond with every counter
                    // clean. That is the same shape of zero-as-receipt the
                    // reminders work was written to remove, one note deeper.
                    //
                    // [`reminder_covers_note`] is a coarse shared-content-word
                    // test and says so in its own doc: it cannot follow a
                    // paraphrase, and where it cannot match it answers NO. So
                    // this number over-reports rather than under-reports, on
                    // purpose -- a loss the household can see and correct beats
                    // a keep the pond cannot substantiate.
                    //
                    // Only rows count, never answers. A candidate the store
                    // already held counts, because the earlier walk put it
                    // there; one that failed to write does not, because nothing
                    // did.
                    //
                    // A stale window counts as lost outright: it was never shown
                    // the reminders half of the schema, so a date in one of its
                    // notes had nowhere to go at all.
                    let kept_the_date = allow_reminders
                        && outcome.reminders_kept_about.iter().any(|about| {
                            reminder_covers_note(about, &content, &subject.gate_aliases)
                        });
                    if !kept_the_date {
                        outcome.dates_lost += 1;
                    }
                }
                tracing::debug!(
                    session_id = %session_id,
                    defect = %defect,
                    "[batch-extraction] refused a candidate"
                );
                continue;
            }

            // ── Whose fact is it ─────────────────────────────────────────
            let mut segment = memory.kind.segment();
            let mut importance = memory.kind.base_importance();
            let tier = memory.kind.tier();

            // Four of the five kinds are claims ABOUT the subject. A
            // well-formed sentence about somebody else passes every other rule
            // here -- that is how five William Ruto biography facts came to sit
            // in `identity` beside the household's home city. Demoted rather
            // than rejected, because a demotion is reversible by consolidation
            // and keeps a genuinely useful fact, while a rejection loses it.
            let about_the_subject = matches!(
                segment,
                MemorySegment::Identity
                    | MemorySegment::Relationship
                    | MemorySegment::Preference
                    | MemorySegment::Routine
            );
            if about_the_subject && !names_subject(&content, &subject.gate_aliases) {
                segment = MemorySegment::Knowledge;
                importance = MemorySegment::Knowledge.default_importance();
                outcome.demoted += 1;
                tracing::debug!(
                    session_id = %session_id,
                    kind = memory.kind.as_str(),
                    "[batch-extraction] a candidate in a subject bin never named the subject"
                );
            }

            // ── The bands ────────────────────────────────────────────────
            // A correction is the one thing dedup must never swallow: it
            // restates the claim it fixes in almost the same words, which both
            // measures score as a duplicate, and dropping it leaves the STALE
            // row standing -- so the store ends up asserting the very thing the
            // household just took the trouble to deny.
            let is_correction = segment == MemorySegment::Correction;

            let lexical_dupe = recent.iter().any(|e| is_duplicate_content(e, &content));
            let mut matched: Option<String> = None;
            // Kept, so a candidate that goes on to be stored is embedded ONCE.
            // On the Orin an embedding is not free, and a second call per
            // stored row is a second call per row for the life of the pond.
            let mut vector_for_row: Option<Vec<f32>> = None;
            let band = if lexical_dupe {
                Band::Same
            } else {
                match self.embed(&content).await {
                    None => Band::Unscored,
                    Some(vector) => {
                        let neighbours = repo
                            .search_similar(&vector, scope, BAND_NEIGHBOURS)
                            .await
                            .unwrap_or_default();
                        let best = neighbours
                            .iter()
                            .filter_map(|n| {
                                n.embedding
                                    .as_deref()
                                    .map(|e| (n.id.clone(), cosine_similarity(&vector, e)))
                            })
                            .fold(None::<(String, f32)>, |acc, (id, s)| match acc {
                                Some((_, best)) if best >= s => acc,
                                _ => Some((id, s)),
                            });
                        vector_for_row = Some(vector);
                        match best {
                            // A store with no comparable vector at all cannot
                            // say whether this is new. On an empty store that
                            // is literally true and "new" would be the right
                            // word; on a store whose rows are unembedded it is
                            // not, and the two are indistinguishable from here.
                            None => Band::Unscored,
                            Some((id, sim)) => {
                                matched = Some(id);
                                if sim >= config.reinforce_threshold {
                                    Band::Same
                                } else if sim >= config.relate_threshold {
                                    Band::Related
                                } else {
                                    Band::Fresh
                                }
                            }
                        }
                    }
                }
            };
            outcome.bands.count(band);

            // ── Store it, or record the loss ─────────────────────────────
            let drop_it = matches!(band, Band::Same | Band::Related) && !is_correction;
            if drop_it {
                // No reinforcement in this phase: a match is DROPPED, exactly
                // as the per-turn path dropped it. What is new is that the loss
                // is written down with its band and with the id it lost to, so
                // the phase that builds on a match instead of discarding it can
                // be measured against this one rather than argued about.
                outcome
                    .dropped_onto
                    .push(matched.unwrap_or_else(|| "lexical".to_string()));
                tracing::debug!(
                    session_id = %session_id,
                    band = band.as_str(),
                    "[batch-extraction] dropped a candidate the store already holds"
                );
                continue;
            }

            if !config.mode.writes() {
                // Shadow mode: banded, counted, and nothing written. Kept as a
                // live mode rather than deleted, because it is how a threshold
                // gets measured against a real history before it decides, for
                // every memory this pond writes, whether a better-said version
                // replaces the first one or is thrown away.
                continue;
            }

            let id = uuid::Uuid::new_v4().to_string();
            let mut fragment = MemoryFragment::from_window_extraction(
                id.clone(),
                subject.profile_id.clone(),
                Some(session_id.to_string()),
                content.clone(),
                segment,
                importance,
                tier,
            );
            // The vector the banding pass already computed, or one taken now
            // for the lexical-duplicate path that never needed one. Best-effort
            // either way: an embed failure downgrades this row to an unembedded
            // one, which `IndexMaintenance`'s sweep repairs on a later tick, and
            // it never aborts the window.
            fragment.embedding = match vector_for_row {
                Some(vector) => Some(vector),
                None => self.embed(&content).await,
            };

            if let Err(e) = repo.add(fragment).await {
                tracing::warn!("[batch-extraction] failed to store a memory: {e}");
                continue;
            }
            outcome.written += 1;
            recent.push(content.to_lowercase());
            // The fact itself is never logged at INFO. INFO is what the on-disk
            // log under <data_dir>/logs keeps, so an extracted memory logged
            // there is the household's private sentence in a second place with
            // none of the store's scoping, retention or redaction. The id
            // correlates the line with the row; the row is the record.
            tracing::info!(
                memory_id = %id,
                chars = content.chars().count(),
                "[batch-extraction] stored a memory"
            );
            // `"window"` is this row's provenance: which exact stretch of which
            // conversation it was taken from. It is written from the first row
            // this engine ever stores, because a row written without it can
            // never be attributed later.
            //
            // It is NOT what the re-walk guard reads -- that reads the
            // window-level row this window also writes, keyed through
            // `window_event_id` and therefore through an index that exists (see
            // `already_mined`). Saying which of the two is read matters: a
            // guard that is only written reads, to anyone maintaining this, as
            // protection that is not there.
            let _ = repo
                .log_event(
                    MemoryEventKind::Extracted,
                    &id,
                    Some(session_id),
                    Some(&format!(
                        "{{\"window\":\"{}\",\"kind\":\"{}\",\"band\":\"{}\"}}",
                        escape_json(window_id),
                        memory.kind.as_str(),
                        band.as_str(),
                    )),
                )
                .await;
        }

        outcome
    }

    /// Store a dated item, which is never a memory.
    ///
    /// This is where the date rule's promise is kept. The gate refuses any note
    /// carrying a one-off calendar date on the understanding that the date goes
    /// somewhere else instead, and for one release it did not: this method
    /// incremented a counter, emitted a DEBUG line and dropped the candidate, so
    /// a pond whose model obeyed the whole prompt still lost every date it was
    /// asked to keep. The row lands now, and the counter stays because it is how
    /// the pass reports what it read.
    ///
    /// Deduplication belongs to the store, not to this call. The engine
    /// re-walks -- a watermark naming a deleted message clears the cursor and
    /// the conversation is read from its first message again -- and the
    /// window-level guard that covers memories cannot cover this: `already_mined`
    /// only fires for a window that WROTE one, and the windows that matter here
    /// are precisely those whose only yield was a reminder. So the guard is the
    /// UNIQUE constraint on `(window_id, about_key)`, and `capture` answers with
    /// whether the row was new.
    ///
    /// The three outcomes are counted apart because they mean different things
    /// to the household: a new row is the date kept, a duplicate is the date
    /// already kept, and an error -- or a pond with no store wired -- is the date
    /// gone, which is the one that has to be visible.
    async fn capture_reminder(&self, candidate: ReminderCandidate, outcome: &mut WindowOutcome) {
        outcome.reminders += 1;
        // The words themselves stay at DEBUG. INFO is what the on-disk log under
        // <data_dir>/logs keeps, and a household's appointment written there is
        // their private sentence in a second place with none of the store's
        // scoping or retention -- the same rule the stored-memory line follows.
        tracing::debug!(
            session_id = %candidate.session_id,
            "[batch-extraction] reminder candidate: {} ({})",
            candidate.about,
            candidate.when_said
        );

        let reminder = CapturedReminder::from_candidate(
            candidate,
            uuid::Uuid::new_v4().to_string(),
            Utc::now(),
        );

        let Some(repository) = self.reminder_repository.as_ref() else {
            outcome.reminders_lost += 1;
            // WARN, not DEBUG. A pond running without a reminder store refuses
            // dated memories and keeps nothing in their place, which is the
            // worst of both rules, and nothing else about it looks wrong.
            tracing::warn!(
                session_id = %reminder.session_id,
                "[batch-extraction] no reminder store is wired; this date is being dropped"
            );
            return;
        };

        match repository.capture(&reminder).await {
            Ok(true) => {
                outcome.reminders_stored += 1;
                outcome.reminders_kept_about.push(reminder.about.clone());
            }
            Ok(false) => {
                outcome.reminders_deduped += 1;
                // The row is in the store, put there by an earlier walk, so a
                // note this one covers is just as kept as a freshly written one.
                outcome.reminders_kept_about.push(reminder.about.clone());
                tracing::debug!(
                    window_id = %reminder.window_id,
                    "[batch-extraction] this window's reminder is already stored"
                );
            }
            Err(e) => {
                outcome.reminders_lost += 1;
                // The error, the id and the window -- never the content. What
                // the household needs from a log line here is which write
                // failed and why, and the sentence is in the row that did not
                // land.
                tracing::warn!(
                    reminder_id = %reminder.id,
                    window_id = %reminder.window_id,
                    "[batch-extraction] a reminder could not be stored, so the date is lost: {e}"
                );
            }
        }
    }

    /// Embed, recording health either way.
    ///
    /// `None` covers both "no embedder" and "the embedder failed", because the
    /// caller's response to them is the same: this candidate cannot be scored.
    /// The health record is what makes them differ at the LANE level -- a
    /// failure stands the engine down for a cooldown, an absent embedder never
    /// let it start.
    async fn embed(&self, text: &str) -> Option<Vec<f32>> {
        let provider = self.embedding_provider.as_ref()?;
        match provider.embed(text).await {
            Ok(vector) => {
                self.health.record_success();
                Some(vector)
            }
            Err(e) => {
                self.health.record_failure();
                tracing::debug!("[batch-extraction] embed failed: {e}");
                None
            }
        }
    }
}

/// Why a finished pass could do nothing, or `None` because it could.
///
/// One decision, taken once, from the whole pass. The rule is the one the
/// unnameable case already had and the provider cases did not: a pass that read
/// even one window is WORKING, and saying otherwise puts a warning in front of a
/// household about a pond that is doing its job. A single transient timeout in
/// the first of three windows used to do exactly that.
///
/// Parse failures deliberately set nothing. A model answering unreadably is not
/// an engine that is stopped -- the pass is spending its budget, the give-up
/// rung is moving the walk on, and `parse_failures` and `gave_up` already carry
/// the number. Reporting it as blocked would say the model could not be reached,
/// which is false.
fn pass_blocker(report: &PassReport) -> Option<String> {
    if report.windows_examined > 0 {
        return None;
    }
    if report.no_provider {
        return Some("no_provider".to_string());
    }
    if report.provider_failures > 0 {
        return Some("provider_error".to_string());
    }
    if report.sessions_unnameable > 0 {
        return Some("unnameable_subject".to_string());
    }
    None
}

/// The `memory_id` a shadow window's audit row carries.
///
/// Shadow mode stores no memory, so there is no id to key the event to. The
/// window id stands in, prefixed so it can never collide with a real fragment's
/// UUID and so a reader of `memory_events` can tell at a glance that this row
/// describes a window rather than a memory.
///
/// It is also the key the re-walk guard reads: [`BatchExtractionService::already_mined`]
/// asks for this id through `get_events`, which is served by
/// `idx_memory_events_memory_id`. That is what makes the cursor advance a commit
/// point -- a conversation whose watermark was cleared re-walks without paying
/// for the stretches it has already mined.
fn window_event_id(window_id: &str) -> String {
    format!("window:{window_id}")
}

/// Escape a value going into a hand-built JSON string.
///
/// The `data` column is hand-assembled rather than serialised because it is
/// three integers and an id, and a struct per event shape would be more code
/// than it saves. The ids are database-generated, but "the input is safe" is
/// exactly the assumption that stops being true later.
fn escape_json(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How a stored segment is described to the model in the "already remembered"
/// block.
///
/// The existing store predates the five-value catalogue: its rows carry
/// `identity`, `project` and `knowledge`, which the model may no longer choose.
/// They are mapped onto the nearest thing it CAN choose rather than shown
/// verbatim, so the block never demonstrates a label the schema forbids -- a
/// small model shown `"kind":"project"` in its own context will produce one.
fn segment_label(segment: &MemorySegment) -> &'static str {
    match segment {
        MemorySegment::Relationship => "relationship",
        MemorySegment::Preference => "preference",
        MemorySegment::Correction => "correction",
        MemorySegment::Routine => "routine",
        MemorySegment::Identity | MemorySegment::Context => "context",
        // Neither is in the catalogue. A project is an ongoing way somebody
        // spends their time, which is the closest the five values come; a bare
        // fact the user taught is part of the picture of who they are.
        MemorySegment::Project => "routine",
        MemorySegment::Knowledge => "context",
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::models::domain::message::ChatMessage;
    use crate::user_data::domain::memory::MemoryTier;
    use crate::user_data::domain::session::SessionMessage;
    use crate::user_data::mocks::mock_memory::MockMemoryRepository;
    use crate::user_data::mocks::mock_reminder::{
        FailingReminderRepository, MockReminderRepository,
    };
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;
    use crate::user_data::ports::conversation_extractor::{
        ExtractedMemory, ExtractedReminder, MemoryKind,
    };
    use async_trait::async_trait;
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Doubles ──────────────────────────────────────────────────────────

    /// Deterministic bag-of-tokens embedder.
    ///
    /// `MockEmbeddingProvider` returns a zero vector, whose cosine against
    /// anything is zero -- which would make every band assertion below pass for
    /// the wrong reason. This gives the semantic path a real, stable signal
    /// with no model and no network, over `content_tokens`, which is the same
    /// tokeniser the lexical dedup uses.
    struct HashEmbedder;

    #[async_trait]
    impl EmbeddingProvider for HashEmbedder {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            const DIMS: usize = 64;
            let mut v = vec![0.0_f32; DIMS];
            for token in
                crate::user_data::services::memory_relevance::content_tokens(&text.to_lowercase())
            {
                let mut hash: u64 = 1469598103934665603;
                for byte in token.as_bytes() {
                    hash ^= *byte as u64;
                    hash = hash.wrapping_mul(1099511628211);
                }
                v[(hash % DIMS as u64) as usize] += 1.0;
            }
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in &mut v {
                    *x /= norm;
                }
            }
            Ok(v)
        }

        fn dimensions(&self) -> usize {
            64
        }
    }

    /// An embedder that always fails, for the health path.
    struct BrokenEmbedder;

    #[async_trait]
    impl EmbeddingProvider for BrokenEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Err(anyhow::anyhow!("no embedding model loaded"))
        }

        fn dimensions(&self) -> usize {
            0
        }
    }

    /// Answers every window the same way, counting calls.
    struct ScriptedExtractor {
        reply: std::result::Result<WindowExtraction, &'static str>,
        calls: AtomicUsize,
        windows: std::sync::Mutex<Vec<String>>,
    }

    impl ScriptedExtractor {
        fn yielding(memories: Vec<(&str, MemoryKind)>) -> Self {
            Self {
                reply: Ok(WindowExtraction {
                    memories: memories
                        .into_iter()
                        .map(|(note, kind)| ExtractedMemory {
                            note: note.to_string(),
                            kind,
                        })
                        .collect(),
                    reminders: Vec::new(),
                    rejected: 0,
                }),
                calls: AtomicUsize::new(0),
                windows: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// The same, plus the reminders the model filed in the same reply.
        ///
        /// The two halves of one answer: the date rule refuses a dated NOTE,
        /// and whether the date survives at all depends entirely on whether the
        /// model also filed it here.
        fn yielding_with_reminders(
            memories: Vec<(&str, MemoryKind)>,
            reminders: Vec<(&str, &str)>,
        ) -> Self {
            let mut scripted = Self::yielding(memories);
            if let Ok(extraction) = &mut scripted.reply {
                extraction.reminders = reminders
                    .into_iter()
                    .map(|(about, when)| ExtractedReminder {
                        about: about.to_string(),
                        when_said: when.to_string(),
                    })
                    .collect();
            }
            scripted
        }

        fn unparseable() -> Self {
            Self {
                reply: Err("Sure! Here is what I remembered."),
                calls: AtomicUsize::new(0),
                windows: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn windows(&self) -> Vec<String> {
            self.windows.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ConversationExtractor for ScriptedExtractor {
        async fn extract_window(
            &self,
            window: ExtractionWindow<'_>,
        ) -> std::result::Result<WindowExtraction, ExtractionError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.windows
                .lock()
                .unwrap()
                .push(window.window_id.to_string());
            match &self.reply {
                Ok(e) => Ok(e.clone()),
                Err(raw) => Err(ExtractionError::Unparseable {
                    raw_head: (*raw).to_string(),
                }),
            }
        }
    }

    // ── Fixtures ─────────────────────────────────────────────────────────

    fn config() -> BatchExtractionConfig {
        BatchExtractionConfig {
            mode: ExtractionMode::Shadow,
            sessions_per_pass: 3,
            window_messages: 20,
            window_chars: EXTRACTION_WINDOW_CHARS,
            max_memories: 3,
            relate_threshold: 0.78,
            reinforce_threshold: 0.94,
            assistant_name: "Goose".to_string(),
            fallback_subject: WindowSubject::named("Jerry"),
            roster: HouseholdRoster::default(),
            allow_reminders: true,
            max_pass_secs: EXTRACTION_PASS_MAX_SECS,
        }
    }

    /// The engine as a pond runs it: an embedder and somewhere for a date to
    /// go.
    ///
    /// The reminder store is part of the ordinary fixture rather than an extra a
    /// reminder test opts into, because `memories_dates_lost` now asks whether a
    /// row landed. A default engine with nowhere to put a date is a broken pond,
    /// not a neutral one, and a fixture that shipped it would report a loss in
    /// every unrelated test.
    fn service() -> BatchExtractionService {
        service_storing_into(Arc::new(MockReminderRepository::new()))
    }

    fn service_storing_into(reminders: Arc<dyn ReminderRepository>) -> BatchExtractionService {
        BatchExtractionService::new()
            .with_embedding_provider(Arc::new(HashEmbedder) as Arc<dyn EmbeddingProvider>)
            .with_reminder_repository(reminders)
    }

    async fn seed(storage: &InMemorySessionStorage, session: &str, pairs: usize) {
        storage.create_session(session.to_string()).await.unwrap();
        for i in 0..pairs {
            for (role, text) in [
                ("user", format!("question {i} about the greenhouse")),
                ("assistant", format!("answer {i}")),
            ] {
                let message = if role == "user" {
                    ChatMessage::user(text)
                } else {
                    ChatMessage::assistant(text)
                };
                storage
                    .add_message(
                        session.to_string(),
                        SessionMessage::new(
                            format!("{session}-m{}", i * 2 + usize::from(role == "assistant")),
                            session.to_string(),
                            message,
                        ),
                    )
                    .await
                    .unwrap();
            }
        }
    }

    /// Seed a conversation whose messages happened at a given moment.
    ///
    /// The reminder cutoff is asked about the CONVERSATION, not about the
    /// moment the backlog reached it, so a fixture that cannot place a
    /// conversation in the past cannot test the rule.
    async fn seed_at(
        storage: &InMemorySessionStorage,
        session: &str,
        pairs: usize,
        at: DateTime<Utc>,
    ) {
        storage.create_session(session.to_string()).await.unwrap();
        for i in 0..pairs {
            for (role, text) in [
                ("user", format!("question {i} about the greenhouse")),
                ("assistant", format!("answer {i}")),
            ] {
                let message = if role == "user" {
                    ChatMessage::user(text)
                } else {
                    ChatMessage::assistant(text)
                };
                let mut row = SessionMessage::new(
                    format!("{session}-m{}", i * 2 + usize::from(role == "assistant")),
                    session.to_string(),
                    message,
                );
                row.created_at = at;
                storage.add_message(session.to_string(), row).await.unwrap();
            }
        }
    }

    /// Fails the first `failures` calls and answers every later one.
    ///
    /// The shape of a provider that is reloading a model: one window times out,
    /// the next two are served normally. A double that fails on ALL of them
    /// could not tell a sticky `blocked_on` from an accurate one.
    struct FlakyExtractor {
        failures: usize,
        calls: AtomicUsize,
        reply: WindowExtraction,
    }

    impl FlakyExtractor {
        fn failing_first(failures: usize, memories: Vec<(&str, MemoryKind)>) -> Self {
            Self {
                failures,
                calls: AtomicUsize::new(0),
                reply: WindowExtraction {
                    memories: memories
                        .into_iter()
                        .map(|(note, kind)| ExtractedMemory {
                            note: note.to_string(),
                            kind,
                        })
                        .collect(),
                    reminders: Vec::new(),
                    rejected: 0,
                },
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ConversationExtractor for FlakyExtractor {
        async fn extract_window(
            &self,
            _window: ExtractionWindow<'_>,
        ) -> std::result::Result<WindowExtraction, ExtractionError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.failures {
                return Err(ExtractionError::Provider(anyhow::anyhow!(
                    "the model is reloading"
                )));
            }
            Ok(self.reply.clone())
        }
    }

    /// Records the KNOWN block each window was shown.
    #[derive(Default)]
    struct KnownRecordingExtractor {
        shown: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl KnownRecordingExtractor {
        fn shown(&self) -> Vec<Vec<String>> {
            self.shown.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl ConversationExtractor for KnownRecordingExtractor {
        async fn extract_window(
            &self,
            window: ExtractionWindow<'_>,
        ) -> std::result::Result<WindowExtraction, ExtractionError> {
            self.shown
                .lock()
                .unwrap()
                .push(window.known.iter().map(|k| k.note.clone()).collect());
            Ok(WindowExtraction::default())
        }
    }

    /// Records what the window was ASKED for, rather than what it answered.
    #[derive(Default)]
    struct AskRecordingExtractor {
        asked: std::sync::Mutex<Option<bool>>,
    }

    impl AskRecordingExtractor {
        fn asked_for_reminders(&self) -> Option<bool> {
            *self.asked.lock().unwrap()
        }
    }

    #[async_trait]
    impl ConversationExtractor for AskRecordingExtractor {
        async fn extract_window(
            &self,
            window: ExtractionWindow<'_>,
        ) -> std::result::Result<WindowExtraction, ExtractionError> {
            *self.asked.lock().unwrap() = Some(window.allow_reminders);
            Ok(WindowExtraction::default())
        }
    }

    /// The messages of a carve that produced some, or a failure naming what it
    /// produced instead.
    fn ready(carve: WindowCarve<'_>) -> &[WindowMessage] {
        match carve {
            WindowCarve::Ready(messages) => messages,
            other => panic!("expected a window, got {other:?}"),
        }
    }

    fn msg(id: &str, role: &str, content: &str) -> WindowMessage {
        WindowMessage {
            id: id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: Utc::now(),
        }
    }

    // ── The mode ─────────────────────────────────────────────────────────

    /// An unreadable mode must not be able to start writing.
    ///
    /// This is the same narrowing argument the orchestrator and unprompted-speech
    /// toggles carry, with a sharper consequence: the value arrives from a
    /// settings ROW, so a typo, a half-finished rollout or a hand-edited
    /// database is enough to produce it, and the thing on the other side of the
    /// decision is the household's permanent memory store.
    #[test]
    fn an_unrecognised_mode_reads_as_shadow() {
        for raw in ["", "  ", "reinforced", "writes", "on", "true", "nonsense"] {
            let mode = ExtractionMode::parse(raw);
            assert_eq!(
                mode,
                ExtractionMode::Shadow,
                "{raw:?} parsed to {mode:?}, which is allowed to write to the store"
            );
            assert!(!mode.writes());
        }

        // Vacuity control: the two writing modes really do parse, so "shadow"
        // above is a decision about unknown input and not a parser that only
        // ever returns one value.
        assert_eq!(ExtractionMode::parse("write"), ExtractionMode::Write);
        assert_eq!(
            ExtractionMode::parse("  Reinforce  "),
            ExtractionMode::Reinforce
        );
    }

    /// The shipped default WRITES, and an unreadable one does not.
    ///
    /// Inverted at the cutover, deliberately. While the per-turn path was live
    /// the default was `shadow`: a new background job must not start writing to
    /// the household's memory store on upgrade. With that path gone the same
    /// caution means a pond that reads its own history, bands every candidate
    /// and remembers nothing -- silently, with a memory section that never
    /// grows. The narrowing rule that still holds is the one about UNREADABLE
    /// input: a typo in a settings row costs a pass, never a write.
    #[test]
    fn the_shipped_default_writes_and_an_unreadable_mode_does_not() {
        let settings = crate::user_data::domain::settings::Settings::default();
        let config = BatchExtractionConfig::from_settings(&settings);
        assert_eq!(config.mode, ExtractionMode::Write);
        assert!(config.mode.writes());

        let from_nothing: crate::user_data::domain::settings::Settings =
            serde_json::from_str("{}").expect("every Settings field has a serde default");
        assert_eq!(
            ExtractionMode::parse(&from_nothing.memory_extraction_mode),
            ExtractionMode::Write,
            "with nothing else extracting, a pond that stays in shadow never remembers \
             anything at all"
        );

        assert!(
            !ExtractionMode::parse("wrtie").writes(),
            "an unrecognised mode must still fall to shadow"
        );
    }

    /// `Friend` is a placeholder, not a name.
    #[test]
    fn an_unconfigured_pond_extracts_about_the_user_not_about_friend() {
        let mut settings = crate::user_data::domain::settings::Settings::default();
        assert_eq!(settings.user_name, "Friend", "guard: the shipped default");
        assert_eq!(
            BatchExtractionConfig::from_settings(&settings).fallback_subject,
            WindowSubject::anonymous()
        );

        settings.user_name = "Jerry".to_string();
        assert_eq!(
            BatchExtractionConfig::from_settings(&settings).fallback_subject,
            WindowSubject::named("Jerry")
        );
    }

    // ── Ordering ─────────────────────────────────────────────────────────

    fn candidate(id: &str, updated: i64, extracted: Option<i64>) -> SessionCandidate {
        let base = DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        SessionCandidate {
            id: id.to_string(),
            updated_at: base + chrono::Duration::minutes(updated),
            extracted_at: extracted.map(|m| base + chrono::Duration::minutes(m)),
        }
    }

    /// Today's conversation never waits behind the backlog, and the backlog
    /// still drains oldest-first behind it.
    ///
    /// The two jobs pull opposite ways, which is why the first slot is carved
    /// out rather than folded into one sort. Pure recency never finishes the
    /// history; pure backlog order means a conversation somebody is having
    /// right now is read for the first time in a fortnight.
    #[test]
    fn the_newest_conversation_leads_and_the_backlog_drains_behind_it() {
        let ordered = order_pass(vec![
            candidate("old-unread", 10, None),
            candidate("today", 900, Some(800)),
            candidate("stale-read", 20, Some(30)),
            candidate("recently-read", 30, Some(700)),
        ]);
        let ids: Vec<&str> = ordered.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["today", "old-unread", "stale-read", "recently-read"],
            "slot one is the most recently active; never-examined leads the backlog, then \
             least-recently-examined"
        );
    }

    /// The pond's own conversations are not somebody's life.
    ///
    /// A cron line firing at 3am mints a session, and the proactive reviewer
    /// owns a permanent one. Mining those reads the pond's own output back to
    /// itself as though a person had said it -- the mechanism that put the
    /// assistant's self-description into the live store as the user's identity.
    #[test]
    fn machine_authored_conversations_are_never_mined() {
        let ordered = order_pass(vec![
            candidate("sched-backup-1717", 900, None),
            candidate(PROPOSAL_SESSION_ID, 800, None),
            candidate("a-real-chat", 10, None),
        ]);
        let ids: Vec<&str> = ordered.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["a-real-chat"]);
    }

    // ── The window ───────────────────────────────────────────────────────

    /// A window never ends mid-exchange.
    ///
    /// The cursor advances to the window's last message, so a window ending on
    /// a question moves the watermark past a question whose answer nobody read.
    #[test]
    fn a_window_ends_on_a_reply_never_on_a_question() {
        let messages = vec![
            msg("m1", "user", "where do we keep the starter"),
            msg("m2", "assistant", "in the pantry"),
            msg("m3", "user", "and the flour"),
        ];
        let carved = ready(carve_window(&messages, 6_000));
        assert_eq!(carved.len(), 2);
        assert_eq!(carved.last().unwrap().id, "m2");
    }

    /// A run of questions nobody answered carves nothing.
    ///
    /// A real state, not an error: somebody sent three messages and the pond
    /// has not replied yet. The caller's rule for a FULL window of this is what
    /// stops the walk re-reading it forever, and it is tested separately below.
    #[test]
    fn a_window_with_no_reply_in_it_carves_nothing() {
        let messages = vec![
            msg("m1", "user", "are you there"),
            msg("m2", "user", "hello"),
        ];
        assert_eq!(carve_window(&messages, 6_000), WindowCarve::NoExchange);
    }

    /// The character budget trims the OLDEST end.
    ///
    /// Trimming the newest end would mean a long window is read for what was
    /// said furthest in the past, which is the opposite of what a window is
    /// chosen for.
    #[test]
    fn the_character_budget_drops_the_oldest_messages_first() {
        let messages = vec![
            msg("m1", "user", &"a".repeat(500)),
            msg("m2", "assistant", &"b".repeat(500)),
            msg("m3", "user", &"c".repeat(100)),
            msg("m4", "assistant", &"d".repeat(100)),
        ];
        let carved = ready(carve_window(&messages, 400));
        let ids: Vec<&str> = carved.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m3", "m4"]);
    }

    /// One oversized exchange is REFUSED, where it used to be sent whole.
    ///
    /// The inversion of `one_message_longer_than_the_whole_budget_still_carves`,
    /// which asserted the old fallback to the full untrimmed page. That
    /// fallback's justification was that "the prompt builder clamps it", and no
    /// prompt builder clamped anything: a 40 KB pasted log went to a provider
    /// with an 8192-token ceiling, which truncates from the FRONT -- taking the
    /// schema and the rules with it -- so the reply comes back unparseable and
    /// the window burns the give-up rung. Refusing costs the same one exchange
    /// and costs no inference, and the caller records the loss.
    #[test]
    fn one_exchange_longer_than_the_whole_budget_is_refused_not_sent() {
        let messages = vec![
            msg("m1", "user", &"a".repeat(9_000)),
            msg("m2", "assistant", "noted"),
        ];
        match carve_window(&messages, 6_000) {
            WindowCarve::Oversized { chars, through } => {
                assert_eq!(chars, 9_005);
                assert_eq!(
                    through, "m2",
                    "the caller advances past the exchange it could not read, or the walk \
                     stalls on it forever"
                );
            }
            other => panic!("expected an oversize refusal, got {other:?}"),
        }

        // Vacuity control: the same pair inside a budget that fits it carves
        // normally, so the refusal above is about the SIZE and not about the
        // carve having stopped working.
        assert!(matches!(
            carve_window(&messages, 10_000),
            WindowCarve::Ready(_)
        ));
    }

    /// The three bounds compose: what the window may cost, what the known block
    /// may cost, and what the authored prompt costs, all inside one budget.
    ///
    /// The budget was previously named in a constant that only its own test
    /// read. This is the coupling that makes it mean something: if the window
    /// or the known block grows, this fails here rather than at a provider's
    /// prompt clamp.
    #[test]
    fn the_window_and_the_known_block_fit_the_prompt_budget_together() {
        use crate::user_data::ports::conversation_extractor::{
            CHARS_PER_TOKEN, EXTRACTION_PROMPT_BUDGET_TOKENS,
        };

        // The authored half, as `conversation_extractor.rs` caps it. Named
        // here rather than imported because pond-core may not depend on the
        // adapter; the adapter asserts its own side against its own ceiling.
        const SYSTEM_PROMPT_CEILING_CHARS: usize = 2_300;

        let worst_case =
            (EXTRACTION_WINDOW_CHARS + KNOWN_MEMORIES_CHARS + SYSTEM_PROMPT_CEILING_CHARS)
                .div_ceil(CHARS_PER_TOKEN);
        assert!(
            worst_case <= EXTRACTION_PROMPT_BUDGET_TOKENS,
            "a maximal window, a maximal known block and the longest allowed system prompt \
             come to {worst_case} tokens against a budget of {EXTRACTION_PROMPT_BUDGET_TOKENS}"
        );
    }

    // ── The cursor ───────────────────────────────────────────────────────

    /// The offset arithmetic, which is what decides WHICH messages are read.
    #[test]
    fn unread_messages_start_where_the_watermark_stopped() {
        assert_eq!(CursorState::Unstarted { total: 12 }.unread(), Some((12, 0)));
        assert_eq!(
            CursorState::InProgress {
                remaining: 4,
                total: 12
            }
            .unread(),
            Some((4, 8))
        );
        // A watermark naming a deleted message has no offset to give. Guessing
        // one is how a walk silently re-reads or silently skips.
        assert_eq!(CursorState::Reset.unread(), None);
    }

    // ── The pass ─────────────────────────────────────────────────────────

    /// The whole point of this phase: it reads, it bands, it writes nothing.
    #[tokio::test]
    async fn a_shadow_pass_reads_a_window_and_stores_no_memory() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![
            (
                "Jerry keeps his sourdough starter in the pantry.",
                MemoryKind::Preference,
            ),
            (
                "Jerry waters the greenhouse before work.",
                MemoryKind::Routine,
            ),
        ]);

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.windows_examined, 1);
        assert_eq!(report.memories_offered, 2);
        assert_eq!(report.memories_written, 0);
        assert_eq!(
            repo.search_recent(&ProfileScope::Household, 100)
                .await
                .unwrap()
                .len(),
            0,
            "shadow mode must not put a single row in the store"
        );

        // It is not silent, though: the pass leaves an audit row per window so
        // the histogram is recoverable from a device nobody is watching.
        let events = repo.events().await;
        assert_eq!(events.len(), 1);
        assert!(events[0].1.starts_with("window:"));
        let data = events[0].2.as_deref().unwrap_or_default();
        assert!(data.contains("\"mode\":\"shadow\""), "{data}");
        assert!(data.contains("\"offered\":2"), "{data}");
        assert!(data.contains("\"kept\":0"), "{data}");
    }

    /// The cursor advances on EXAMINED, not on PRODUCED.
    ///
    /// A window the model read and found nothing in has been examined. Tying
    /// the advance to output instead would make the pond re-read every
    /// uneventful conversation it owns, forever, and never reach the ones it
    /// has not read at all.
    #[tokio::test]
    async fn a_window_that_produced_nothing_still_advances_the_cursor() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.windows_examined, 1);
        assert_eq!(report.memories_offered, 0);
        assert!(storage
            .extraction_cursor("sess-1")
            .await
            .unwrap()
            .through_message_id
            .is_some());
    }

    /// A second pass reads the NEXT window, and a conversation with nothing new
    /// is not read again.
    #[tokio::test]
    async fn the_walk_moves_forward_and_then_stops() {
        let storage = InMemorySessionStorage::new();
        // 30 messages: more than one window of 20, and the first window carves
        // to a whole number of pairs.
        seed(&storage, "sess-1", 15).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);
        let service = service();
        let config = config();
        let cancel = CancellationToken::new();

        let first = service
            .run_pass(&storage, &repo, &extractor, &config, &cancel)
            .await;
        assert_eq!(first.windows_examined, 1);
        let after_first = storage.extraction_cursor("sess-1").await.unwrap();

        let second = service
            .run_pass(&storage, &repo, &extractor, &config, &cancel)
            .await;
        assert_eq!(second.windows_examined, 1);
        let after_second = storage.extraction_cursor("sess-1").await.unwrap();
        assert_ne!(
            after_first.through_message_id,
            after_second.through_message_id
        );

        // Nothing left: the conversation has been read to the end.
        let third = service
            .run_pass(&storage, &repo, &extractor, &config, &cancel)
            .await;
        assert_eq!(third.windows_examined, 0);
        assert_eq!(third.windows_skipped, 1);
        assert_eq!(
            extractor.calls(),
            2,
            "a conversation with nothing new must not cost an inference call"
        );
        assert_eq!(extractor.windows().len(), 2);
    }

    /// A reply with no JSON in it does NOT advance the cursor, and three of
    /// them in a row do.
    ///
    /// Both halves are the point. Advancing on the first failure loses a real
    /// conversation to a parser; never advancing lets one window absorb the
    /// whole backlog's inference budget forever.
    #[tokio::test]
    async fn three_unparseable_replies_give_up_on_a_window_and_say_so() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::unparseable();
        let service = service();
        let config = config();
        let cancel = CancellationToken::new();

        for expected_attempts in 1..MAX_PARSE_ATTEMPTS {
            let report = service
                .run_pass(&storage, &repo, &extractor, &config, &cancel)
                .await;
            assert_eq!(report.parse_failures, 1);
            assert_eq!(report.gave_up, 0);
            assert_eq!(report.windows_examined, 0);
            let cursor = storage.extraction_cursor("sess-1").await.unwrap();
            assert_eq!(cursor.attempts, expected_attempts);
            assert_eq!(
                cursor.through_message_id, None,
                "a window nobody could read has not been examined"
            );
        }

        let final_pass = service
            .run_pass(&storage, &repo, &extractor, &config, &cancel)
            .await;
        assert_eq!(final_pass.gave_up, 1);
        let cursor = storage.extraction_cursor("sess-1").await.unwrap();
        assert!(
            cursor.through_message_id.is_some(),
            "after the give-up rung the walk moves past the window"
        );

        // The loss is recorded rather than silent.
        let events = repo.events().await;
        let data = events.last().unwrap().2.as_deref().unwrap_or_default();
        assert!(data.contains("\"parse_failed\":true"), "{data}");
        assert!(data.contains("\"skipped\":true"), "{data}");
    }

    /// A watermark naming a deleted message clears itself rather than freezing.
    ///
    /// `messages_after` returns `None` for an anchor that is gone -- a truncated
    /// history, an edited turn. Without a defined answer the walk either
    /// freezes on a dead watermark forever or treats "cannot find it" as
    /// "nothing after it" and skips the rest of the conversation.
    #[tokio::test]
    async fn a_deleted_anchor_clears_the_cursor_instead_of_freezing_the_walk() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        storage
            .set_extraction_cursor("sess-1", Some("a-message-that-was-deleted"))
            .await
            .unwrap();
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);
        let service = service();
        let config = config();
        let cancel = CancellationToken::new();

        let reset_pass = service
            .run_pass(&storage, &repo, &extractor, &config, &cancel)
            .await;
        assert_eq!(reset_pass.cursor_resets, 1);
        assert_eq!(reset_pass.windows_examined, 0);
        assert_eq!(extractor.calls(), 0, "a reset spends no inference");
        assert_eq!(
            storage.extraction_cursor("sess-1").await.unwrap(),
            crate::user_data::domain::session::ExtractionCursor::unstarted()
        );

        // And the next pass re-walks it from the beginning.
        let walk = service
            .run_pass(&storage, &repo, &extractor, &config, &cancel)
            .await;
        assert_eq!(walk.windows_examined, 1);
    }

    /// Cancellation is checked before a window, so a pass that loses the
    /// household loses at most the call it is already in.
    #[tokio::test]
    async fn a_cancelled_pass_starts_no_further_windows() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        seed(&storage, "sess-2", 4).await;
        seed(&storage, "sess-3", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);

        let cancel = CancellationToken::new();
        cancel.cancel();
        let report = service()
            .run_pass(&storage, &repo, &extractor, &config(), &cancel)
            .await;

        assert_eq!(report.windows_examined, 0);
        assert_eq!(extractor.calls(), 0);
    }

    /// Without an embedder the pass does not run at all.
    ///
    /// Not caution: banding without an embedder is BLIND. Every candidate lands
    /// in `unscored`, so the pass would spend the single inference slot to
    /// produce the one thing this phase exists for -- a histogram -- entirely
    /// empty.
    #[tokio::test]
    async fn a_pond_with_no_embedder_never_takes_the_slot() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);

        let service = BatchExtractionService::new();
        assert!(!service.embedder_is_usable());
        let report = service
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.blocked_on.as_deref(), Some("no_embedder"));
        assert_eq!(extractor.calls(), 0);
    }

    /// An embedder that fails stands the engine down, and the lane can see it.
    ///
    /// The predicate is health, not emptiness. The rejected alternative --
    /// "stand down while any row lacks a vector" -- is a latch that can never
    /// re-open inside a process: three ordinary paths mint unembedded rows and
    /// only a one-shot startup backfill fills them.
    #[tokio::test]
    async fn an_embedder_that_fails_stands_the_engine_down_for_a_cooldown() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![(
            "Jerry waters the greenhouse before work.",
            MemoryKind::Routine,
        )]);

        let service = BatchExtractionService::new()
            .with_embedding_provider(Arc::new(BrokenEmbedder) as Arc<dyn EmbeddingProvider>);
        assert!(
            service.embedder_is_usable(),
            "a wired embedder that has not failed yet is usable"
        );

        let report = service
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.windows_examined, 1);
        assert_eq!(
            report.bands.unscored, 1,
            "a candidate nobody could score is not evidence that it is new"
        );
        assert_eq!(report.bands.fresh, 0);
        assert!(
            !service.embedder_is_usable(),
            "the next tick must find the engine stood down rather than spending the slot \
             to produce an empty histogram"
        );
    }

    /// The histogram is what this phase is FOR, so it has to separate the three
    /// bands on real wordings rather than report everything as new.
    #[tokio::test]
    async fn the_histogram_separates_a_restatement_from_something_new() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;

        // A store that already knows one thing.
        let repo = MockMemoryRepository::new();
        let embedder = HashEmbedder;
        let known = "Jerry waters the greenhouse before work.";
        let mut fragment = MemoryFragment::from_extraction(
            "existing".to_string(),
            None,
            known.to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        );
        fragment.embedding = Some(embedder.embed(known).await.unwrap());
        repo.add(fragment).await.unwrap();

        let extractor = ScriptedExtractor::yielding(vec![
            // Word-for-word: the lexical half of the Same band catches this
            // before the embedder is even asked.
            (known, MemoryKind::Routine),
            // Nothing to do with it.
            (
                "Jerry's sister Amara lives in Kisumu.",
                MemoryKind::Relationship,
            ),
        ]);

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.bands.same, 1, "bands: {:?}", report.bands);
        assert_eq!(report.bands.fresh, 1, "bands: {:?}", report.bands);
        assert_eq!(report.bands.total(), 2);
    }

    /// The window the model is shown carries what the store already knows.
    ///
    /// Without it the prompt's "Already remembered" block is empty and the
    /// build-on instruction has nothing to build on -- the model restates, and
    /// the restatement is thrown away by dedup, which is the loss this design
    /// exists to recover.
    #[tokio::test]
    async fn the_model_is_shown_what_the_store_already_knows() {
        struct Capturing(std::sync::Mutex<Vec<KnownMemory>>);

        #[async_trait]
        impl ConversationExtractor for Capturing {
            async fn extract_window(
                &self,
                window: ExtractionWindow<'_>,
            ) -> std::result::Result<WindowExtraction, ExtractionError> {
                *self.0.lock().unwrap() = window.known.to_vec();
                Ok(WindowExtraction::default())
            }
        }

        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        repo.add(MemoryFragment::from_extraction(
            "known-1".to_string(),
            None,
            "Jerry waters the greenhouse before work.".to_string(),
            MemorySegment::Preference,
            0.7,
            None,
        ))
        .await
        .unwrap();

        let extractor = Capturing(std::sync::Mutex::new(Vec::new()));
        service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;

        let shown = extractor.0.lock().unwrap().clone();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].kind_label, "preference");
        assert!(
            !shown[0].pattern,
            "nothing counts observations yet, so nothing may be shown as established"
        );
    }

    /// The block never demonstrates a label the schema forbids.
    ///
    /// The existing store predates the five-value catalogue and holds
    /// `identity`, `project` and `knowledge` rows. A small model shown
    /// `[project]` in its own context produces `"kind":"project"`, and the
    /// parser then rejects the item -- so the pond would lose a real memory
    /// because of how an old row was filed.
    #[test]
    fn the_known_block_only_ever_shows_a_label_the_model_may_choose() {
        use crate::user_data::ports::conversation_extractor::MemoryKind;
        for segment in [
            MemorySegment::Identity,
            MemorySegment::Preference,
            MemorySegment::Correction,
            MemorySegment::Relationship,
            MemorySegment::Project,
            MemorySegment::Knowledge,
            MemorySegment::Context,
        ] {
            let label = segment_label(&segment);
            assert!(
                MemoryKind::parse(label).is_some(),
                "{segment:?} renders as {label:?}, which is outside the catalogue"
            );
        }
    }

    /// A full window with no reply in it is stepped past rather than re-read
    /// forever.
    #[tokio::test]
    async fn a_full_window_of_unanswered_messages_is_stepped_past() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("sess-1".to_string()).await.unwrap();
        for i in 0..20 {
            storage
                .add_message(
                    "sess-1".to_string(),
                    SessionMessage::new(
                        format!("m{i}"),
                        "sess-1".to_string(),
                        ChatMessage::user(format!("unanswered {i}")),
                    ),
                )
                .await
                .unwrap();
        }
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.windows_examined, 0);
        assert_eq!(report.windows_skipped, 1);
        assert_eq!(extractor.calls(), 0, "there was nothing to read");
        assert_eq!(
            storage
                .extraction_cursor("sess-1")
                .await
                .unwrap()
                .through_message_id
                .as_deref(),
            Some("m19"),
            "leaving the cursor would re-read the same twenty messages on every pass"
        );
    }

    /// One window per conversation per pass, and no more conversations than the
    /// pass allows.
    #[tokio::test]
    async fn a_pass_reads_one_window_each_from_at_most_the_configured_conversations() {
        let storage = InMemorySessionStorage::new();
        for i in 0..5 {
            seed(&storage, &format!("sess-{i}"), 15).await;
        }
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.windows_examined, 3);
        let windows = extractor.windows();
        assert_eq!(windows.len(), 3);
        let sessions: BTreeSet<String> = windows
            .iter()
            .map(|w| w.split("-m").next().unwrap().to_string())
            .collect();
        assert_eq!(
            sessions.len(),
            3,
            "one window per conversation, not three windows of one"
        );
    }

    // ── The write path ───────────────────────────────────────────────────
    //
    // Everything below is the write gate, which moved here from the per-turn
    // service when that service was deleted. The assertions came with it: they
    // are about what may reach the store, and that question did not change
    // when the unit did.

    fn writing() -> BatchExtractionConfig {
        BatchExtractionConfig {
            mode: ExtractionMode::Write,
            ..config()
        }
    }

    /// Run one window over one seeded conversation and hand back the store.
    async fn write_window(
        config: &BatchExtractionConfig,
        memories: Vec<(&str, MemoryKind)>,
    ) -> (MockMemoryRepository, PassReport) {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(memories);
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                config,
                &CancellationToken::new(),
            )
            .await;
        (repo, report)
    }

    async fn stored(repo: &MockMemoryRepository) -> Vec<MemoryFragment> {
        repo.search_recent(&ProfileScope::Household, 100)
            .await
            .unwrap()
    }

    /// The cutover, in one assertion: the engine writes.
    #[tokio::test]
    async fn a_writing_pass_puts_the_window_in_the_store() {
        let (repo, report) = write_window(
            &writing(),
            vec![
                (
                    "Jerry waters the greenhouse before work.",
                    MemoryKind::Routine,
                ),
                (
                    "Jerry's sister Amara lives in Nakuru.",
                    MemoryKind::Relationship,
                ),
            ],
        )
        .await;

        assert_eq!(report.memories_written, 2);
        let rows = stored(&repo).await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|f| f.source == "extraction"));
        assert!(
            rows.iter().all(|f| f.embedding.is_some()),
            "a row written with no vector is invisible to semantic search until a sweep \
             repairs it, and the embedder was available here"
        );
    }

    /// Every kind keeps its own segment when the subject is named by name.
    ///
    /// This is the widening `names_user` needed. The prompt tells the model to
    /// write "Jerry ...", and the old literal-token gate read every one of
    /// those as a fact about somebody else: `relationship`, `preference` and
    /// `context` all demoted to `knowledge`, which is the one bin the
    /// catalogue says the extractor may never produce.
    #[tokio::test]
    async fn the_five_kinds_survive_the_write_gate_under_a_configured_name() {
        let (repo, report) = write_window(
            &writing(),
            vec![
                (
                    "Jerry's sister Amara lives in Nakuru.",
                    MemoryKind::Relationship,
                ),
                (
                    "Jerry prefers short answers with no preamble.",
                    MemoryKind::Preference,
                ),
                (
                    "Jerry runs the Jarida workshop in Nairobi.",
                    MemoryKind::Context,
                ),
            ],
        )
        .await;

        assert_eq!(
            report.memories_demoted, 0,
            "nothing should have been demoted"
        );
        let segments: BTreeSet<String> = stored(&repo)
            .await
            .iter()
            .filter_map(|f| f.segment.as_ref().map(|s| segment_label(s).to_string()))
            .collect();
        assert_eq!(
            segments,
            ["context", "preference", "relationship"]
                .iter()
                .map(|s| s.to_string())
                .collect::<BTreeSet<_>>()
        );
    }

    /// A `context` memory is Long, never Permanent.
    ///
    /// `from_extraction` would have made it Permanent by way of `Identity`,
    /// which means never decayed and never pruned. A household's circumstances
    /// change, and a row asserting a job somebody left, that nothing may ever
    /// remove, is worse than one that fades.
    #[tokio::test]
    async fn a_context_memory_is_long_lived_but_not_permanent() {
        let (repo, _) = write_window(
            &writing(),
            vec![(
                "Jerry runs the Jarida workshop in Nairobi.",
                MemoryKind::Context,
            )],
        )
        .await;
        let rows = stored(&repo).await;
        assert_eq!(rows[0].tier, Some(MemoryTier::Long));
    }

    /// A fact about somebody else is filed as knowledge, whatever label the
    /// model put on it.
    ///
    /// The measured failure: five William Ruto biography facts sat in
    /// `identity` beside the household's own home city, and 14 of 24 stored
    /// rows named the user nowhere at all. Demoted rather than rejected --
    /// a demotion is reversible by consolidation, a rejection loses the fact.
    #[tokio::test]
    async fn a_fact_about_somebody_else_is_demoted_to_knowledge() {
        let (repo, report) = write_window(
            &writing(),
            vec![(
                "William Ruto is the president of Kenya.",
                MemoryKind::Context,
            )],
        )
        .await;
        assert_eq!(report.memories_demoted, 1);
        assert_eq!(
            stored(&repo).await[0].segment,
            Some(MemorySegment::Knowledge)
        );
    }

    /// A fact the gate rejects never reaches the store.
    #[tokio::test]
    async fn a_defective_candidate_never_reaches_the_store() {
        let (repo, report) = write_window(
            &writing(),
            vec![
                ("I keep my starter in the pantry.", MemoryKind::Preference),
                ("He lives there now.", MemoryKind::Context),
            ],
        )
        .await;
        assert_eq!(report.memories_refused, 2);
        assert_eq!(report.memories_written, 0);
        assert!(stored(&repo).await.is_empty());
    }

    /// Two wordings of one thing inside a single window store once.
    #[tokio::test]
    async fn two_rewordings_in_one_window_store_once() {
        let (repo, report) = write_window(
            &writing(),
            vec![
                (
                    "Jerry keeps his sourdough starter in the pantry.",
                    MemoryKind::Preference,
                ),
                (
                    "Jerry keeps the sourdough starter in the pantry.",
                    MemoryKind::Preference,
                ),
            ],
        )
        .await;
        assert_eq!(report.memories_written, 1);
        assert_eq!(report.memories_dropped, 1);
        assert_eq!(stored(&repo).await.len(), 1);
    }

    /// A correction is the one thing dedup may never swallow.
    ///
    /// It restates the claim it fixes in almost the same words, which both
    /// measures score as a duplicate -- and dropping it leaves the STALE row
    /// standing, so the store asserts the very thing the household just took
    /// the trouble to deny.
    #[tokio::test]
    async fn a_correction_survives_a_band_that_would_drop_anything_else() {
        let note = "Jerry's sister Amara lives in Nakuru.";
        let (repo, report) = write_window(
            &writing(),
            vec![
                (note, MemoryKind::Relationship),
                (note, MemoryKind::Correction),
            ],
        )
        .await;
        assert_eq!(
            report.memories_written, 2,
            "the correction was dropped as a duplicate of the claim it overturns"
        );
        assert!(stored(&repo)
            .await
            .iter()
            .any(|f| f.segment == Some(MemorySegment::Correction)));
    }

    /// A second window does not re-store what the first one wrote.
    #[tokio::test]
    async fn a_restatement_from_a_later_window_is_dropped_and_recorded() {
        let storage = InMemorySessionStorage::new();
        // Two conversations rather than one conversation read twice. The
        // re-walk this used to use is now stopped a rung earlier, by the
        // window-keyed idempotence guard, and never reaches dedup at all --
        // see `a_re_walked_stretch_is_never_mined_a_second_time`. A restatement
        // from a genuinely different window is the case dedup is for.
        seed(&storage, "sess-1", 4).await;
        seed(&storage, "sess-2", 4).await;
        let repo = MockMemoryRepository::new();
        let note = "Jerry waters the greenhouse before work.";

        service()
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding(vec![(note, MemoryKind::Routine)]),
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            stored(&repo).await.len(),
            1,
            "the second window wrote a duplicate"
        );

        // The loss is written down, with the band and with the id it lost to,
        // so the phase that builds on a match can be judged against this one.
        let events = repo.events().await;
        let window_rows: Vec<String> = events
            .iter()
            .filter(|(_, id, _)| id.starts_with("window:"))
            .filter_map(|(_, _, data)| data.clone())
            .collect();
        assert!(
            window_rows
                .iter()
                .any(|d| d.contains("\"same\":1") && !d.contains("\"dropped_onto\":[]")),
            "the drop was not recorded with its band and the row it lost to: {window_rows:?}"
        );
    }

    // ── Dates ────────────────────────────────────────────────────────────

    /// A dated note is REFUSED, whole, and never rewritten.
    ///
    /// The change this half of the design is: the store gets the model's
    /// sentence or it gets nothing. It used to get "Jerry moved to Kisumu." --
    /// which reads perfectly and is the same edit that produced "The user
    /// swims morning." and "The user prefers model of the tractor." from
    /// sentences a word list read one token differently.
    #[tokio::test]
    async fn a_dated_note_is_refused_whole_and_never_edited() {
        let (repo, report) = write_window(
            &writing(),
            vec![("Jerry moved to Kisumu in 2019.", MemoryKind::Context)],
        )
        .await;

        assert!(
            stored(&repo).await.is_empty(),
            "a dated note reached the store; nothing may edit it into an undated one"
        );
        assert_eq!(report.memories_refused, 1);
        assert_eq!(report.memories_dated, 1);
        // The model filed no reminder, so the date is gone from the pond. That
        // is a loss, and it is counted rather than papered over.
        assert_eq!(report.memories_dates_lost, 1);
    }

    /// The date survives the refusal when the model put it where it was asked.
    ///
    /// The whole reason refusing is affordable. The shipped model files a
    /// reminder on 31 of its 36 dated windows and loses no date at all, so this
    /// is the ordinary path and the one above is the exception.
    #[tokio::test]
    async fn a_refused_note_still_yields_the_reminder_the_model_filed() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding_with_reminders(
                    vec![(
                        "Jerry has a dentist appointment next Tuesday.",
                        MemoryKind::Context,
                    )],
                    vec![("the dentist", "next Tuesday")],
                ),
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert!(stored(&repo).await.is_empty());
        assert_eq!(report.memories_dated, 1);
        assert_eq!(
            report.memories_dates_lost, 0,
            "the model filed the date as a reminder, so refusing the note cost nothing"
        );
        assert_eq!(report.reminders_captured, 1);
    }

    /// Two dated notes, one reminder: the note the reminder is not about is a
    /// LOSS, and the pond says so.
    ///
    /// The failure this accounting was rewritten for. The window test read
    /// "some reminder landed" and applied it to both notes, so the tractor left
    /// the pond with dates_lost at 0 and the panel printing a reassurance.
    #[tokio::test]
    async fn a_second_dated_note_with_no_reminder_of_its_own_is_counted_lost() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let reminders = Arc::new(MockReminderRepository::new());

        let report = service_storing_into(reminders.clone())
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding_with_reminders(
                    vec![
                        (
                            "Jerry has a dentist appointment next Tuesday.",
                            MemoryKind::Context,
                        ),
                        (
                            "Jerry is collecting the tractor on 3 March.",
                            MemoryKind::Context,
                        ),
                    ],
                    vec![("the dentist", "next Tuesday")],
                ),
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert!(stored(&repo).await.is_empty(), "both notes carry a date");
        assert_eq!(report.memories_dated, 2);
        assert_eq!(
            report.reminders_written, 1,
            "the model filed one reminder, about the dentist"
        );
        assert_eq!(
            report.memories_dates_lost, 1,
            "the tractor date reached no reminder and is gone; only the dentist was kept"
        );
        assert_eq!(reminders.rows().len(), 1);
    }

    /// Both dated notes covered, and neither is reported lost.
    ///
    /// The control for the test above: the per-note rule must not turn every
    /// window with more than one date into a false alarm.
    #[tokio::test]
    async fn two_dated_notes_with_a_reminder_each_lose_nothing() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding_with_reminders(
                    vec![
                        (
                            "Jerry has a dentist appointment next Tuesday.",
                            MemoryKind::Context,
                        ),
                        (
                            "Jerry is collecting the tractor on 3 March.",
                            MemoryKind::Context,
                        ),
                    ],
                    vec![
                        ("the dentist", "next Tuesday"),
                        ("collecting the tractor", "3 March"),
                    ],
                ),
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.memories_dated, 2);
        assert_eq!(report.reminders_written, 2);
        assert_eq!(
            report.memories_dates_lost, 0,
            "each date has a reminder of its own; nothing was lost"
        );
    }

    /// A habit keeps its timing and reaches the store word for word.
    ///
    /// The two-sided control, in the engine rather than in the domain: a date
    /// rule that holds its leak rate at zero by destroying every weekday it
    /// sees has thrown away the pattern the extractor exists to find. The
    /// models keep writing this shape: 12 recurring windows of 12 across the
    /// shipped model and the larger one, at greedy and at both seeds.
    #[tokio::test]
    async fn a_recurrence_reaches_the_store_verbatim() {
        let note = "Jerry swims at the club each Saturday morning.";
        let (repo, report) = write_window(&writing(), vec![(note, MemoryKind::Routine)]).await;

        let rows = stored(&repo).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].content, note,
            "the habit reached the store as something other than what was said"
        );
        assert_eq!(report.memories_dated, 0);
    }

    /// A note whose whole content was a date is not a memory at all.
    ///
    /// The one the old gate got wrong in the other direction: it was filed as
    /// short-lived `Context` and injected for a few days. Under this design it
    /// is a reminder, and a reminder is never a memory row -- a stored "on
    /// Tuesday" is read back months later as a claim about a Tuesday that has
    /// gone.
    #[tokio::test]
    async fn a_note_that_was_only_a_date_becomes_a_reminder_and_not_a_row() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding_with_reminders(
                    vec![("Next Tuesday at 09:00.", MemoryKind::Context)],
                    vec![("the appointment", "next Tuesday at 09:00")],
                ),
                &writing(),
                &CancellationToken::new(),
            )
            .await;
        assert!(stored(&repo).await.is_empty());
        assert_eq!(report.memories_refused, 1);
        assert_eq!(report.reminders_captured, 1);
    }

    /// The whole point of the date rule, and the half that was missing: the
    /// date the pond refused to remember is in the store, readable back, in the
    /// household's own words.
    #[tokio::test]
    async fn a_refused_note_leaves_a_reminder_that_can_be_read_back() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let reminders = Arc::new(MockReminderRepository::new());

        let report = service_storing_into(reminders.clone())
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding_with_reminders(
                    vec![(
                        "Jerry has a dentist appointment next Tuesday.",
                        MemoryKind::Context,
                    )],
                    vec![("the dentist", "next Tuesday")],
                ),
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert!(
            stored(&repo).await.is_empty(),
            "the dated note is still refused as a memory"
        );
        assert_eq!(report.reminders_written, 1);
        assert_eq!(report.reminders_lost, 0);

        let rows = reminders
            .list_pending(
                &crate::user_data::domain::profile::ProfileScope::Household,
                10,
            )
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "the date must be somewhere, and this is where"
        );
        assert_eq!(rows[0].about, "the dentist");
        assert_eq!(
            rows[0].when_said, "next Tuesday",
            "the subject's own words, unparsed -- a resolved date here would be a guess"
        );
        // Provenance: the household can ask where this came from and be told.
        assert_eq!(rows[0].session_id, "sess-1");
        assert!(
            !rows[0].window_id.is_empty(),
            "a row written without its window can never be attributed later"
        );
    }

    /// The engine re-walks. The same reminder must not accumulate.
    ///
    /// The cursor is cleared between passes, which is exactly what a watermark
    /// naming a deleted message does, and the window-level guard does not cover
    /// this case: `already_mined` only steps past a window that WROTE a memory,
    /// and this window's note was refused for its date.
    ///
    /// What is under test here is the engine's half of the bargain -- that a
    /// second walk hands the store the same `(window_id, about)` a first walk
    /// did, so the key can hold. The constraint itself is SQL and is tested
    /// against the real table in `sqlite_reminder.rs`.
    #[tokio::test]
    async fn re_walking_a_window_does_not_file_the_reminder_twice() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let reminders = Arc::new(MockReminderRepository::new());
        let service = service_storing_into(reminders.clone());
        let extractor = ScriptedExtractor::yielding_with_reminders(
            vec![(
                "Jerry has a dentist appointment next Tuesday.",
                MemoryKind::Context,
            )],
            vec![("the dentist", "next Tuesday")],
        );

        let first = service
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &writing(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(first.reminders_written, 1);

        // The anchor message is gone, so the walk starts over -- the one path
        // that reads a stretch of conversation the pond has already read.
        storage.set_extraction_cursor("sess-1", None).await.unwrap();

        let second = service
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            second.reminders_written, 0,
            "the second walk wrote a second row for one appointment"
        );
        assert_eq!(
            second.reminders_lost, 0,
            "a duplicate is not a loss -- the date is in the store either way"
        );
        assert_eq!(reminders.rows().len(), 1);
        assert_eq!(
            second.memories_dates_lost, 0,
            "the date was already kept, so refusing the note a second time cost nothing"
        );
    }

    /// A store that will not take the row is counted, not swallowed.
    ///
    /// This is the failure with no other symptom: the note is refused, the
    /// candidate is counted as captured, and the date is gone. Before this the
    /// pond reported that exact state as a clean pass.
    #[tokio::test]
    async fn a_reminder_that_cannot_be_stored_is_counted_as_a_loss() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();

        let report = service_storing_into(Arc::new(FailingReminderRepository))
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding_with_reminders(
                    vec![(
                        "Jerry has a dentist appointment next Tuesday.",
                        MemoryKind::Context,
                    )],
                    vec![("the dentist", "next Tuesday")],
                ),
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.reminders_captured, 1, "the model did its half");
        assert_eq!(report.reminders_written, 0);
        assert_eq!(
            report.reminders_lost, 1,
            "a write that failed must be a number somebody can read"
        );
        assert_eq!(
            report.memories_dates_lost, 1,
            "no row landed, so the date is gone -- whatever the model answered"
        );
    }

    /// A pond with nowhere to put a date loses it, and says so.
    ///
    /// Wiring is a thing that can be forgotten, and forgetting it here is
    /// invisible from every other angle.
    #[tokio::test]
    async fn an_engine_with_no_reminder_store_reports_the_loss() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let service = BatchExtractionService::new()
            .with_embedding_provider(Arc::new(HashEmbedder) as Arc<dyn EmbeddingProvider>);

        let report = service
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding_with_reminders(
                    vec![(
                        "Jerry has a dentist appointment next Tuesday.",
                        MemoryKind::Context,
                    )],
                    vec![("the dentist", "next Tuesday")],
                ),
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.reminders_lost, 1);
        assert_eq!(report.memories_dates_lost, 1);
    }

    /// What the status surface says about a pass, which is what the household
    /// reads.
    #[tokio::test]
    async fn the_status_surface_carries_what_the_pass_kept_and_what_it_lost() {
        let mut status = ExtractionEngineStatus::default();
        status.record(
            ExtractionMode::Write,
            &PassReport {
                reminders_captured: 2,
                reminders_written: 1,
                reminders_lost: 1,
                ..Default::default()
            },
        );
        assert_eq!(status.last_pass_reminders_written, 1);
        assert_eq!(status.last_pass_reminders_lost, 1);
    }

    /// A conversation from months ago is mined for memories and never for
    /// reminders.
    #[tokio::test]
    async fn a_stale_window_is_never_asked_for_a_reminder() {
        let storage = InMemorySessionStorage::new();
        seed_at(
            &storage,
            "sess-old",
            4,
            Utc::now() - chrono::Duration::days(60),
        )
        .await;
        let repo = MockMemoryRepository::new();
        let extractor = AskRecordingExtractor::default();
        service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &writing(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(extractor.asked_for_reminders(), Some(false));

        // Vacuity control: today's conversation IS asked.
        let fresh = InMemorySessionStorage::new();
        seed(&fresh, "sess-new", 4).await;
        let asked = AskRecordingExtractor::default();
        service()
            .run_pass(
                &fresh,
                &MockMemoryRepository::new(),
                &asked,
                &writing(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(asked.asked_for_reminders(), Some(true));
    }

    // ── Subject and scope ────────────────────────────────────────────────

    /// A conversation nobody has identified, on a pond where several people
    /// live, is left unread rather than filed under one of them.
    #[tokio::test]
    async fn a_window_nobody_can_name_is_skipped_and_counted() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![(
            "Jerry waters the greenhouse before work.",
            MemoryKind::Routine,
        )]);
        let config = BatchExtractionConfig {
            roster: HouseholdRoster::new(vec![
                ("p1".to_string(), "Jerry".to_string()),
                ("p2".to_string(), "Amara".to_string()),
            ]),
            ..writing()
        };

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.sessions_unnameable, 1);
        assert_eq!(report.windows_examined, 0);
        assert_eq!(extractor.calls(), 0, "no inference was spent on it");
        assert_eq!(report.blocked_on.as_deref(), Some("unnameable_subject"));
        assert!(stored(&repo).await.is_empty());

        // And the cursor did not move, so the day somebody identifies the
        // conversation the walk picks it up from where it stopped.
        assert_eq!(
            storage
                .extraction_cursor("sess-1")
                .await
                .unwrap()
                .through_message_id,
            None
        );
    }

    /// What the pond is failing to remember is counted in full, whatever the
    /// pass had budget to look at.
    ///
    /// The case this exists for is the one the containment is worst on: a
    /// household with several members whose typed chats are identified and
    /// whose SPOKEN ones are not. The voice child is a separate process with no
    /// HTTP request, so nothing on its path can call
    /// `set_session_identity_if_stronger`, and every conversation it writes
    /// resolves unnameable forever. Skipping one is free, so the window loop
    /// used to reach only as many as the model call budget left room for and
    /// then stop: with the identified chat first in the ordering and a budget
    /// of one, the pass spent its budget, broke, and reported that nothing had
    /// been skipped -- on a pond where three conversations were being skipped
    /// permanently. The count is resolved before the ordering now, so the
    /// number is over the store.
    #[tokio::test]
    async fn every_conversation_nobody_can_name_is_counted_not_just_the_ones_the_budget_reached() {
        let storage = InMemorySessionStorage::new();
        // The identified conversation is seeded last, so it is the most
        // recently updated and `order_pass` puts it first -- which is what
        // makes the budget run out before the others are reached.
        for spoken in ["voice-1", "voice-2", "voice-3"] {
            seed(&storage, spoken, 4).await;
        }
        seed(&storage, "typed-1", 4).await;
        storage
            .set_session_identity(
                "typed-1",
                &SessionIdentity {
                    profile_id: Some("p1".to_string()),
                    source: crate::user_data::domain::session::IdentificationSource::PairedDevice,
                    confidence: None,
                },
            )
            .await
            .unwrap();

        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![(
            "Jerry waters the greenhouse before work.",
            MemoryKind::Routine,
        )]);
        let config = BatchExtractionConfig {
            roster: HouseholdRoster::new(vec![
                ("p1".to_string(), "Jerry".to_string()),
                ("p2".to_string(), "Amara".to_string()),
            ]),
            sessions_per_pass: 1,
            ..writing()
        };

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            report.windows_examined, 1,
            "the budget is one window and the identified conversation is it"
        );
        assert_eq!(
            report.sessions_unnameable, 3,
            "all three unattributed conversations are counted, not the zero of them the pass \
             had budget left to walk to"
        );
        // And the pond is not reported as blocked: it is extracting, and it is
        // also losing three conversations. Both are true and the status says
        // both.
        assert_eq!(report.blocked_on, None);
        assert_eq!(extractor.calls(), 1, "no inference was spent on the rest");
        for spoken in ["voice-1", "voice-2", "voice-3"] {
            assert_eq!(
                storage
                    .extraction_cursor(spoken)
                    .await
                    .unwrap()
                    .through_message_id,
                None,
                "{spoken} was skipped, so its watermark must still be where it was"
            );
        }

        // The status surface carries the same total, because a number that
        // only reaches a tracing line is not a surface.
        let mut status = ExtractionEngineStatus::default();
        status.record(config.mode, &report);
        assert_eq!(status.unattributed_sessions, 3);
    }

    /// An identified member's conversation is mined under their own name, and
    /// every memory from it is theirs.
    #[tokio::test]
    async fn an_identified_members_memories_are_stamped_with_their_own_name() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        storage
            .set_session_identity(
                "sess-1",
                &SessionIdentity {
                    profile_id: Some("p2".to_string()),
                    source: crate::user_data::domain::session::IdentificationSource::PairedDevice,
                    confidence: None,
                },
            )
            .await
            .unwrap();
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![(
            "Amara waters the greenhouse before work.",
            MemoryKind::Routine,
        )]);
        let config = BatchExtractionConfig {
            roster: HouseholdRoster::new(vec![
                ("p1".to_string(), "Jerry".to_string()),
                ("p2".to_string(), "Amara".to_string()),
            ]),
            ..writing()
        };

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(report.windows_examined, 1);
        assert_eq!(report.memories_demoted, 0, "Amara names Amara");
        let rows = stored(&repo).await;
        assert_eq!(rows[0].profile_id.as_deref(), Some("p2"));
        assert_eq!(rows[0].segment, Some(MemorySegment::Routine));
    }

    /// The resolution table, without a database.
    #[test]
    fn who_a_window_is_about_is_decided_by_four_rules() {
        let roster = HouseholdRoster::new(vec![
            ("p1".to_string(), "Jerry".to_string()),
            ("p2".to_string(), "Amara".to_string()),
        ]);
        let solo = HouseholdRoster::new(vec![("p1".to_string(), "Jerry".to_string())]);
        let nobody = HouseholdRoster::default();
        let configured = WindowSubject::named("Jerry");

        let identified = SessionIdentity {
            profile_id: Some("p2".to_string()),
            source: crate::user_data::domain::session::IdentificationSource::Face,
            confidence: Some(0.9),
        };

        // 1. The session names a member the roster knows.
        assert_eq!(
            resolve_window_subject(&identified, &roster, &configured),
            SubjectResolution::Named(WindowSubject::member("p2", "Amara"))
        );
        // 2. One member or none, with a configured name.
        assert_eq!(
            resolve_window_subject(&SessionIdentity::unknown(), &solo, &configured),
            SubjectResolution::Named(configured.clone())
        );
        // 3. One member or none, with no configured name.
        assert_eq!(
            resolve_window_subject(
                &SessionIdentity::unknown(),
                &nobody,
                &WindowSubject::anonymous()
            ),
            SubjectResolution::Named(WindowSubject::anonymous())
        );
        // 4. Several members, and nothing says which.
        assert_eq!(
            resolve_window_subject(&SessionIdentity::unknown(), &roster, &configured),
            SubjectResolution::Unnameable
        );
        // An identity naming a member who is gone is evidence that somebody
        // SPECIFIC was here, which is what makes the pond-wide name wrong.
        let deleted = SessionIdentity {
            profile_id: Some("p9".to_string()),
            ..SessionIdentity::unknown()
        };
        assert_eq!(
            resolve_window_subject(&deleted, &solo, &configured),
            SubjectResolution::Unnameable
        );
    }

    // ── What one pass may spend ──────────────────────────────────────────

    /// A model that cannot emit the schema costs three calls, not three hundred.
    ///
    /// `sessions_per_pass` bounded SUCCESSES: `windows_examined` is incremented
    /// as the last line of a window, after the cursor advance, so every failure
    /// path returned without counting and the loop walked the entire eligible
    /// list at one model call per conversation. Measured before this fix at 40
    /// calls where 3 were configured -- 50-75 minutes of the single inference
    /// slot on Jerry's pond, writing nothing, repeating every pass. That is the
    /// exact failure the lane exists to prevent, reintroduced inside a lane job.
    #[tokio::test]
    async fn a_pass_that_never_parses_stops_at_its_model_call_budget() {
        let storage = InMemorySessionStorage::new();
        for i in 0..40 {
            seed(&storage, &format!("sess-{i:02}"), 2).await;
        }
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::unparseable();
        let config = config();
        assert_eq!(config.model_call_budget(), 3, "guard: the shipped budget");

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            extractor.calls(),
            3,
            "the pass is bounded by model calls, whatever they come to"
        );
        assert_eq!(report.model_calls, 3);
        assert_eq!(report.parse_failures, 3);
        assert_eq!(
            report.windows_examined, 0,
            "nothing was successfully read, which is what makes the old bound vacuous"
        );
        assert_eq!(
            report.blocked_on, None,
            "a model answering unreadably is not an engine that is stopped -- the pass spent \
             its budget, the give-up rung is moving the walk on, and saying the model could \
             not be reached would be false"
        );
    }

    /// The wall clock, which the call budget cannot catch.
    ///
    /// A provider that is slow rather than wrong -- a model reloading, a
    /// timeout that resolves at 60 s -- spends the slot within its call budget
    /// and still holds it far longer than the pass was ever meant to. Checked
    /// only once a call has been paid for, so a pass always attempts at least
    /// one window and a deadline can never freeze the walk.
    #[tokio::test]
    async fn a_pass_stops_when_its_wall_clock_is_spent() {
        let storage = InMemorySessionStorage::new();
        for i in 0..3 {
            seed(&storage, &format!("sess-{i}"), 2).await;
        }
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);

        let spent = BatchExtractionConfig {
            max_pass_secs: 0,
            ..config()
        };
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &spent,
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            extractor.calls(),
            1,
            "a spent wall clock stops the pass after the window in flight, and never before \
             the first one"
        );
        assert!(report.deadline_reached);

        // Vacuity control: the same three conversations under the shipped
        // deadline are all read, so the stop above is about the CLOCK. A fresh
        // storage, because the pass above already moved one watermark to the
        // end of its conversation.
        let storage = InMemorySessionStorage::new();
        for i in 0..3 {
            seed(&storage, &format!("sess-{i}"), 2).await;
        }
        let extractor = ScriptedExtractor::yielding(vec![]);
        service()
            .run_pass(
                &storage,
                &MockMemoryRepository::new(),
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(extractor.calls(), 3);
    }

    // ── Scope ────────────────────────────────────────────────────────────

    /// Seed a memory owned by one member, embedded so it is scorable.
    async fn seed_owned(repo: &MockMemoryRepository, id: &str, owner: &str, content: &str) {
        let mut fragment = MemoryFragment::from_extraction(
            id.to_string(),
            None,
            content.to_string(),
            MemorySegment::Routine,
            0.65,
            None,
        );
        fragment.profile_id = Some(owner.to_string());
        fragment.embedding = Some(HashEmbedder.embed(content).await.unwrap());
        repo.add(fragment).await.unwrap();
    }

    /// A pond with two members, and a conversation that is one of theirs.
    async fn two_member_pond() -> (InMemorySessionStorage, BatchExtractionConfig) {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-amara", 4).await;
        storage
            .set_session_identity(
                "sess-amara",
                &SessionIdentity {
                    profile_id: Some("p-amara".to_string()),
                    source: crate::user_data::domain::session::IdentificationSource::PairedDevice,
                    confidence: None,
                },
            )
            .await
            .unwrap();
        let config = BatchExtractionConfig {
            mode: ExtractionMode::Write,
            roster: HouseholdRoster::new(vec![
                ("p-jerry".to_string(), "Jerry".to_string()),
                ("p-amara".to_string(), "Amara".to_string()),
            ]),
            ..config()
        };
        (storage, config)
    }

    /// One member's memories are never shown to the model as another's.
    ///
    /// The engine resolved a per-window subject and then read the store with
    /// `ProfileScope::Household`, which `scope_sql` renders as NO filter. So
    /// Jerry's rows came back for Amara's window and were printed under the
    /// literal header "Already remembered about Amara", with the prompt
    /// instructing the model to "write that memory again as the BETTER version
    /// of itself". A PAI-1 boundary violation and a falsehood in the prompt, in
    /// the same read.
    #[tokio::test]
    async fn one_members_window_is_never_shown_another_members_memories() {
        let (storage, config) = two_member_pond().await;
        let repo = MockMemoryRepository::new();
        seed_owned(
            &repo,
            "jerrys-row",
            "p-jerry",
            "Jerry takes his blood-pressure tablet with breakfast.",
        )
        .await;
        seed_owned(
            &repo,
            "amaras-row",
            "p-amara",
            "Amara proofs her bread overnight.",
        )
        .await;

        let extractor = KnownRecordingExtractor::default();
        service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;

        let shown = extractor.shown();
        assert_eq!(shown.len(), 1, "one window was read");
        assert!(
            !shown[0].iter().any(|note| note.contains("blood-pressure")),
            "Jerry's memory was offered to the model as something already known about \
             Amara: {:?}",
            shown[0]
        );
        // Vacuity control: her own row IS shown, so the filter above is a
        // scope and not an empty read.
        assert!(
            shown[0]
                .iter()
                .any(|note| note.contains("proofs her bread")),
            "the member's own memories must still reach the prompt: {:?}",
            shown[0]
        );
    }

    /// One member's true memory is not dropped as a duplicate of another's.
    ///
    /// The symmetric half, and the quieter one: the dedup band scored member
    /// A's candidate against member B's rows, so Amara saying a thing Jerry has
    /// already said means Amara never gets that memory at all, and nothing in
    /// the store records that she said it.
    #[tokio::test]
    async fn a_members_memory_is_not_dropped_as_a_duplicate_of_another_members() {
        let (storage, config) = two_member_pond().await;
        let repo = MockMemoryRepository::new();
        let note = "The user keeps the sourdough starter in the pantry.";
        seed_owned(&repo, "jerrys-row", "p-jerry", note).await;

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &ScriptedExtractor::yielding(vec![(note, MemoryKind::Routine)]),
                &config,
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            report.memories_written, 1,
            "the same true sentence about two different people is two memories, not one"
        );
        assert_eq!(report.memories_dropped, 0);
        let hers: Vec<MemoryFragment> = stored(&repo)
            .await
            .into_iter()
            .filter(|f| f.profile_id.as_deref() == Some("p-amara"))
            .collect();
        assert_eq!(hers.len(), 1);
        assert_eq!(hers[0].content, note);
    }

    // ── What the banner may say ──────────────────────────────────────────

    /// One transient timeout does not tell the household extraction is stopped.
    ///
    /// `blocked_on` was a plain assignment inside the per-window loop and was
    /// never cleared, so a single provider timeout in window 1 survived two
    /// successful windows and rendered as "Memory extraction is stopped: the
    /// language model could not be reached on the last pass" -- in the same
    /// pass that wrote to the household's store. The inverse of what the banner
    /// is for.
    #[tokio::test]
    async fn a_transient_provider_failure_does_not_report_a_working_pass_as_stopped() {
        let storage = InMemorySessionStorage::new();
        for i in 0..3 {
            seed(&storage, &format!("sess-{i}"), 2).await;
        }
        let repo = MockMemoryRepository::new();
        let extractor = FlakyExtractor::failing_first(
            1,
            vec![(
                "Jerry waters the greenhouse before work.",
                MemoryKind::Routine,
            )],
        );

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(extractor.calls(), 3);
        assert_eq!(report.provider_failures, 1);
        assert!(
            report.windows_examined >= 1 && report.memories_written >= 1,
            "guard: the later windows really did work ({report:?})"
        );
        assert_eq!(
            report.blocked_on, None,
            "a pass that wrote to the store is not a stopped engine"
        );
    }

    /// A pass where EVERY window failed still says so.
    ///
    /// The other direction, and the reason `blocked_on` is computed rather than
    /// deleted: a banner that never speaks is as dishonest as one that always
    /// does.
    #[tokio::test]
    async fn a_pass_that_never_reached_the_model_says_so() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 2).await;
        let repo = MockMemoryRepository::new();
        let extractor = FlakyExtractor::failing_first(99, vec![]);

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &writing(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.blocked_on.as_deref(), Some("provider_error"));
    }

    // ── Idempotence ──────────────────────────────────────────────────────

    /// A re-walked stretch is never read a second time.
    ///
    /// The guard was written and never read: every stored row carried a window
    /// key and nothing anywhere queried it. A guard that is only written is
    /// worse than none, because it reads as protection that is not there -- and
    /// the trigger is routine. `DELETE /sessions/{id}/messages/{mid}` removes
    /// that message and everything after it, which is exactly where the
    /// watermark sits once extraction has caught up; `messages_after` then
    /// returns `Ok(None)`, the cursor is cleared, and the conversation sorts to
    /// the FRONT of the backlog to be walked again from message one.
    #[tokio::test]
    async fn a_re_walked_stretch_is_never_mined_a_second_time() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 2).await;
        let repo = MockMemoryRepository::new();
        let note = "Jerry waters the greenhouse before work.";

        let first = ScriptedExtractor::yielding(vec![(note, MemoryKind::Routine)]);
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &first,
                &writing(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.memories_written, 1, "guard: the first pass mined it");

        // What a cleared watermark does.
        storage.set_extraction_cursor("sess-1", None).await.unwrap();

        let second = ScriptedExtractor::yielding(vec![(note, MemoryKind::Routine)]);
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &second,
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            second.calls(),
            0,
            "the re-walk paid for a window this engine had already mined"
        );
        assert_eq!(report.windows_already_mined, 1);
        assert_eq!(stored(&repo).await.len(), 1);
        // And the walk moved on rather than sitting on the same watermark.
        assert_eq!(
            storage
                .extraction_cursor("sess-1")
                .await
                .unwrap()
                .through_message_id
                .as_deref(),
            Some("sess-1-m3")
        );
    }

    /// A window that produced nothing IS read again.
    ///
    /// The guard is keyed on rows produced, not on the window having been
    /// looked at, and that distinction is load-bearing: a shadow pass reads
    /// every window and writes nothing, so a guard keyed on "examined" would
    /// make a pond that ran in shadow mode first permanently unable to mine its
    /// own history.
    #[tokio::test]
    async fn a_window_that_stored_nothing_is_read_again() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 2).await;
        let repo = MockMemoryRepository::new();
        let note = "Jerry waters the greenhouse before work.";

        // A shadow pass: read, banded, nothing stored.
        let shadow = ScriptedExtractor::yielding(vec![(note, MemoryKind::Routine)]);
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &shadow,
                &config(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.memories_written, 0, "guard: shadow wrote nothing");

        storage.set_extraction_cursor("sess-1", None).await.unwrap();

        let writing_pass = ScriptedExtractor::yielding(vec![(note, MemoryKind::Routine)]);
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &writing_pass,
                &writing(),
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(writing_pass.calls(), 1);
        assert_eq!(report.windows_already_mined, 0);
        assert_eq!(report.memories_written, 1);
    }

    // ── The prompt budget ────────────────────────────────────────────────

    /// A pasted log is stepped past, not sent.
    ///
    /// `carve_window` fell back to the FULL untrimmed page when no complete
    /// pair fit the character budget, on the reasoning that the prompt builder
    /// would clamp it. Nothing clamped it: a 40 KB paste went to a provider
    /// with an 8192-token ceiling. The walk moves on and the loss is counted,
    /// because a window that will never fit does not get smaller by being read
    /// again.
    #[tokio::test]
    async fn an_exchange_too_large_for_the_budget_is_stepped_past_and_counted() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("sess-1".to_string()).await.unwrap();
        for (id, message) in [
            ("sess-1-m0", ChatMessage::user("x".repeat(40_000))),
            ("sess-1-m1", ChatMessage::assistant("noted".to_string())),
        ] {
            storage
                .add_message(
                    "sess-1".to_string(),
                    SessionMessage::new(id.to_string(), "sess-1".to_string(), message),
                )
                .await
                .unwrap();
        }
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![]);

        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &writing(),
                &CancellationToken::new(),
            )
            .await;

        assert_eq!(
            extractor.calls(),
            0,
            "a prompt that cannot fit the clamp was sent anyway"
        );
        assert_eq!(report.windows_oversized, 1);
        assert_eq!(
            storage
                .extraction_cursor("sess-1")
                .await
                .unwrap()
                .through_message_id
                .as_deref(),
            Some("sess-1-m1"),
            "the walk must move past an exchange it can never read, or it stalls on that \
             conversation for the life of the pond"
        );
    }

    /// The known-memories block is bounded in CHARACTERS, not only in rows.
    ///
    /// Eight rows of arbitrary-length content is an unbounded block sharing a
    /// budget with the window. One long stored memory could spend the whole of
    /// it, and nothing measured that.
    #[tokio::test]
    async fn the_known_block_is_bounded_in_characters_not_only_in_rows() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 4).await;
        let repo = MockMemoryRepository::new();
        for i in 0..KNOWN_MEMORIES_SHOWN {
            let content = format!("The user remembers thing {i}. {}", "long ".repeat(120));
            let mut fragment = MemoryFragment::from_extraction(
                format!("known-{i}"),
                None,
                content.clone(),
                MemorySegment::Routine,
                0.65,
                None,
            );
            fragment.embedding = Some(HashEmbedder.embed(&content).await.unwrap());
            repo.add(fragment).await.unwrap();
        }

        let extractor = KnownRecordingExtractor::default();
        service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config(),
                &CancellationToken::new(),
            )
            .await;

        let shown = &extractor.shown()[0];
        let chars: usize = shown.iter().map(|n| n.chars().count()).sum();
        assert!(
            shown.len() < KNOWN_MEMORIES_SHOWN,
            "eight overlong rows all fit a block budgeted at {KNOWN_MEMORIES_CHARS} characters"
        );
        assert!(
            !shown.is_empty(),
            "the most relevant row is kept whatever it costs -- an empty block is a worse \
             prompt than an overlong one"
        );
        assert!(
            chars <= KNOWN_MEMORIES_CHARS + shown[0].chars().count(),
            "the block spent {chars} characters against a budget of {KNOWN_MEMORIES_CHARS}"
        );
        // Whole rows, never a truncated one: half a remembered sentence shown
        // to a model at this size is one it finishes in its own words.
        for note in shown {
            assert!(
                note.ends_with("long "),
                "a known memory was truncated: {note:?}"
            );
        }
    }

    // ── Nothing falls between the two paths ──────────────────────────────

    /// Every surface that persists a turn is now mined, including the two that
    /// never were.
    ///
    /// `/chat` (non-streaming) and the voice loop persisted their turns through
    /// `ChatService` and never extracted from them: the per-turn path was wired
    /// onto the two SSE handlers only, so a whole conversation held by voice
    /// contributed nothing to memory. The batch walk has no such wiring -- it
    /// reads the `sessions` table -- and the only thing that can exclude a
    /// conversation is [`is_eligible_session`]. This pins that the session ids
    /// those surfaces actually mint are not excluded.
    #[test]
    fn the_surfaces_that_never_extracted_are_eligible_now() {
        for id in [
            // The desktop and the web UI: a UUID.
            "3f1c8a2e-59d1-4a7b-9d3e-2b6f4c8a1d70",
            // The CLI and the voice child: whatever --session-id was given.
            "voice-session",
            "cli-voice-memory-test",
            // A phone through the REST API.
            "mobile-1",
        ] {
            assert!(
                is_eligible_session(id),
                "{id} is a conversation a person had, and nothing may exclude it"
            );
        }
        // The deny-list still denies. A cron line firing at 3am mints one of
        // these, and mining it reads the pond's own output back to itself as
        // though a person had said it.
        assert!(!is_eligible_session("sched-morning-brief-1757937600"));
        assert!(!is_eligible_session(PROPOSAL_SESSION_ID));
    }
}
