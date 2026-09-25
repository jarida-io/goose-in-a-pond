//! Pond Compute mesh wire protocol: hashing, identity and messages. Keep it free of
//! `tokio`, `sqlx`, `reqwest`, `goose` and `rmcp`; transport is `pond-adapters-mesh-libp2p`.

pub mod hashing;
pub mod identity;
pub mod wire;
