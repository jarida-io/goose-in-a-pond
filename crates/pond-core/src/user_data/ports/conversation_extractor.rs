//! ConversationExtractor port: read one conversation window and say what is worth remembering.

use crate::user_data::domain::interest::ObservedInterest;
use crate::user_data::domain::memory::{MemorySegment, MemoryTier};
use crate::user_data::domain::profile::ProfileScope;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Token budget for one window's whole prompt, well under the local providers' 8192 clamp.
/// Shared: `pond-core` carves the window to fit it and the `pond-server` adapter trims to it.
pub const EXTRACTION_PROMPT_BUDGET_TOKENS: usize = 2_400;

/// A heuristic (no tokeniser is reachable here); the budget leaves room for it to be a third off.
pub const CHARS_PER_TOKEN: usize = 4;

/// Estimated token cost, rounded up so it never under-states.
pub fn estimated_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// The closed catalogue the model may choose from; an unknown label is rejected, never defaulted.
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

    /// Parse a model-written label, forgiving case, padding and a trailing plural "s".
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

    /// The store segment for a kind. `Context` goes to `Identity`: the store's own `Context`
    /// segment is transient and decays in about a week.
    pub fn segment(self) -> MemorySegment {
        match self {
            MemoryKind::Relationship => MemorySegment::Relationship,
            MemoryKind::Preference => MemorySegment::Preference,
            MemoryKind::Routine => MemorySegment::Routine,
            MemoryKind::Correction => MemorySegment::Correction,
            MemoryKind::Context => MemorySegment::Identity,
        }
    }

    /// Starting importance: fixed per kind, never taken from the model.
    pub fn base_importance(self) -> f32 {
        self.segment().default_importance()
    }

    /// `Long` for all five. Not `segment().default_tier()`, which would make `Context` permanent
    /// through `Identity`, though a household's circumstances change.
    pub fn tier(self) -> MemoryTier {
        MemoryTier::Long
    }
}

/// Who the window is about, and the names the write gate accepts for them. Resolved per
/// window, not from `settings.user_name`, since a pond can have several members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSubject {
    /// What the prompt calls them: `the user` when unnamed, never the `Friend` default.
    pub name: String,
    /// Extra names `names_subject` must accept; empty for `the user`, which it already matches.
    pub gate_aliases: Vec<String>,
    /// The member every memory from this window is stamped to; `None` means unattributed. Carried
    /// here, not re-resolved at write time, so the prompt's name and the row's owner agree.
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

    /// A named subject with no household member behind it, as `settings.user_name` gives.
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

    /// Scope for every store read about this window. `Household` is no filter, so it serves only
    /// unattributed subjects, which exist only on ponds with at most one member.
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
    /// `"user"` or `"assistant"`; a window never includes system messages.
    pub role: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
}

impl WindowMessage {
    pub fn is_user(&self) -> bool {
        self.role == "user"
    }
}

/// A stored memory, shown to the model so new evidence builds on it rather than restates it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownMemory {
    pub note: String,
    /// The row's segment, as a string: older rows carry segments no [`MemoryKind`] maps to.
    pub kind_label: String,
    /// Seen more than once. Always `false` for now: nothing counts observations yet.
    pub pattern: bool,
}

pub struct ExtractionWindow<'a> {
    pub subject: &'a WindowSubject,
    pub assistant_name: &'a str,
    pub session_id: &'a str,
    /// Last message id in the window: the idempotence key and the cursor's next value.
    pub window_id: &'a str,
    pub messages: &'a [WindowMessage],
    /// What is already known about the subject, most relevant first.
    pub known: &'a [KnownMemory],
    pub max_memories: usize,
    /// `false` when the window's last message is older than the staleness cutoff.
    pub allow_reminders: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMemory {
    pub note: String,
    pub kind: MemoryKind,
}

/// Something with a date or a time in it, which is never a memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedReminder {
    pub about: String,
    /// The subject's own words about the timing, never a parsed date: a wrong date is worse.
    pub when_said: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowExtraction {
    pub memories: Vec<ExtractedMemory>,
    pub reminders: Vec<ExtractedReminder>,
    /// What the window showed the subject caring about; never a memory of who they are.
    pub interests: Vec<ObservedInterest>,
    /// Items labelled outside the catalogue; counted so a misread prompt shows up as a number.
    pub rejected: usize,
}

impl WindowExtraction {
    pub fn is_empty(&self) -> bool {
        self.memories.is_empty() && self.reminders.is_empty() && self.interests.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    /// No model configured. The cursor stays and no attempt is counted.
    #[error("no LLM provider available")]
    NoProvider,
    /// The model was called and failed. Same disposition as `NoProvider`.
    #[error("extraction provider failed: {0}")]
    Provider(#[from] anyhow::Error),
    /// The reply held no recoverable JSON. Unlike an empty answer the cursor stays, but it counts
    /// toward `MAX_PARSE_ATTEMPTS`, after which the walk gives up on the window.
    #[error("no JSON recoverable from the model's reply")]
    Unparseable {
        /// Start of the reply, for the log; bounded because logs bypass the store's retention.
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

    #[test]
    fn an_unnamed_pond_writes_the_user_and_needs_no_alias() {
        let subject = WindowSubject::anonymous();
        assert_eq!(subject.name, "the user");
        assert!(subject.gate_aliases.is_empty());

        let named = WindowSubject::named("Jerry");
        assert_eq!(named.gate_aliases, vec!["Jerry".to_string()]);
    }

    #[test]
    fn a_named_members_window_is_read_under_their_own_scope() {
        let member = WindowSubject::member("profile-amara", "Amara");
        assert_eq!(
            member.scope(),
            ProfileScope::Owner("profile-amara".to_string())
        );

        // Unattributed subjects exist only where nobody can be told apart.
        assert_eq!(WindowSubject::anonymous().scope(), ProfileScope::Household);
        assert_eq!(
            WindowSubject::named("Jerry").scope(),
            ProfileScope::Household
        );
    }

    #[test]
    fn the_token_estimate_never_understates() {
        assert_eq!(estimated_tokens(""), 0);
        assert_eq!(estimated_tokens("a"), 1);
        assert_eq!(estimated_tokens(&"a".repeat(CHARS_PER_TOKEN)), 1);
        assert_eq!(estimated_tokens(&"a".repeat(CHARS_PER_TOKEN + 1)), 2);
    }
}
