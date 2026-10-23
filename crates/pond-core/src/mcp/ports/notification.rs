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
    /// Arbitrary payload for the client to act on. An optional `open` key, written only through
    /// [`OpenScreen`], names the client screen a tap on the notification should open.
    pub data: Option<serde_json::Value>,
}

/// The client screen a tapped notification opens: the `open` route hint in [`Notification::data`].
///
/// The set is closed on purpose. A client validates `open` against its own allow-list and ignores
/// anything else, so a value added here does nothing until the client learns it, and a client
/// talking to an older pond, which sends no hint, opens its default screen as before.
/// [`OpenScreen::Schedule`] becomes `{"open": "schedule", "schedule_id": "...", "run_id": "..."}`
/// and [`OpenScreen::Devices`] becomes `{"open": "devices"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenScreen {
    /// The schedules list, with the schedule and the run the notification reports on.
    Schedule { schedule_id: String, run_id: String },
    /// The paired and smart-home devices list.
    Devices,
    /// The chat screen.
    Chat,
}

impl OpenScreen {
    /// The hint as a complete `data` payload.
    pub fn data(&self) -> serde_json::Value {
        serde_json::Value::Object(self.fields())
    }

    /// `fields` with the hint's keys added. The hint wins a clash, since `open` is reserved for it.
    pub fn merged_into(
        &self,
        mut fields: serde_json::Map<String, serde_json::Value>,
    ) -> serde_json::Value {
        fields.extend(self.fields());
        serde_json::Value::Object(fields)
    }

    fn fields(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut fields = serde_json::Map::new();
        let screen = match self {
            Self::Schedule {
                schedule_id,
                run_id,
            } => {
                fields.insert("schedule_id".into(), schedule_id.as_str().into());
                fields.insert("run_id".into(), run_id.as_str().into());
                "schedule"
            }
            Self::Devices => "devices",
            Self::Chat => "chat",
        };
        fields.insert("open".into(), screen.into());
        fields
    }
}

/// Driven Port: push notifications to connected clients.
#[async_trait]
pub trait NotificationSender: Send + Sync {
    /// Send a notification to a specific device or broadcast.
    async fn send(&self, notification: Notification) -> Result<()>;

    /// A household-wide notice: every connected screen sees it live, and every paired phone
    /// gets its own durable copy (queued, and woken through the push relay) so a phone that is
    /// asleep or offline still receives it.
    async fn broadcast(&self, notification: Notification) -> Result<()>;
}

/// The id of one device's copy of `notification_id`: `"{notification_id}:{device_id}"`.
///
/// Deterministic, so a re-send replaces that device's queued row instead of adding one, and so
/// the notification stream can tell whether a household notice already has a copy queued for
/// the device it serves. The app dedupes by this id across the stream, a push wake and
/// `GET /notifications/pending`.
pub fn per_device_notification_id(notification_id: &str, device_id: &str) -> String {
    format!("{notification_id}:{device_id}")
}

/// What became of a notification for one household member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberDelivery {
    /// Queued for these devices of theirs; never empty.
    Reached(Vec<String>),
    /// The member has no paired device of their own.
    NoPhone,
    /// Nothing was delivered: their devices could not be read, the id is not a member, or every
    /// write failed. The reason is for logs.
    Failed(String),
}

/// Driven Port: deliver to one household member's own devices, never a broadcast.
#[async_trait]
pub trait MemberNotifier: Send + Sync {
    async fn notify_member(&self, profile_id: &str, notification: Notification) -> MemberDelivery;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_schedule_hint_names_the_schedule_and_the_run() {
        let hint = OpenScreen::Schedule {
            schedule_id: "sched-1".into(),
            run_id: "run-9".into(),
        };
        assert_eq!(
            hint.data(),
            json!({"open": "schedule", "schedule_id": "sched-1", "run_id": "run-9"})
        );
    }

    #[test]
    fn the_devices_and_chat_hints_carry_only_the_screen() {
        assert_eq!(OpenScreen::Devices.data(), json!({"open": "devices"}));
        assert_eq!(OpenScreen::Chat.data(), json!({"open": "chat"}));
    }

    #[test]
    fn merging_keeps_the_existing_fields_and_the_hint_owns_open() {
        let fields = json!({"proposal_id": "p-1", "rationale": "why", "open": "bogus"});
        let serde_json::Value::Object(fields) = fields else {
            unreachable!("a json object literal")
        };
        assert_eq!(
            OpenScreen::Chat.merged_into(fields),
            json!({"proposal_id": "p-1", "rationale": "why", "open": "chat"})
        );
    }
}
