//! Driven port: push events from GIAP to connected clients (e.g. the GOTG mobile app).
//!
//! TODO: categories, acknowledgement, history and persistence, a WebSocket or SSE transport.

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// A notification to push to connected devices.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    /// Target device ID, or "broadcast" for all
    pub target: String,
    /// "alert", "info", "action_required"
    pub category: String,
    pub title: String,
    pub body: String,
    pub timestamp: String,
    /// Arbitrary payload for the client to act on
    pub data: Option<serde_json::Value>,
}

/// Driven Port: push notifications to connected clients.
#[async_trait]
pub trait NotificationSender: Send + Sync {
    /// Send a notification to a specific device or broadcast.
    async fn send(&self, notification: Notification) -> Result<()>;

    async fn broadcast(&self, notification: Notification) -> Result<()>;
}
