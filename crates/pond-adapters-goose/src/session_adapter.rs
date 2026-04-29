use chrono::{DateTime, Utc};
use goose::config::GooseMode;
use goose::conversation::message::Message as GooseMessage;
use goose::session::{Session as GooseSession, SessionManager, SessionType};
use pond_core::domain::message::{ChatMessage, Role};
use pond_core::domain::session::{Session, SessionMessage};
use pond_core::ports::session_storage::{SessionStorage, SessionStorageError};
use rmcp::model::Role as GooseRole;
use std::path::PathBuf;
use uuid::Uuid;

/// Adapter: GooseSessionAdapter
///
/// Thin bridge that delegates to Goose's real SessionManager.
/// Maps between Pond's simple domain types and Goose's richer types.
pub struct GooseSessionAdapter {
    manager: SessionManager,
}

impl GooseSessionAdapter {
    /// Create an adapter using Goose's global singleton SessionManager.
    pub fn new() -> Self {
        Self {
            manager: SessionManager::instance(),
        }
    }

    /// Create an adapter with a custom data directory.
    pub fn with_data_dir(data_dir: PathBuf) -> Self {
        Self {
            manager: SessionManager::new(data_dir),
        }
    }
}

impl Default for GooseSessionAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// ── Type mapping helpers ────────────────────────────────────────────────────

fn goose_session_to_pond(gs: &GooseSession) -> Session {
    Session {
        id: gs.id.clone(),
        title: Some(gs.name.clone()),
        created_at: gs.created_at,
        updated_at: gs.updated_at,
    }
}

fn pond_role_to_goose_message(role: &Role, content: &str) -> GooseMessage {
    match role {
        Role::User | Role::System => GooseMessage::user().with_text(content),
        Role::Assistant => GooseMessage::assistant().with_text(content),
    }
}

fn goose_message_to_pond(
    msg: &GooseMessage,
    session_id: &str,
) -> SessionMessage {
    let role = match msg.role {
        GooseRole::User => Role::User,
        GooseRole::Assistant => Role::Assistant,
    };
    let content = msg.as_concat_text();
    let created_at: DateTime<Utc> =
        DateTime::from_timestamp(msg.created, 0).unwrap_or_else(Utc::now);

    SessionMessage {
        id: msg
            .id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        session_id: session_id.to_string(),
        message: ChatMessage { role, content, images: Vec::new() },
        created_at,
    }
}

fn to_storage_err(e: anyhow::Error) -> SessionStorageError {
    let msg = e.to_string();
    if msg.contains("not found") || msg.contains("No session") {
        SessionStorageError::SessionNotFound(msg)
    } else {
        SessionStorageError::StorageError(msg)
    }
}

// ── SessionStorage implementation ───────────────────────────────────────────

#[async_trait::async_trait]
impl SessionStorage for GooseSessionAdapter {
    async fn create_session(
        &self,
        session_id: String,
    ) -> Result<Session, SessionStorageError> {
        let working_dir =
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let gs = self
            .manager
            .create_session(working_dir, session_id, SessionType::User, GooseMode::default())
            .await
            .map_err(to_storage_err)?;
        Ok(goose_session_to_pond(&gs))
    }

    async fn get_session(
        &self,
        session_id: &str,
    ) -> Result<Session, SessionStorageError> {
        let gs = self
            .manager
            .get_session(session_id, false)
            .await
            .map_err(to_storage_err)?;
        Ok(goose_session_to_pond(&gs))
    }

    async fn add_message(
        &self,
        session_id: String,
        message: SessionMessage,
    ) -> Result<SessionMessage, SessionStorageError> {
        let goose_msg =
            pond_role_to_goose_message(&message.message.role, &message.message.content);
        self.manager
            .add_message(&session_id, &goose_msg)
            .await
            .map_err(to_storage_err)?;
        Ok(message)
    }

    async fn get_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        let gs = self
            .manager
            .get_session(session_id, true)
            .await
            .map_err(to_storage_err)?;

        let messages = gs
            .conversation
            .as_ref()
            .map(|conv| {
                conv.messages()
                    .iter()
                    .map(|m| goose_message_to_pond(m, session_id))
                    .collect()
            })
            .unwrap_or_default();

        Ok(messages)
    }

    async fn update_title(
        &self,
        session_id: &str,
        title: String,
    ) -> Result<(), SessionStorageError> {
        self.manager
            .update(session_id)
            .user_provided_name(&title)
            .apply()
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn delete_session(
        &self,
        session_id: &str,
    ) -> Result<(), SessionStorageError> {
        self.manager
            .delete_session(session_id)
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<Session>, SessionStorageError> {
        let goose_sessions = self
            .manager
            .list_sessions()
            .await
            .map_err(to_storage_err)?;
        Ok(goose_sessions.iter().map(goose_session_to_pond).collect())
    }

    async fn get_recent_messages(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        let all = self.get_messages(session_id).await?;
        let recent = all.into_iter().rev().take(limit).collect::<Vec<_>>().into_iter().rev().collect();
        Ok(recent)
    }

    async fn get_messages_paginated(
        &self,
        session_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<SessionMessage>, SessionStorageError> {
        // Goose has no native pagination — fetch all and slice in memory.
        // Acceptable for thin bridge; can be optimized later if needed.
        let all = self.get_messages(session_id).await?;
        let paginated = all.into_iter().skip(offset).take(limit).collect();
        Ok(paginated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goose_session_maps_to_pond() {
        let gs = GooseSession {
            id: "test-123".to_string(),
            working_dir: PathBuf::from("."),
            name: "My Chat".to_string(),
            user_set_name: false,
            session_type: SessionType::User,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            extension_data: Default::default(),
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            accumulated_total_tokens: None,
            accumulated_input_tokens: None,
            accumulated_output_tokens: None,
            schedule_id: None,
            recipe: None,
            user_recipe_values: None,
            conversation: None,
            message_count: 0,
            provider_name: None,
            model_config: None,
            goose_mode: GooseMode::default(),
        };

        let pond = goose_session_to_pond(&gs);
        assert_eq!(pond.id, "test-123");
        assert_eq!(pond.title, Some("My Chat".to_string()));
    }

    #[test]
    fn pond_message_maps_to_goose_and_back() {
        let goose_msg = pond_role_to_goose_message(&Role::User, "Hello");
        assert_eq!(goose_msg.as_concat_text(), "Hello");
        assert_eq!(goose_msg.role, GooseRole::User);

        let pond_msg = goose_message_to_pond(&goose_msg, "sess-1");
        assert_eq!(pond_msg.message.content, "Hello");
        assert_eq!(pond_msg.message.role, Role::User);
        assert_eq!(pond_msg.session_id, "sess-1");
    }
}
