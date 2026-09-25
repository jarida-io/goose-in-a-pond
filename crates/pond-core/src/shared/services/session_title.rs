//! Idle re-titling: swaps the six-word fallback, or a stale model title, for a model-written one.
//! Never touches a `user` title, or a legacy row's title unless it is exactly the fallback.
//! Every model call races a [`CancellationToken`] and persists nothing if it loses.

use std::sync::Arc;

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::models::domain::message::{ChatMessage, Role};
use crate::models::ports::provider::LlmProvider;
use crate::user_data::ports::session_storage::SessionStorage;

pub const MAX_TITLE_WORDS: usize = 10;

/// Character cap, so a title of ten very long words still fits the sidebar.
pub const MAX_TITLE_CHARS: usize = 72;

/// Past this the reply is prose, not a name: reject it whole rather than truncate to a fragment.
const REJECT_OVER_WORDS: usize = 20;

/// Fewer messages than this are left to the six-word fallback.
pub const MIN_MESSAGES_TO_TITLE: usize = 3;

/// Messages past a model title's coverage before it is rebuilt.
pub const REFRESH_AFTER_MESSAGES: usize = 8;

/// How many recent messages to show the model when there is no rolling summary.
const EVIDENCE_MESSAGES: usize = 10;

/// Per-message evidence cap, so a long paste can't blow a small board's context.
const EVIDENCE_CHARS_PER_MESSAGE: usize = 280;

/// Who last wrote `sessions.title`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleSource {
    /// The deterministic six-word fallback.
    Derived,
    /// This service.
    Model,
    /// A person, through the rename endpoint.
    User,
}

impl TitleSource {
    /// Stable string for the database column.
    pub fn as_str(self) -> &'static str {
        match self {
            TitleSource::Derived => "derived",
            TitleSource::Model => "model",
            TitleSource::User => "user",
        }
    }

    /// An unrecognised value reads as `None`, which the gate treats like a legacy row.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "derived" => Some(TitleSource::Derived),
            "model" => Some(TitleSource::Model),
            "user" => Some(TitleSource::User),
            _ => None,
        }
    }
}

/// The gate's inputs as plain values, so the policy tests need no database or model.
#[derive(Debug, Clone, Copy)]
pub struct TitleGateInputs<'a> {
    pub title: Option<&'a str>,
    /// `sessions.title_source`, or `None` for a row that predates the column.
    pub source: Option<TitleSource>,
    /// The fallback title for the first user message; only used to spot a legacy fallback.
    pub derived_fallback: Option<&'a str>,
    pub total_messages: usize,
    /// Only meaningful when `source` is [`TitleSource::Model`].
    pub messages_since_covered: usize,
}

/// Why a session was left alone this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Not enough conversation to name yet.
    TooShort,
    /// Someone typed this name. Off limits.
    UserNamed,
    /// Legacy row whose title isn't the fallback, so assumed chosen by a person.
    UnknownProvenance,
    /// Model-written and still current enough.
    StillCurrent,
}

impl SkipReason {
    /// Short, stable label for structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::TooShort => "too_short",
            SkipReason::UserNamed => "user_named",
            SkipReason::UnknownProvenance => "unknown_provenance",
            SkipReason::StillCurrent => "still_current",
        }
    }
}

/// The gate's verdict for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleDecision {
    Retitle,
    Skip(SkipReason),
}

impl TitleDecision {
    pub fn is_retitle(self) -> bool {
        matches!(self, TitleDecision::Retitle)
    }
}

pub fn should_retitle(inputs: TitleGateInputs<'_>) -> TitleDecision {
    if inputs.total_messages < MIN_MESSAGES_TO_TITLE {
        return TitleDecision::Skip(SkipReason::TooShort);
    }

    match inputs.source {
        Some(TitleSource::User) => TitleDecision::Skip(SkipReason::UserNamed),

        Some(TitleSource::Derived) => TitleDecision::Retitle,

        Some(TitleSource::Model) => {
            if inputs.messages_since_covered >= REFRESH_AFTER_MESSAGES {
                TitleDecision::Retitle
            } else {
                TitleDecision::Skip(SkipReason::StillCurrent)
            }
        }

        // Legacy/unknown source: only an empty or exact-fallback title is surely not a person's.
        None => match (inputs.title, inputs.derived_fallback) {
            (None, _) => TitleDecision::Retitle,
            (Some(t), _) if t.trim().is_empty() => TitleDecision::Retitle,
            (Some(t), Some(fallback)) if t == fallback => TitleDecision::Retitle,
            _ => TitleDecision::Skip(SkipReason::UnknownProvenance),
        },
    }
}

/// Strips the quotes, "Title:" labels and full stops models add; refuses prose outright.
pub fn normalise_title(raw: &str) -> Option<String> {
    // Only the first non-empty line; the rest is the model explaining itself.
    let first_line = raw.lines().find(|l| !l.trim().is_empty())?;

    let mut cleaned = first_line.trim();

    // "Title: ..." / "title - ..." — a label, not part of the name.
    for prefix in ["chat title:", "title:", "title -", "name:"] {
        if let Some(rest) = strip_prefix_ci(cleaned, prefix) {
            cleaned = rest.trim();
            break;
        }
    }

    let cleaned = cleaned
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('*')
        .trim()
        .trim_end_matches('.')
        .trim();

    if cleaned.is_empty() {
        return None;
    }

    let words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    if words.len() > REJECT_OVER_WORDS {
        return None;
    }

    let mut title = words
        .iter()
        .take(MAX_TITLE_WORDS)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");

    // Char cap, cut on a word boundary where there is one.
    if title.chars().count() > MAX_TITLE_CHARS {
        let truncated: String = title.chars().take(MAX_TITLE_CHARS).collect();
        title = match truncated.rsplit_once(' ') {
            Some((head, _)) if !head.trim().is_empty() => head.trim().to_string(),
            _ => truncated.trim().to_string(),
        };
    }

    if !title.chars().any(|c| c.is_alphanumeric()) {
        return None;
    }

    Some(title)
}

/// Case-insensitive prefix strip; uses `str::get` so a multi-byte char at the cut can't panic.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    if head.eq_ignore_ascii_case(prefix) {
        s.get(prefix.len()..)
    } else {
        None
    }
}

/// Title prompt for a small on-device model: short rules, positive where possible.
/// Public so the chat service's first-exchange naming uses the same prompt.
pub const TITLE_SYSTEM_PROMPT: &str = "\
You name conversations. Reply with the name and nothing else.

- Ten words at most. Fewer is better.
- Name the real subject, so it is recognisable in a list weeks from now.
- Be concrete: name the thing itself, not the category it belongs to.
- Sentence case. No quotes, no full stop, no \"Chat about\".
- If it covers several things, name the one it kept coming back to.";

/// What one attempt at renaming a session did.
#[derive(Debug, PartialEq, Eq)]
pub enum RetitleOutcome {
    /// Renamed and persisted.
    Retitled {
        title: String,
        through_message_id: String,
    },
    /// The gate declined.
    Skipped(SkipReason),
    /// A person came back. Nothing was written.
    Cancelled,
    /// The model answered with something unusable. The old title stands.
    Unusable,
}

/// Renames one session, when the gate allows it.
pub struct SessionTitleService {
    provider: Arc<dyn LlmProvider>,
    storage: Arc<dyn SessionStorage>,
}

impl SessionTitleService {
    pub fn new(provider: Arc<dyn LlmProvider>, storage: Arc<dyn SessionStorage>) -> Self {
        Self { provider, storage }
    }

    /// Sweep entry point: renames only if the gate allows; a cancelled attempt writes nothing.
    pub async fn retitle(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
    ) -> Result<RetitleOutcome> {
        self.run(session_id, false, cancel).await
    }

    /// On-request rename (a click is consent): overrides every skip except `TooShort`.
    pub async fn retitle_now(
        &self,
        session_id: &str,
        cancel: &CancellationToken,
    ) -> Result<RetitleOutcome> {
        self.run(session_id, true, cancel).await
    }

    async fn run(
        &self,
        session_id: &str,
        force: bool,
        cancel: &CancellationToken,
    ) -> Result<RetitleOutcome> {
        if cancel.is_cancelled() {
            return Ok(RetitleOutcome::Cancelled);
        }

        let session = self.storage.get_session(session_id).await?;
        let (source_raw, covered_through) = self.storage.get_title_provenance(session_id).await?;
        let source = source_raw.as_deref().and_then(TitleSource::parse);

        // Cheap queries only (every session, every 5 min); history is read only after a yes.
        // An adapter without `count_messages` reports 0, so it skips as `TooShort`: the safe side.
        let total_messages = self.storage.count_messages(session_id).await? as usize;

        let messages_since_covered = match (source, covered_through.as_deref()) {
            (Some(TitleSource::Model), Some(through)) => self
                .storage
                .messages_after(session_id, through)
                .await?
                // Covered message gone: the title covers nothing, rather than being current.
                .map_or(total_messages, |n| n as usize),
            _ => total_messages,
        };

        // Only a legacy row needs this, to recognise the six-word fallback.
        let derived_fallback = match source {
            None => self
                .storage
                .first_user_message(session_id)
                .await?
                .map(|text| derive_fallback_title(&text)),
            _ => None,
        };

        let decision = should_retitle(TitleGateInputs {
            title: session.title.as_deref(),
            source,
            derived_fallback: derived_fallback.as_deref(),
            total_messages,
            messages_since_covered,
        });

        if let TitleDecision::Skip(reason) = decision {
            // Force overrides permission, not possibility: `TooShort` is nothing to describe.
            let overridable = force && reason != SkipReason::TooShort;
            if !overridable {
                return Ok(RetitleOutcome::Skipped(reason));
            }
        }

        let messages = self.storage.get_messages(session_id).await?;
        let Some(newest) = messages.last() else {
            return Ok(RetitleOutcome::Skipped(SkipReason::TooShort));
        };
        let through_message_id = newest.id.clone();

        // Prefer the rolling summary: already condensed, so a much cheaper call on a small board.
        let (summary, _) = self.storage.get_rolling_summary(session_id).await?;
        let evidence = match summary {
            Some(s) if !s.trim().is_empty() => {
                format!("Summary of the conversation:\n{}", s.trim())
            }
            _ => transcript_evidence(&messages),
        };

        if cancel.is_cancelled() {
            return Ok(RetitleOutcome::Cancelled);
        }

        let prompt = vec![ChatMessage::user(format!(
            "{evidence}\n\nName this conversation."
        ))];

        // Race cancellation so a returning user reclaims the serial on-device engine at once.
        let response = tokio::select! {
            r = self.provider.complete(TITLE_SYSTEM_PROMPT, prompt) => r?,
            _ = cancel.cancelled() => return Ok(RetitleOutcome::Cancelled),
        };

        if cancel.is_cancelled() {
            return Ok(RetitleOutcome::Cancelled);
        }

        let Some(title) = normalise_title(&response.content) else {
            return Ok(RetitleOutcome::Unusable);
        };

        self.storage
            .set_generated_title(session_id, &title, &through_message_id)
            .await?;

        Ok(RetitleOutcome::Retitled {
            title,
            through_message_id,
        })
    }
}

/// A copy of chat's private `derive_title_from_text` rule, so the gate can spot a fallback.
/// If the two drift, legacy fallbacks are merely left alone: the safe direction.
fn derive_fallback_title(text: &str) -> String {
    const MAX_WORDS: usize = 6;
    const MAX_CHARS: usize = 60;

    let cleaned = text.trim().trim_matches('"').trim_matches('\'');
    let title = cleaned
        .split_whitespace()
        .take(MAX_WORDS)
        .collect::<Vec<_>>()
        .join(" ");
    let title = title.trim().trim_matches('"').trim_matches('\'').trim();

    title.chars().take(MAX_CHARS).collect()
}

/// First user message plus the tail: what the conversation was for, and what it became.
fn transcript_evidence(messages: &[crate::user_data::domain::session::SessionMessage]) -> String {
    let mut lines: Vec<String> = Vec::new();

    if let Some(first_user) = messages.iter().find(|m| m.message.role == Role::User) {
        lines.push(format!(
            "Opened with: {}",
            truncate(&first_user.message.content, EVIDENCE_CHARS_PER_MESSAGE)
        ));
    }

    let tail_start = messages.len().saturating_sub(EVIDENCE_MESSAGES);
    for m in &messages[tail_start..] {
        let who = match m.message.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            _ => continue,
        };
        let body = truncate(&m.message.content, EVIDENCE_CHARS_PER_MESSAGE);
        if !body.trim().is_empty() {
            lines.push(format!("{who}: {body}"));
        }
    }

    lines.join("\n")
}

/// Char-boundary truncate that marks the cut, so the model doesn't take it as a whole thought.
fn truncate(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let head: String = trimmed.chars().take(max_chars).collect();
    format!("{}…", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small models copy worked examples, and a copied one would be persisted as the real name.
    #[test]
    fn the_prompt_offers_no_title_a_model_could_copy() {
        let quoted: Vec<&str> = TITLE_SYSTEM_PROMPT.split('"').collect();
        for (i, chunk) in quoted.iter().enumerate() {
            if i % 2 == 0 {
                continue; // outside quotes
            }
            assert!(
                chunk.split_whitespace().count() < 3,
                "TITLE_SYSTEM_PROMPT quotes {chunk:?}, which is long enough to be \
                 emitted as a title and would then be persisted as a real \
                 conversation name"
            );
        }
        assert!(
            !TITLE_SYSTEM_PROMPT.contains("Jetson"),
            "the prompt names this household's own hardware"
        );
    }

    fn base() -> TitleGateInputs<'static> {
        TitleGateInputs {
            title: Some("so i was wondering whether"),
            source: Some(TitleSource::Derived),
            derived_fallback: Some("so i was wondering whether"),
            total_messages: 6,
            messages_since_covered: 0,
        }
    }

    // ── the gate ────────────────────────────────────────────────────────────

    #[test]
    fn a_fallback_title_is_replaced() {
        assert_eq!(should_retitle(base()), TitleDecision::Retitle);
    }

    #[test]
    fn a_name_someone_typed_is_never_touched() {
        let inputs = TitleGateInputs {
            title: Some("Jetson deploy notes"),
            source: Some(TitleSource::User),
            // Even when the conversation has moved on enormously.
            messages_since_covered: 500,
            total_messages: 500,
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::UserNamed)
        );
    }

    #[test]
    fn a_short_session_is_left_to_the_fallback() {
        let inputs = TitleGateInputs {
            total_messages: MIN_MESSAGES_TO_TITLE - 1,
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::TooShort)
        );
    }

    #[test]
    fn a_model_title_holds_until_the_conversation_moves_on() {
        let inputs = TitleGateInputs {
            title: Some("Wake word fires twice on the Jetson"),
            source: Some(TitleSource::Model),
            messages_since_covered: REFRESH_AFTER_MESSAGES - 1,
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::StillCurrent)
        );
    }

    #[test]
    fn a_model_title_is_rebuilt_once_the_conversation_moves_on() {
        let inputs = TitleGateInputs {
            title: Some("Wake word fires twice on the Jetson"),
            source: Some(TitleSource::Model),
            messages_since_covered: REFRESH_AFTER_MESSAGES,
            ..base()
        };
        assert_eq!(should_retitle(inputs), TitleDecision::Retitle);
    }

    // ── legacy rows, where the asymmetry lives ──────────────────────────────

    #[test]
    fn a_legacy_fallback_title_is_recognised_and_replaced() {
        let inputs = TitleGateInputs {
            title: Some("so i was wondering whether"),
            source: None,
            derived_fallback: Some("so i was wondering whether"),
            ..base()
        };
        assert_eq!(should_retitle(inputs), TitleDecision::Retitle);
    }

    #[test]
    fn a_legacy_title_that_is_not_the_fallback_is_assumed_deliberate() {
        let inputs = TitleGateInputs {
            title: Some("Jetson deploy notes"),
            source: None,
            derived_fallback: Some("so i was wondering whether"),
            ..base()
        };
        assert_eq!(
            should_retitle(inputs),
            TitleDecision::Skip(SkipReason::UnknownProvenance)
        );
    }

    #[test]
    fn an_absent_or_blank_title_is_always_fillable() {
        for title in [None, Some(""), Some("   ")] {
            let inputs = TitleGateInputs {
                title,
                source: None,
                derived_fallback: Some("anything at all"),
                ..base()
            };
            assert_eq!(should_retitle(inputs), TitleDecision::Retitle);
        }
    }

    #[test]
    fn an_unrecognised_column_value_is_treated_as_legacy() {
        assert_eq!(TitleSource::parse("something-else"), None);
    }

    #[test]
    fn source_strings_round_trip() {
        for s in [TitleSource::Derived, TitleSource::Model, TitleSource::User] {
            assert_eq!(TitleSource::parse(s.as_str()), Some(s));
        }
    }

    // ── normalisation ───────────────────────────────────────────────────────

    #[test]
    fn the_ten_word_ceiling_is_absolute() {
        let raw = "one two three four five six seven eight nine ten eleven twelve";
        let title = normalise_title(raw).expect("usable");
        assert_eq!(title.split_whitespace().count(), MAX_TITLE_WORDS);
        assert!(title.starts_with("one two"));
    }

    #[test]
    fn a_short_answer_is_left_exactly_as_it_is() {
        assert_eq!(
            normalise_title("Wake word fires twice on the Jetson").as_deref(),
            Some("Wake word fires twice on the Jetson")
        );
    }

    #[test]
    fn the_decorations_models_add_are_stripped() {
        for raw in [
            "\"Wake word fires twice\"",
            "Title: Wake word fires twice",
            "title: \"Wake word fires twice\"",
            "**Wake word fires twice**",
            "Wake word fires twice.",
            "  Wake word fires twice  ",
        ] {
            assert_eq!(
                normalise_title(raw).as_deref(),
                Some("Wake word fires twice"),
                "failed to clean {raw:?}"
            );
        }
    }

    #[test]
    fn only_the_first_line_is_used() {
        let raw = "Wake word fires twice\n\nI chose this because the conversation was about…";
        assert_eq!(
            normalise_title(raw).as_deref(),
            Some("Wake word fires twice")
        );
    }

    #[test]
    fn prose_is_refused_rather_than_truncated() {
        let raw = "Certainly, here is a suitable name for this particular conversation \
                   which covered a number of different topics over its course including \
                   several that were only briefly mentioned";
        assert_eq!(normalise_title(raw), None);
    }

    #[test]
    fn an_empty_or_punctuation_only_answer_is_refused() {
        for raw in ["", "   ", "\n\n", "\"\"", "...", "-- --"] {
            assert_eq!(normalise_title(raw), None, "should refuse {raw:?}");
        }
    }

    #[test]
    fn the_character_cap_never_ends_mid_word() {
        let raw = "Supercalifragilistic expialidocious antidisestablishmentarianism \
                   pneumonoultramicroscopicsilicovolcanoconiosis";
        let title = normalise_title(raw).expect("usable");
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
        // Whatever survived, it is whole words from the original.
        for word in title.split_whitespace() {
            assert!(raw.contains(word), "{word:?} is not a word from the input");
        }
    }

    #[test]
    fn a_title_of_exactly_ten_words_survives_intact() {
        let raw = "one two three four five six seven eight nine ten";
        assert_eq!(normalise_title(raw).as_deref(), Some(raw));
    }

    #[test]
    fn a_multi_byte_character_at_a_prefix_boundary_does_not_panic() {
        for raw in [
            "Déjà vu on the Jetson",
            "café",
            "日本語のタイトル",
            "Ω",
            "naïve retry logic",
            "—",
        ] {
            let _ = normalise_title(raw); // must not panic
        }
    }

    #[test]
    fn unicode_is_counted_by_character_not_byte() {
        let title = normalise_title("Déjà vu on the Jetson café build").expect("usable");
        assert!(title.chars().count() <= MAX_TITLE_CHARS);
        assert!(title.starts_with("Déjà vu"));
    }

    // ── evidence ────────────────────────────────────────────────────────────

    #[test]
    fn truncation_marks_that_it_cut() {
        assert_eq!(truncate("short", 40), "short");
        let long = "x".repeat(100);
        let cut = truncate(&long, 10);
        assert!(cut.ends_with('…'));
        assert_eq!(cut.chars().count(), 11);
    }

    // ── the service, end to end ─────────────────────────────────────────────

    mod service {
        use super::*;
        use crate::user_data::domain::session::SessionMessage;
        use crate::user_data::mocks::mock_session::InMemorySessionStorage;
        use crate::user_data::ports::session_storage::SessionStorage;
        use async_trait::async_trait;
        use std::sync::Mutex;

        struct StubProvider {
            reply: String,
            calls: Mutex<usize>,
        }

        impl StubProvider {
            fn new(reply: &str) -> Arc<Self> {
                Arc::new(Self {
                    reply: reply.to_string(),
                    calls: Mutex::new(0),
                })
            }
            fn calls(&self) -> usize {
                *self.calls.lock().unwrap()
            }
        }

        #[async_trait]
        impl LlmProvider for StubProvider {
            async fn complete(
                &self,
                _system: &str,
                _messages: Vec<ChatMessage>,
            ) -> Result<ChatMessage> {
                *self.calls.lock().unwrap() += 1;
                Ok(ChatMessage::assistant(self.reply.clone()))
            }
            fn model_name(&self) -> String {
                "stub".to_string()
            }
        }

        async fn seed(storage: &InMemorySessionStorage, session: &str, n: usize) {
            storage.create_session(session.to_string()).await.unwrap();
            for i in 0..n {
                let msg = if i % 2 == 0 {
                    ChatMessage::user(format!("question {i}"))
                } else {
                    ChatMessage::assistant(format!("answer {i}"))
                };
                storage
                    .add_message(
                        session.to_string(),
                        SessionMessage::new(format!("m{i}"), session.to_string(), msg),
                    )
                    .await
                    .unwrap();
            }
        }

        #[tokio::test]
        async fn a_derived_title_is_replaced_and_its_reach_recorded() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 8).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            let provider = StubProvider::new("Wake word fires twice on the Jetson");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert_eq!(
                outcome,
                RetitleOutcome::Retitled {
                    title: "Wake word fires twice on the Jetson".to_string(),
                    through_message_id: "m7".to_string(),
                }
            );

            let session = storage.get_session("s").await.unwrap();
            assert_eq!(
                session.title.as_deref(),
                Some("Wake word fires twice on the Jetson")
            );
            assert_eq!(
                storage.get_title_provenance("s").await.unwrap(),
                (Some("model".to_string()), Some("m7".to_string()))
            );
        }

        #[tokio::test]
        async fn a_user_named_session_costs_no_inference_at_all() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 40).await;
            storage
                .update_title("s", "Jetson deploy notes".to_string())
                .await
                .unwrap();

            let provider = StubProvider::new("Something else entirely");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert_eq!(outcome, RetitleOutcome::Skipped(SkipReason::UserNamed));
            assert_eq!(provider.calls(), 0, "the model must never have been asked");
            assert_eq!(
                storage.get_session("s").await.unwrap().title.as_deref(),
                Some("Jetson deploy notes"),
                "the name someone typed must survive untouched"
            );
        }

        #[tokio::test]
        async fn a_model_title_holds_then_is_rebuilt_as_the_conversation_moves_on() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 10).await;
            // Covers through m9 — the newest message. Nothing has moved on.
            storage
                .set_generated_title("s", "An early name", "m9")
                .await
                .unwrap();

            let provider = StubProvider::new("A later and better name");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            assert_eq!(
                svc.retitle("s", &CancellationToken::new()).await.unwrap(),
                RetitleOutcome::Skipped(SkipReason::StillCurrent)
            );
            assert_eq!(provider.calls(), 0);

            // The conversation carries on well past what the name covers.
            seed_more(&storage, "s", 10, REFRESH_AFTER_MESSAGES).await;

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert!(
                matches!(outcome, RetitleOutcome::Retitled { ref title, .. } if title == "A later and better name"),
                "got {outcome:?}"
            );
            assert_eq!(provider.calls(), 1);
        }

        async fn seed_more(storage: &InMemorySessionStorage, session: &str, from: usize, n: usize) {
            for i in from..from + n {
                storage
                    .add_message(
                        session.to_string(),
                        SessionMessage::new(
                            format!("m{i}"),
                            session.to_string(),
                            ChatMessage::user(format!("more {i}")),
                        ),
                    )
                    .await
                    .unwrap();
            }
        }

        #[tokio::test]
        async fn a_cancelled_pass_writes_nothing() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 8).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            let provider = StubProvider::new("A name that must never land");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let cancel = CancellationToken::new();
            cancel.cancel();

            assert_eq!(
                svc.retitle("s", &cancel).await.unwrap(),
                RetitleOutcome::Cancelled
            );
            assert_eq!(provider.calls(), 0);
            assert_eq!(
                storage.get_session("s").await.unwrap().title.as_deref(),
                Some("question 0"),
                "a cancelled pass must leave the old title exactly as it was"
            );
        }

        #[tokio::test]
        async fn an_unusable_answer_leaves_the_old_title_standing() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 8).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            // Prose, not a name — the model did not do the task.
            let provider = StubProvider::new(
                "Certainly, here is a suitable name for this particular conversation \
                 which ranged over a number of quite different topics during its course",
            );
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            assert_eq!(
                svc.retitle("s", &CancellationToken::new()).await.unwrap(),
                RetitleOutcome::Unusable
            );
            assert_eq!(
                storage.get_session("s").await.unwrap().title.as_deref(),
                Some("question 0")
            );
            assert_eq!(
                storage.get_title_provenance("s").await.unwrap(),
                (Some("derived".to_string()), None),
                "a refused answer must not claim the title as the model's"
            );
        }

        #[tokio::test]
        async fn too_short_a_conversation_is_left_to_the_fallback() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", MIN_MESSAGES_TO_TITLE - 1).await;
            storage.set_derived_title("s", "question 0").await.unwrap();

            let provider = StubProvider::new("A name");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            assert_eq!(
                svc.retitle("s", &CancellationToken::new()).await.unwrap(),
                RetitleOutcome::Skipped(SkipReason::TooShort)
            );
            assert_eq!(provider.calls(), 0);
        }

        #[tokio::test]
        async fn a_title_pointing_at_a_vanished_message_is_rebuilt() {
            let storage = Arc::new(InMemorySessionStorage::new());
            seed(&storage, "s", 12).await;
            storage
                .set_generated_title("s", "An early name", "no-such-message")
                .await
                .unwrap();

            let provider = StubProvider::new("A name built from what is actually there");
            let svc = SessionTitleService::new(provider.clone(), storage.clone());

            let outcome = svc.retitle("s", &CancellationToken::new()).await.unwrap();
            assert!(
                matches!(outcome, RetitleOutcome::Retitled { .. }),
                "got {outcome:?}"
            );
        }
    }

    #[test]
    fn the_fallback_rule_matches_the_chat_services_shape() {
        assert_eq!(
            derive_fallback_title("  so I was wondering whether we could ship it "),
            "so I was wondering whether we"
        );
        assert_eq!(derive_fallback_title("   "), "");
    }
}
