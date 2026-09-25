//! Session history loading from `SessionStorage`.

use pond_core::models::domain::message::ChatMessage;
use pond_core::user_data::ports::session_storage::SessionStorage;

/// The last `limit` messages, oldest first as providers expect; empty for an unknown session.
pub async fn load_history(
    storage: &dyn SessionStorage,
    session_id: &str,
    limit: usize,
) -> Vec<ChatMessage> {
    storage
        .get_recent_messages(session_id, limit)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|m| m.message)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::message::Role;
    use pond_core::user_data::domain::session::{Session, SessionMessage};
    use pond_core::user_data::ports::session_storage::SessionStorageError;

    struct MockSessionStorage {
        messages: Vec<SessionMessage>,
    }

    #[async_trait::async_trait]
    impl SessionStorage for MockSessionStorage {
        async fn create_session(&self, _id: String) -> Result<Session, SessionStorageError> {
            unimplemented!()
        }
        async fn get_session(&self, _id: &str) -> Result<Session, SessionStorageError> {
            unimplemented!()
        }
        async fn add_message(
            &self,
            _sid: String,
            _msg: SessionMessage,
        ) -> Result<SessionMessage, SessionStorageError> {
            unimplemented!()
        }
        async fn get_messages(
            &self,
            _sid: &str,
        ) -> Result<Vec<SessionMessage>, SessionStorageError> {
            unimplemented!()
        }
        async fn update_title(
            &self,
            _sid: &str,
            _title: String,
        ) -> Result<(), SessionStorageError> {
            unimplemented!()
        }
        async fn delete_session(&self, _sid: &str) -> Result<(), SessionStorageError> {
            unimplemented!()
        }
        async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
            unimplemented!()
        }
        async fn get_messages_paginated(
            &self,
            _sid: &str,
            _limit: usize,
            _offset: usize,
        ) -> Result<Vec<SessionMessage>, SessionStorageError> {
            unimplemented!()
        }
        async fn get_recent_messages(
            &self,
            _sid: &str,
            limit: usize,
        ) -> Result<Vec<SessionMessage>, SessionStorageError> {
            Ok(self.messages.iter().take(limit).cloned().collect())
        }
    }

    #[tokio::test]
    async fn loads_messages_as_chat_messages() {
        let storage = MockSessionStorage {
            messages: vec![
                SessionMessage::new(
                    "1".to_string(),
                    "s1".to_string(),
                    ChatMessage::user("hello"),
                ),
                SessionMessage::new(
                    "2".to_string(),
                    "s1".to_string(),
                    ChatMessage::assistant("hi there"),
                ),
            ],
        };

        let history = load_history(&storage, "s1", 10).await;
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(history[0].content, "hello");
        assert_eq!(history[1].role, Role::Assistant);
        assert_eq!(history[1].content, "hi there");
    }

    #[tokio::test]
    async fn empty_session_returns_empty_vec() {
        let storage = MockSessionStorage { messages: vec![] };
        let history = load_history(&storage, "nonexistent", 10).await;
        assert!(history.is_empty());
    }

    #[tokio::test]
    async fn respects_limit() {
        let storage = MockSessionStorage {
            messages: vec![
                SessionMessage::new(
                    "1".to_string(),
                    "s1".to_string(),
                    ChatMessage::user("first"),
                ),
                SessionMessage::new(
                    "2".to_string(),
                    "s1".to_string(),
                    ChatMessage::assistant("second"),
                ),
                SessionMessage::new(
                    "3".to_string(),
                    "s1".to_string(),
                    ChatMessage::user("third"),
                ),
            ],
        };

        let history = load_history(&storage, "s1", 2).await;
        assert_eq!(history.len(), 2);
    }
}
