//! The household's notification history: every durable notice the Pond raised, as a screen that
//! was not connected at the time (the dashboard after a reload) can list it. Who may read which
//! notice is [`HistoryAudience`]; the store is the `NotificationHistory` port.
//!
//! Live-only status (`NotificationSender::publish_live`, the model warming up) is never recorded:
//! it is transient by definition, and the dashboard's warm-up banner already shows it.

use serde::Serialize;

/// Who may read one notice in the history, decided when it is raised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryAudience {
    /// Every paired device, except the one whose action caused it (a removal is never news to
    /// the device that did it), exactly like `NotificationSender::broadcast_except`.
    Household {
        /// The device that caused the notice, which does not see it; `None` for a plain broadcast.
        except_device: Option<String>,
    },
    /// Only these devices: one member's own devices, or the one device a notice was sent to.
    /// The same devices a notice is queued for at the time, so a device attributed to the member
    /// later does not see notices raised before. Empty means nobody reads it.
    Devices(Vec<String>),
}

impl HistoryAudience {
    /// Whether `device_id` may read a notice with this audience. The store applies the same rule
    /// in its query; this is the rule's single statement, for the mock and for the tests.
    pub fn admits(&self, device_id: &str) -> bool {
        match self {
            Self::Household { except_device } => except_device.as_deref() != Some(device_id),
            Self::Devices(devices) => devices.iter().any(|d| d == device_id),
        }
    }
}

/// One notice as a device reads it from the history: the notice under its household id (never a
/// per-device copy's id), and whether this device has marked it read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistoryEntry {
    /// The notice's own id. A device's queued copy of it carries
    /// `per_device_notification_id(id, device)`, which a client maps back to this id.
    pub id: String,
    /// `alert`, `info` or `action_required`.
    pub category: String,
    /// The headline, at most 200 characters.
    pub title: String,
    /// The text, at most 2000 characters.
    pub body: String,
    /// When the notice was raised (RFC 3339), as its producer stamped it.
    pub timestamp: String,
    /// The notice's payload, `open` route hint included.
    pub data: Option<serde_json::Value>,
    /// Whether the reading device has marked it read.
    pub read: bool,
}

/// The most entries one history read returns.
pub const HISTORY_LIST_MAX: usize = 200;

/// The most ids one mark-read request names; as many as a device can hold in its history.
pub const HISTORY_MARK_READ_MAX: usize = 500;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_household_notice_reaches_every_device_but_the_one_that_caused_it() {
        let everyone = HistoryAudience::Household {
            except_device: None,
        };
        assert!(everyone.admits("dashboard"));
        assert!(everyone.admits("phone-a"));

        let not_the_actor = HistoryAudience::Household {
            except_device: Some("phone-a".into()),
        };
        assert!(!not_the_actor.admits("phone-a"));
        assert!(not_the_actor.admits("phone-b"));
        assert!(not_the_actor.admits("dashboard"));
    }

    #[test]
    fn an_addressed_notice_reaches_only_its_devices() {
        let liz = HistoryAudience::Devices(vec!["phone-liz".into(), "dashboard-liz".into()]);
        assert!(liz.admits("phone-liz"));
        assert!(liz.admits("dashboard-liz"));
        assert!(!liz.admits("phone-jerry"));
        assert!(!liz.admits("kitchen-tablet"));

        assert!(
            !HistoryAudience::Devices(Vec::new()).admits("anyone"),
            "a member with no device has nobody reading their notice, not everybody"
        );
    }
}
