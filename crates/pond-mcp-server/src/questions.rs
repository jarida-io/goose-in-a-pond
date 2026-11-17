//! giap-questions: the `ask` tool. The chat model puts one to four structured questions to the
//! member speaking; the card is drawn from the call's `tool_call` frame, and the tool waits for
//! the answers (docs/architecture/structured-questions.md, section 5).

use std::sync::{Arc, OnceLock};

use chrono::Utc;
use pond_core::mcp::domain::tool_group::QUESTIONS_EXTENSION;
use pond_core::mcp::services::screenless_sessions;
use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::shared::services::turn_authority::TurnAuthorityRegistry;
use pond_core::user_data::domain::interest::topic_key;
use pond_core::user_data::domain::question::{QuestionDraft, QuestionOption};
use pond_core::user_data::services::question_desk::{QuestionDesk, SPOKEN_REFUSAL};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        RequestMetaObject, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::DuplexStream;

use crate::Dep;

static QUESTION_DESK: OnceLock<Arc<QuestionDesk>> = OnceLock::new();

/// Install the desk the answer route also holds, once per binary path, before the first turn.
pub fn init_questions_deps(desk: Arc<QuestionDesk>) {
    let _ = QUESTION_DESK.set(desk);
}

fn installed_desk() -> Option<Arc<QuestionDesk>> {
    QUESTION_DESK.get().cloned()
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_questions_server(reader: DuplexStream, writer: DuplexStream) {
    crate::serve_builtin(
        QUESTIONS_EXTENSION,
        QuestionsMcpServer::new(),
        reader,
        writer,
    );
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskOption {
    /// The answer as the member taps it, at most 60 characters.
    pub label: String,
    /// One line on what choosing it means.
    #[serde(default)]
    pub description: String,
    /// Only when options are hard to tell apart from their labels: {"kind": "draft", "text": "<the first lines it leads to>"}, {"kind": "place", "name": "...", "price": "...", "url": "..."} from a web source, or {"kind": "table", "columns": [...], "rows": [[...]]} comparing the options.
    #[serde(default)]
    pub preview: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskQuestion {
    /// A chip of at most 12 characters naming what is asked, for example "Dates" or "Budget".
    #[serde(default)]
    pub header: String,
    /// One question to the member, as "you", at most 200 characters.
    #[serde(default, alias = "question")]
    pub text: String,
    /// Two to four answers they can tap, each different. Never an "Other": every card offers one.
    #[serde(default)]
    #[schemars(with = "Vec<AskOption>")]
    pub options: Vec<QuestionOption>,
    /// The place, from 0, of the option you would pick; it is shown first, marked Recommended.
    #[serde(default)]
    #[schemars(with = "Option<usize>")]
    pub recommended: Option<serde_json::Value>,
    /// True only when several options can be chosen together.
    #[serde(default)]
    pub multi: bool,
}

impl AskQuestion {
    fn draft(self) -> QuestionDraft {
        let recommended = match &self.recommended {
            Some(serde_json::Value::Number(at)) => {
                at.as_u64().and_then(|at| usize::try_from(at).ok())
            }
            Some(serde_json::Value::String(label)) => {
                let key = topic_key(label);
                self.options.iter().position(|o| topic_key(&o.label) == key)
            }
            _ => None,
        };
        QuestionDraft {
            header: self.header,
            text: self.text,
            options: self.options,
            recommended,
            multi: self.multi,
            gap: None,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AskParams {
    /// One to four questions, shown together on one card.
    #[serde(default)]
    pub questions: Vec<AskQuestion>,
}

/// Why a call asked nothing. Returned as tool success text: a small model retries a protocol
/// error verbatim.
#[derive(Debug, Clone, PartialEq)]
enum Refusal {
    Unavailable,
    NotAMember,
    NotAChat,
}

impl Refusal {
    fn message(&self) -> &'static str {
        match self {
            Self::Unavailable => "Questions are not available on this pond; ask in plain words.",
            Self::NotAMember => {
                "I can only ask a household member I have recognised; ask in plain words."
            }
            Self::NotAChat => "There is no chat to show the questions in; ask in plain words.",
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::NotAMember => "not_a_member",
            Self::NotAChat => "not_a_chat",
        }
    }
}

#[derive(Clone)]
pub struct QuestionsMcpServer {
    desk: Dep<QuestionDesk>,
    authority: Dep<dyn DraftAuthority>,
    /// `None` to read the registry installed with the orchestrator.
    turns: Option<Arc<TurnAuthorityRegistry>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

impl Default for QuestionsMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl QuestionsMcpServer {
    pub fn new() -> Self {
        Self {
            desk: Dep::Installed(installed_desk),
            authority: Dep::Installed(crate::speaker_authority),
            turns: None,
            tool_router: Self::tool_router(),
        }
    }

    #[cfg(test)]
    fn with(
        desk: Arc<QuestionDesk>,
        authority: Arc<dyn DraftAuthority>,
        turns: Arc<TurnAuthorityRegistry>,
    ) -> Self {
        Self {
            desk: Dep::Fixed(Some(desk)),
            authority: Dep::Fixed(Some(authority)),
            turns: Some(turns),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Ask the person one to four quick questions when a detail you need is missing and its answer \
would change what you do: dates, who is coming, a budget, a choice between real options. They \
see a card with your options and can always write their own answer. This waits for their \
answers and returns them as lines like \"Dates: 12-15 Dec\". Never ask what you already know or \
can look up.")]
    async fn ask(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<AskParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("ask");
        let reply = match self.run_ask(&ctx.meta, params.0).await {
            Ok(reply) => reply,
            Err(refusal) => {
                tracing::warn!(
                    target: "giap::trace",
                    kind = "ask_tool_refused",
                    reason = refusal.code(),
                    "an ask call was refused"
                );
                refusal.message().to_string()
            }
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(reply)]))
    }
}

impl QuestionsMcpServer {
    async fn run_ask(&self, meta: &RequestMetaObject, p: AskParams) -> Result<String, Refusal> {
        let engine_session =
            crate::session_meta::session_from_meta(meta).ok_or(Refusal::NotAChat)?;
        if screenless_sessions::global().is_screenless(&engine_session) {
            return Ok(SPOKEN_REFUSAL.to_string());
        }
        let desk = self.desk.get().ok_or(Refusal::Unavailable)?;
        let member = crate::session_meta::speaking_member(self.authority.get(), meta)
            .await
            .map_err(|_| Refusal::NotAMember)?;
        let session =
            crate::session_meta::turn_session(meta, self.turns.clone()).ok_or(Refusal::NotAChat)?;
        let drafts: Vec<QuestionDraft> = p.questions.into_iter().map(AskQuestion::draft).collect();
        desk.ask(&member, &session, drafts, false, Utc::now())
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "the ask tool could not store its questions");
                Refusal::Unavailable
            })
    }
}

#[tool_handler]
impl ServerHandler for QuestionsMcpServer {
    fn get_info(&self) -> ServerConfig {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                QUESTIONS_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP structured questions: a card of one to four questions with options to tap, \
                 answered by the member before the turn carries on.",
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_meta::fakes::{meta, FixedSpeaker};
    use pond_core::shared::domain::orchestration::DelegationAuthority;
    use pond_core::user_data::domain::profile::ProfileScope;
    use pond_core::user_data::domain::question::{Answer, QuestionPurpose};
    use pond_core::user_data::mocks::mock_questions::InMemoryQuestionRepository;
    use pond_core::user_data::ports::questions::QuestionRepository;
    use std::time::Duration;

    fn server(
        repo: Arc<InMemoryQuestionRepository>,
        scope: Option<ProfileScope>,
    ) -> (QuestionsMcpServer, Arc<TurnAuthorityRegistry>) {
        let desk = Arc::new(QuestionDesk::new(repo).waiting_for(Duration::from_secs(5)));
        let turns = Arc::new(TurnAuthorityRegistry::new());
        let speaker = Arc::new(FixedSpeaker { scope, sole: None });
        (
            QuestionsMcpServer::with(desk, speaker, turns.clone()),
            turns,
        )
    }

    fn params(raw: serde_json::Value) -> AskParams {
        serde_json::from_value(raw).unwrap()
    }

    fn dates() -> AskParams {
        params(serde_json::json!({"questions": [{
            "header": "Dates", "text": "When do you want to go?",
            "options": [{"label": "12-15 Dec", "description": "A long weekend"}, "19-22 Dec"],
            "recommended": "19-22 Dec"
        }]}))
    }

    #[test]
    fn a_question_reads_with_its_pick_as_a_place_or_a_label() {
        let draft = dates().questions.into_iter().next().unwrap().draft();
        assert_eq!(draft.recommended, Some(1));
        assert_eq!(draft.options[1].label, "19-22 Dec");
        let by_place = params(serde_json::json!({"questions": [{
            "header": "Dates", "question": "When?", "options": ["A", "B"], "recommended": 0
        }]}));
        let draft = by_place.questions.into_iter().next().unwrap().draft();
        assert_eq!((draft.text.as_str(), draft.recommended), ("When?", Some(0)));
    }

    #[tokio::test]
    async fn the_tool_stores_the_card_for_the_chat_and_returns_the_answers() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let (server, turns) = server(repo.clone(), Some(ProfileScope::Owner("amina".into())));
        let _lease = turns.publish(
            "engine-1",
            DelegationAuthority::root(
                "chat-7",
                ProfileScope::Owner("amina".into()),
                Default::default(),
            ),
            Default::default(),
        );
        let asking = tokio::spawn({
            let server = server.clone();
            async move { server.run_ask(&meta(), dates()).await }
        });
        let mut batch = Vec::new();
        for _ in 0..200 {
            batch = repo.for_session("amina", "chat-7").await.unwrap();
            if !batch.is_empty() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].purpose, QuestionPurpose::Chat);
        let desk = server.desk.get().unwrap();
        desk.answer_batch(
            "amina",
            &batch[0].batch_id,
            vec![(
                batch[0].id.clone(),
                Answer {
                    choices: vec!["19-22 Dec".into()],
                    text: None,
                },
            )],
            Utc::now(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(asking.await.unwrap().unwrap(), "Dates: 19-22 Dec");
    }

    #[tokio::test]
    async fn a_spoken_turn_a_guest_and_a_call_outside_a_chat_are_refused() {
        let repo = Arc::new(InMemoryQuestionRepository::new());
        let (amina, _turns) = server(repo.clone(), Some(ProfileScope::Owner("amina".into())));
        assert_eq!(
            amina.run_ask(&meta(), dates()).await,
            Err(Refusal::NotAChat),
            "no live turn names the chat"
        );
        let (guest, _) = server(repo.clone(), Some(ProfileScope::Guest));
        assert_eq!(
            guest.run_ask(&meta(), dates()).await,
            Err(Refusal::NotAMember)
        );
        let mut spoken_meta = RequestMetaObject::new();
        spoken_meta.0.insert(
            crate::session_meta::SESSION_ID_META_KEY.to_string(),
            serde_json::json!("engine-spoken-ask"),
        );
        screenless_sessions::global().set("engine-spoken-ask", true);
        let spoken = amina.run_ask(&spoken_meta, dates()).await;
        screenless_sessions::global().set("engine-spoken-ask", false);
        assert_eq!(spoken.as_deref(), Ok(SPOKEN_REFUSAL));
        assert!(repo
            .for_session("amina", "chat-7")
            .await
            .unwrap()
            .is_empty());
    }

    #[test]
    fn the_schema_tells_the_model_the_limits_code_holds_it_to() {
        let schema = serde_json::to_string(&schemars::schema_for!(AskParams)).unwrap();
        for words in [
            "12 characters",
            "200 characters",
            "60 characters",
            "Two to four",
        ] {
            assert!(schema.contains(words), "{words}: {schema}");
        }
    }
}
