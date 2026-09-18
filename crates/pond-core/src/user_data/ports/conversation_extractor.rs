//! ConversationExtractor port — read one window of a conversation and say what
//! is worth remembering about the person in it.
//!
//! # Why the unit is a window
//!
//! The port this replaced asked a question about a *turn*: "given this
//! exchange, what facts are in it?" It ran after every single turn, on the same
//! model that was serving chat, and paid a fresh prefill each time. The unit was
//! also the wrong one for the question — whether something is a habit cannot be
//! seen in one exchange, so "is this a pattern?", which is a third of what a
//! household wants remembered, was unanswerable by construction.
//!
//! This port asks about a *window*: twenty messages, read once, in the pond's
//! idle time. One call per twenty messages instead of twenty calls, and the
//! model can see a thing happen twice.
//!
//! # The one type change that matters
//!
//! [`ExtractionError::Unparseable`] exists because today a malformed response
//! comes back as `Ok(vec![])` — indistinguishable from "nothing here was worth
//! keeping". In a per-turn design that costs one turn's facts. In a batch
//! design it is much worse: the cursor advances past a window nobody read, and
//! the conversation is never revisited. A parse failure has to be a different
//! shape from an empty answer, or the engine cannot tell the difference between
//! progress and silence.

use crate::user_data::domain::memory::{MemorySegment, MemoryTier};
use crate::user_data::domain::profile::ProfileScope;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Everything sent for one window, in tokens.
///
/// ~575 system + ~200 known-memories + ~1,500 window, against a prompt-side
/// clamp of 8192 on the local providers. It lives here, beside the port, because
/// it binds two crates that must agree about it: `pond-core` carves the window
/// to fit it, and the adapter in `pond-server` measures the assembled prompt
/// against it before it spends the inference slot. It was previously an adapter
/// constant with no reader outside its own test, which is how a 40 KB pasted log
/// came to be sent whole.
pub const EXTRACTION_PROMPT_BUDGET_TOKENS: usize = 2_400;

/// Characters per token, for the estimate the budget is spent in.
///
/// The same four-characters-per-token heuristic the injection loop already
/// spends its memory budget with. It is an ESTIMATE and is named as one: a
/// tokeniser is per-model and is not reachable from here, so the budget is set
/// low enough that the estimate being wrong by a third still fits the clamp.
pub const CHARS_PER_TOKEN: usize = 4;

/// Roughly how many tokens a string costs.
///
/// Rounded UP, so the estimate can only ever over-state what a piece of the
/// prompt costs. An under-stating estimate is the one that overruns a clamp.
pub fn estimated_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// The closed catalogue the model may choose from.
///
/// Five values, validated exactly — an unknown label is REJECTED, never
/// defaulted. Defaulting is how five William Ruto biography facts entered the
/// live store: `parse_segment_str` mapped everything it did not recognise onto
/// `knowledge`, so a model answering the wrong question still got a row.
///
/// `Routine` is the variant the design exists for. "Is this a habit, a pattern,
/// a standing way they do things?" is requirement 3, and it is the one question
/// a per-turn extractor cannot be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MemoryKind {
    /// A person or pet the subject knows, and who they are to them.
    Relationship,
    /// How the subject likes things: style, defaults, likes, dislikes.
    Preference,
    /// Something the subject does again and again.
    Routine,
    /// The subject fixed something that was wrong.
    Correction,
    /// Who the subject is: role, home, the work they are living through.
    Context,
}

impl MemoryKind {
    /// The label the prompt uses and the parser accepts.
    pub fn as_str(self) -> &'static str {
        match self {
            MemoryKind::Relationship => "relationship",
            MemoryKind::Preference => "preference",
            MemoryKind::Routine => "routine",
            MemoryKind::Correction => "correction",
            MemoryKind::Context => "context",
        }
    }

    /// Parse a model-written label.
    ///
    /// Normalises case, surrounding whitespace and a trailing plural "s",
    /// because those three are what a small model actually gets wrong about a
    /// closed list. Anything else returns `None` and the item is dropped: a
    /// label outside the catalogue means the model answered a question that was
    /// not asked, and the honest response to that is to keep nothing.
    pub fn parse(raw: &str) -> Option<Self> {
        let cleaned = raw.trim().to_lowercase();
        let cleaned = cleaned.strip_suffix('s').unwrap_or(&cleaned);
        match cleaned {
            "relationship" => Some(MemoryKind::Relationship),
            "preference" => Some(MemoryKind::Preference),
            "routine" => Some(MemoryKind::Routine),
            "correction" => Some(MemoryKind::Correction),
            "context" => Some(MemoryKind::Context),
            _ => None,
        }
    }

    /// Every variant, for tests and for rendering the catalogue.
    pub const ALL: [MemoryKind; 5] = [
        MemoryKind::Relationship,
        MemoryKind::Preference,
        MemoryKind::Routine,
        MemoryKind::Correction,
        MemoryKind::Context,
    ];

    /// Where a kind is filed in the store.
    ///
    /// The catalogue is mapped onto the existing `segment` column rather than
    /// onto free-text `tags`, because `segment` is populated on 379 of 379 live
    /// rows and indexed, while `tags` is populated on 7 of 379 with four near
    /// synonyms among them and has no reader anywhere.
    ///
    /// `Context` lands in `Identity` — "who the subject is: role, home, the
    /// work they are living through" is what that segment already means, and
    /// the store's `Context` segment means something else entirely (transient
    /// state, short tier, decays in about a week).
    pub fn segment(self) -> MemorySegment {
        match self {
            MemoryKind::Relationship => MemorySegment::Relationship,
            MemoryKind::Preference => MemorySegment::Preference,
            MemoryKind::Routine => MemorySegment::Routine,
            MemoryKind::Correction => MemorySegment::Correction,
            MemoryKind::Context => MemorySegment::Identity,
        }
    }

    /// The importance a memory of this kind starts at.
    ///
    /// Stated here rather than taken from the model. The old prompt asked for
    /// an importance and then clamped it to the segment's ceiling, so the model
    /// could only ever lower it — which is a dial that looks like it does
    /// something and does not. Phase 3's reinforcement raises this from
    /// observation count, which is evidence rather than an opinion.
    pub fn base_importance(self) -> f32 {
        self.segment().default_importance()
    }

    /// How long a memory of this kind is kept.
    ///
    /// **`Long` for all five, including `Context`.** Taken explicitly rather
    /// than through `segment().default_tier()`, which would make a `context`
    /// memory `Permanent` by way of `Identity`. A household's circumstances
    /// change: a row asserting a job somebody left, which nothing may ever
    /// prune, is worse than one that fades.
    pub fn tier(self) -> MemoryTier {
        MemoryTier::Long
    }
}

/// Who the window is about, and the names the write gate will accept for them.
///
/// Not `settings.user_name` read at the call site, because a pond can have more
/// than one member and a batch job has no request to resolve one from. It is
/// resolved per window from the session's own identity, falling back to
/// settings, and a window whose subject cannot be named is skipped rather than
/// mined under somebody else's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSubject {
    /// What the prompt calls them. Literally `the user` on a pond with no
    /// configured name — which is the wording the existing write gate already
    /// accepts, rather than writing the `Friend` placeholder into permanent
    /// storage.
    pub name: String,
    /// Extra names `names_subject` must accept as naming this person. Empty
    /// when the subject is `the user`, because the gate already matches that.
    pub gate_aliases: Vec<String>,
    /// The household member every memory from this window is stamped to, or
    /// `None` for the unattributed rows a single-member pond has always
    /// written.
    ///
    /// Carried on the subject rather than resolved again at write time, so the
    /// name in the prompt and the owner on the row cannot come apart. They are
    /// two faces of one decision, and a disagreement between them is a memory
    /// filed under one member and worded about another.
    pub profile_id: Option<String>,
}

impl WindowSubject {
    /// The pond-wide fallback: nobody is named, so the prompt says `the user`.
    pub fn anonymous() -> Self {
        Self {
            name: "the user".to_string(),
            gate_aliases: Vec::new(),
            profile_id: None,
        }
    }

    /// A named subject with no household member behind the name.
    ///
    /// What `settings.user_name` produces: a pond can be named without having
    /// any profile rows, and everything it writes stays unattributed — which is
    /// what all 379 rows already in the store are.
    pub fn named(name: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            gate_aliases: vec![name.clone()],
            name,
            profile_id: None,
        }
    }

    /// A named household member. Memories from their windows are theirs.
    pub fn member(profile_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            profile_id: Some(profile_id.into()),
            ..Self::named(display_name)
        }
    }

    /// The scope every store read about this window must take.
    ///
    /// The subject is resolved per window and then the STORE has to be read
    /// under it, or the resolution is enforced on the prompt's wording and
    /// nowhere else. [`ProfileScope::Household`] renders as no filter at all,
    /// so reading a named member's window under it pulls every other member's
    /// rows -- which are then printed to the model under the literal header
    /// "Already remembered about {this member}", and scored against this
    /// member's candidates so one person's true memory is dropped as a
    /// duplicate of another's.
    ///
    /// [`Owner`](ProfileScope::Owner) is that member's rows plus the
    /// unattributed ones, which is exactly what "what does the pond already
    /// know about this person" means: the shared household rows are theirs too.
    ///
    /// `None` is not a silent fallback to unfiltered. It is only reachable
    /// where nobody could be told apart in the first place -- a pond with one
    /// member or none, which is where [`super::super::services::memory_extraction::resolve_window_subject`]
    /// allows the pond-wide fallback at all -- and there `Household` returns the
    /// same rows `Owner` would, while keeping the 379 unattributed rows already
    /// in the store reachable.
    pub fn scope(&self) -> ProfileScope {
        match &self.profile_id {
            Some(id) => ProfileScope::Owner(id.clone()),
            None => ProfileScope::Household,
        }
    }
}

/// One message of the window, projected onto what the prompt needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowMessage {
    pub id: String,
    /// `"user"` or `"assistant"`. Not the full `Role` enum: a window is built
    /// from what a person and the pond said to each other, and a system message
    /// in the middle of it is plumbing.
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

impl WindowMessage {
    pub fn is_user(&self) -> bool {
        self.role == "user"
    }
}

/// A memory the store already holds, shown to the model so new evidence can
/// build on it rather than restate it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownMemory {
    pub note: String,
    /// How the row is filed. A string rather than a [`MemoryKind`] because the
    /// existing store predates the catalogue: its rows carry segments like
    /// `project` and `knowledge` that the model may no longer choose. It is a
    /// hint to the reader, not a contract.
    pub kind_label: String,
    /// Whether this memory has been seen more than once. Always `false` until
    /// the observation columns land; rendering it as established before
    /// anything counts observations would be the pond asserting evidence it
    /// does not have.
    pub pattern: bool,
}

/// One window, ready to be read.
pub struct ExtractionWindow<'a> {
    pub subject: &'a WindowSubject,
    pub assistant_name: &'a str,
    pub session_id: &'a str,
    /// The last message id in the window. This is the idempotence key and the
    /// value the cursor advances to.
    pub window_id: &'a str,
    pub messages: &'a [WindowMessage],
    /// What is already known about the subject, most relevant first.
    pub known: &'a [KnownMemory],
    pub max_memories: usize,
    /// Whether to ask for reminders at all. `false` for a window whose last
    /// message is older than the staleness cutoff, which also drops the
    /// reminders paragraph from the prompt entirely.
    pub allow_reminders: bool,
}

/// One thing worth remembering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMemory {
    pub note: String,
    pub kind: MemoryKind,
}

/// Something with a date or a time in it, which is never a memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedReminder {
    pub about: String,
    /// The subject's own words about the timing, copied from the conversation.
    /// Deliberately not a parsed date: the model is not asked to work one out,
    /// because a wrong date is worse than no date.
    pub when_said: String,
}

/// What one window produced.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowExtraction {
    pub memories: Vec<ExtractedMemory>,
    pub reminders: Vec<ExtractedReminder>,
    /// Items the model offered that carried a label outside the catalogue.
    /// Counted rather than salvaged, so a prompt the model is systematically
    /// misreading shows up as a number instead of as a store full of
    /// misfiled rows.
    pub rejected: usize,
}

impl WindowExtraction {
    pub fn is_empty(&self) -> bool {
        self.memories.is_empty() && self.reminders.is_empty()
    }
}

/// Why a window could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    /// No model is configured. The cursor must not move and no attempt is
    /// counted: nothing was wrong with the window.
    #[error("no LLM provider available")]
    NoProvider,
    /// The model was called and failed. Same disposition as `NoProvider`.
    #[error("extraction provider failed: {0}")]
    Provider(#[from] anyhow::Error),
    /// The model answered, and nothing in the answer was JSON. The cursor must
    /// not move, and this one DOES count an attempt -- three of these against
    /// one watermark and the walk gives up on that window rather than reading
    /// it forever.
    #[error("no JSON recoverable from the model's reply")]
    Unparseable {
        /// The first few hundred characters of what came back, for the log.
        /// Bounded because this reaches a log file, and an unbounded model
        /// reply in a log is the whole conversation written somewhere with none
        /// of the store's retention.
        raw_head: String,
    },
}

/// Driven port: read one conversation window and say what is worth remembering.
#[async_trait]
pub trait ConversationExtractor: Send + Sync {
    async fn extract_window(
        &self,
        window: ExtractionWindow<'_>,
    ) -> Result<WindowExtraction, ExtractionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalogue is closed, and an unknown label is dropped rather than
    /// filed somewhere.
    ///
    /// This is the measured failure: `parse_segment_str` maps everything it
    /// does not recognise onto `knowledge`, and five third-party biography
    /// facts reached the live store that way. A closed list whose parser has a
    /// fallback is not a closed list.
    #[test]
    fn an_unknown_kind_is_refused_rather_than_defaulted() {
        for label in ["identity", "project", "knowledge", "fact", "", "  "] {
            assert_eq!(
                MemoryKind::parse(label),
                None,
                "{label:?} is outside the catalogue and must not be salvaged onto a kind"
            );
        }
    }

    /// The three things a small model actually gets wrong about a closed list:
    /// case, padding, and pluralising the label it was shown.
    #[test]
    fn the_parser_forgives_case_padding_and_a_trailing_plural() {
        assert_eq!(MemoryKind::parse("  Routines  "), Some(MemoryKind::Routine));
        assert_eq!(
            MemoryKind::parse("PREFERENCE"),
            Some(MemoryKind::Preference)
        );
        for kind in MemoryKind::ALL {
            assert_eq!(MemoryKind::parse(kind.as_str()), Some(kind));
        }
    }

    /// A pond with no configured name says `the user`, not `Friend`.
    ///
    /// `Friend` is the shipped default of `settings.user_name`, and writing it
    /// into permanent storage would produce memories about a person who does
    /// not exist. `the user` is also the wording the existing write gate
    /// already accepts, so the anonymous path needs no alias at all.
    #[test]
    fn an_unnamed_pond_writes_the_user_and_needs_no_alias() {
        let subject = WindowSubject::anonymous();
        assert_eq!(subject.name, "the user");
        assert!(subject.gate_aliases.is_empty());

        let named = WindowSubject::named("Jerry");
        assert_eq!(named.gate_aliases, vec!["Jerry".to_string()]);
    }

    /// A member's window is read under THEIR scope, never the household's.
    ///
    /// `Household` renders as no filter at all (`scope_sql`), so a read taken
    /// under it while the prompt says "Already remembered about Amara" shows
    /// Jerry's rows to a model that has just been told they are Amara's. The
    /// subject and the scope are two faces of one decision and this is the
    /// place they are tied together.
    #[test]
    fn a_named_members_window_is_read_under_their_own_scope() {
        let member = WindowSubject::member("profile-amara", "Amara");
        assert_eq!(
            member.scope(),
            ProfileScope::Owner("profile-amara".to_string())
        );

        // The two unattributed shapes. Both are only reachable on a pond where
        // nobody can be told apart, and both keep the existing unattributed
        // rows readable.
        assert_eq!(WindowSubject::anonymous().scope(), ProfileScope::Household);
        assert_eq!(
            WindowSubject::named("Jerry").scope(),
            ProfileScope::Household
        );
    }

    /// The estimate rounds up, so it can only over-state a cost.
    #[test]
    fn the_token_estimate_never_understates() {
        assert_eq!(estimated_tokens(""), 0);
        // One character is one token's worth of budget spent, not zero.
        assert_eq!(estimated_tokens("a"), 1);
        assert_eq!(estimated_tokens(&"a".repeat(CHARS_PER_TOKEN)), 1);
        assert_eq!(estimated_tokens(&"a".repeat(CHARS_PER_TOKEN + 1)), 2);
    }
}
