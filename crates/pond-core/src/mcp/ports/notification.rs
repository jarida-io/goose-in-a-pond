//! Driven port: notices from the Pond to the household's screens and phones (the GOTG app).
//! How they are delivered, and the catalogue of what the Pond notifies about, are in
//! `docs/architecture/notifications.md`.

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
/// and [`OpenScreen::ScheduleDetail`] the same without `run_id`; every other screen carries only
/// its name, e.g. `{"open": "devices"}`.
///
/// `models`, `extensions` and `remote_access` (2026-10-27) are newer than the app's allow-list:
/// an app that does not know one opens where it was, exactly as for a notice with no hint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenScreen {
    /// The schedules list, with the schedule and the run the notification reports on.
    Schedule { schedule_id: String, run_id: String },
    /// One schedule's detail screen, where a missed run is answered.
    ScheduleDetail { schedule_id: String },
    /// The paired and smart-home devices list.
    Devices,
    /// The chat screen.
    Chat,
    /// The models screen: downloads, and the disk space they take.
    Models,
    /// The extensions screen: what the assistant can use, and what failed to start.
    Extensions,
    /// The remote-access screen: reaching the Pond from outside the home.
    RemoteAccess,
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
            Self::ScheduleDetail { schedule_id } => {
                fields.insert("schedule_id".into(), schedule_id.as_str().into());
                "schedule"
            }
            Self::Devices => "devices",
            Self::Chat => "chat",
            Self::Models => "models",
            Self::Extensions => "extensions",
            Self::RemoteAccess => "remote_access",
        };
        fields.insert("open".into(), screen.into());
        fields
    }
}

/// Driven Port: push notifications to connected clients.
#[async_trait]
pub trait NotificationSender: Send + Sync {
    /// Send a notification to one device: queued for it, woken through the push relay, and
    /// published live.
    async fn send(&self, notification: Notification) -> Result<()>;

    /// A transient status for whichever screens are connected right now (the model warming up):
    /// published live as a broadcast, never queued for a phone and never pushed. A phone that was
    /// asleep through it has no use for it later. Use [`Self::broadcast`] for anything the
    /// household should hear about.
    async fn publish_live(&self, notification: Notification) -> Result<()>;

    /// A household-wide notice: every connected screen sees it live, and every paired phone
    /// gets its own durable copy (queued, and woken through the push relay) so a phone that is
    /// asleep or offline still receives it.
    async fn broadcast(&self, notification: Notification) -> Result<()>;

    /// A household notice about something `except_device` just did: every other paired phone
    /// gets its own durable copy, and `except_device` gets nothing, so a device is never told
    /// what it did itself. No live broadcast copy goes out, since the stream would hand that one
    /// to `except_device` too.
    async fn broadcast_except(&self, notification: Notification, except_device: &str)
        -> Result<()>;
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
    fn a_schedule_detail_hint_names_the_schedule_and_no_run() {
        let hint = OpenScreen::ScheduleDetail {
            schedule_id: "sched-1".into(),
        };
        assert_eq!(
            hint.data(),
            json!({"open": "schedule", "schedule_id": "sched-1"})
        );
    }

    #[test]
    fn every_other_hint_carries_only_the_screen() {
        assert_eq!(OpenScreen::Devices.data(), json!({"open": "devices"}));
        assert_eq!(OpenScreen::Chat.data(), json!({"open": "chat"}));
        assert_eq!(OpenScreen::Models.data(), json!({"open": "models"}));
        assert_eq!(OpenScreen::Extensions.data(), json!({"open": "extensions"}));
        assert_eq!(
            OpenScreen::RemoteAccess.data(),
            json!({"open": "remote_access"})
        );
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
