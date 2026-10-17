//! A catalogue extension that needs a key: the key lives in the secret store only, removing it
//! stops the extension, and it cannot be restarted without it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Method, Request, StatusCode};
use pond_core::mcp::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort, ToolInfo,
};
use pond_core::mcp::ports::mcp_server::{McpServerConfig, McpServerRepository};
use pond_core::mcp::services::marketplace::BundledMarketplace;
use pond_core::security::ports::secret::SecretRepository;
use serde_json::{json, Value};
use tower::ServiceExt;

/// The bundled GitHub extension, which cannot run without this key.
const EXT: &str = "github";
const KEY: &str = "GITHUB_PERSONAL_ACCESS_TOKEN";
const VALUE: &str = "not-a-real-key";

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

/// The saved extension rows, as the database would hold them.
#[derive(Default)]
struct Rows(Mutex<Vec<McpServerConfig>>);

#[async_trait::async_trait]
impl McpServerRepository for Rows {
    async fn list(&self) -> anyhow::Result<Vec<McpServerConfig>> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn save(&self, config: &McpServerConfig) -> anyhow::Result<()> {
        let mut rows = self.0.lock().unwrap();
        rows.retain(|r| r.id != config.id);
        rows.push(config.clone());
        Ok(())
    }
    async fn delete(&self, name: &str) -> anyhow::Result<()> {
        self.0.lock().unwrap().retain(|r| r.name != name);
        Ok(())
    }
    async fn set_enabled(&self, name: &str, enabled: bool) -> anyhow::Result<()> {
        for row in self.0.lock().unwrap().iter_mut().filter(|r| r.name == name) {
            row.enabled = enabled;
        }
        Ok(())
    }
}

/// Records what was started and stopped; every start succeeds.
#[derive(Default)]
struct Manager {
    added: Mutex<Vec<AddExtensionRequest>>,
    removed: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ExtensionManagerPort for Manager {
    async fn list_extensions(&self) -> anyhow::Result<Vec<ExtensionInfo>> {
        Ok(vec![])
    }
    async fn add_extension(&self, request: AddExtensionRequest) -> anyhow::Result<ExtensionInfo> {
        let info = ExtensionInfo {
            name: request.name.clone(),
            kind: request.kind.clone(),
            description: request.description.clone(),
            tools: vec![],
            enabled: true,
            status: "connected".into(),
            last_error: None,
        };
        self.added.lock().unwrap().push(request);
        Ok(info)
    }
    async fn remove_extension(&self, name: &str) -> anyhow::Result<()> {
        self.removed.lock().unwrap().push(name.to_string());
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
    secrets: Arc<Secrets>,
    rows: Arc<Rows>,
    manager: Arc<Manager>,
    _dir: tempfile::TempDir,
}

async fn pond() -> Pond {
    // The store reads the process environment first; a developer's exported token must not leak in.
    std::env::remove_var(KEY);
    let pond_api::test_support::TestState { state, dir, .. } =
        pond_api::test_support::app_state().await;
    let handshake = Arc::new(pond_infra::mock_handshake::MockHandshake::new());
    handshake.add_valid_token("desktop-token".to_string()).await;
    let secrets = Arc::new(Secrets::default());
    let rows = Arc::new(Rows::default());
    let manager = Arc::new(Manager::default());
    let state = Arc::new(pond_api::AppState {
        handshake,
        secret_repo: Some(secrets.clone()),
        mcp_server_repo: Some(rows.clone()),
        extension_manager: Some(manager.clone()),
        marketplace: Some(Arc::new(BundledMarketplace::new())),
        ..state
    });
    let router =
        pond_api::build_router(state, std::path::PathBuf::from("pond-desktop/dist")).layer(
            MockConnectInfo(SocketAddr::from(([192, 168, 1, 44], 40_000))),
        );
    Pond {
        router,
        secrets,
        rows,
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

async fn install(pond: &Pond) {
    let (status, body) = call(
        pond,
        Method::POST,
        &format!("/api/v1/marketplace/{EXT}/install"),
        Some(json!({ "secrets": { KEY: VALUE } })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn install_starts_with_the_key_and_saves_no_key_value() {
    let pond = pond().await;
    install(&pond).await;

    let started = pond.manager.added.lock().unwrap().last().cloned().unwrap();
    assert_eq!(started.env.get(KEY).map(String::as_str), Some(VALUE));

    let rows = pond.rows.0.lock().unwrap().clone();
    assert_eq!(rows.len(), 1);
    assert!(
        !rows[0].env.contains_key(KEY),
        "the saved row carries the key: {:?}",
        rows[0].env.keys().collect::<Vec<_>>()
    );
    assert!(
        rows[0].env.values().all(|v| v != VALUE),
        "the key's value is in the saved row"
    );
    assert!(
        !rows[0]
            .env
            .contains_key(pond_api::oauth_callback::INTERNAL_TOKEN_ENV_KEY),
        "the per-run token is minted at each start, never saved"
    );
    assert!(rows[0]
        .env
        .contains_key(pond_api::oauth_callback::GIAP_SERVER_URL_ENV_KEY));
    assert_eq!(pond.secrets.get(KEY).await.unwrap().as_deref(), Some(VALUE));
}

#[tokio::test]
async fn install_without_the_key_is_refused_and_starts_nothing() {
    let pond = pond().await;
    let (status, body) = call(
        &pond,
        Method::POST,
        &format!("/api/v1/marketplace/{EXT}/install"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED, "{body}");
    assert_eq!(body["missing"][0]["key"], KEY);
    assert!(pond.manager.added.lock().unwrap().is_empty());
    assert!(pond.rows.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn deleting_the_key_stops_the_extension_and_keeps_it_installed() {
    let pond = pond().await;
    install(&pond).await;

    let (status, _) = call(
        &pond,
        Method::DELETE,
        &format!("/api/v1/secrets/{KEY}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(*pond.manager.removed.lock().unwrap(), vec![EXT.to_string()]);
    let rows = pond.rows.0.lock().unwrap().clone();
    assert!(
        rows.iter().any(|r| r.name == EXT && r.enabled),
        "the row stays, so saving the key again brings it back"
    );
}

#[tokio::test]
async fn deleting_a_key_nothing_needs_stops_nothing() {
    let pond = pond().await;
    install(&pond).await;
    pond.secrets.set("UNRELATED_KEY", "x").await.unwrap();

    let (status, _) = call(&pond, Method::DELETE, "/api/v1/secrets/UNRELATED_KEY", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(pond.manager.removed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_restart_without_the_key_is_refused_by_name_and_saving_it_restarts() {
    let pond = pond().await;
    install(&pond).await;
    let (status, _) = call(
        &pond,
        Method::DELETE,
        &format!("/api/v1/secrets/{KEY}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let started_before = pond.manager.added.lock().unwrap().len();

    // Saving nothing restarts an installed extension, which must refuse without its key.
    let (status, body) = call(
        &pond,
        Method::POST,
        &format!("/api/v1/extensions/{EXT}/secrets"),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restarted"], false, "{body}");
    let error = body["restart_error"].as_str().unwrap_or_default();
    assert!(error.contains(KEY), "the refusal names the key: {body}");
    assert!(!error.contains(VALUE));
    assert_eq!(pond.manager.added.lock().unwrap().len(), started_before);

    // Saving the key again brings it back.
    let (status, body) = call(
        &pond,
        Method::POST,
        &format!("/api/v1/extensions/{EXT}/secrets"),
        Some(json!({ KEY: VALUE })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["restarted"], true, "{body}");
    let started = pond.manager.added.lock().unwrap().last().cloned().unwrap();
    assert_eq!(started.env.get(KEY).map(String::as_str), Some(VALUE));
}
