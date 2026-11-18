//! The live [`ExtractionGate`]: one critic call over what a conversation window would save.

use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use pond_core::user_data::domain::dont_list::Strength;
use pond_core::user_data::ports::conversation_extractor::{
    estimated_tokens, ExtractionWindow, EXTRACTION_PROMPT_BUDGET_TOKENS,
};
use pond_core::user_data::ports::dont_list::DontListRepository;
use pond_core::user_data::ports::extraction_gate::ExtractionGate;
use pond_core::user_data::services::dont_list::for_brief;
use pond_core::user_data::services::extraction_gate::{
    build_gate_message, parse_gate_drops, GateCandidate, GATE_SYSTEM,
};

use crate::conversation_extractor::render_window_message;
use crate::engine_runner::{EngineModel, ModelStop};

pub struct ModelExtractionGate {
    model: Arc<dyn EngineModel>,
    dont: Arc<dyn DontListRepository>,
}

impl ModelExtractionGate {
    pub fn new(model: Arc<dyn EngineModel>, dont: Arc<dyn DontListRepository>) -> Self {
        Self { model, dont }
    }

    /// The member's don't-list as the critic reads it: every `never` rule, then the newest
    /// `avoid` rules. Empty for a window nobody is named for.
    async fn dont_list(&self, profile_id: Option<&str>) -> Result<String> {
        let Some(member) = profile_id else {
            return Ok(String::new());
        };
        let never = self.dont.list(member, Some(Strength::Never)).await?;
        let avoid = self.dont.list(member, Some(Strength::Avoid)).await?;
        Ok(for_brief(&never, &avoid))
    }
}

#[async_trait]
impl ExtractionGate for ModelExtractionGate {
    async fn judge(
        &self,
        window: ExtractionWindow<'_>,
        candidates: &[GateCandidate],
    ) -> Result<Option<Vec<(usize, String)>>> {
        // A list that cannot be read fails the call: a critic blind to it keeps what a rule bans.
        let dont_list = self
            .dont_list(window.subject.profile_id.as_deref())
            .await
            .context("read the member's don't-list")?;
        let fixed = estimated_tokens(GATE_SYSTEM)
            + estimated_tokens(&build_gate_message("", candidates, &dont_list));
        let conversation = render_window_message(
            &window,
            EXTRACTION_PROMPT_BUDGET_TOKENS.saturating_sub(fixed),
        );
        let message = build_gate_message(&conversation, candidates, &dont_list);

        // On its own task: the call holds the bus's Task floor, and a pass dropped mid-call
        // would leave that floor held with nothing left to give it up.
        let model = self.model.clone();
        let reply = tokio::spawn(async move { model.complete(GATE_SYSTEM, message).await })
            .await
            .context("the critic's call did not finish")?;
        match reply {
            Ok(reply) => Ok(parse_gate_drops(&reply, candidates.len())),
            Err(ModelStop::Cut) => Err(anyhow!("a person's turn took the model")),
            Err(ModelStop::Failed(why)) => Err(anyhow!(why)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pond_core::user_data::domain::dont_list::{DontRule, RuleOrigin};
    use pond_core::user_data::mocks::mock_dont_list::InMemoryDontListRepository;
    use pond_core::user_data::ports::conversation_extractor::{
        KnownMemory, WindowMessage, WindowSubject,
    };
    use pond_core::user_data::services::extraction_gate::GateKind;
    use std::sync::Mutex;

    /// Answers every call the same way and keeps what it was asked.
    struct ScriptedModel {
        reply: Result<String, ModelStop>,
        asked: Mutex<Vec<(String, String)>>,
    }

    impl ScriptedModel {
        fn replying(reply: Result<&str, ModelStop>) -> Arc<Self> {
            Arc::new(Self {
                reply: reply.map(str::to_string),
                asked: Mutex::new(Vec::new()),
            })
        }

        fn asked(&self) -> Vec<(String, String)> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl EngineModel for ScriptedModel {
        async fn complete(&self, system: &str, user: String) -> Result<String, ModelStop> {
            self.asked.lock().unwrap().push((system.to_string(), user));
            self.reply.clone()
        }
    }

    fn messages() -> Vec<WindowMessage> {
        [
            ("m1", "user", "I am going to the AI Summit in December."),
            ("m2", "assistant", "Noted. Shall I look up the agenda?"),
        ]
        .into_iter()
        .map(|(id, role, content)| WindowMessage {
            id: id.to_string(),
            role: role.to_string(),
            content: content.to_string(),
            created_at: Utc::now(),
        })
        .collect()
    }

    fn known() -> Vec<KnownMemory> {
        vec![KnownMemory {
            note: "Jerry lives in Nairobi.".to_string(),
            kind_label: "context".to_string(),
            pattern: false,
        }]
    }

    fn candidates() -> Vec<GateCandidate> {
        vec![
            GateCandidate::new(GateKind::Memory, "Jerry lives in Nairobi."),
            GateCandidate::new(GateKind::Interest, "AI Summit"),
        ]
    }

    async fn judge(
        model: &Arc<ScriptedModel>,
        dont: Arc<InMemoryDontListRepository>,
        subject: &WindowSubject,
    ) -> Result<Option<Vec<(usize, String)>>> {
        let (messages, known) = (messages(), known());
        ModelExtractionGate::new(model.clone(), dont)
            .judge(
                ExtractionWindow {
                    subject,
                    assistant_name: "Goose",
                    session_id: "sess-1",
                    window_id: "m2",
                    messages: &messages,
                    known: &known,
                    max_memories: 3,
                    allow_reminders: true,
                    allow_plans: true,
                },
                &candidates(),
            )
            .await
    }

    async fn jerrys_rules() -> Arc<InMemoryDontListRepository> {
        let dont = Arc::new(InMemoryDontListRepository::new());
        for (member, rule) in [
            ("p-jerry", "recipes built on a colour preference"),
            ("p-amara", "anything about football"),
        ] {
            let rule = DontRule::new(member, rule, Strength::Never, RuleOrigin::You, Utc::now())
                .expect("a rule with words");
            dont.add(&rule).await.unwrap();
        }
        dont
    }

    #[tokio::test]
    async fn the_critic_is_shown_the_window_the_members_rules_and_the_candidates() {
        let model = ScriptedModel::replying(Ok(r#"{"drop": [{"n": 1, "why": "already known"}]}"#));
        let subject = WindowSubject::member("p-jerry", "Jerry");

        let ruling = judge(&model, jerrys_rules().await, &subject).await.unwrap();

        assert_eq!(ruling, Some(vec![(0, "already known".to_string())]));
        let asked = model.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, GATE_SYSTEM);
        assert_eq!(
            asked[0].1,
            "Already remembered about Jerry:\n\
             1. [context] Jerry lives in Nairobi.\n\n\
             Conversation:\n\
             Jerry: I am going to the AI Summit in December.\n\
             Goose: Noted. Shall I look up the agenda?\n\n\
             The member's don't-list:\n\
             Never: recipes built on a colour preference\n\n\
             Candidates to check:\n\
             1. memory: Jerry lives in Nairobi.\n\
             2. interest: AI Summit\n\n\
             JSON:"
        );
    }

    #[tokio::test]
    async fn a_window_nobody_is_named_for_is_judged_with_no_dont_list() {
        let model = ScriptedModel::replying(Ok(r#"{"drop": []}"#));

        let ruling = judge(&model, jerrys_rules().await, &WindowSubject::named("Jerry")).await;

        assert_eq!(ruling.unwrap(), Some(vec![]));
        assert!(!model.asked()[0].1.contains("don't-list"));
    }

    #[tokio::test]
    async fn a_reply_that_cannot_be_read_is_no_ruling() {
        let model = ScriptedModel::replying(Ok("They all look fine to me."));
        let subject = WindowSubject::member("p-jerry", "Jerry");

        let ruling = judge(&model, jerrys_rules().await, &subject).await;

        assert_eq!(ruling.unwrap(), None);
    }

    #[tokio::test]
    async fn a_call_that_gave_no_reply_is_an_error_not_a_ruling() {
        let subject = WindowSubject::member("p-jerry", "Jerry");
        for stop in [
            ModelStop::Cut,
            ModelStop::Failed("no model is chosen".to_string()),
        ] {
            let model = ScriptedModel::replying(Err(stop.clone()));
            let ruling = judge(&model, jerrys_rules().await, &subject).await;
            assert!(ruling.is_err(), "{stop:?}");
        }
    }
}
