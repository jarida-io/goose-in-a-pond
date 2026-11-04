//! The catalogue's Brave Search extension against the built-in web search: refused while the
//! built-in search is on, listed with the state the dashboard shows, installable while it is off.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_core::mcp::domain::tool_group::BRAVE_SEARCH_API_KEY;
use pond_core::mcp::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort, ToolInfo,
};
use pond_core::mcp::services::marketplace::BundledMarketplace;
use pond_core::security::ports::secret::SecretRepository;
use pond_infra::sqlite_settings::SqliteSettingsRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

const EXT: &str = "brave-search";

#[derive(Default)]
struct Secrets(Mutex<HashMap<String, String>>);

#[async_trait::async_trait]
impl SecretRepository for Secrets {
    async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().remove(key);
        Ok(())
    }
    async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.0.lock().unwrap().keys().cloned().collect())
    }
    async fn has(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.0.lock().unwrap().contains_key(key))
    }
}

/// Records what was started; every start succeeds.
#[derive(Default)]
struct Manager(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl ExtensionManagerPort for Manager {
    async fn list_extensions(&self) -> anyhow::Result<Vec<ExtensionInfo>> {
        Ok(vec![])
    }
    async fn add_extension(&self, request: AddExtensionRequest) -> anyhow::Result<ExtensionInfo> {
        self.0.lock().unwrap().push(request.name.clone());
        Ok(ExtensionInfo {
            name: request.name,
            kind: request.kind,
            description: request.description,
            tools: vec![],
            enabled: true,
            status: "connected".into(),
            last_error: None,
            status_reason: None,
        })
    }
    async fn remove_extension(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn list_tools(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn list_tools_detailed(&self) -> anyhow::Result<Vec<ToolInfo>> {
        Ok(vec![])
    }
    async fn set_enabled(&self, _: &str, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

struct Pond {
    router: axum::Router,
    state: Arc<pond_api::AppState>,
    secrets: Arc<Secrets>,
    manager: Arc<Manager>,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    let pond_api::test_support::TestState { state, dir, .. } =
        pond_api::test_support::app_state().await;
    let handshake = Arc::new(pond_infra::mock_handshake::MockHandshake::new());
    handshake.add_valid_token("desktop-token".to_string()).await;
    let secrets = Arc::new(Secrets::default());
    let manager = Arc::new(Manager::default());
    let state = Arc::new(pond_api::AppState {
        handshake,
        secret_repo: Some(secrets.clone()),
        extension_manager: Some(manager.clone()),
        marketplace: Some(Arc::new(BundledMarketplace::new())),
        // The real store: the Knowledge switch must round-trip as it does in a running pond.
        settings_repo: Arc::new(SqliteSettingsRepository::new(state.db.system.clone())),
        ..state
    });
    let router =
        pond_api::build_router(state.clone(), std::path::PathBuf::from("pond-desktop/dist")).layer(
            MockConnectInfo(SocketAddr::from(([192, 168, 1, 44], 40_000))),
        );
    Pond {
        router,
        state,
        secrets,
        manager,
        _dir: dir,
    }
}

async fn call(pond: &Pond, method: Method, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", "Bearer desktop-token");
    let req = match body {
        Some(b) => req
            .header("Content-Type", "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let resp = pond.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn store_brave_key(pond: &Pond, value: &str) {
    pond.secrets.set(BRAVE_SEARCH_API_KEY, value).await.unwrap();
}

async fn set_knowledge(pond: &Pond, enabled: bool) {
    let mut settings = pond.state.settings_repo.get().await.unwrap();
    settings.ext_knowledge_enabled = enabled;
    pond.state.settings_repo.update(&settings).await.unwrap();
}

async fn install(pond: &Pond) -> (StatusCode, Value) {
    call(
        pond,
        Method::POST,
        &format!("/api/v1/marketplace/{EXT}/install"),
        Some(json!({ "secrets": { "BRAVE_API_KEY": "not-a-real-key" } })),
    )
    .await
}

async fn listed_state(pond: &Pond, id: &str) -> Value {
    let (status, body) = call(pond, Method::GET, "/api/v1/marketplace", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entry = body["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap_or_else(|| panic!("{id} is not listed"))
        .clone();
    entry["built_in_state"].clone()
}

#[tokio::test]
async fn install_is_refused_while_the_built_in_search_is_on() {
    let pond = pond().await;
    store_brave_key(&pond, "a-key").await;

    let (status, body) = install(&pond).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], pond_api::routes::BUILT_IN_SEARCH_ON);
    assert_eq!(body["reason"], pond_api::routes::BUILT_IN_SEARCH_ON_REASON);
    assert!(
        pond.manager.0.lock().unwrap().is_empty(),
        "nothing is started"
    );
    assert_eq!(
        pond.secrets.get("BRAVE_API_KEY").await.unwrap(),
        None,
        "a refused install stores none of the keys it was sent"
    );
}

#[tokio::test]
async fn install_goes_ahead_while_the_built_in_search_is_off() {
    let no_key = pond().await;
    let (status, body) = install(&no_key).await;
    assert_eq!(status, StatusCode::CREATED, "no key stored: {body}");

    let knowledge_off = pond().await;
    store_brave_key(&knowledge_off, "a-key").await;
    set_knowledge(&knowledge_off, false).await;
    let (status, body) = install(&knowledge_off).await;
    assert_eq!(status, StatusCode::CREATED, "Knowledge off: {body}");

    let blank_key = pond().await;
    store_brave_key(&blank_key, "   ").await;
    let (status, body) = install(&blank_key).await;
    assert_eq!(status, StatusCode::CREATED, "a blank key is no key: {body}");
}

#[tokio::test]
async fn the_listing_carries_the_built_in_state_on_the_brave_entry_only() {
    let pond = pond().await;
    assert_eq!(listed_state(&pond, EXT).await, "needs_key");
    assert_eq!(listed_state(&pond, "github").await, Value::Null);

    store_brave_key(&pond, "a-key").await;
    assert_eq!(listed_state(&pond, EXT).await, "on");

    set_knowledge(&pond, false).await;
    assert_eq!(listed_state(&pond, EXT).await, "knowledge_off");
}
