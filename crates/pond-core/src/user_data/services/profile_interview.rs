//! Runs each member's setup interview over the interview, notes, session and profile stores.
//!
//! One instance serves both the routes and the interview's tools: the lock makes each
//! read-modify-write of an interview whole, since a tool call and the next turn can overlap.

use std::sync::Arc;

use anyhow::Result;
use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use crate::models::domain::message::{ChatMessage, Role};
use crate::user_data::domain::profile_interview::{
    Interview, InterviewRefusal, InterviewStatus, NoteDraft, Topic,
};
use crate::user_data::domain::prompt_extra::PromptExtra;
use crate::user_data::domain::session::SessionMessage;
use crate::user_data::ports::profile::ProfileRepository;
use crate::user_data::ports::profile_interview::ProfileInterviewRepository;
use crate::user_data::ports::prompt_extra::PromptExtraRepository;
use crate::user_data::ports::session_storage::SessionStorage;

/// Whether a caller whose device sits on `rung` may read or change `profile_id`'s own notes and
/// interview. Only the member's own device may; in a household of one, a caller attributed to
/// nobody can only be that member. A failed read narrows.
pub fn may_act_for(
    rung: &crate::security::domain::proven_device::DeviceRung,
    profile_id: &str,
    sole_member: Option<&str>,
) -> bool {
    use crate::security::domain::proven_device::DeviceRung;
    match rung {
        DeviceRung::Member(id) => id == profile_id,
        DeviceRung::NoDevice | DeviceRung::Unattributed => sole_member == Some(profile_id),
        DeviceRung::Unavailable(_) => false,
    }
}

/// The chat session's title, so the interview is easy to find in the conversation list.
pub const INTERVIEW_SESSION_TITLE: &str = "Getting to know you";

/// What an interview turn adds to the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterviewTurn {
    pub profile_id: String,
    /// Rides the turn's `<system-context>`.
    pub context: String,
}

pub struct InterviewService {
    interviews: Arc<dyn ProfileInterviewRepository>,
    extras: Arc<dyn PromptExtraRepository>,
    sessions: Arc<dyn SessionStorage>,
    profiles: Arc<dyn ProfileRepository + Send + Sync>,
    lock: Mutex<()>,
}

impl InterviewService {
    pub fn new(
        interviews: Arc<dyn ProfileInterviewRepository>,
        extras: Arc<dyn PromptExtraRepository>,
        sessions: Arc<dyn SessionStorage>,
        profiles: Arc<dyn ProfileRepository + Send + Sync>,
    ) -> Self {
        Self {
            interviews,
            extras,
            sessions,
            profiles,
            lock: Mutex::new(()),
        }
    }

    pub async fn get(&self, profile_id: &str) -> Result<Option<Interview>> {
        self.interviews.get(profile_id).await
    }

    /// What the member wants to be called, else their display name.
    async fn name_of(&self, profile_id: &str) -> String {
        match self.profiles.get(profile_id).await {
            Ok(Some(p)) => p
                .preferences
                .get("preferred_name")
                .filter(|n| !n.trim().is_empty())
                .cloned()
                .unwrap_or(p.display_name),
            _ => String::new(),
        }
    }

    /// Starts the member's interview, or takes up the open or stopped one. The pond speaks
    /// first: its question is written into the session, so the phone opens on it.
    pub async fn start(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Interview> {
        let _held = self.lock.lock().await;
        let name = self.name_of(profile_id).await;

        if let Some(mut interview) = self.interviews.get(profile_id).await? {
            if interview.status != InterviewStatus::Done {
                let was_stopped = interview.status == InterviewStatus::Stopped;
                interview.resume(now);
                // A question already waiting is the greeting; repeat nothing.
                let waiting = matches!(
                    self.sessions.get_recent_messages(&interview.session_id, 1).await,
                    Ok(rows) if rows.last().is_some_and(|m| m.message.role == Role::Assistant)
                );
                if was_stopped || !waiting {
                    let opening = interview.opening(&name, true);
                    self.say(&interview.session_id, &opening).await?;
                    interview.pending_opening = Some(opening);
                }
                self.interviews.save(&interview).await?;
                return Ok(interview);
            }
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        self.sessions.create_session(session_id.clone()).await?;
        if let Err(e) = self
            .sessions
            .update_title(&session_id, INTERVIEW_SESSION_TITLE.to_string())
            .await
        {
            tracing::warn!(error = %e, "could not title the interview's session");
        }
        let mut interview = Interview::new(profile_id, &session_id, now);
        let opening = interview.opening(&name, false);
        self.say(&session_id, &opening).await?;
        interview.pending_opening = Some(opening);
        self.interviews.save(&interview).await?;
        tracing::info!(
            target: "giap::trace",
            kind = "interview_started",
            profile_id,
            session_id = %session_id,
        );
        Ok(interview)
    }

    async fn say(&self, session_id: &str, text: &str) -> Result<()> {
        let message = SessionMessage::new(
            uuid::Uuid::new_v4().to_string(),
            session_id.to_string(),
            ChatMessage::assistant(text),
        );
        self.sessions
            .add_message(session_id.to_string(), message)
            .await?;
        Ok(())
    }

    /// The interview turn for this session and speaker, or `None` for an ordinary turn. Marks
    /// the speaker's reply, so a draft shown before it counts as seen.
    pub async fn turn_for_session(
        &self,
        session_id: &str,
        speaker: &str,
        now: DateTime<Utc>,
    ) -> Result<Option<InterviewTurn>> {
        let _held = self.lock.lock().await;
        let Some(mut interview) = self.interviews.get_by_session(session_id).await? else {
            return Ok(None);
        };
        if !interview.status.is_open() {
            return Ok(None);
        }
        if interview.profile_id != speaker {
            tracing::warn!(
                kind = "interview_other_speaker",
                session_id,
                "someone other than the interviewed member spoke in the interview's session; \
                 the turn runs as ordinary chat"
            );
            return Ok(None);
        }
        let name = self.name_of(speaker).await;
        let context = interview.turn_context(&name);
        interview.pending_opening = None;
        interview.note_reply(now);
        self.interviews.save(&interview).await?;
        Ok(Some(InterviewTurn {
            profile_id: speaker.to_string(),
            context,
        }))
    }

    async fn with_open<T>(
        &self,
        profile_id: &str,
        change: impl FnOnce(&mut Interview) -> Result<T, InterviewRefusal>,
    ) -> Result<Result<(Interview, T), InterviewRefusal>> {
        let Some(mut interview) = self.interviews.get(profile_id).await? else {
            return Ok(Err(InterviewRefusal::NotOpen));
        };
        match change(&mut interview) {
            Ok(value) => {
                self.interviews.save(&interview).await?;
                Ok(Ok((interview, value)))
            }
            Err(refusal) => Ok(Err(refusal)),
        }
    }

    pub async fn record(
        &self,
        profile_id: &str,
        topic: &str,
        answer: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<(Interview, Option<Topic>), InterviewRefusal>> {
        let _held = self.lock.lock().await;
        let Some(topic) = Topic::from_id(topic) else {
            return Ok(Err(InterviewRefusal::UnknownTopic));
        };
        self.with_open(profile_id, |i| i.record(topic, answer, now))
            .await
    }

    pub async fn skip(
        &self,
        profile_id: &str,
        topic: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<(Interview, Option<Topic>), InterviewRefusal>> {
        let _held = self.lock.lock().await;
        if topic.trim().eq_ignore_ascii_case("all") {
            return self
                .with_open(profile_id, |i| i.skip_rest(now).map(|()| None))
                .await;
        }
        let Some(topic) = Topic::from_id(topic) else {
            return Ok(Err(InterviewRefusal::UnknownTopic));
        };
        self.with_open(profile_id, |i| i.skip(topic, now)).await
    }

    pub async fn stop(
        &self,
        profile_id: &str,
        now: DateTime<Utc>,
    ) -> Result<Result<(Interview, ()), InterviewRefusal>> {
        let _held = self.lock.lock().await;
        self.with_open(profile_id, |i| i.stop(now)).await
    }

    /// Drafts notes from `(topic, instruction)` pairs, checked against what is already saved.
    pub async fn propose(
        &self,
        profile_id: &str,
        drafts: Vec<(String, String)>,
        now: DateTime<Utc>,
    ) -> Result<Result<(Interview, Vec<PromptExtra>), InterviewRefusal>> {
        let _held = self.lock.lock().await;
        let mut parsed = Vec::with_capacity(drafts.len());
        for (topic, instruction) in drafts {
            let Some(topic) = Topic::from_id(&topic) else {
                return Ok(Err(InterviewRefusal::UnknownTopic));
            };
            parsed.push(NoteDraft { topic, instruction });
        }
        let household = self.extras.list_all().await?;
        let existing = self.extras.list_for_profile(profile_id).await?;
        self.with_open(profile_id, |i| {
            i.propose(parsed, &household, &existing, now)
        })
        .await
    }

    /// Saves the draft: `by_tap` from the member's phone, otherwise from the conversation, where
    /// the member must have replied since the draft was shown. Returns the keys saved.
    pub async fn approve(
        &self,
        profile_id: &str,
        by_tap: bool,
        now: DateTime<Utc>,
    ) -> Result<Result<Vec<String>, InterviewRefusal>> {
        let _held = self.lock.lock().await;
        let Some(mut interview) = self.interviews.get(profile_id).await? else {
            return Ok(Err(InterviewRefusal::NoDraft));
        };
        let notes = match interview.approve(by_tap, now) {
            Ok(notes) => notes,
            Err(refusal) => return Ok(Err(refusal)),
        };
        // Notes first: a crash between the two writes leaves the draft to approve again.
        self.extras.upsert_all(&notes).await?;
        self.interviews.save(&interview).await?;
        tracing::info!(
            target: "giap::trace",
            kind = "interview_notes_saved",
            profile_id,
            notes = notes.len(),
            by_tap,
        );
        Ok(Ok(notes.into_iter().map(|n| n.key).collect()))
    }

    /// Removes the member's interview and every note they own, when the member is removed.
    pub async fn forget_member(&self, profile_id: &str) -> Result<()> {
        let _held = self.lock.lock().await;
        self.extras.delete_all_for_profile(profile_id).await?;
        self.interviews.delete(profile_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::mocks::mock_profile::MockProfileRepository;
    use crate::user_data::mocks::mock_profile_interview::MockProfileInterviewRepository;
    use crate::user_data::mocks::mock_prompt_extra::MockPromptExtraRepository;
    use chrono::Duration;

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-31T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    struct Fixture {
        service: InterviewService,
        extras: Arc<MockPromptExtraRepository>,
        sessions: Arc<dyn SessionStorage>,
    }

    async fn fixture() -> Fixture {
        // Member ids need no profile rows here: a missing name only drops it from the greeting.
        let profiles = Arc::new(MockProfileRepository::default());
        let extras = Arc::new(MockPromptExtraRepository::default());
        let sessions: Arc<dyn SessionStorage> =
            Arc::new(crate::user_data::mocks::mock_session::InMemorySessionStorage::default());
        Fixture {
            service: InterviewService::new(
                Arc::new(MockProfileInterviewRepository::default()),
                extras.clone(),
                sessions.clone(),
                profiles,
            ),
            extras,
            sessions,
        }
    }

    #[test]
    fn only_the_members_own_device_may_act_for_them() {
        use crate::security::domain::proven_device::DeviceRung;
        let amina = DeviceRung::Member("amina".into());
        assert!(may_act_for(&amina, "amina", None));
        assert!(!may_act_for(&amina, "otieno", None));
        assert!(!may_act_for(&DeviceRung::NoDevice, "amina", None));
        assert!(may_act_for(&DeviceRung::NoDevice, "amina", Some("amina")));
        assert!(may_act_for(
            &DeviceRung::Unattributed,
            "amina",
            Some("amina")
        ));
        assert!(!may_act_for(
            &DeviceRung::Unattributed,
            "otieno",
            Some("amina")
        ));
        assert!(!may_act_for(
            &DeviceRung::Unavailable("x".into()),
            "amina",
            Some("amina")
        ));
    }

    #[tokio::test]
    async fn the_pond_asks_first_and_a_resume_does_not_repeat_a_waiting_question() {
        let f = fixture().await;
        let started = f.service.start("amina", t0()).await.unwrap();
        let rows = f.sessions.get_messages(&started.session_id).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.role, Role::Assistant);
        assert!(rows[0]
            .message
            .content
            .contains("What would you like me to call you?"));

        let again = f.service.start("amina", t0()).await.unwrap();
        assert_eq!(again.session_id, started.session_id);
        assert_eq!(
            f.sessions
                .get_messages(&started.session_id)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn an_interview_turn_belongs_to_its_member_only() {
        let f = fixture().await;
        let i = f.service.start("amina", t0()).await.unwrap();
        assert!(f
            .service
            .turn_for_session(&i.session_id, "otieno", t0())
            .await
            .unwrap()
            .is_none());
        let turn = f
            .service
            .turn_for_session(&i.session_id, "amina", t0())
            .await
            .unwrap()
            .unwrap();
        assert!(turn
            .context
            .contains("You opened this conversation by saying"));
        // The opening is repeated once; the engine has the conversation from then on.
        let next = f
            .service
            .turn_for_session(&i.session_id, "amina", t0())
            .await
            .unwrap()
            .unwrap();
        assert!(!next.context.contains("You opened"));
        assert!(f
            .service
            .turn_for_session("another-session", "amina", t0())
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn notes_are_saved_for_the_member_only_after_they_reply_to_the_draft() {
        let f = fixture().await;
        let i = f.service.start("amina", t0()).await.unwrap();
        f.service
            .turn_for_session(&i.session_id, "amina", t0())
            .await
            .unwrap();
        f.service
            .record("amina", "address", "Mama Amina", t0())
            .await
            .unwrap()
            .unwrap();
        f.service.skip("amina", "all", t0()).await.unwrap().unwrap();
        f.service
            .propose(
                "amina",
                vec![("address".into(), "Call her Mama Amina.".into())],
                t0() + Duration::seconds(1),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            f.service
                .approve("amina", false, t0() + Duration::seconds(2))
                .await
                .unwrap(),
            Err(InterviewRefusal::NotYetShown)
        );
        f.service
            .turn_for_session(&i.session_id, "amina", t0() + Duration::seconds(10))
            .await
            .unwrap();
        let saved = f
            .service
            .approve("amina", false, t0() + Duration::seconds(11))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved, vec!["me_address".to_string()]);
        assert_eq!(f.extras.list_for_profile("amina").await.unwrap().len(), 1);
        assert!(f
            .extras
            .list_for_profile("otieno")
            .await
            .unwrap()
            .is_empty());
        assert!(
            f.extras.list_active().await.unwrap().is_empty(),
            "never a household note"
        );

        // A finished interview starts a new round in a new session.
        let round_two = f.service.start("amina", t0()).await.unwrap();
        assert_ne!(round_two.session_id, i.session_id);
        assert_eq!(round_two.status, InterviewStatus::InProgress);
    }

    #[tokio::test]
    async fn a_stopped_interview_greets_again_on_resume_and_forgetting_removes_everything() {
        let f = fixture().await;
        let i = f.service.start("amina", t0()).await.unwrap();
        f.service.stop("amina", t0()).await.unwrap().unwrap();
        assert!(f
            .service
            .turn_for_session(&i.session_id, "amina", t0())
            .await
            .unwrap()
            .is_none());
        let resumed = f.service.start("amina", t0()).await.unwrap();
        assert_eq!(resumed.status, InterviewStatus::InProgress);
        let rows = f.sessions.get_messages(&i.session_id).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].message.content.contains("Welcome back"));

        f.extras
            .upsert(&PromptExtra {
                key: "me_food".into(),
                instruction: "No pork.".into(),
                active: true,
                sort_order: 0,
                profile_id: Some("amina".into()),
            })
            .await
            .unwrap();
        f.service.forget_member("amina").await.unwrap();
        assert!(f.service.get("amina").await.unwrap().is_none());
        assert!(f.extras.list_for_profile("amina").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn tools_refuse_when_no_interview_is_open() {
        let f = fixture().await;
        assert_eq!(
            f.service
                .record("amina", "address", "x", t0())
                .await
                .unwrap()
                .map(|_| ()),
            Err(InterviewRefusal::NotOpen)
        );
        f.service.start("amina", t0()).await.unwrap();
        assert_eq!(
            f.service
                .skip("amina", "hobbies", t0())
                .await
                .unwrap()
                .map(|_| ()),
            Err(InterviewRefusal::UnknownTopic)
        );
    }
}
