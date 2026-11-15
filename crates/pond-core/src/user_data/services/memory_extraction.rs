//! Background memory extraction over every stored conversation; must never block the chat stream.
//!
//! This is the write gate: adapters are not trusted to enforce their prompt (a 4B model ignores
//! parts of it), so every candidate passes [`fact_defect`] (dates included), the subject gate and
//! dedup here. A dedup match is dropped, not reinforced, and recorded with the id it lost to.
//!
//! On a multi-member pond an unidentified conversation (every voice-child one included) is never
//! mined, only counted in [`PassReport::sessions_unnameable`]: misfiling is worse than losing it.
use crate::models::domain::message::Role;
use crate::models::ports::embedding::EmbeddingProvider;
use crate::shared::domain::session_activity::SessionOrigin;
use crate::user_data::domain::interest::Observed;
use crate::user_data::domain::member_profile::{file_under, same_sentence, About};
use crate::user_data::domain::memory::{
    cosine_similarity, fact_defect, names_subject, normalise_fact_content, reminder_covers_note,
    FactDefect, MemoryEventKind, MemoryFragment, MemorySegment, MemoryTier,
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
use crate::user_data::ports::interests::InterestRepository;
use crate::user_data::ports::memory_repository::MemoryRepository;
use crate::user_data::ports::reminder_repository::ReminderRepository;
use crate::user_data::ports::session_storage::SessionStorage;
use crate::user_data::services::memory_relevance::{
    is_duplicate_content, rank_by_relevance, DEDUP_RECENT_WINDOW,
};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Characters in one window, across all its messages; usually binds before the message count.
pub const EXTRACTION_WINDOW_CHARS: usize = 6_000;

/// Fewest new messages worth a window: one is never a complete exchange.
pub const MIN_NEW_MESSAGES: u64 = 2;

/// Unparseable replies against one watermark before the walk gives up and steps past it.
pub const MAX_PARSE_ATTEMPTS: u32 = 3;

/// Known memories shown to the model, so it builds on them rather than restating them.
pub const KNOWN_MEMORIES_SHOWN: usize = 8;

/// Characters the known-memories block may spend (~200 tokens). Rows past it are dropped whole:
/// a model this size finishes a truncated sentence in its own words.
pub const KNOWN_MEMORIES_CHARS: usize = 800;

/// Wall-clock cap on one pass (~6x its budgeted inference), for calls that are slow, not many.
/// Checked between windows, so an in-flight call can overrun it.
pub const EXTRACTION_PASS_MAX_SECS: u64 = 300;

/// How many neighbours to score a candidate against when banding it.
pub const BAND_NEIGHBOURS: usize = 5;

/// How long an embed failure stands the engine down: banding without an embedder is blind.
pub const EMBED_FAILURE_COOLDOWN_SECS: u64 = 300;

/// What the engine may do; an unknown setting parses as `Shadow`, so a typo can never write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionMode {
    /// Read, band and log; write no memory.
    Shadow,
    /// Write new memories; dedup drops a match.
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

    pub fn writes(self) -> bool {
        !matches!(self, ExtractionMode::Shadow)
    }
}

/// Where a conversation's walk currently stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorState {
    /// Never examined. Walk from the first message.
    Unstarted { total: u64 },
    /// Examined up to a message that still exists, with this many after it.
    InProgress { remaining: u64, total: u64 },
    /// The watermark names a deleted message: clear the cursor and re-walk from the start.
    Reset,
}

impl CursorState {
    /// How many messages are unread and the offset they start at; `None` for `Reset`.
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

/// A conversation to read and its subject, resolved once so the window never re-resolves it.
struct WindowTarget<'a> {
    session_id: &'a str,
    subject: WindowSubject,
}

/// Whether a person had this conversation. Cron and proactive-review sessions are the pond's
/// own output, and mining them files it as facts about the user.
pub fn is_eligible_session(session_id: &str) -> bool {
    SessionOrigin::of(session_id).is_human() && session_id != PROPOSAL_SESSION_ID
}

/// Order a pass: the most recently active conversation first, so today's chat never waits, then
/// the backlog oldest-first with never-examined conversations at the front.
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

/// Result of carving a window. The failures need opposite handling: `NoExchange` waits for a
/// reply, while `Oversized` must be stepped past or the walk stalls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowCarve<'a> {
    Ready(&'a [WindowMessage]),
    /// No complete exchange yet: unanswered user messages, or only system messages.
    NoExchange,
    /// The first complete exchange alone overruns the budget; `through` is its last message.
    Oversized {
        chars: usize,
        through: &'a str,
    },
}

/// Cut a window from the messages after the cursor: the oldest prefix within `max_chars` that
/// ends on a reply. Never over budget: providers truncate from the front, losing the schema.
pub fn carve_window(messages: &[WindowMessage], max_chars: usize) -> WindowCarve<'_> {
    // Oldest prefix, never newest suffix: the cursor moves to the window's end, so anything before
    // it would never be read. The first message is always taken so an oversize gets named below.
    let mut used = 0usize;
    let mut end = 0usize;
    for (i, m) in messages.iter().enumerate() {
        let cost = m.content.chars().count();
        if used + cost > max_chars && end > 0 {
            break;
        }
        used += cost;
        end = i + 1;
    }

    if let Some(carved) = last_complete_pair(&messages[..end]) {
        return WindowCarve::Ready(carved);
    }

    // Nothing fit, so the page's FIRST complete exchange (if any) is the oversize: had it fitted,
    // the prefix above would hold it. Only that exchange is skipped.
    match first_complete_pair(messages) {
        Some(first) => WindowCarve::Oversized {
            chars: first.iter().map(|m| m.content.chars().count()).sum(),
            through: first
                .last()
                .map(|m| m.id.as_str())
                .expect("first_complete_pair never returns an empty slice"),
        },
        None => WindowCarve::NoExchange,
    }
}

/// The prefix through the first reply that follows a user message: the unit an oversize skips.
fn first_complete_pair(slice: &[WindowMessage]) -> Option<&[WindowMessage]> {
    let mut seen_user = false;
    for (i, m) in slice.iter().enumerate() {
        if m.is_user() {
            seen_user = true;
        } else if seen_user {
            return Some(&slice[..i + 1]);
        }
    }
    None
}

/// The prefix through the last reply that follows a user message, if any.
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

/// Candidates per band, measured at offer time: over stored rows alone everything reads as new,
/// since dedup already dropped the pairs the bands are about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BandHistogram {
    /// At or above the reinforce threshold, or a lexical duplicate.
    pub same: usize,
    /// Between the relate and reinforce thresholds.
    pub related: usize,
    /// Below the relate threshold.
    pub fresh: usize,
    /// No embedder, an embed failure, no comparable vectors, or a model whose cosine scale is
    /// unmeasured; not evidence the candidate is new.
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WindowOutcome {
    bands: BandHistogram,
    written: usize,
    refused: usize,
    /// Of `refused`, the ones refused for carrying a calendar date.
    dated: usize,
    /// Of `dated`, the notes no stored reminder covers, asked per note by [`reminder_covers_note`].
    dates_lost: usize,
    demoted: usize,
    reminders: usize,
    /// Of `reminders`, the ones that became a new row.
    reminders_stored: usize,
    /// Of `reminders`, the ones an earlier walk already stored: not a loss.
    reminders_deduped: usize,
    /// Of `reminders`, the ones that reached no store: the write failed or none is wired.
    reminders_lost: usize,
    /// `about` of each reminder this window has in the store, for the per-note `dates_lost` check.
    reminders_kept_about: Vec<String>,
    /// Per dropped candidate, the stored id it lost to, or `lexical` for a lexical duplicate.
    dropped_onto: Vec<String>,
    /// Interests this window added a conversation to: a new guess or one more behind a row.
    interests_noted: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Windows read by the model and banded.
    pub windows_examined: usize,
    /// Model calls made, whatever the outcome: the loop's bound, so failure paths are not free.
    pub model_calls: usize,
    /// Conversations passed over: too few new messages, or no complete exchange.
    pub windows_skipped: usize,
    /// Watermarks that named a deleted message and were cleared.
    pub cursor_resets: usize,
    /// Replies with no recoverable JSON.
    pub parse_failures: usize,
    /// Windows the walk moved past after `MAX_PARSE_ATTEMPTS` failures.
    pub gave_up: usize,
    pub memories_offered: usize,
    pub reminders_offered: usize,
    /// Interests the model named, whatever became of them.
    pub interests_offered: usize,
    /// Interests a window added its conversation to: a new guess, or evidence for a row.
    pub interests_noted: usize,
    /// Items carrying a label outside the five-value catalogue.
    pub rejected_kinds: usize,
    pub bands: BandHistogram,
    /// Memories actually written; zero in shadow mode.
    pub memories_written: usize,
    /// Candidates the write gate refused, for any defect.
    pub memories_refused: usize,
    /// Of those, the ones refused for carrying a calendar date: the whole cost of the date rule.
    pub memories_dated: usize,
    /// Of those, the notes no stored reminder covers, so the date is gone. Errs high:
    /// [`reminder_covers_note`] says no whenever it cannot match.
    pub memories_dates_lost: usize,
    /// Candidates filed as `Knowledge` because they never named the window's subject.
    pub memories_demoted: usize,
    /// Candidates dropped because the store already holds something close enough.
    pub memories_dropped: usize,
    /// Reminder candidates lifted from windows, whatever became of them; not evidence of a row.
    pub reminders_captured: usize,
    /// New reminder rows written; one an earlier walk already stored is neither written nor lost.
    pub reminders_written: usize,
    /// Reminder candidates that reached no store: the write failed, or no store is wired.
    pub reminders_lost: usize,
    /// Eligible conversations nobody can attribute on a multi-member pond, so never mined. Counted
    /// over the whole store, not just the windows the pass had budget to reach.
    pub sessions_unnameable: usize,
    /// Windows stepped past because a single exchange overruns the prompt budget.
    pub windows_oversized: usize,
    /// Windows skipped because that exact stretch already produced memories (a re-walk).
    pub windows_already_mined: usize,
    /// Windows where the provider was called and failed.
    pub provider_failures: usize,
    /// Whether any window found no provider at all.
    pub no_provider: bool,
    /// Whether the pass stopped because its wall clock ran out.
    pub deadline_reached: bool,
    /// Why the pass could do nothing, decided once at the end from the whole pass.
    pub blocked_on: Option<String>,
}

/// What the engine did lately, for `GET /api/v1/memories/extraction-status`; resets on restart.
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
    /// Notes the last pass refused for a date, and how many no stored reminder covers.
    pub last_pass_dated: usize,
    pub last_pass_dates_lost: usize,
    /// Reminder rows the last pass wrote, and candidates it could not store: a pond fault.
    pub last_pass_reminders_written: usize,
    pub last_pass_reminders_lost: usize,
    /// Conversations in the store nobody can attribute, as the last pass counted them. Separate
    /// from `blocked_on`, which only speaks when a whole pass did nothing.
    pub unattributed_sessions: usize,
    /// Why the last pass could do nothing; without it a broken pond looks like an idle one.
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

/// Whether the embedder is worth asking: wired and not failed recently. Not "no unembedded
/// rows", which would latch shut while the embedding slot waits for quiet.
#[derive(Debug, Default)]
pub struct EmbedderHealth {
    /// When the embedder last failed; a sync `Mutex` so the lane can read it without an await.
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
            // A poisoned lock reads as unhealthy: that costs one cooldown, not a wasted slot.
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
    /// Subject from `settings.user_name` for unidentified sessions; see [`resolve_window_subject`].
    pub fallback_subject: WindowSubject,
    /// The household, as this pass sees it.
    pub roster: HouseholdRoster,
    /// Whether reminders are wanted; a window must also pass [`window_is_fresh_enough`].
    pub allow_reminders: bool,
    /// Wall-clock cap per pass, in seconds. A safety bound, deliberately not a setting; a field
    /// only so a test can shrink it.
    pub max_pass_secs: u64,
}

impl BatchExtractionConfig {
    /// Read the dials off a settings snapshot.
    pub fn from_settings(settings: &crate::user_data::domain::settings::Settings) -> Self {
        // The shipped "Friend" is a placeholder, not a name; use "the user", as the gate accepts.
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
            // Empty until `with_household`: assuming one member would stamp one name on everybody.
            roster: HouseholdRoster::default(),
            allow_reminders: settings.memory_date_proposals_enabled,
            max_pass_secs: EXTRACTION_PASS_MAX_SECS,
        }
    }

    /// Max model calls per pass, failed or not: the dial buys inference, not outcomes.
    pub fn model_call_budget(&self) -> usize {
        self.sessions_per_pass
    }

    /// Tell the pass who lives here; the caller reads the profile store once per pass.
    pub fn with_household(mut self, roster: HouseholdRoster) -> Self {
        self.roster = roster;
        self
    }
}

/// Household members as data, not a port, so subject resolution stays a pure, testable function.
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

    /// Whether the pond must tell people apart; with one member the pond-wide name is safe.
    pub fn has_several_members(&self) -> bool {
        self.members.len() > 1
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// The only member's id, when the household has exactly one.
    pub fn sole_member(&self) -> Option<&str> {
        match self.members.as_slice() {
            [(id, _)] => Some(id.as_str()),
            _ => None,
        }
    }
}

/// Who a window is about, or the refusal to guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubjectResolution {
    /// Extract, about this person, under this name.
    Named(WindowSubject),
    /// Skip the window, keep the cursor, and count it: mining it could file one member's habits
    /// under another's name, where nobody would notice.
    Unnameable,
}

/// Decide whose conversation this is from its persisted `SessionIdentity`: a known member it
/// names; with no identity, the fallback unless several members live here; else `Unnameable`.
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
        // An unknown member (deleted profile, stale row) still means someone specific was here.
        return SubjectResolution::Unnameable;
    }
    if roster.has_several_members() {
        return SubjectResolution::Unnameable;
    }
    SubjectResolution::Named(fallback.clone())
}

/// The batch engine. Settings arrive per pass, so a change applies on the next tick.
pub struct BatchExtractionService {
    embedding_provider: Option<Arc<dyn EmbeddingProvider>>,
    /// Where dated items go, since they are never memories; `None` counts every one as lost.
    reminder_repository: Option<Arc<dyn ReminderRepository>>,
    /// Where what a member cares about goes; `None` drops it.
    interest_repository: Option<Arc<dyn InterestRepository>>,
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
            interest_repository: None,
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

    pub fn with_interest_repository(mut self, repository: Arc<dyn InterestRepository>) -> Self {
        self.interest_repository = Some(repository);
        self
    }

    /// The lane's enable predicate, asked before it takes the slot: is there a working embedder?
    pub fn embedder_is_usable(&self) -> bool {
        self.embedding_provider
            .as_ref()
            .is_some_and(|p| p.available())
            && self.health.is_stale()
    }

    /// Whether the embedder's failure budget still allows another pass once it is loaded on demand.
    pub fn health_is_usable(&self) -> bool {
        self.health.is_stale()
    }

    /// Run one pass: up to `sessions_per_pass` windows, one per conversation.
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
            // Extracting blind is pointless: without an embedder every candidate is unscored.
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

        // Every eligible cursor, not a prefix: `list_sessions()` is newest-first, so a count bound
        // would never reach the oldest. It is also unpaginated, so a huge store means a slow tick.
        let mut with_cursors: Vec<(SessionCandidate, WindowSubject)> = Vec::new();
        for mut candidate in candidates {
            match storage.extraction_cursor(&candidate.id).await {
                Ok(cursor) => {
                    // Given up already; not counted as skipped, or it would recount every pass.
                    if cursor.attempts >= MAX_PARSE_ATTEMPTS {
                        continue;
                    }
                    // Resolved here, over every eligible conversation, so `sessions_unnameable`
                    // counts the whole store, not just the prefix the call budget reached.
                    let identity = match storage.get_session_identity(&candidate.id).await {
                        Ok(identity) => identity,
                        Err(e) => {
                            // Treated as unknown, never guessed at.
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
                            // Cursor stays put, so the walk resumes once it is identified.
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

        // Once per pass at WARN: it is not transient, and this line is for a pond with no screen.
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

        // The subject rides beside each candidate so it is never re-resolved inside the window.
        let ordered = order_pass(with_cursors.iter().map(|(c, _)| c.clone()).collect());
        let mut subjects: std::collections::HashMap<String, WindowSubject> = with_cursors
            .into_iter()
            .map(|(candidate, subject)| (candidate.id, subject))
            .collect();

        for candidate in ordered {
            if report.model_calls >= config.model_call_budget() {
                break;
            }
            // Same number as the call budget; only reached first when every call succeeded.
            if report.windows_examined >= config.sessions_per_pass {
                break;
            }
            // For slow calls; only after the first call, so a deadline can never freeze the walk.
            if report.model_calls > 0 && started.elapsed() >= deadline {
                report.deadline_reached = true;
                tracing::debug!(
                    calls = report.model_calls,
                    secs = started.elapsed().as_secs(),
                    "[batch-extraction] pass wall clock spent; stopping"
                );
                break;
            }
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
        let WindowTarget {
            session_id,
            subject,
        } = target;
        let Some(state) = self.cursor_state(storage, session_id).await else {
            return;
        };

        let (remaining, offset) = match state {
            CursorState::Reset => {
                // Anchor gone (truncated history, edited turn): re-walk from the start. The
                // window guard and dedup keep that from duplicating rows.
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
                    // System messages are plumbing, not something anybody said.
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
                // Step past it: a provider would truncate the rules and schema or refuse the
                // call, and leaving the cursor here would stall the conversation for good.
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
                // A FULL window with no exchange is stepped past, or it is re-read forever.
                // Judged on the raw page: an all-system page filters to nothing, never full.
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

        // ── The idempotence guard ────────────────────────────────────────
        // A deleted message clears the cursor and re-walks from the start; skip windows that
        // already produced rows. Keyed on rows, not reads, so a shadow-read window is still mined.
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

        // Ask for reminders only while one could still lie ahead: a backfill reads months of
        // history, and the date can't be parsed to tell. Older windows still yield memories.
        let last_said_at = carved.last().map(|m| m.created_at).unwrap_or_else(Utc::now);
        let allow_reminders =
            config.allow_reminders && window_is_fresh_enough(last_said_at, Utc::now());

        // Counted before the call, on every path: this is the counter that bounds the pass.
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
                // Not the window's fault: no attempt, no advance; `pass_blocker` judges the pass.
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
                    // Give up: losing one window beats re-reading it forever; the loss is logged.
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
        report.interests_offered += extraction.interests.len();
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
        report.interests_noted += outcome.interests_noted;

        let _ = repo
            .log_event(
                MemoryEventKind::Extracted,
                &window_event_id(&window_id),
                Some(session_id),
                Some(&format!(
                    "{{\"window\":\"{}\",\"mode\":\"{}\",\"offered\":{},\"kept\":{},\
                     \"refused\":{},\"dated\":{},\"dates_lost\":{},\"demoted\":{},\
                     \"reminders\":{},\"reminders_stored\":{},\"reminders_lost\":{},\
                     \"rejected\":{},\"interests\":{},\
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
                    outcome.interests_noted,
                    outcome.bands.same,
                    outcome.bands.related,
                    outcome.bands.fresh,
                    outcome.bands.unscored,
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

    /// Where the walk stands in one conversation; `None` on a read error, which costs no attempt.
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

    /// `Some(kept)` if this stretch's window already stored rows, else `None` (errors included):
    /// a wasted call beats a window never mined. Keyed on the indexed `memory_id`.
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

    /// The memories to show alongside this window, ranked as turn-time retrieval ranks them, under
    /// the window's own scope so one member's memories never reach another's prompt.
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

        // Facts about the world are not the person: shown, they invite a "better version".
        pool.retain(|(fragment, _)| fragment.segment != Some(MemorySegment::Knowledge));
        rank_by_relevance(&mut pool, Utc::now());

        let mut used = 0usize;
        let mut known = Vec::new();
        for (fragment, _) in pool.into_iter().take(KNOWN_MEMORIES_SHOWN) {
            let cost = fragment.content.chars().count();
            if used + cost > KNOWN_MEMORIES_CHARS && !known.is_empty() {
                break;
            }
            // The first row always goes in: an empty block is a worse prompt than an overlong one.
            used += cost;
            known.push(KnownMemory {
                kind_label: fragment
                    .segment
                    .as_ref()
                    .map(|s| segment_label(s).to_string())
                    .unwrap_or_else(|| "context".to_string()),
                note: fragment.content,
                // No column counts observations yet, so nothing may be shown as established.
                pattern: false,
            });
        }
        known
    }

    /// Put each candidate through the write gate in one pass: banding and writing share a vector.
    #[allow(clippy::too_many_arguments)]
    async fn dispose_candidates(
        &self,
        repo: &dyn MemoryRepository,
        extraction: &WindowExtraction,
        config: &BatchExtractionConfig,
        subject: &WindowSubject,
        // Resolved once by the caller so the prompt name, row owner and dedup scope agree.
        scope: &ProfileScope,
        session_id: &str,
        window_id: &str,
        said_at: DateTime<Utc>,
        allow_reminders: bool,
    ) -> WindowOutcome {
        let mut outcome = WindowOutcome::default();

        // Reminders first, so they count even when nothing else does. Gated here too: an extractor
        // may return reminders for a stale window that was never asked for them.
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

        self.note_interests(extraction, config, subject, session_id, &mut outcome)
            .await;

        if extraction.memories.is_empty() {
            return outcome;
        }

        // Lexical half of the Same band, kept current as this window writes. Subject-scoped:
        // another member's identical row is the same fact about a different person.
        let mut recent: Vec<String> = repo
            .search_recent(scope, DEDUP_RECENT_WINDOW)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.content.to_lowercase())
            .collect();

        for memory in &extraction.memories {
            // ── The quality gate ─────────────────────────────────────────
            // A defective or dated note is refused whole, never repaired: mechanical rewrites
            // store wreckage like "The user keeps the oven."; the model's sentence or nothing.
            let content = normalise_fact_content(&memory.note);
            // A question asked is curiosity, which the interests count; never a trait.
            if describes_a_question(&content) {
                outcome.refused += 1;
                tracing::debug!(
                    session_id = %session_id,
                    "[batch-extraction] refused a candidate that only says what was asked"
                );
                continue;
            }
            // A yearly date is the one date a memory may carry.
            let yearly = memory.about.is_some_and(About::keeps_a_date);
            if let Some(defect) =
                fact_defect(&content).filter(|d| !(yearly && *d == FactDefect::CalendarDate))
            {
                outcome.refused += 1;
                if defect == FactDefect::CalendarDate {
                    outcome.dated += 1;
                    // Kept only if a stored reminder covers THIS note; the check errs toward
                    // reporting a loss. A stale window was never asked for reminders at all.
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

            // Subject kinds must name the subject. A fact about someone else is demoted, not
            // rejected: a demotion keeps the fact, and consolidation can reverse it.
            let about_the_subject = matches!(
                segment,
                MemorySegment::Identity
                    | MemorySegment::Relationship
                    | MemorySegment::Preference
                    | MemorySegment::Routine
            );
            let names_them = names_subject(&content, &subject.gate_aliases);
            // Filed in a member's profile, a fact that never names them is not theirs to keep.
            if memory.about.is_some() && !names_them {
                outcome.refused += 1;
                tracing::debug!(
                    session_id = %session_id,
                    "[batch-extraction] refused a profile fact that never named its member"
                );
                continue;
            }
            if about_the_subject && !names_them {
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
            // A correction is never deduped: it echoes the claim it fixes, and dropping it
            // would leave the stale row asserting what the household just denied.
            let is_correction = segment == MemorySegment::Correction;

            let lexical_dupe = recent.iter().any(|e| is_duplicate_content(e, &content));
            let mut matched: Option<String> = None;
            // Kept so a stored candidate is embedded once; embeddings are not free on the Orin.
            let mut vector_for_row: Option<Vec<f32>> = None;
            let band = if lexical_dupe {
                Band::Same
            } else {
                match (self.embed(&content).await, self.cosine_bands(config)) {
                    (None, _) => Band::Unscored,
                    (Some(vector), None) => {
                        vector_for_row = Some(vector);
                        Band::Unscored
                    }
                    (Some(vector), Some((reinforce, relate))) => {
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
                            // Can't tell an empty store from an unembedded one, so unscored.
                            None => Band::Unscored,
                            Some((id, sim)) => {
                                matched = Some(id);
                                if sim >= reinforce {
                                    Band::Same
                                } else if sim >= relate {
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
            // A new value for a single-valued fact is near the old one by nature, and replaces
            // it below; only the same fact said again is dropped.
            let replaces = memory.about.is_some_and(About::single_valued);
            let drop_it =
                !is_correction && (band == Band::Same || (band == Band::Related && !replaces));
            if drop_it {
                // No reinforcement yet: a match is dropped, and the id it lost to recorded.
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
                // Shadow: counted, not written; kept for measuring thresholds on real history.
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
            if let Some(about) = memory.about {
                file_under(&mut fragment, about);
                // Heard in passing, not told to the Pond: it lasts, but is never permanent.
                if fragment.tier == Some(MemoryTier::Permanent) {
                    fragment.tier = Some(MemoryTier::Long);
                    fragment.decay_rate = Some(MemoryTier::Long.default_decay_rate());
                }
            }
            // Reuse the banding vector if any. Best-effort: an embed failure leaves the row
            // for the embedding slot and never aborts the window.
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
            if let Some(about) = memory.about.filter(|a| a.single_valued()) {
                let older = repo.live_about(scope, about).await.unwrap_or_default();
                for old in older.iter().filter(|old| {
                    old.id != id
                        && old.profile_id == subject.profile_id
                        && !same_sentence(&old.content, &content)
                }) {
                    if repo.mark_superseded(&old.id, &id).await.is_ok() {
                        let _ = repo
                            .log_event(MemoryEventKind::Superseded, &old.id, Some(session_id), None)
                            .await;
                    }
                }
            }
            // Never log the fact itself: INFO lands in the on-disk log, outside the store's
            // scoping, retention and redaction. The id links the line to the row.
            tracing::info!(
                memory_id = %id,
                chars = content.chars().count(),
                "[batch-extraction] stored a memory"
            );
            // `window` is the row's provenance, needed from the first row on. The re-walk
            // guard reads the window-level row instead (see `already_mined`).
            let _ = repo
                .log_event(
                    MemoryEventKind::Extracted,
                    &id,
                    Some(session_id),
                    Some(&format!(
                        "{{\"window\":\"{}\",\"kind\":\"{}\",\"about\":\"{}\",\"band\":\"{}\"}}",
                        escape_json(window_id),
                        memory.kind.as_str(),
                        memory.about.map_or("", About::as_str),
                        band.as_str(),
                    )),
                )
                .await;
        }

        outcome
    }

    /// What the window showed the member caring about, as guesses on their interests. Only a named
    /// member's: an interest row always has an owner. The store counts a conversation once per row.
    async fn note_interests(
        &self,
        extraction: &WindowExtraction,
        config: &BatchExtractionConfig,
        subject: &WindowSubject,
        session_id: &str,
        outcome: &mut WindowOutcome,
    ) {
        let (Some(repository), Some(profile_id)) = (
            self.interest_repository.as_ref(),
            subject.profile_id.as_deref(),
        ) else {
            return;
        };
        if !config.mode.writes() {
            return;
        }
        for seen in &extraction.interests {
            match repository.observe(profile_id, session_id, seen).await {
                Ok(Observed::Created | Observed::Counted) => outcome.interests_noted += 1,
                Ok(Observed::AlreadyCounted | Observed::Avoided) => {}
                Err(e) => {
                    tracing::warn!("[batch-extraction] an interest could not be noted: {e}");
                }
            }
        }
    }

    /// Store a dated item as a reminder, never a memory. Dedup is the store's UNIQUE
    /// `(window_id, about_key)`: `already_mined` only skips windows that wrote memories.
    async fn capture_reminder(&self, candidate: ReminderCandidate, outcome: &mut WindowOutcome) {
        outcome.reminders += 1;
        // Content stays at DEBUG: INFO lands in the on-disk log, outside the store's scoping.
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
            // WARN: dates are being dropped and nothing else about it looks wrong.
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
                // Stored by an earlier walk: the date is kept.
                outcome.reminders_kept_about.push(reminder.about.clone());
                tracing::debug!(
                    window_id = %reminder.window_id,
                    "[batch-extraction] this window's reminder is already stored"
                );
            }
            Err(e) => {
                outcome.reminders_lost += 1;
                // Never log the content; the id, window and error are what's needed.
                tracing::warn!(
                    reminder_id = %reminder.id,
                    window_id = %reminder.window_id,
                    "[batch-extraction] a reminder could not be stored, so the date is lost: {e}"
                );
            }
        }
    }

    /// `(reinforce, relate)` on the embedder's own scale. The dials are cosines on MiniLM's, so
    /// another measured model brings its bands, and an unmeasured one dedups lexically alone.
    fn cosine_bands(&self, config: &BatchExtractionConfig) -> Option<(f32, f32)> {
        let floors = self.embedding_provider.as_ref()?.similarity_floors()?;
        Some(
            floors
                .dedup
                .map_or((config.reinforce_threshold, config.relate_threshold), |b| {
                    (b.reinforce, b.relate)
                }),
        )
    }

    /// Embed, recording health; `None` whether there is no embedder or it failed.
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

pub use crate::user_data::domain::member_profile::describes_a_question;

/// Why a finished pass could do nothing, or `None` once any window was read. Parse failures set
/// nothing: a model answering badly is not a stopped engine.
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

/// `memory_id` of a window's audit row, prefixed so it can't collide with a fragment UUID;
/// [`BatchExtractionService::already_mined`] reads it through `idx_memory_events_memory_id`.
fn window_event_id(window_id: &str) -> String {
    format!("window:{window_id}")
}

/// Escape a value going into a hand-built JSON string.
fn escape_json(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A stored segment's label in the known block, mapped onto the five-value catalogue: a small
/// model shown a legacy label like `project` will produce one.
fn segment_label(segment: &MemorySegment) -> &'static str {
    match segment {
        MemorySegment::Relationship => "relationship",
        MemorySegment::Preference => "preference",
        MemorySegment::Correction => "correction",
        MemorySegment::Routine => "routine",
        MemorySegment::Identity | MemorySegment::Context => "context",
        // Not in the catalogue: a project is nearest a routine, a taught fact nearest context.
        MemorySegment::Project => "routine",
        MemorySegment::Knowledge | MemorySegment::Place | MemorySegment::Date => "context",
        MemorySegment::FoodHealth | MemorySegment::Instruction => "preference",
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use crate::models::domain::embedding_models::{DedupBands, SimilarityFloors, ALL_MINILM_L6_V2};
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

    /// Deterministic bag-of-tokens embedder; `MockEmbeddingProvider`'s zero vector would pass every
    /// band assertion for the wrong reason.
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

        /// Stands in for the shipped MiniLM, whose scale the relate and reinforce dials are on.
        fn similarity_floors(&self) -> Option<SimilarityFloors> {
            ALL_MINILM_L6_V2.floors
        }
    }

    /// Every text lands on one vector, so every cosine is 1.0 and only the floors decide.
    struct ConstantEmbedder(Option<SimilarityFloors>);

    #[async_trait]
    impl EmbeddingProvider for ConstantEmbedder {
        async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
            Ok(vec![1.0, 0.0, 0.0, 0.0])
        }

        fn dimensions(&self) -> usize {
            4
        }

        fn similarity_floors(&self) -> Option<SimilarityFloors> {
            self.0
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
                            about: None,
                        })
                        .collect(),
                    reminders: Vec::new(),
                    interests: Vec::new(),
                    rejected: 0,
                }),
                calls: AtomicUsize::new(0),
                windows: std::sync::Mutex::new(Vec::new()),
            }
        }

        /// The same, plus the reminders the model filed in the same reply.
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

    /// The engine as a pond runs it; without a reminder store every test would report lost dates.
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

    /// Fails the first `failures` calls and answers every later one, like a model reloading.
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
                            about: None,
                        })
                        .collect(),
                    reminders: Vec::new(),
                    interests: Vec::new(),
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

    /// The messages of a `Ready` carve; panics with whatever else it produced.
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

    /// The value comes from a settings row: a typo must never start writing to permanent memory.
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

        // Vacuity control: the writing modes do parse, so the above isn't a one-value parser.
        assert_eq!(ExtractionMode::parse("write"), ExtractionMode::Write);
        assert_eq!(
            ExtractionMode::parse("  Reinforce  "),
            ExtractionMode::Reinforce
        );
    }

    /// A shadow default would read the whole history and silently remember nothing.
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

    #[test]
    fn a_window_with_no_reply_in_it_carves_nothing() {
        let messages = vec![
            msg("m1", "user", "are you there"),
            msg("m2", "user", "hello"),
        ];
        assert_eq!(carve_window(&messages, 6_000), WindowCarve::NoExchange);
    }

    #[test]
    fn the_character_budget_keeps_the_oldest_end_and_leaves_the_rest() {
        let messages = vec![
            msg("m1", "user", &"a".repeat(100)),
            msg("m2", "assistant", &"b".repeat(100)),
            msg("m3", "user", &"c".repeat(500)),
            msg("m4", "assistant", &"d".repeat(500)),
        ];
        let carved = ready(carve_window(&messages, 400));
        let ids: Vec<&str> = carved.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["m1", "m2"],
            "the oldest exchange that fits is read first"
        );
    }

    /// Walked as the cursor walks, every message is either read or named as an oversize.
    #[test]
    fn walking_a_page_leaves_no_message_behind_the_cursor_unread() {
        let messages = vec![
            msg("u1", "user", &"a".repeat(100)),
            msg("a1", "assistant", &"b".repeat(100)),
            msg("u2", "user", &"c".repeat(100)),
            msg("a2", "assistant", &"d".repeat(100)),
            msg("u3", "user", &"e".repeat(3_000)),
            msg("a3", "assistant", &"f".repeat(3_000)),
            msg("u4", "user", &"g".repeat(100)),
            msg("a4", "assistant", &"h".repeat(100)),
        ];
        let (mut read, mut skipped) = (Vec::new(), Vec::new());
        let mut from = 0usize;
        while from < messages.len() {
            let page = &messages[from..];
            let through = match carve_window(page, 450) {
                WindowCarve::Ready(carved) => {
                    read.extend(carved.iter().map(|m| m.id.clone()));
                    carved.last().unwrap().id.clone()
                }
                WindowCarve::Oversized { through, .. } => {
                    let upto = page.iter().position(|m| m.id == through).unwrap() + 1;
                    skipped.extend(page[..upto].iter().map(|m| m.id.clone()));
                    through.to_string()
                }
                WindowCarve::NoExchange => break,
            };
            from += page.iter().position(|m| m.id == through).unwrap() + 1;
        }
        assert_eq!(read, vec!["u1", "a1", "u2", "a2", "u4", "a4"]);
        assert_eq!(
            skipped,
            vec!["u3", "a3"],
            "only the exchange that is genuinely too long is skipped -- not the short ones \
             around it"
        );
    }

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

        // Vacuity control: with room for it the same pair carves, so the refusal is about SIZE.
        assert!(matches!(
            carve_window(&messages, 10_000),
            WindowCarve::Ready(_)
        ));
    }

    /// Couples the three costs to one budget: growing either block fails here, not at a provider.
    #[test]
    fn the_window_and_the_known_block_fit_the_prompt_budget_together() {
        use crate::user_data::ports::conversation_extractor::{
            CHARS_PER_TOKEN, EXTRACTION_PROMPT_BUDGET_TOKENS,
        };

        // Mirrors the adapter's system-prompt cap: pond-core may not depend on the adapter.
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
        // A deleted anchor has no offset to give; guessing one silently re-reads or skips.
        assert_eq!(CursorState::Reset.unread(), None);
    }

    // ── The pass ─────────────────────────────────────────────────────────

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

        // Still leaves one audit row per window, so the histogram is recoverable later.
        let events = repo.events().await;
        assert_eq!(events.len(), 1);
        assert!(events[0].1.starts_with("window:"));
        let data = events[0].2.as_deref().unwrap_or_default();
        assert!(data.contains("\"mode\":\"shadow\""), "{data}");
        assert!(data.contains("\"offered\":2"), "{data}");
        assert!(data.contains("\"kept\":0"), "{data}");
    }

    /// Advancing only on output would re-read every uneventful conversation forever.
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

    #[tokio::test]
    async fn the_walk_moves_forward_and_then_stops() {
        let storage = InMemorySessionStorage::new();
        // 30 messages: more than one 20-message window, carving to whole pairs.
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

    fn fielder_seen() -> crate::user_data::domain::interest::ObservedInterest {
        crate::user_data::domain::interest::ObservedInterest {
            topic: "Toyota Fielder".to_string(),
            category: "cars".to_string(),
            facets: vec!["Kilimall".to_string()],
            signal: crate::user_data::domain::interest::Signal::Does,
        }
    }

    /// The member's complaint: interests count conversations, not windows.
    #[tokio::test]
    async fn an_interest_counts_once_per_conversation_however_many_windows_show_it() {
        use crate::user_data::mocks::mock_interests::InMemoryInterestRepository;
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 15).await;
        seed(&storage, "sess-2", 3).await;
        let repo = MockMemoryRepository::new();
        let mut extractor = ScriptedExtractor::yielding(vec![]);
        if let Ok(extraction) = &mut extractor.reply {
            extraction.interests = vec![fielder_seen()];
        }
        let interests = Arc::new(InMemoryInterestRepository::new());
        let service = service().with_interest_repository(interests.clone());
        let mut config = config();
        config.mode = ExtractionMode::Write;
        config.fallback_subject = WindowSubject::member("p-jerry", "Jerry");
        let cancel = CancellationToken::new();

        for _ in 0..3 {
            service
                .run_pass(&storage, &repo, &extractor, &config, &cancel)
                .await;
        }
        assert!(
            extractor.calls() >= 3,
            "both windows of sess-1 and sess-2 were read"
        );
        let rows = interests.all();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].profile_id, "p-jerry");
        assert_eq!(
            rows[0].evidence, 2,
            "two conversations, however many windows"
        );
        assert!(!rows[0].confirmed);
    }

    #[tokio::test]
    async fn interests_are_never_noted_for_nobody_or_in_shadow() {
        use crate::user_data::mocks::mock_interests::InMemoryInterestRepository;
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 3).await;
        let repo = MockMemoryRepository::new();
        let mut extractor = ScriptedExtractor::yielding(vec![]);
        if let Ok(extraction) = &mut extractor.reply {
            extraction.interests = vec![fielder_seen()];
        }
        let interests = Arc::new(InMemoryInterestRepository::new());
        let service = service().with_interest_repository(interests.clone());
        let mut config = config();
        config.mode = ExtractionMode::Write;
        let report = service
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.interests_offered, 1);
        assert_eq!(
            report.interests_noted, 0,
            "an unattributed subject owns no rows"
        );
        assert!(interests.all().is_empty());
    }

    #[tokio::test]
    async fn a_note_that_only_says_what_was_asked_is_never_stored() {
        let storage = InMemorySessionStorage::new();
        seed(&storage, "sess-1", 3).await;
        let repo = MockMemoryRepository::new();
        let extractor = ScriptedExtractor::yielding(vec![
            (
                "Jerry inquires about the new psychological thriller titled Primetime.",
                MemoryKind::Preference,
            ),
            ("Jerry is lactose intolerant.", MemoryKind::Context),
        ]);
        let mut config = config();
        config.mode = ExtractionMode::Write;
        let report = service()
            .run_pass(
                &storage,
                &repo,
                &extractor,
                &config,
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(report.memories_written, 1);
        assert_eq!(report.memories_refused, 1);
        let stored = repo
            .search_recent(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert!(stored.iter().all(|m| !m.content.contains("inquires")));
    }

    #[test]
    fn asking_about_something_is_told_apart_from_doing_it() {
        for asked in [
            "Jerry inquires about the new psychological thriller titled Primetime.",
            "Jerry asked about the plot of Avengers Doomsday.",
            "Jerry wants to know who directed Primetime.",
            "Jerry is curious about P vs NP.",
        ] {
            assert!(describes_a_question(asked), "{asked}");
        }
        for fact in [
            "Jerry is lactose intolerant.",
            "Jerry asks Emmanuel to cook on Fridays.",
            "Jerry is shopping for a Toyota Fielder under KSh 1.2M.",
        ] {
            assert!(!describes_a_question(fact), "{fact}");
        }
    }

    /// One failure must not lose the window, and endless failures must not stall the backlog.
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

    /// `messages_after` answers `None` for a deleted anchor; that must not freeze or skip the walk.
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
            // Word-for-word: the lexical half of the Same band catches this.
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

    /// MiniLM drops a candidate its dials match. A model with its own bands is judged on its own
    /// scale, and one with no measured floors drops only what the lexical check calls a repeat.
    #[tokio::test]
    async fn dedup_reads_the_bands_of_the_model_in_use() {
        let known = "Jerry waters the greenhouse before work.";
        let unrelated = "Jerry's sister Amara lives in Kisumu.";
        // Above every cosine, so a model on this scale calls even an identical vector fresh.
        let own_scale = Some(SimilarityFloors {
            tool_relevance: 0.5,
            closest_memory: 0.5,
            dedup: Some(DedupBands {
                reinforce: 1.01,
                relate: 1.01,
            }),
        });
        for (floors, stored) in [
            (ALL_MINILM_L6_V2.floors, None),
            (own_scale, Some("fresh")),
            (None, Some("unscored")),
        ] {
            let storage = InMemorySessionStorage::new();
            seed(&storage, "sess-1", 4).await;
            let repo = MockMemoryRepository::new();
            let mut fragment = MemoryFragment::from_extraction(
                "existing".to_string(),
                None,
                known.to_string(),
                MemorySegment::Preference,
                0.7,
                None,
            );
            fragment.embedding = Some(vec![1.0, 0.0, 0.0, 0.0]);
            repo.add(fragment).await.unwrap();

            let extractor = ScriptedExtractor::yielding(vec![
                (known, MemoryKind::Routine),
                (unrelated, MemoryKind::Relationship),
            ]);
            let service = BatchExtractionService::new()
                .with_embedding_provider(
                    Arc::new(ConstantEmbedder(floors)) as Arc<dyn EmbeddingProvider>
                )
                .with_reminder_repository(Arc::new(MockReminderRepository::new()));
            let report = service
                .run_pass(
                    &storage,
                    &repo,
                    &extractor,
                    &BatchExtractionConfig {
                        mode: ExtractionMode::Write,
                        ..config()
                    },
                    &CancellationToken::new(),
                )
                .await;

            let rows = repo
                .search_recent(&ProfileScope::Household, 10)
                .await
                .unwrap();
            let kept = rows.iter().find(|f| f.content.contains("Amara"));
            if let Some(band) = stored {
                assert_eq!(
                    report.bands.same, 1,
                    "only the lexical repeat: {:?}",
                    report.bands
                );
                let counted = match band {
                    "fresh" => report.bands.fresh,
                    _ => report.bands.unscored,
                };
                assert_eq!(counted, 1, "{band}: {:?}", report.bands);
                let kept = kept.expect("the unrelated fact is stored");
                assert!(kept.embedding.is_some(), "it is stored with its vector");
            } else {
                assert_eq!(report.bands.same, 2, "bands: {:?}", report.bands);
                assert!(
                    kept.is_none(),
                    "a cosine of 1.0 is the same memory to MiniLM"
                );
            }
        }
    }

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

    /// The prompt has the model write "Jerry ...", so the subject gate must accept the name.
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

    /// Via `Identity` it would be Permanent, never pruned; circumstances change, so it must fade.
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

    #[tokio::test]
    async fn a_restatement_from_a_later_window_is_dropped_and_recorded() {
        let storage = InMemorySessionStorage::new();
        // Two conversations: a re-walk of one would stop at the idempotence guard, before dedup.
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

        // The drop is recorded with the id it lost to.
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
        // No reminder was filed, so the date is lost, and counted.
        assert_eq!(report.memories_dates_lost, 1);
    }

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

    /// Control for the test above: the per-note rule must not raise false alarms.
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

    /// Control: a date rule that ate every weekday would lose the habits extraction exists for.
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

    /// A stored "on Tuesday" is read back months later as a claim about a Tuesday long gone.
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

    /// `already_mined` skips only windows that wrote a memory, so the store's key must dedup; the
    /// UNIQUE constraint itself is tested in `sqlite_reminder.rs`.
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

        // As if the anchor were deleted: the walk starts over.
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

    /// A months-old conversation is still mined for memories, but never asked for reminders.
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

    /// On a multi-member pond, an unidentified conversation is left unread, not filed under one.
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

        // The cursor stays put, so the walk resumes once somebody identifies the conversation.
        assert_eq!(
            storage
                .extraction_cursor("sess-1")
                .await
                .unwrap()
                .through_message_id,
            None
        );
    }

    #[tokio::test]
    async fn every_conversation_nobody_can_name_is_counted_not_just_the_ones_the_budget_reached() {
        let storage = InMemorySessionStorage::new();
        // Seeded last, so `order_pass` puts it first and the budget runs out before the others.
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
        // Not blocked: it is extracting, and also losing three conversations.
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

        // The status surface carries the same total.
        let mut status = ExtractionEngineStatus::default();
        status.record(config.mode, &report);
        assert_eq!(status.unattributed_sessions, 3);
    }

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
        // A deleted member's identity still means someone specific was here.
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

        // Vacuity control, on fresh storage: under the shipped deadline all three are read.
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

    /// `ProfileScope::Household` is no filter at all in `scope_sql`; reads use the window's scope.
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
        // Vacuity control: her own row IS shown, so this is a scope, not an empty read.
        assert!(
            shown[0]
                .iter()
                .any(|note| note.contains("proofs her bread")),
            "the member's own memories must still reach the prompt: {:?}",
            shown[0]
        );
    }

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

    /// Control for the test above: a banner that never speaks is as wrong as one that always does.
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
        // Whole rows, never a truncated one.
        for note in shown {
            assert!(
                note.ends_with("long "),
                "a known memory was truncated: {note:?}"
            );
        }
    }

    // ── Every surface is eligible ────────────────────────────────────────

    /// Only [`is_eligible_session`] can exclude a surface; these are the ids surfaces mint.
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
        // The deny-list still denies the pond's own sessions.
        assert!(!is_eligible_session("sched-morning-brief-1757937600"));
        assert!(!is_eligible_session(PROPOSAL_SESSION_ID));
    }
}
