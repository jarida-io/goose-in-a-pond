//! A setup channel for transport tests: the transports only move bytes, so `hello` is enough.

use std::sync::Arc;

use async_trait::async_trait;
use pond_core::network::mocks::mock_code_announcer::MockCodeAnnouncer;
use pond_core::network::mocks::mock_setup_code_store::MockSetupCodeStore;
use pond_core::network::services::setup_claim::SetupClaimService;
use pond_core::network::services::spoken_code::SpokenCodeService;
use pond_core::security::ports::handshake::{Handshake, HandshakeRequest, HandshakeResponse};
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

use crate::setup_channel::{SetupChannel, SetupChannelConfig, SetupChannelContext};

pub struct NoHandshake;

#[async_trait]
impl Handshake for NoHandshake {
    async fn handshake(&self, _: HandshakeRequest) -> anyhow::Result<HandshakeResponse> {
        anyhow::bail!("unused")
    }
    async fn validate_token(&self, _: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    async fn revoke_device(&self, _: &str) -> anyhow::Result<u64> {
        Ok(0)
    }
    async fn revoke_token(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

pub struct TestPond {
    pub channel: SetupChannel,
    pub store: Arc<MockSetupCodeStore>,
    pub claims: Arc<SetupClaimService>,
    pub settings: Arc<MockSettingsRepository>,
}

/// An unclaimed pond whose card id is [`MockSetupCodeStore::DISCRIMINATOR`] (`7f3a`).
pub fn test_pond() -> TestPond {
    let store = Arc::new(MockSetupCodeStore::unclaimed());
    let claims = Arc::new(SetupClaimService::new(store.clone()));
    let settings = Arc::new(MockSettingsRepository::new());
    let channel = SetupChannel::new(
        SetupChannelContext::new(
            SetupChannelConfig {
                discriminator: MockSetupCodeStore::DISCRIMINATOR,
                pin: "sha256/pin".into(),
                https_port: 8443,
                hostname: "pond".into(),
            },
            Arc::new(NoHandshake),
            Arc::new(SpokenCodeService::new(Some(Arc::new(
                MockCodeAnnouncer::new(),
            )))),
            settings.clone(),
            Arc::new(MockProfileRepository::new()),
        )
        .with_claims(claims.clone()),
    );
    TestPond {
        channel,
        store,
        claims,
        settings,
    }
}
