//! A device driven over HTTP, with no chat turn, runs as the caller its paired device proves,
//! and only the owner decides which rooms a guest may use.

#[path = "support/household.rs"]
mod household;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use household::{household, household_with, ADA, JERRY, LIZ, TABLET};
use pond_core::mcp::ports::tools::tool_dispatcher::{ToolCallResult, ToolDispatcher};
use pond_core::user_data::domain::profile::ProfileScope;
use serde_json::json;

/// Answers every call, keeping the scope each one ran as.
#[derive(Default)]
struct Scopes(Mutex<Vec<ProfileScope>>);

#[async_trait]
impl ToolDispatcher for Scopes {
    async fn dispatch(
        &self,
        _tool_name: &str,
        _arguments: serde_json::Value,
        scope: &ProfileScope,
    ) -> anyhow::Result<ToolCallResult> {
        self.0.lock().unwrap().push(scope.clone());
        Ok(ToolCallResult {
            content: "power=on".into(),
            success: true,
            keep: None,
        })
    }
    async fn available_tools(&self) -> Vec<String> {
        Vec::new()
    }
}

async fn scope_of_a_lamp_switched_by(token: &str, route: &str) -> (ProfileScope, String, String) {
    let scopes = Arc::new(Scopes::default());
    let dispatcher: Arc<dyn ToolDispatcher> = scopes.clone();
    let h = household_with(|state| state.tool_dispatcher = Some(dispatcher)).await;
    let body = match route {
        "/api/v1/tools/invoke" => json!({
            "server": "giap-device-control",
            "tool": "set_device_state",
            "args": {"device_id": "lamp", "power": true},
        }),
        _ => json!({
            "name": "giap-device-control__set_device_state",
            "arguments": {"device_id": "lamp", "power": true},
        }),
    };
    let (status, reply) = h.send(Method::POST, route, token, Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let scope = scopes
        .0
        .lock()
        .unwrap()
        .pop()
        .expect("the tool was dispatched");
    (scope, h.ada, h.liz)
}

#[tokio::test]
async fn a_guests_phone_switches_a_device_as_that_guest() {
    for route in ["/api/v1/tools/invoke", "/api/v1/mcp/tools/call"] {
        let (scope, ada, _) = scope_of_a_lamp_switched_by(ADA, route).await;
        assert_eq!(scope, ProfileScope::Owner(ada), "{route}");
    }
}

#[tokio::test]
async fn a_device_that_belongs_to_nobody_switches_a_device_as_a_guest() {
    for route in ["/api/v1/tools/invoke", "/api/v1/mcp/tools/call"] {
        let (scope, _, _) = scope_of_a_lamp_switched_by(TABLET, route).await;
        assert_eq!(scope, ProfileScope::Guest, "{route}");
    }
}

#[tokio::test]
async fn a_members_phone_switches_a_device_as_that_member() {
    let (scope, _, liz) = scope_of_a_lamp_switched_by(LIZ, "/api/v1/tools/invoke").await;
    assert_eq!(scope, ProfileScope::Owner(liz));
}

async fn guest_access(h: &household::Household, token: &str, room: &str) -> StatusCode {
    h.send(
        Method::PUT,
        &format!("/api/v1/rooms/{room}/guest-access"),
        token,
        Some(json!({"accessible": true})),
    )
    .await
    .0
}

#[tokio::test]
async fn only_the_owner_opens_a_room_to_guests() {
    let h = household().await;
    let room = h.state.room_registry.create("Kitchen").await.unwrap().id;

    assert_eq!(guest_access(&h, ADA, &room).await, StatusCode::FORBIDDEN);
    assert_eq!(guest_access(&h, LIZ, &room).await, StatusCode::FORBIDDEN);
    assert_eq!(guest_access(&h, TABLET, &room).await, StatusCode::FORBIDDEN);
    assert!(!h
        .state
        .guest_room_access
        .is_guest_accessible(&room)
        .await
        .unwrap());

    assert_eq!(guest_access(&h, JERRY, &room).await, StatusCode::OK);
    assert!(h
        .state
        .guest_room_access
        .is_guest_accessible(&room)
        .await
        .unwrap());
}

#[tokio::test]
async fn the_room_list_says_who_may_change_guest_access() {
    let h = household().await;
    h.state.room_registry.create("Kitchen").await.unwrap();

    let (_, owner) = h.send(Method::GET, "/api/v1/rooms", JERRY, None).await;
    let (_, member) = h.send(Method::GET, "/api/v1/rooms", LIZ, None).await;

    assert_eq!(owner["rooms"][0]["guest_access_editable"], true, "{owner}");
    assert_eq!(
        member["rooms"][0]["guest_access_editable"], false,
        "{member}"
    );
}
