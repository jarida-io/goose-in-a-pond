//! The one conversation per person that the pond writes into by itself: a schedule's answer, a
//! finished task, something it looked up, a reminder that came due, a suggestion. A notice is a
//! pointer; this is where the thing itself is kept, and where the person can answer it.
//!
//! One per person, not one per person plus one for the household: a result that is nobody's in
//! particular (a household routine's answer) is kept in every member's own thread. A separate
//! household thread was a second "Goose" conversation beside each member's own, and the first
//! member to answer in it took it over, so nobody else could read it again. The household thread
//! is kept only while the Pond has no member to give it to.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};

use crate::models::domain::message::ChatMessage;
use crate::user_data::domain::message_note::MessageNote;
use crate::user_data::domain::profile::Role;
use crate::user_data::domain::session::{
    IdentificationSource, SessionIdentity, SessionMessage, SessionSurface,
};
use crate::user_data::ports::profile::ProfileRepository;
use crate::user_data::ports::session_storage::SessionStorage;

/// Listed in `POND_AUTHORED_SESSION_PREFIXES`: the pond writing here is not a person arriving.
pub const THREAD_PREFIX: &str = "goose-";
const HOUSEHOLD: &str = "household";
pub const THREAD_TITLE: &str = "Goose";
/// A message longer than this is cut: the thread is read on a phone.
const MESSAGE_CHARS: usize = 8000;

/// The thread of `member`, or the household's while the Pond has no members.
pub fn thread_id(member: Option<&str>) -> String {
    format!("{THREAD_PREFIX}{}", member.unwrap_or(HOUSEHOLD))
}

pub fn is_thread(session_id: &str) -> bool {
    session_id.starts_with(THREAD_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posted {
    pub session_id: String,
    pub message_id: String,
}

pub struct GooseThread {
    sessions: Arc<dyn SessionStorage>,
    /// Who a result that is nobody's in particular is kept for.
    members: Option<Arc<dyn ProfileRepository + Send + Sync>>,
}

impl GooseThread {
    pub fn new(sessions: Arc<dyn SessionStorage>) -> Self {
        Self {
            sessions,
            members: None,
        }
    }

    /// What is the household's is kept for each of these profiles, guests aside.
    pub fn with_members(mut self, members: Arc<dyn ProfileRepository + Send + Sync>) -> Self {
        self.members = Some(members);
        self
    }

    /// The household's members, guests aside: a guest is visiting, and the household's routines
    /// are not theirs to read.
    async fn household(&self) -> Result<Vec<String>> {
        let Some(members) = &self.members else {
            return Ok(Vec::new());
        };
        let members = members
            .list()
            .await
            .context("could not read who the household's result is for")?;
        Ok(members
            .into_iter()
            .filter(|m| m.role != Role::Guest)
            .map(|m| m.id)
            .collect())
    }

    /// Adds a message from the pond to `member`'s thread, making the thread on first use.
    pub async fn post(&self, member: &str, text: &str, note: MessageNote) -> Result<Posted> {
        self.post_to(Some(member), text, note).await
    }

    /// Keeps a result that is nobody's in particular in every member's own thread, or in the
    /// household thread while there is no member to keep it for.
    pub async fn post_to_household(&self, text: &str, note: MessageNote) -> Result<Vec<Posted>> {
        let members = self.household().await?;
        if members.is_empty() {
            return Ok(vec![self.post_to(None, text, note).await?]);
        }
        // One member's thread failing does not keep the result from the others.
        let mut posted = Vec::with_capacity(members.len());
        let mut last_error = None;
        for member in &members {
            match self.post_to(Some(member), text, note.clone()).await {
                Ok(p) => posted.push(p),
                Err(error) => {
                    tracing::warn!(%error, %member, "could not keep the household's result for a member");
                    last_error = Some(error);
                }
            }
        }
        match last_error {
            Some(error) if posted.is_empty() => Err(error),
            _ => Ok(posted),
        }
    }

    async fn post_to(&self, member: Option<&str>, text: &str, note: MessageNote) -> Result<Posted> {
        let text = text.trim();
        anyhow::ensure!(
            !text.is_empty() || note.suggestion.is_some(),
            "nothing to post to the thread"
        );
        let session_id = thread_id(member);
        self.ensure(&session_id, member).await?;
        let cut: String = text.chars().take(MESSAGE_CHARS).collect();
        let message = SessionMessage::new(
            uuid::Uuid::new_v4().to_string(),
            session_id.clone(),
            ChatMessage::assistant(cut),
        )
        .with_note(note);
        let message_id = message.id.clone();
        self.sessions
            .add_message(session_id.clone(), message)
            .await
            .context("could not add to the Goose thread")?;
        Ok(Posted {
            session_id,
            message_id,
        })
    }

    async fn ensure(&self, session_id: &str, member: Option<&str>) -> Result<()> {
        if self.sessions.get_session(session_id).await.is_ok() {
            return Ok(());
        }
        self.sessions
            .create_session(session_id.to_string())
            .await
            .context("could not open the Goose thread")?;
        // Best-effort from here: an untitled thread still holds its messages.
        let _ = self
            .sessions
            .mark_surface(session_id, SessionSurface::Goose, None)
            .await;
        let _ = self
            .sessions
            .update_title(session_id, THREAD_TITLE.to_string())
            .await;
        if let Some(member) = member {
            let theirs = SessionIdentity {
                profile_id: Some(member.to_string()),
                source: IdentificationSource::Explicit,
                confidence: None,
            };
            if let Err(error) = self
                .sessions
                .set_session_identity_if_stronger(session_id, &theirs)
                .await
            {
                tracing::warn!(%error, "the Goose thread could not be bound to its member");
            }
        }
        Ok(())
    }
}

static INSTALLED: OnceLock<Arc<GooseThread>> = OnceLock::new();

/// Makes the thread reachable from producers in every crate. The first install wins.
pub fn install(thread: Arc<GooseThread>) {
    let _ = INSTALLED.set(thread);
}

pub fn installed() -> Option<Arc<GooseThread>> {
    INSTALLED.get().cloned()
}

/// Posts when a thread is installed. A failure is logged, not raised: the notice that points
/// here still goes out, and says the thing itself.
pub async fn post(member: &str, text: &str, note: MessageNote) -> Option<Posted> {
    let thread = installed()?;
    match thread.post(member, text, note).await {
        Ok(posted) => Some(posted),
        Err(error) => {
            tracing::warn!(%error, "could not post to the Goose thread");
            None
        }
    }
}

/// [`GooseThread::post_to_household`] when a thread is installed; empty when it is not, or when
/// the post failed, which is logged.
pub async fn post_to_household(text: &str, note: MessageNote) -> Vec<Posted> {
    let Some(thread) = installed() else {
        return Vec::new();
    };
    match thread.post_to_household(text, note).await {
        Ok(posted) => posted,
        Err(error) => {
            tracing::warn!(%error, "could not post the household's result to the Goose threads");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::session_activity::SessionOrigin;
    use crate::user_data::domain::message_note::SourceKind;
    use crate::user_data::domain::profile::CreateProfileRequest;
    use crate::user_data::mocks::mock_profile::MockProfileRepository;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;

    #[tokio::test]
    async fn every_post_for_a_member_lands_in_their_one_thread() {
        let sessions = Arc::new(InMemorySessionStorage::new());
        let thread = GooseThread::new(sessions.clone());
        let brief = MessageNote::from_source(SourceKind::Schedule, "Daily Briefing", None);

        let first = thread
            .post("jerry", "Here is your brief.", brief.clone())
            .await
            .unwrap();
        let second = thread
            .post("jerry", "  The dollar is at 129.  ", brief)
            .await
            .unwrap();

        assert_eq!(first.session_id, "goose-jerry");
        assert_eq!(second.session_id, first.session_id);
        assert_ne!(second.message_id, first.message_id);

        let kept = sessions.get_messages("goose-jerry").await.unwrap();
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[1].message.content, "The dollar is at 129.");
        assert_eq!(
            kept[1]
                .note
                .as_ref()
                .and_then(|n| n.from.as_ref())
                .map(|f| f.label.as_str()),
            Some("Daily Briefing")
        );
    }

    async fn household_of(
        names: &[&str],
        guest: Option<&str>,
    ) -> (Arc<MockProfileRepository>, Vec<String>) {
        let profiles = Arc::new(MockProfileRepository::new());
        let mut ids = Vec::new();
        for name in names {
            let made = profiles
                .create(CreateProfileRequest {
                    display_name: (*name).to_string(),
                    avatar_emoji: String::new(),
                })
                .await
                .unwrap();
            ids.push(made.id);
        }
        if let Some(name) = guest {
            let made = profiles
                .create(CreateProfileRequest {
                    display_name: name.to_string(),
                    avatar_emoji: String::new(),
                })
                .await
                .unwrap();
            profiles
                .set_role(&made.id, Role::Guest, None)
                .await
                .unwrap();
        }
        (profiles, ids)
    }

    /// The bug this replaced: a household of two read a "Goose" thread of their own and a second
    /// "Goose" thread holding every routine's answer.
    #[tokio::test]
    async fn a_households_result_is_kept_in_each_members_own_thread_and_no_other() {
        let sessions = Arc::new(InMemorySessionStorage::new());
        let (profiles, ids) = household_of(&["Amina", "Baraka"], Some("Visitor")).await;
        let thread = GooseThread::new(sessions.clone()).with_members(profiles);
        let brief = MessageNote::from_source(SourceKind::Schedule, "Daily Briefing", None);

        let posted = thread
            .post_to_household("The bins go out tonight.", brief.clone())
            .await
            .unwrap();
        thread
            .post(&ids[0], "Your parcel came.", brief)
            .await
            .unwrap();

        let mut threads: Vec<_> = posted.iter().map(|p| p.session_id.clone()).collect();
        threads.sort();
        let mut expected: Vec<_> = ids.iter().map(|id| thread_id(Some(id))).collect();
        expected.sort();
        assert_eq!(threads, expected, "one copy per member, none for the guest");
        assert!(sessions.get_session(&thread_id(None)).await.is_err());
        assert_eq!(
            sessions
                .get_messages(&thread_id(Some(&ids[0])))
                .await
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            sessions
                .get_messages(&thread_id(Some(&ids[1])))
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn with_nobody_to_keep_it_for_the_households_result_waits_in_the_household_thread() {
        let sessions = Arc::new(InMemorySessionStorage::new());
        let (profiles, _) = household_of(&[], None).await;
        let thread = GooseThread::new(sessions.clone()).with_members(profiles);
        let note = MessageNote::from_source(SourceKind::Schedule, "News", None);

        let posted = thread
            .post_to_household("Nothing new.", note)
            .await
            .unwrap();

        assert_eq!(posted.len(), 1);
        assert_eq!(posted[0].session_id, "goose-household");
    }

    #[tokio::test]
    async fn an_empty_post_is_refused_and_the_thread_is_not_a_person_arriving() {
        let thread = GooseThread::new(Arc::new(InMemorySessionStorage::new()));
        assert!(thread
            .post("jerry", "   ", MessageNote::default())
            .await
            .is_err());
        assert!(is_thread(&thread_id(Some("jerry"))));
        assert!(!SessionOrigin::of(&thread_id(None)).is_human());
        assert!(!is_thread("f47ac10b-58cc-4372-a567-0e02b2c3d479"));
    }
}
