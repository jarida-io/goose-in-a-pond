//! The one conversation per person that the pond writes into by itself: a schedule's answer, a
//! finished task, something it looked up, a reminder that came due, a suggestion. A notice is a
//! pointer; this is where the thing itself is kept, and where the person can answer it.

use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};

use crate::models::domain::message::ChatMessage;
use crate::user_data::domain::message_note::MessageNote;
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

/// The thread of `member`, or the household's when the result is nobody's in particular.
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
    /// Lets a result that is nobody's in particular go to the only member there is.
    members: Option<Arc<dyn ProfileRepository + Send + Sync>>,
}

impl GooseThread {
    pub fn new(sessions: Arc<dyn SessionStorage>) -> Self {
        Self {
            sessions,
            members: None,
        }
    }

    /// In a household of one, what is the household's is theirs.
    pub fn with_members(mut self, members: Arc<dyn ProfileRepository + Send + Sync>) -> Self {
        self.members = Some(members);
        self
    }

    async fn sole_member(&self) -> Option<String> {
        let members = self.members.as_ref()?.list().await.ok()?;
        match members.as_slice() {
            [only] => Some(only.id.clone()),
            _ => None,
        }
    }

    /// Adds a message from the pond to `member`'s thread, making the thread on first use.
    pub async fn post(
        &self,
        member: Option<&str>,
        text: &str,
        note: MessageNote,
    ) -> Result<Posted> {
        let text = text.trim();
        anyhow::ensure!(
            !text.is_empty() || note.suggestion.is_some(),
            "nothing to post to the thread"
        );
        let member = match member {
            Some(member) => Some(member.to_string()),
            None => self.sole_member().await,
        };
        let member = member.as_deref();
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
pub async fn post(member: Option<&str>, text: &str, note: MessageNote) -> Option<Posted> {
    let thread = installed()?;
    match thread.post(member, text, note).await {
        Ok(posted) => Some(posted),
        Err(error) => {
            tracing::warn!(%error, "could not post to the Goose thread");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::session_activity::SessionOrigin;
    use crate::user_data::domain::message_note::SourceKind;
    use crate::user_data::mocks::mock_session::InMemorySessionStorage;

    #[tokio::test]
    async fn every_post_for_a_member_lands_in_their_one_thread() {
        let sessions = Arc::new(InMemorySessionStorage::new());
        let thread = GooseThread::new(sessions.clone());
        let brief = MessageNote::from_source(SourceKind::Schedule, "Daily Briefing", None);

        let first = thread
            .post(Some("jerry"), "Here is your brief.", brief.clone())
            .await
            .unwrap();
        let second = thread
            .post(Some("jerry"), "  The dollar is at 129.  ", brief.clone())
            .await
            .unwrap();
        let house = thread
            .post(None, "The bins go out tonight.", brief)
            .await
            .unwrap();

        assert_eq!(first.session_id, "goose-jerry");
        assert_eq!(second.session_id, first.session_id);
        assert_ne!(second.message_id, first.message_id);
        assert_eq!(house.session_id, "goose-household");

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

    #[tokio::test]
    async fn an_empty_post_is_refused_and_the_thread_is_not_a_person_arriving() {
        let thread = GooseThread::new(Arc::new(InMemorySessionStorage::new()));
        assert!(thread
            .post(Some("jerry"), "   ", MessageNote::default())
            .await
            .is_err());
        assert!(is_thread(&thread_id(Some("jerry"))));
        assert!(!SessionOrigin::of(&thread_id(None)).is_human());
        assert!(!is_thread("f47ac10b-58cc-4372-a567-0e02b2c3d479"));
    }
}
