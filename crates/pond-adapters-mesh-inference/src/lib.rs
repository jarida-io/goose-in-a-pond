//! Borrow and lend compute over the private libp2p mesh; single-peer, text-only. Both roles share
//! one `MeshInferenceService` because `MeshTransport::recv()` is a single-consumer queue.

mod provider;
mod service;

pub use provider::{MeshInferenceError, MeshInferenceProvider};
pub use service::{InvoiceRequestError, MeshInferenceService};

use pond_core::models::domain::message::{ChatMessage, Role};
use pond_mesh_protocol::wire::ChatMessageWire;

fn role_to_wire(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn to_wire_message(message: &ChatMessage) -> ChatMessageWire {
    ChatMessageWire {
        role: role_to_wire(&message.role).to_string(),
        content: message.content.clone(),
    }
}

/// Unknown roles degrade to user rather than failing, so a newer peer's roles don't break it.
fn from_wire_message(message: &ChatMessageWire) -> ChatMessage {
    match message.role.as_str() {
        "system" => ChatMessage::system(message.content.clone()),
        "assistant" => ChatMessage::assistant(message.content.clone()),
        "tool" => ChatMessage::user(message.content.clone()), // no tool_call_id on the wire
        _ => ChatMessage::user(message.content.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip_preserves_role_and_content() {
        for original in [
            ChatMessage::system("be helpful"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi there"),
        ] {
            let wire = to_wire_message(&original);
            let restored = from_wire_message(&wire);
            assert_eq!(restored.role, original.role);
            assert_eq!(restored.content, original.content);
        }
    }

    #[test]
    fn unrecognized_wire_role_degrades_to_user() {
        let wire = ChatMessageWire {
            role: "from_the_future".to_string(),
            content: "hi".to_string(),
        };
        assert_eq!(from_wire_message(&wire).role, Role::User);
    }
}
