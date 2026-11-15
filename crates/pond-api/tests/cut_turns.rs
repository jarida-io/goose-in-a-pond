//! A turn a stop cut off leaves the person's message stored with no reply. When the pond is
//! back it answers that message again, once, as an ordinary turn.

use std::sync::Arc;

use pond_api::routes::answer_cut_turns;
use pond_core::models::domain::message::{ChatMessage, Role};
use pond_core::user_data::domain::session::SessionMessage;
use pond_core::user_data::ports::session_storage::SessionStorage;

async fn said(sessions: &dyn SessionStorage, session: &str, id: &str, message: ChatMessage) {
    sessions
        .add_message(
            session.to_string(),
            SessionMessage::new(id.to_string(), session.to_string(), message),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_message_left_without_a_reply_is_answered_once_the_pond_is_back() {
    let built = pond_api::test_support::app_state().await;
    let state = Arc::new(built.state);
    let sessions = state.session_storage.clone();
    for id in ["cut", "answered"] {
        sessions.create_session(id.to_string()).await.unwrap();
    }
    said(
        sessions.as_ref(),
        "cut",
        "c1",
        ChatMessage::user("What is on today?"),
    )
    .await;
    said(sessions.as_ref(), "answered", "a1", ChatMessage::user("Hi")).await;
    said(
        sessions.as_ref(),
        "answered",
        "a2",
        ChatMessage::assistant("Hello"),
    )
    .await;
    let since = chrono::Utc::now() - chrono::Duration::minutes(60);

    assert_eq!(answer_cut_turns(state.clone(), since).await, 1);

    let kept = sessions.get_messages("cut").await.unwrap();
    let roles: Vec<&Role> = kept.iter().map(|m| &m.message.role).collect();
    assert_eq!(roles.first(), Some(&&Role::User));
    assert_eq!(roles.last(), Some(&&Role::Assistant), "{kept:?}");
    assert_eq!(
        kept.iter().filter(|m| m.message.role == Role::User).count(),
        1,
        "the message is stored once, not twice"
    );
    assert_eq!(kept[0].message.content, "What is on today?");
    assert_eq!(sessions.get_messages("answered").await.unwrap().len(), 2);

    assert_eq!(
        answer_cut_turns(state, since).await,
        0,
        "an answered turn is not answered again"
    );
}
