//! A household of three over one pond: Jerry, its main member and so its owner; Liz, a member;
//! and Ada, a guest. Each has a phone, and a kitchen tablet belongs to nobody.
// Each test binary that includes this uses only part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use pond_api::{build_router, AppState};
use pond_core::security::domain::pairing_lock::PairingLockState;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};
use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::domain::profile::{CreateProfileRequest, Role};
use pond_core::user_data::ports::device_attribution::DeviceAttribution;
use pond_core::user_data::ports::device_registry::RegisterDeviceRequest;
use pond_infra::sqlite_device_attribution::SqliteDeviceAttribution;
use pond_infra::sqlite_handshake::SqliteHandshakeAdapter;
use serde_json::Value;
use tower::ServiceExt;

pub const JERRY: &str = "jerry-token";
pub const LIZ: &str = "liz-token";
pub const ADA: &str = "ada-token";
pub const TABLET: &str = "tablet-token";

/// Each token names its own device, as pairing would issue them; the pairing lock is the real one.
struct Phones {
    devices: HashMap<String, String>,
    lock: Arc<SqliteHandshakeAdapter>,
}

#[async_trait]
impl Handshake for Phones {
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used here")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(self.devices.contains_key(token))
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        Ok(self.devices.get(token).map(|device| TokenCaller {
            client_id: device.clone(),
            device_id: device.clone(),
        }))
    }
    async fn revoke_device(&self, _: &str) -> Result<u64> {
        Ok(0)
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        Ok(())
    }
    async fn pairing_lock(&self) -> Result<PairingLockState> {
        self.lock.pairing_lock().await
    }
    async fn set_pairing_owner(&self, profile_id: &str, replace: bool) -> Result<bool> {
        self.lock.set_pairing_owner(profile_id, replace).await
    }
}

pub struct Household {
    pub router: axum::Router,
    pub state: Arc<AppState>,
    pub jerry: String,
    pub liz: String,
    pub ada: String,
    pub dir: tempfile::TempDir,
}

pub async fn household() -> Household {
    household_with(|_| {}).await
}

/// The household, with `adjust` applied to the state before it is shared.
pub async fn household_with(adjust: impl FnOnce(&mut AppState)) -> Household {
    let built = pond_api::test_support::app_state().await;
    let mut state = built.state;
    state.settings_repo = Arc::new(pond_infra::sqlite_settings::SqliteSettingsRepository::new(
        state.db.system.clone(),
    ));
    state
        .onboarding_repo
        .save_step(OnboardingStep::Completed)
        .await
        .unwrap();
    let member = |name: &str| CreateProfileRequest {
        display_name: name.into(),
        avatar_emoji: "duck".into(),
    };
    let jerry = state.profile_repo.create(member("Jerry")).await.unwrap().id;
    let liz = state.profile_repo.create(member("Liz")).await.unwrap().id;
    let ada = state.profile_repo.create(member("Ada")).await.unwrap().id;
    state
        .profile_repo
        .set_role(&ada, Role::Guest, None)
        .await
        .unwrap();
    assert!(built
        .handshake
        .set_pairing_owner(&jerry, false)
        .await
        .unwrap());
    state
        .settings_repo
        .set_key("primary_profile_id", jerry.clone())
        .await
        .unwrap();

    let attribution = SqliteDeviceAttribution::new(state.db.system.clone());
    let mut devices = HashMap::new();
    for (token, name, owner) in [
        (JERRY, "Jerry's phone", Some(&jerry)),
        (LIZ, "Liz's phone", Some(&liz)),
        (ADA, "Ada's phone", Some(&ada)),
        (TABLET, "Kitchen tablet", None),
    ] {
        let device = state
            .device_registry
            .register(RegisterDeviceRequest {
                id: None,
                name: name.into(),
                device_type: "gotg".into(),
                hostname: None,
                capabilities: vec![],
                room_id: None,
            })
            .await
            .unwrap()
            .id;
        attribution
            .set_device_profile(&device, owner.map(String::as_str))
            .await
            .unwrap();
        devices.insert(token.to_string(), device);
    }
    state.handshake = Arc::new(Phones {
        devices,
        lock: built.handshake.clone(),
    });
    adjust(&mut state);

    let state = Arc::new(state);
    Household {
        router: build_router(state.clone(), std::path::PathBuf::from("pond-desktop/dist")),
        state,
        jerry,
        liz,
        ada,
        dir: built.dir,
    }
}

impl Household {
    pub async fn send(
        &self,
        method: Method,
        uri: &str,
        token: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {token}"));
        let body = match body {
            Some(value) => {
                builder = builder.header("Content-Type", "application/json");
                Body::from(serde_json::to_vec(&value).unwrap())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        (status, body)
    }

    pub async fn role_of(&self, profile_id: &str) -> Role {
        self.state
            .profile_repo
            .get(profile_id)
            .await
            .unwrap()
            .expect("the member exists")
            .role
    }

    pub async fn main_member(&self) -> Option<String> {
        self.state
            .handshake
            .pairing_lock()
            .await
            .unwrap()
            .owner_profile_id
    }
}
