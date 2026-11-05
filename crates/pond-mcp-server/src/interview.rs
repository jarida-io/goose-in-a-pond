//! giap-interview: the setup interview's tools. Each acts for the member speaking in the call's
//! session and only on that member's own interview and notes; nobody else's are reachable.

use std::sync::{Arc, OnceLock};

use chrono::Utc;
use pond_core::mcp::domain::tool_group::INTERVIEW_EXTENSION;
use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::profile_interview::{
    Interview, InterviewRefusal, InterviewStatus, Topic, TopicStatus,
};
use pond_core::user_data::domain::prompt_extra::PromptExtra;
use pond_core::user_data::services::profile_interview::InterviewService;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorData, Implementation, InitializeResult, Meta,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::io::DuplexStream;

static INTERVIEW_SERVICE: OnceLock<Arc<InterviewService>> = OnceLock::new();

/// Install the interview service once per binary path, before the first turn.
pub fn init_interview_deps(service: Arc<InterviewService>) {
    let _ = INTERVIEW_SERVICE.set(service);
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_interview_server(reader: DuplexStream, writer: DuplexStream) {
    crate::serve_builtin(
        INTERVIEW_EXTENSION,
        InterviewMcpServer::new(),
        reader,
        writer,
    );
}

/// Why a call did nothing. Returned as tool success text: a small model retries a protocol error
/// verbatim.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    Unavailable,
    /// No session, an unmapped one, a guest, or an unidentified speaker in a household of several.
    NotAMember,
    Interview(InterviewRefusal),
}

impl Refusal {
    fn message(&self) -> String {
        match self {
            Refusal::Unavailable => "The interview is not available on this pond.".to_string(),
            Refusal::NotAMember => {
                "I can only run the interview for a household member I have recognised.".to_string()
            }
            Refusal::Interview(r) => r.message(),
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Refusal::Unavailable => "unavailable",
            Refusal::NotAMember => "not_a_member",
            Refusal::Interview(r) => r.code(),
        }
    }
}

fn answer(tool: &'static str, outcome: Result<String, Refusal>) -> CallToolResult {
    let text = match outcome {
        Ok(text) => text,
        Err(refusal) => {
            tracing::warn!(
                target: "giap::trace",
                kind = "interview_tool_refused",
                tool,
                reason = refusal.code(),
                "an interview tool call was refused"
            );
            refusal.message()
        }
    };
    CallToolResult::success(vec![ContentBlock::text(text)])
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct NoParams {}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecordParams {
    /// The topic answered: address, identity, language, household, routine, food, access, style, avoid or other.
    #[serde(alias = "key", alias = "id")]
    pub topic: String,
    /// The note to keep from their answer: one short standing instruction about them, in the third person, for example "Call her Mama Amina." When the answer gives nothing to keep, call skip_topic instead.
    #[serde(alias = "note", alias = "summary", alias = "text")]
    pub answer: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TopicParams {
    /// The topic: address, identity, language, household, routine, food, access, style, avoid or other; `all` for every topic not yet asked.
    #[serde(alias = "key", alias = "id")]
    pub topic: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DraftNote {
    /// The answered topic this note comes from.
    #[serde(alias = "key", alias = "id")]
    pub topic: String,
    /// One short standing instruction about them, for example "Call her Mama Amina."
    #[serde(alias = "instruction", alias = "text", alias = "content")]
    pub note: String,
}

/// Optional rewrites, one field per topic: the draft itself comes from what record_answer kept,
/// so an empty call (as Ollama delivered it) still drafts every answered topic.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ProposeParams {
    /// What to call them, for example "Call her Mama Amina."
    pub address: Option<String>,
    /// Who they are and what fills their days.
    pub identity: Option<String>,
    /// Which languages to use with them.
    pub language: Option<String>,
    /// Who they live with.
    pub household: Option<String>,
    /// Their usual week.
    pub routine: Option<String>,
    /// Food preferences, allergies and dietary needs.
    pub food: Option<String>,
    /// Hearing, sight or speech to keep in mind.
    pub access: Option<String>,
    /// How to talk to them: length, tone, units.
    pub style: Option<String>,
    /// Topics to avoid or handle with care.
    pub avoid: Option<String>,
    /// Anything else to always keep in mind.
    pub other: Option<String>,
    /// The list shape a model sometimes sends instead: `[{topic, note}]` or `{topic: note}`.
    #[serde(default)]
    #[schemars(skip)]
    pub notes: Option<serde_json::Value>,
}

impl ProposeParams {
    /// Every note given, as `(topic, note)`, and how many came with no topic to place them under.
    fn drafts(self) -> (Vec<(String, String)>, usize) {
        let mut out: Vec<(String, String)> = [
            ("address", self.address),
            ("identity", self.identity),
            ("language", self.language),
            ("household", self.household),
            ("routine", self.routine),
            ("food", self.food),
            ("access", self.access),
            ("style", self.style),
            ("avoid", self.avoid),
            ("other", self.other),
        ]
        .into_iter()
        .filter_map(|(topic, note)| {
            note.filter(|n| !n.trim().is_empty())
                .map(|n| (topic.to_string(), n))
        })
        .collect();
        let mut unplaced = 0;
        match self.notes {
            Some(serde_json::Value::Object(map)) => {
                for (topic, note) in map {
                    match note.as_str() {
                        Some(n) => out.push((topic, n.to_string())),
                        None => unplaced += 1,
                    }
                }
            }
            Some(serde_json::Value::Array(items)) => {
                for item in items {
                    match serde_json::from_value::<DraftNote>(item) {
                        Ok(d) => out.push((d.topic, d.note)),
                        Err(_) => unplaced += 1,
                    }
                }
            }
            _ => {}
        }
        (out, unplaced)
    }
}

#[derive(Clone)]
pub struct InterviewMcpServer {
    /// Tests only; in production the installed deps are read on every call.
    fixed: Option<(Arc<InterviewService>, Arc<dyn DraftAuthority>)>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

impl Default for InterviewMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl InterviewMcpServer {
    pub fn new() -> Self {
        Self {
            fixed: None,
            tool_router: Self::tool_router(),
        }
    }

    #[cfg(test)]
    fn with(service: Arc<InterviewService>, authority: Arc<dyn DraftAuthority>) -> Self {
        Self {
            fixed: Some((service, authority)),
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Where the setup interview stands: the next topic to ask about, which topics are answered or \
skipped, the person's saved notes, and any draft waiting for their approval.")]
    async fn interview_status(
        &self,
        ctx: RequestContext<RoleServer>,
        _params: Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("interview_status");
        Ok(answer("interview_status", self.run_status(&ctx.meta).await))
    }

    #[tool(description = "\
Keep the note from the person's answer on one topic, right after they give it. Returns the next \
topic.")]
    async fn record_answer(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<RecordParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("record_answer");
        Ok(answer(
            "record_answer",
            self.run_record(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
Skip a topic the person would rather not talk about, or `all` when they want to finish now. \
Returns the next topic.")]
    async fn skip_topic(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<TopicParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("skip_topic");
        Ok(answer(
            "skip_topic",
            self.run_skip(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
Stop the interview because the person wants to. Nothing is saved; they can carry on later.")]
    async fn stop_interview(
        &self,
        ctx: RequestContext<RoleServer>,
        _params: Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("stop_interview");
        Ok(answer("stop_interview", self.run_stop(&ctx.meta).await))
    }

    #[tool(description = "\
Draft the person's notes from what you recorded, for them to approve. Needs no arguments; a \
topic's field replaces its note. Show them the returned notes exactly and ask whether to save them.")]
    async fn propose_notes(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ProposeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("propose_notes");
        Ok(answer(
            "propose_notes",
            self.run_propose(&ctx.meta, params.0).await,
        ))
    }

    #[tool(description = "\
Save the drafted notes. Only after the person said yes to the notes you showed them.")]
    async fn save_notes(
        &self,
        ctx: RequestContext<RoleServer>,
        _params: Parameters<NoParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("save_notes");
        Ok(answer("save_notes", self.run_save(&ctx.meta).await))
    }
}

fn next_line(next: Option<Topic>) -> String {
    match next {
        Some(topic) => format!(
            "Next topic: {} (for example: \"{}\").",
            topic.id(),
            topic.question()
        ),
        None => "No topic is left: draft their notes with propose_notes.".to_string(),
    }
}

/// What to do next, after an answer or a skip: the next topic, or the draft that formed once
/// none was left, so the model shows it rather than inventing a save.
fn progress(interview: &Interview, next: Option<Topic>) -> String {
    if interview.status == InterviewStatus::AwaitingApproval {
        let draft: Vec<String> = interview
            .draft
            .iter()
            .map(|d| format!("- {}", d.instruction))
            .collect();
        return format!(
            "No topic is left, so their notes are drafted. They are NOT saved. Show them these \
             notes exactly, and ask whether to save them or change any:\n{}",
            draft.join("\n")
        );
    }
    next_line(next)
}

fn notes_lines(notes: &[PromptExtra]) -> String {
    notes
        .iter()
        .map(|n| format!("- {}", n.instruction))
        .collect::<Vec<_>>()
        .join("\n")
}

impl InterviewMcpServer {
    fn deps(&self) -> Result<(Arc<InterviewService>, Arc<dyn DraftAuthority>), Refusal> {
        if let Some((service, authority)) = &self.fixed {
            return Ok((service.clone(), authority.clone()));
        }
        let service = INTERVIEW_SERVICE
            .get()
            .cloned()
            .ok_or(Refusal::Unavailable)?;
        let authority = crate::speaker_authority().ok_or(Refusal::Unavailable)?;
        Ok((service, authority))
    }

    /// The member speaking in this call's session. In a household of one, an unidentified
    /// speaker can only be that member.
    async fn member(&self, meta: &Meta) -> Result<(Arc<InterviewService>, String), Refusal> {
        let (service, authority) = self.deps()?;
        let session = crate::session_meta::session_from_meta(meta).ok_or(Refusal::NotAMember)?;
        let (scope, _) = authority
            .actor_for_engine_session(&session)
            .await
            .ok_or(Refusal::NotAMember)?;
        let member = match scope {
            ProfileScope::Owner(id) => Some(id),
            ProfileScope::Household => authority.sole_member().await,
            ProfileScope::Guest => None,
        };
        member
            .filter(|id| !id.trim().is_empty())
            .map(|id| (service, id))
            .ok_or(Refusal::NotAMember)
    }

    async fn run_status(&self, meta: &Meta) -> Result<String, Refusal> {
        let (service, member) = self.member(meta).await?;
        let interview = service
            .get(&member)
            .await
            .map_err(|_| Refusal::Unavailable)?
            .filter(|i| i.status.is_open())
            .ok_or(Refusal::Interview(InterviewRefusal::NotOpen))?;
        Ok(describe(&interview))
    }

    async fn run_record(&self, meta: &Meta, p: RecordParams) -> Result<String, Refusal> {
        let (service, member) = self.member(meta).await?;
        let (interview, next) = service
            .record(&member, &p.topic, &p.answer, Utc::now())
            .await
            .map_err(|_| Refusal::Unavailable)?
            .map_err(Refusal::Interview)?;
        Ok(format!("Noted. {}", progress(&interview, next)))
    }

    async fn run_skip(&self, meta: &Meta, p: TopicParams) -> Result<String, Refusal> {
        let (service, member) = self.member(meta).await?;
        let (interview, next) = service
            .skip(&member, &p.topic, Utc::now())
            .await
            .map_err(|_| Refusal::Unavailable)?
            .map_err(Refusal::Interview)?;
        Ok(format!(
            "Skipped; do not ask about it again. {}",
            progress(&interview, next)
        ))
    }

    async fn run_stop(&self, meta: &Meta) -> Result<String, Refusal> {
        let (service, member) = self.member(meta).await?;
        service
            .stop(&member, Utc::now())
            .await
            .map_err(|_| Refusal::Unavailable)?
            .map_err(Refusal::Interview)?;
        Ok("Stopped. Nothing was saved, and they can carry on another time.".to_string())
    }

    async fn run_propose(&self, meta: &Meta, p: ProposeParams) -> Result<String, Refusal> {
        let (service, member) = self.member(meta).await?;
        // A note with no topic replaces nothing; the draft comes from the record either way.
        let (drafts, _unplaced): (Vec<(String, String)>, usize) = p.drafts();
        let given: Vec<Topic> = drafts
            .iter()
            .filter_map(|(t, _)| Topic::from_id(t))
            .collect();
        let (interview, notes) = service
            .propose(&member, drafts, Utc::now())
            .await
            .map_err(|_| Refusal::Unavailable)?
            .map_err(Refusal::Interview)?;
        let mut out = format!(
            "Drafted, not saved. Show them these notes exactly, and ask whether to save them or \
             change any:\n{}",
            notes_lines(&notes)
        );
        let dropped: Vec<&str> = given
            .iter()
            .filter(|t| !interview.draft.iter().any(|d| d.topic == **t))
            .map(|t| t.id())
            .collect();
        if !dropped.is_empty() {
            out.push_str(&format!(
                "\nLeft out, since they skipped it: {}.",
                dropped.join(", ")
            ));
        }
        Ok(out)
    }

    async fn run_save(&self, meta: &Meta) -> Result<String, Refusal> {
        let (service, member) = self.member(meta).await?;
        let saved = service
            .approve(&member, false, Utc::now())
            .await
            .map_err(|_| Refusal::Unavailable)?
            .map_err(Refusal::Interview)?;
        Ok(format!(
            "Saved {} notes. Thank them; the interview is finished.",
            saved.len()
        ))
    }
}

fn describe(i: &Interview) -> String {
    let list = |status: TopicStatus| -> String {
        let ids: Vec<&str> = i
            .topics
            .iter()
            .filter(|t| t.status == status)
            .map(|t| t.topic.id())
            .collect();
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.join(", ")
        }
    };
    let mut out = format!(
        "Answered: {}. Skipped: {}. ",
        list(TopicStatus::Answered),
        list(TopicStatus::Skipped)
    );
    if i.status == InterviewStatus::AwaitingApproval {
        let draft: Vec<String> = i
            .draft
            .iter()
            .map(|d| format!("- {}", d.instruction))
            .collect();
        out.push_str(&format!(
            "A draft is waiting for their approval:\n{}",
            draft.join("\n")
        ));
    } else {
        out.push_str(&next_line(i.next_topic()));
    }
    out
}

#[tool_handler]
impl ServerHandler for InterviewMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                INTERVIEW_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP setup interview: one question at a time about the person, then notes they \
                 approve before anything is saved.",
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pond_core::security::ports::policy::{PolicyDecision, PolicyMode};
    use pond_core::user_data::domain::session::IdentificationSource;
    use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
    use pond_core::user_data::mocks::mock_profile_interview::MockProfileInterviewRepository;
    use pond_core::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository;
    use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
    use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;

    /// Engine session `s-amina` is Amina's, `s-house` is unidentified, `s-guest` a guest's.
    struct Speakers {
        sole_member: Option<String>,
    }

    #[async_trait]
    impl DraftAuthority for Speakers {
        async fn policy_mode(&self) -> PolicyMode {
            PolicyMode::Audit
        }
        async fn actor_for_engine_session(
            &self,
            engine_session_id: &str,
        ) -> Option<(ProfileScope, IdentificationSource)> {
            let scope = match engine_session_id {
                "s-amina" => ProfileScope::Owner("amina".into()),
                "s-otieno" => ProfileScope::Owner("otieno".into()),
                "s-house" => ProfileScope::Household,
                "s-guest" => ProfileScope::Guest,
                _ => return None,
            };
            Some((scope, IdentificationSource::PairedDevice))
        }
        async fn sole_member(&self) -> Option<String> {
            self.sole_member.clone()
        }
        async fn audit(&self, _: &str, _: &str, _: &PolicyDecision) {}
    }

    fn meta(engine_session: &str) -> Meta {
        let mut m = Meta::new();
        m.0.insert(
            crate::SESSION_ID_META_KEY.to_string(),
            serde_json::Value::String(engine_session.to_string()),
        );
        m
    }

    fn server(
        sole_member: Option<&str>,
    ) -> (
        InterviewMcpServer,
        Arc<InterviewService>,
        Arc<MockPromptExtraRepository>,
    ) {
        let extras = Arc::new(MockPromptExtraRepository::default());
        let service = Arc::new(InterviewService::new(
            Arc::new(MockProfileInterviewRepository::default()),
            extras.clone(),
            Arc::new(InMemorySessionStorage::default()),
            Arc::new(MockProfileRepository::default()),
        ));
        let authority = Arc::new(Speakers {
            sole_member: sole_member.map(str::to_string),
        });
        (
            InterviewMcpServer::with(service.clone(), authority),
            service,
            extras,
        )
    }

    #[tokio::test]
    async fn a_member_runs_only_their_own_interview() {
        let (srv, service, extras) = server(None);
        service.start("amina", Utc::now()).await.unwrap();

        let said = srv
            .run_record(
                &meta("s-amina"),
                RecordParams {
                    topic: "address".into(),
                    answer: "Mama Amina".into(),
                },
            )
            .await
            .unwrap();
        assert!(said.contains("Next topic: identity"));

        // Otieno has no interview open, and his calls never touch Amina's.
        assert_eq!(
            srv.run_skip(
                &meta("s-otieno"),
                TopicParams {
                    topic: "identity".into()
                }
            )
            .await,
            Err(Refusal::Interview(InterviewRefusal::NotOpen))
        );
        for unknown in ["s-guest", "s-house", "s-nobody"] {
            assert_eq!(
                srv.run_status(&meta(unknown)).await,
                Err(Refusal::NotAMember)
            );
        }
        assert_eq!(srv.run_status(&Meta::new()).await, Err(Refusal::NotAMember));

        srv.run_skip(
            &meta("s-amina"),
            TopicParams {
                topic: "all".into(),
            },
        )
        .await
        .unwrap();
        let drafted = srv
            .run_propose(
                &meta("s-amina"),
                ProposeParams {
                    address: Some("Call her Mama Amina.".into()),
                    household: Some("She does not want to discuss who lives with her.".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(drafted.contains("- Call her Mama Amina."));
        assert!(drafted.contains("Left out, since they skipped it: household."));
        // In the same turn as the draft, the person has not seen it yet.
        assert_eq!(
            srv.run_save(&meta("s-amina")).await,
            Err(Refusal::Interview(InterviewRefusal::NotYetShown))
        );
        assert!(extras.list_for_profile("amina").await.unwrap().is_empty());
    }

    #[test]
    fn a_draft_reads_named_fields_and_the_shapes_a_small_model_reaches_for() {
        let parse =
            |v: serde_json::Value| serde_json::from_value::<ProposeParams>(v).unwrap().drafts();
        let (named, unplaced) = parse(serde_json::json!({
            "address": "Call him Baba Baraka.", "food": "Fish is his favourite.", "avoid": ""
        }));
        assert_eq!(unplaced, 0);
        assert_eq!(named.len(), 2);
        let (listed, _) = parse(serde_json::json!({
            "notes": [{"key": "me_address", "instruction": "Call him Baba Baraka."}]
        }));
        assert_eq!(
            listed,
            vec![("me_address".into(), "Call him Baba Baraka.".into())]
        );
        let (mapped, _) = parse(serde_json::json!({"notes": {"food": "Fish."}}));
        assert_eq!(mapped, vec![("food".into(), "Fish.".into())]);
        // The shape E4B sent on the nano: notes with no topic are not guessed at.
        let (_, unplaced) = parse(serde_json::json!({
            "notes": [{"note": "Call her Mama Amina."}, {"note": "No pork."}]
        }));
        assert_eq!(unplaced, 2);
    }

    /// Ollama delivered `propose_notes` with no arguments at all; the draft must still form.
    #[tokio::test]
    async fn an_empty_draft_call_drafts_what_was_recorded() {
        let (srv, service, _) = server(None);
        service.start("amina", Utc::now()).await.unwrap();
        let record = |topic: &str, note: &str| RecordParams {
            topic: topic.into(),
            answer: note.into(),
        };
        srv.run_record(&meta("s-amina"), record("address", "Call her Mama Amina."))
            .await
            .unwrap();
        srv.run_record(
            &meta("s-amina"),
            record("food", "No pork; allergic to peanuts."),
        )
        .await
        .unwrap();
        let settled = srv
            .run_skip(
                &meta("s-amina"),
                TopicParams {
                    topic: "all".into(),
                },
            )
            .await
            .unwrap();
        assert!(settled.contains("They are NOT saved"), "{settled}");
        assert!(settled.contains("- Call her Mama Amina."), "{settled}");
        let drafted = srv
            .run_propose(&meta("s-amina"), ProposeParams::default())
            .await
            .unwrap();
        assert!(drafted.contains("- Call her Mama Amina."), "{drafted}");
        assert!(
            drafted.contains("- No pork; allergic to peanuts."),
            "{drafted}"
        );
        assert_eq!(
            srv.run_save(&meta("s-amina")).await,
            Err(Refusal::Interview(InterviewRefusal::NotYetShown)),
            "drafted and saved in one turn means the person never saw it"
        );
        assert!(Refusal::Interview(InterviewRefusal::NotYetShown)
            .message()
            .starts_with("Not saved."));
    }

    #[tokio::test]
    async fn an_unidentified_speaker_in_a_household_of_one_is_its_member() {
        let (srv, service, _) = server(Some("amina"));
        service.start("amina", Utc::now()).await.unwrap();
        assert!(srv
            .run_status(&meta("s-house"))
            .await
            .unwrap()
            .contains("Next topic: address"));
        assert_eq!(
            srv.run_status(&meta("s-guest")).await,
            Err(Refusal::NotAMember)
        );
    }
}
