//! A pond with two members, Jerry and Emmanuel, each with a phone of their own, a model that
//! reads pictures or not, and capture pictures kept under the pond's temporary data directory.
// Each test binary that includes this uses only part of it.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use pond_api::{build_router, EngineStores};
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::domain::model_capabilities::ModelCapabilities;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::user_data::domain::profile::CreateProfileRequest;
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use serde_json::Value;
use tower::ServiceExt;

pub const JERRY: &str = "jerry-token";
pub const EMMANUEL: &str = "emmanuel-token";

/// `jerry-token` is Jerry's phone and `emmanuel-token` Emmanuel's.
struct TwoPhones {
    jerry: String,
    emmanuel: String,
}

#[async_trait]
impl Handshake for TwoPhones {
    async fn revoke_device(&self, _device_id: &str) -> Result<u64> {
        Ok(0)
    }
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used by this fixture")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(matches!(token, JERRY | EMMANUEL))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        let device = match token {
            JERRY => &self.jerry,
            EMMANUEL => &self.emmanuel,
            _ => return Ok(None),
        };
        Ok(Some(TokenCaller {
            client_id: device.clone(),
            device_id: device.clone(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        anyhow::bail!("revocation is not used by this fixture")
    }
}

/// A model that says whether it reads pictures and is never asked anything here.
struct Model {
    vision: bool,
}

#[async_trait]
impl LlmProvider for Model {
    async fn complete(&self, _system: &str, _messages: Vec<ChatMessage>) -> Result<ChatMessage> {
        Ok(ChatMessage::assistant("{}"))
    }

    fn model_name(&self) -> String {
        "test".to_string()
    }

    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities {
            vision: self.vision,
            ..ModelCapabilities::default()
        }
    }
}

pub struct Pond {
    pub router: axum::Router,
    pub jerry: String,
    pub emmanuel: String,
    pub stores: EngineStores,
    pub profiles: Arc<dyn ProfileRepository + Send + Sync>,
    dir: tempfile::TempDir,
}

impl Pond {
    /// Where this pond keeps capture pictures.
    pub fn pictures_dir(&self) -> PathBuf {
        self.dir.path().join("captures")
    }

    /// The names of the picture files on disk, sorted.
    pub fn picture_files(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.pictures_dir()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        names.sort();
        names
    }
}

pub async fn pond(vision: bool) -> Pond {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    let pool = state.db.system.clone();
    state.skip_onboarding = true;
    let mut members = Vec::new();
    for name in ["Jerry", "Emmanuel"] {
        let member = state
            .profile_repo
            .create(CreateProfileRequest {
                display_name: name.into(),
                avatar_emoji: "duck".into(),
            })
            .await
            .unwrap()
            .id;
        let device = state
            .device_registry
            .register(RegisterDeviceRequest {
                id: None,
                name: format!("{name}'s phone"),
                device_type: "gotg".into(),
                hostname: None,
                capabilities: vec![],
                room_id: None,
            })
            .await
            .unwrap()
            .id;
        SqliteDeviceAttribution::new(pool.clone())
            .set_device_profile(&device, Some(&member))
            .await
            .unwrap();
        members.push((member, device));
    }
    state.handshake = Arc::new(TwoPhones {
        jerry: members[0].1.clone(),
        emmanuel: members[1].1.clone(),
    });
    *state.llm_provider.write().await = Some(Arc::new(Model { vision }));
    state.engine = state
        .engine
        .clone()
        .keeping_pictures_under(built.dir.path());
    let stores = state.engine.clone();
    let profiles = state.profile_repo.clone();
    Pond {
        router: build_router(Arc::new(state), PathBuf::from("pond-desktop/dist")),
        jerry: members[0].0.clone(),
        emmanuel: members[1].0.clone(),
        stores,
        profiles,
        dir: built.dir,
    }
}

pub async fn send(
    pond: &Pond,
    token: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> axum::response::Response {
    let mut builder = Request::builder()
        .method(method)
        .uri(format!("/api/v1{uri}"))
        .header("Authorization", format!("Bearer {token}"));
    let body = match body {
        Some(value) => {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&value).unwrap())
        }
        None => Body::empty(),
    };
    pond.router
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap()
}

pub async fn call(
    pond: &Pond,
    token: &str,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let response = send(pond, token, method, uri, body).await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

pub async fn get(pond: &Pond, token: &str, uri: &str) -> Value {
    let (status, body) = call(pond, token, Method::GET, uri, None).await;
    assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    body
}

/// The bytes of a small PNG that no other `seed` gives.
pub fn png(seed: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend([seed; 16]);
    bytes
}

/// The bytes of a small JPEG that no other `seed` gives.
pub fn jpeg(seed: u8) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
    bytes.extend([seed; 16]);
    bytes
}

pub fn base64_of(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
