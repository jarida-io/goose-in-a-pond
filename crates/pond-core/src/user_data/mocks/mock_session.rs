use crate::models::domain::message::ImageAttachment;
use crate::user_data::domain::session::{MessageAttachment, Session, SessionMessage};
use crate::user_data::ports::session_storage::{SessionStorage, SessionStorageError};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory session storage for tests.
pub struct InMemorySessionStorage {
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    messages: Arc<RwLock<HashMap<String, Vec<SessionMessage>>>>,
    rolling_summaries: Arc<RwLock<HashMap<String, (String, String)>>>,
    /// GIAP session id -> agent-engine session id.
    engine_sessions: Arc<RwLock<HashMap<String, String>>>,
    /// GIAP session id -> selected tool groups.
    tool_groups: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// GIAP session id -> (title_source, title_through_message_id). Not left to the trait
    /// defaults, or "never overwrite a name a person typed" would be untestable.
    title_provenance: Arc<RwLock<HashMap<String, (Option<String>, Option<String>)>>>,
}

impl InMemorySessionStorage {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            messages: Arc::new(RwLock::new(HashMap::new())),
            rolling_summaries: Arc::new(RwLock::new(HashMap::new())),
            engine_sessions: Arc::new(RwLock::new(HashMap::new())),
            tool_groups: Arc::new(RwLock::new(HashMap::new())),
            title_provenance: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Write a machine-chosen title and its provenance. Leaves `updated_at` alone, as SQLite
    /// does: the idle gate reads that column as a person's activity.
    async fn write_machine_title(
        &self,
        session_id: &str,
        title: &str,
        source: &str,
        through_message_id: Option<&str>,
    ) -> Result<(), SessionStorageError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionStorageError::SessionNotFound(session_id.to_string()))?;
        session.title = Some(title.to_string());
        drop(sessions);

        self.title_provenance.write().await.insert(
            session_id.to_string(),
            (
                Some(source.to_string()),
                through_message_id.map(str::to_string),
            ),
        );
        Ok(())
    }
}

impl Default for InMemorySessionStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SessionStorage for InMemorySessionStorage {
    async fn create_session(&self, session_id: String) -> Result<Session, SessionStorageError> {
        let session = Session::new(session_id.clone());
        self.sessions
            .write()
            .await
            .insert(session_id.clone(), session.clone());
        self.messages.write().await.insert(session_id, Vec::new());
        Ok(session)
    }

    async fn get_session(&self, session_id: &str) -> Result<Session, SessionStorageError> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .cloned()
            .ok_or_else(|| SessionStorageError::SessionNotFound(session_id.to_string()))
    }

    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        self.get_session(&session_id).await?;

        let mut messages = self.messages.write().await;
        if let Some(msgs) = messages.get_mut(&session_id) {
            msgs.push(message.clone());
        } else {
            messages.insert(session_id, vec![message.clone()]);
        }

        Ok(message)
    }

    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self._get_session(session_id).await?;

        Ok(self
            .messages
            .read()
            .await
            .get(session_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError> {
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionStorageError::SessionNotFound(session_id.to_string()))?;
        session.title = Some(title);
        // A person renaming a conversation IS activity, so this one bumps.
        session.updated_at = chrono::Utc::now();
        drop(sessions);
        self.title_provenance
            .write()
            .await
            .insert(session_id.to_string(), (Some("user".to_string()), None));
        Ok(())
    }

    async fn get_title_provenance(
        &self,
        session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok(self
            .title_provenance
            .read()
            .await
            .get(session_id)
            .cloned()
            .unwrap_or((None, None)))
    }

    async fn set_derived_title(
        &self,
        session_id: &str,
        title: &str,
    ) -> Result<(), SessionStorageError> {
        self.write_machine_title(session_id, title, "derived", None)
            .await
    }

    async fn set_generated_title(
        &self,
        session_id: &str,
        title: &str,
        through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        self.write_machine_title(session_id, title, "model", Some(through_message_id))
            .await
    }

    async fn delete_session(&self, session_id: &str) -> Result<(), SessionStorageError> {
        self.sessions.write().await.remove(session_id);
        self.messages.write().await.remove(session_id);
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
        let sessions = self.sessions.read().await;
        let mut result: Vec<Session> = sessions.values().cloned().collect();
        result.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(result)
    }

    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self._get_session(session_id).await?;

        let messages = self.messages.read().await;
        let msgs = messages.get(session_id).cloned().unwrap_or_default();
        let paginated = msgs.into_iter().skip(offset).take(limit).collect();
        Ok(paginated)
    }

    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        self._get_session(session_id).await?;

        let messages = self.messages.read().await;
        let msgs = messages.get(session_id).cloned().unwrap_or_default();
        // Take the last `limit` messages — they are already in chronological order.
        let start = msgs.len().saturating_sub(limit);
        Ok(msgs[start..].to_vec())
    }

    async fn count_messages(&self, session_id: &str) -> Result<u64, SessionStorageError> {
        // A missing session simply has zero messages (matches the SQLite impl).
        Ok(self
            .messages
            .read()
            .await
            .get(session_id)
            .map(|m| m.len() as u64)
            .unwrap_or(0))
    }

    async fn first_user_message(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        use crate::models::domain::message::Role;
        Ok(self.messages.read().await.get(session_id).and_then(|msgs| {
            msgs.iter()
                .find(|m| m.message.role == Role::User)
                .map(|m| m.message.content.clone())
        }))
    }

    async fn get_rolling_summary(
        &self,
        session_id: &str,
    ) -> Result<(Option<String>, Option<String>), SessionStorageError> {
        Ok(self
            .rolling_summaries
            .read()
            .await
            .get(session_id)
            .map(|(s, id)| (Some(s.clone()), Some(id.clone())))
            .unwrap_or((None, None)))
    }

    async fn set_rolling_summary(
        &self,
        session_id: &str,
        summary: &str,
        through_message_id: &str,
    ) -> Result<(), SessionStorageError> {
        self.rolling_summaries.write().await.insert(
            session_id.to_string(),
            (summary.to_string(), through_message_id.to_string()),
        );
        Ok(())
    }

    async fn get_engine_session_id(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, SessionStorageError> {
        Ok(self.engine_sessions.read().await.get(session_id).cloned())
    }

    async fn set_engine_session_id(
        &self,
        session_id: &str,
        engine_session_id: &str,
    ) -> Result<(), SessionStorageError> {
        // Upsert; no session-existence guard, per the port contract.
        self.engine_sessions
            .write()
            .await
            .insert(session_id.to_string(), engine_session_id.to_string());
        Ok(())
    }

    async fn get_session_tool_groups(
        &self,
        session_id: &str,
    ) -> Result<Option<Vec<String>>, SessionStorageError> {
        Ok(self.tool_groups.read().await.get(session_id).cloned())
    }

    async fn set_session_tool_groups(
        &self,
        session_id: &str,
        groups: &[String],
    ) -> Result<(), SessionStorageError> {
        // Replace wholesale; no session-existence guard, per the port contract.
        self.tool_groups
            .write()
            .await
            .insert(session_id.to_string(), groups.to_vec());
        Ok(())
    }

    // ── Image attachments ───────────────────────────────────────────────────
    // Callers must fetch images through these, not `get_messages`, as with SQLite.

    async fn list_session_attachments(
        &self,
        session_id: &str,
    ) -> Result<Vec<MessageAttachment>, SessionStorageError> {
        let messages = self.messages.read().await;
        let Some(msgs) = messages.get(session_id) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for m in msgs {
            for (ordinal, img) in m.message.images.iter().enumerate() {
                out.push(MessageAttachment {
                    // Deterministic so a test can address one without a lookup.
                    id: format!("{}#{}", m.id, ordinal),
                    message_id: m.id.clone(),
                    session_id: m.session_id.clone(),
                    ordinal: ordinal as u32,
                    mime_type: img.mime_type.clone(),
                    byte_size: crate::models::domain::image_limits::decoded_len(&img.data) as u64,
                    created_at: m.created_at,
                });
            }
        }
        Ok(out)
    }

    async fn load_message_images(
        &self,
        message_ids: &[String],
    ) -> Result<HashMap<String, Vec<ImageAttachment>>, SessionStorageError> {
        let wanted: std::collections::HashSet<&str> =
            message_ids.iter().map(String::as_str).collect();
        let messages = self.messages.read().await;
        let mut out: HashMap<String, Vec<ImageAttachment>> = HashMap::new();
        for msgs in messages.values() {
            for m in msgs {
                if wanted.contains(m.id.as_str()) && !m.message.images.is_empty() {
                    out.insert(m.id.clone(), m.message.images.clone());
                }
            }
        }
        Ok(out)
    }

    async fn read_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<Option<(String, Vec<u8>)>, SessionStorageError> {
        let Some((message_id, ordinal)) = attachment_id.rsplit_once('#') else {
            return Ok(None);
        };
        let Ok(ordinal) = ordinal.parse::<usize>() else {
            return Ok(None);
        };
        let messages = self.messages.read().await;
        for msgs in messages.values() {
            for m in msgs {
                if m.id == message_id {
                    if let Some(img) = m.message.images.get(ordinal) {
                        // Stored as base64, but the port contract is decoded bytes out.
                        let bytes = img.data.as_bytes().to_vec();
                        return Ok(Some((img.mime_type.clone(), bytes)));
                    }
                }
            }
        }
        Ok(None)
    }
}

impl InMemorySessionStorage {
    async fn _get_session(&self, session_id: &str) -> Result<(), SessionStorageError> {
        self.sessions
            .read()
            .await
            .contains_key(session_id)
            .then_some(())
            .ok_or_else(|| SessionStorageError::SessionNotFound(session_id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::message::{ChatMessage, Role};

    #[tokio::test]
    async fn test_create_session() {
        let storage = InMemorySessionStorage::new();
        let session = storage
            .create_session("session-1".to_string())
            .await
            .unwrap();
        assert_eq!(session.id, "session-1");
    }

    #[tokio::test]
    async fn test_get_session() {
        let storage = InMemorySessionStorage::new();
        storage
            .create_session("session-1".to_string())
            .await
            .unwrap();
        let retrieved = storage.get_session("session-1").await.unwrap();
        assert_eq!(retrieved.id, "session-1");
    }

    #[tokio::test]
    async fn test_get_nonexistent_session() {
        let storage = InMemorySessionStorage::new();
        let result = storage.get_session("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tool_groups_round_trip_without_a_session_row() {
        let storage = InMemorySessionStorage::new();
        assert_eq!(storage.get_session_tool_groups("s1").await.unwrap(), None);

        let first = vec!["giap-draft".to_string(), "giap-weather".to_string()];
        storage.set_session_tool_groups("s1", &first).await.unwrap();
        assert_eq!(
            storage.get_session_tool_groups("s1").await.unwrap(),
            Some(first)
        );

        // A widen (escape hatch) replaces the list rather than appending twice.
        let widened = vec![
            "giap-draft".to_string(),
            "giap-schedule".to_string(),
            "giap-weather".to_string(),
        ];
        storage
            .set_session_tool_groups("s1", &widened)
            .await
            .unwrap();
        assert_eq!(
            storage.get_session_tool_groups("s1").await.unwrap(),
            Some(widened)
        );
        // Sessions do not leak into each other.
        assert_eq!(storage.get_session_tool_groups("s2").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_add_message() {
        let storage = InMemorySessionStorage::new();
        storage
            .create_session("session-1".to_string())
            .await
            .unwrap();

        let message = ChatMessage::user("Hello");
        let session_message =
            SessionMessage::new("msg-1".to_string(), "session-1".to_string(), message);

        let result = storage
            .add_message("session-1".to_string(), session_message.clone())
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_get_messages() {
        let storage = InMemorySessionStorage::new();
        storage
            .create_session("session-1".to_string())
            .await
            .unwrap();

        let msg1 = ChatMessage::user("Hello");
        let session_msg1 = SessionMessage::new("msg-1".to_string(), "session-1".to_string(), msg1);

        let msg2 = ChatMessage::assistant("Hi there");
        let session_msg2 = SessionMessage::new("msg-2".to_string(), "session-1".to_string(), msg2);

        storage
            .add_message("session-1".to_string(), session_msg1)
            .await
            .unwrap();
        storage
            .add_message("session-1".to_string(), session_msg2)
            .await
            .unwrap();

        let messages = storage.get_messages("session-1").await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message.role, Role::User);
        assert_eq!(messages[1].message.role, Role::Assistant);
    }

    #[tokio::test]
    async fn test_delete_session() {
        let storage = InMemorySessionStorage::new();
        storage
            .create_session("session-1".to_string())
            .await
            .unwrap();

        let result = storage.delete_session("session-1").await;
        assert!(result.is_ok());

        let get_result = storage.get_session("session-1").await;
        assert!(get_result.is_err());
    }

    #[tokio::test]
    async fn test_list_sessions() {
        let storage = InMemorySessionStorage::new();
        storage
            .create_session("session-1".to_string())
            .await
            .unwrap();
        storage
            .create_session("session-2".to_string())
            .await
            .unwrap();

        let sessions = storage.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[tokio::test]
    async fn test_get_messages_paginated() {
        let storage = InMemorySessionStorage::new();
        let session_id = "session-1".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        for i in 0..10 {
            let msg = ChatMessage::user(format!("Message {}", i));
            let session_msg = SessionMessage::new(format!("msg-{}", i), session_id.clone(), msg);
            storage
                .add_message(session_id.clone(), session_msg)
                .await
                .unwrap();
        }

        let page1 = storage
            .get_messages_paginated(&session_id, 3, 0)
            .await
            .unwrap();
        assert_eq!(page1.len(), 3);
        assert_eq!(page1[0].message.content, "Message 0");
        assert_eq!(page1[2].message.content, "Message 2");

        let page2 = storage
            .get_messages_paginated(&session_id, 3, 3)
            .await
            .unwrap();
        assert_eq!(page2.len(), 3);
        assert_eq!(page2[0].message.content, "Message 3");

        let empty = storage
            .get_messages_paginated(&session_id, 3, 100)
            .await
            .unwrap();
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn test_update_title() {
        let storage = InMemorySessionStorage::new();
        storage
            .create_session("session-1".to_string())
            .await
            .unwrap();

        let session = storage.get_session("session-1").await.unwrap();
        assert_eq!(session.title, None);

        storage
            .update_title("session-1", "My Chat".to_string())
            .await
            .unwrap();
        let session = storage.get_session("session-1").await.unwrap();
        assert_eq!(session.title, Some("My Chat".to_string()));

        let result = storage
            .update_title("nonexistent", "Nope".to_string())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_count_messages() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("s".to_string()).await.unwrap();
        assert_eq!(storage.count_messages("s").await.unwrap(), 0);
        assert_eq!(storage.count_messages("missing").await.unwrap(), 0);

        for i in 0..4 {
            let m = SessionMessage::new(
                format!("m{i}"),
                "s".to_string(),
                ChatMessage::user(format!("hi {i}")),
            );
            storage.add_message("s".to_string(), m).await.unwrap();
        }
        assert_eq!(storage.count_messages("s").await.unwrap(), 4);
    }

    #[tokio::test]
    async fn test_first_user_message() {
        let storage = InMemorySessionStorage::new();
        storage.create_session("s".to_string()).await.unwrap();
        assert_eq!(storage.first_user_message("s").await.unwrap(), None);

        storage
            .add_message(
                "s".to_string(),
                SessionMessage::new(
                    "a".to_string(),
                    "s".to_string(),
                    ChatMessage::assistant("hi"),
                ),
            )
            .await
            .unwrap();
        assert_eq!(storage.first_user_message("s").await.unwrap(), None);

        storage
            .add_message(
                "s".to_string(),
                SessionMessage::new(
                    "u1".to_string(),
                    "s".to_string(),
                    ChatMessage::user("what is the capital of Kenya?"),
                ),
            )
            .await
            .unwrap();
        storage
            .add_message(
                "s".to_string(),
                SessionMessage::new(
                    "u2".to_string(),
                    "s".to_string(),
                    ChatMessage::user("second"),
                ),
            )
            .await
            .unwrap();

        assert_eq!(
            storage.first_user_message("s").await.unwrap(),
            Some("what is the capital of Kenya?".to_string())
        );
    }

    #[tokio::test]
    async fn test_messages_persist_across_iterations() {
        let storage = InMemorySessionStorage::new();
        let session_id = "session-1".to_string();
        storage.create_session(session_id.clone()).await.unwrap();

        let user_msg = ChatMessage::user("First message");
        let session_msg1 = SessionMessage::new("msg-1".to_string(), session_id.clone(), user_msg);
        storage
            .add_message(session_id.clone(), session_msg1)
            .await
            .unwrap();

        let assistant_msg = ChatMessage::assistant("First response");
        let session_msg2 =
            SessionMessage::new("msg-2".to_string(), session_id.clone(), assistant_msg);
        storage
            .add_message(session_id.clone(), session_msg2)
            .await
            .unwrap();

        let messages = storage.get_messages(&session_id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message.content, "First message");
        assert_eq!(messages[1].message.content, "First response");
    }
}
