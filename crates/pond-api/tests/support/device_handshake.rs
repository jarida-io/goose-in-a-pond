//! Device-bound test credential for delivery tests. Real issuance is exercised
//! separately in remote_authorization.rs.
use anyhow::Result;
use async_trait::async_trait;
use pond_core::security::ports::handshake::{
    Handshake, HandshakeRequest, HandshakeResponse, TokenCaller,
};

pub struct DeviceHandshake(pub String);

#[async_trait]
impl Handshake for DeviceHandshake {
    async fn handshake(&self, _: HandshakeRequest) -> Result<HandshakeResponse> {
        anyhow::bail!("pairing is not used by this delivery fixture")
    }
    async fn validate_token(&self, token: &str) -> Result<bool> {
        Ok(token == "test-token")
    }
    async fn caller_for_token(&self, token: &str) -> Result<Option<TokenCaller>> {
        Ok((token == "test-token").then(|| TokenCaller {
            client_id: self.0.clone(),
            device_id: self.0.clone(),
        }))
    }
    async fn revoke_token(&self, _: &str) -> Result<()> {
        anyhow::bail!("revocation is not used by this delivery fixture")
    }
}
