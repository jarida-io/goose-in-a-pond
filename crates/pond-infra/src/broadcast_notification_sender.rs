//! `NotificationSender` over a broadcast channel (live clients), an offline queue, and an
//! optional push [`NotificationRelay`]. [`BroadcastNotificationSender::send_to_profile`] never
//! falls back to `broadcast()`: a member it can't address reaches nobody.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::{Notification, NotificationSender};
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::mcp::ports::notification_relay::NotificationRelay;
use pond_core::user_data::ports::device_attribution::{
    DeviceAttribution, TargetedDelivery, Undeliverable, RESERVED_BROADCAST_TARGET,
};
use tokio::sync::broadcast;

/// Sentinel `target` for "every connected device". Keep it the domain constant: the targeted
/// path refuses this string, and a drifted copy would silently disable the refusal.
pub const BROADCAST_TARGET: &str = RESERVED_BROADCAST_TARGET;

/// Payload caps enforced here for every producer; truncation is by char so it can't split UTF-8.
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_CHARS: usize = 2000;

fn clamp(mut n: Notification) -> Notification {
    if n.title.chars().count() > MAX_TITLE_CHARS {
        n.title = n.title.chars().take(MAX_TITLE_CHARS).collect();
    }
    if n.body.chars().count() > MAX_BODY_CHARS {
        n.body = n.body.chars().take(MAX_BODY_CHARS).collect();
    }
    n
}

/// Outcome of addressing one member. Empty `queued` is expected under
/// [`TargetedDelivery::Undeliverable`] but means every write failed under `ToDevices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileDeliveryReport {
    /// Who this was addressed to, and why it reached nobody when it did.
    pub plan: TargetedDelivery,
    /// Device ids the notification was queued and fanned out for.
    pub queued: Vec<String>,
    /// Device ids whose delivery failed, with the error.
    pub failed: Vec<(String, String)>,
}

impl ProfileDeliveryReport {
    pub fn reached_nobody(&self) -> bool {
        self.queued.is_empty()
    }
}

pub struct BroadcastNotificationSender {
    tx: broadcast::Sender<Notification>,
    queue: Arc<dyn NotificationQueueRepository>,
    relay: Option<Arc<dyn NotificationRelay>>,
    /// Device-to-profile lookup; `None` makes [`Self::send_to_profile`] refuse, never fall back.
    attribution: Option<Arc<dyn DeviceAttribution>>,
}

impl BroadcastNotificationSender {
    pub fn new(
        tx: broadcast::Sender<Notification>,
        queue: Arc<dyn NotificationQueueRepository>,
        relay: Option<Arc<dyn NotificationRelay>>,
    ) -> Self {
        Self {
            tx,
            queue,
            relay,
            attribution: None,
        }
    }

    /// Enable addressing a member; without it [`Self::send_to_profile`] delivers to nobody.
    pub fn with_device_attribution(mut self, attribution: Arc<dyn DeviceAttribution>) -> Self {
        self.attribution = Some(attribution);
        self
    }

    /// Deliver one notification to one member's devices; never broadcasts. Each device gets its
    /// own id (`enqueue` is `INSERT OR REPLACE` by id); one device failing doesn't stop the rest.
    pub async fn send_to_profile(
        &self,
        profile_id: &str,
        notification: Notification,
    ) -> ProfileDeliveryReport {
        let Some(attribution) = self.attribution.as_ref() else {
            return ProfileDeliveryReport {
                plan: TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(
                    "no DeviceAttribution wired into the notification sender".to_string(),
                )),
                queued: Vec::new(),
                failed: Vec::new(),
            };
        };
        let plan = TargetedDelivery::plan(
            profile_id,
            attribution.devices_for_profile(profile_id).await,
        );

        let mut queued = Vec::new();
        let mut failed = Vec::new();
        for device_id in plan.devices() {
            let mut per_device = notification.clone();
            per_device.id = per_device_notification_id(&notification.id, device_id);
            per_device.target = device_id.clone();
            match self.send(per_device).await {
                Ok(()) => queued.push(device_id.clone()),
                Err(e) => failed.push((device_id.clone(), e.to_string())),
            }
        }

        if let TargetedDelivery::Undeliverable(reason) = &plan {
            tracing::info!(
                profile = %profile_id,
                reason = reason.as_str(),
                "targeted notification reached nobody; not broadcasting"
            );
        }
        ProfileDeliveryReport {
            plan,
            queued,
            failed,
        }
    }
}

/// Deterministic per-device id, so a re-send replaces each device's row instead of adding one.
fn per_device_notification_id(notification_id: &str, device_id: &str) -> String {
    format!("{notification_id}:{device_id}")
}

#[async_trait]
impl NotificationSender for BroadcastNotificationSender {
    async fn send(&self, notification: Notification) -> Result<()> {
        let notification = clamp(notification);
        let targeted = notification.target != BROADCAST_TARGET;
        if targeted {
            // Persist for offline delivery before fanning out live.
            self.queue.enqueue(notification.clone()).await?;
            // Background push is best-effort — never fail the send on it.
            if let Some(relay) = &self.relay {
                if let Err(e) = relay.relay(&notification).await {
                    tracing::warn!(error = %e, "notification relay failed");
                }
            }
        }
        // No subscribers is normal (no device connected) — ignore the error.
        let _ = self.tx.send(notification);
        Ok(())
    }

    async fn broadcast(&self, notification: Notification) -> Result<()> {
        // Broadcasts are ephemeral (not per-device queued).
        let mut notification = clamp(notification);
        notification.target = BROADCAST_TARGET.to_string();
        let _ = self.tx.send(notification);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::push_token::PushToken;
    use std::sync::Mutex;

    #[derive(Default)]
    struct StubQueue {
        enqueued: Mutex<Vec<Notification>>,
    }
    #[async_trait]
    impl NotificationQueueRepository for StubQueue {
        async fn enqueue(&self, n: Notification) -> Result<()> {
            self.enqueued.lock().unwrap().push(n);
            Ok(())
        }
        async fn list_undelivered(&self, _device_id: &str) -> Result<Vec<Notification>> {
            Ok(self.enqueued.lock().unwrap().clone())
        }
        async fn mark_delivered(&self, _ids: &[String]) -> Result<()> {
            Ok(())
        }
    }

    fn notif(target: &str) -> Notification {
        Notification {
            id: "n1".into(),
            target: target.into(),
            category: "info".into(),
            title: "t".into(),
            body: "b".into(),
            timestamp: "2026-06-29T00:00:00Z".into(),
            data: None,
        }
    }

    #[tokio::test]
    async fn targeted_send_enqueues_and_broadcasts() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        sender.send(notif("dev-1")).await.unwrap();

        assert_eq!(queue.enqueued.lock().unwrap().len(), 1, "targeted enqueued");
        assert_eq!(rx.recv().await.unwrap().target, "dev-1", "fanned out live");
    }

    #[tokio::test]
    async fn oversized_title_and_body_are_truncated_char_safe() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        let mut n = notif("dev-1");
        // Multi-byte chars so a byte-based slice would panic at the boundary.
        n.title = "é".repeat(MAX_TITLE_CHARS + 50);
        n.body = "🦆".repeat(MAX_BODY_CHARS + 50);
        sender.send(n).await.unwrap();

        let got = rx.recv().await.unwrap();
        assert_eq!(got.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(got.body.chars().count(), MAX_BODY_CHARS);
        // The queued copy is clamped too (clamp happens before enqueue).
        let queued = &queue.enqueued.lock().unwrap()[0];
        assert_eq!(queued.body.chars().count(), MAX_BODY_CHARS);
    }

    /// Scripts `devices_for_profile`; other methods panic so a test can't pass via the wrong query.
    struct ScriptedAttribution {
        devices: std::sync::Mutex<Option<Result<Vec<String>>>>,
    }

    impl ScriptedAttribution {
        fn returning(ids: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                devices: Mutex::new(Some(Ok(ids.iter().map(|s| s.to_string()).collect()))),
            })
        }
        fn failing() -> Arc<Self> {
            Arc::new(Self {
                devices: Mutex::new(Some(Err(anyhow::anyhow!("database is locked")))),
            })
        }
    }

    #[async_trait]
    impl DeviceAttribution for ScriptedAttribution {
        async fn set_device_profile(&self, _d: &str, _p: Option<&str>) -> Result<()> {
            unreachable!("delivery never writes an attribution")
        }
        async fn device_profile(&self, _d: &str) -> Result<Option<String>> {
            unreachable!("delivery asks who a member's devices are, not whose a device is")
        }
        async fn devices_for_profile(&self, _p: &str) -> Result<Vec<String>> {
            self.devices
                .lock()
                .unwrap()
                .take()
                .expect("devices_for_profile asked twice for one delivery")
        }
        async fn push_tokens_for_profile(&self, _p: &str) -> Result<Vec<PushToken>> {
            unreachable!("the relay resolves a token from the device id it is handed")
        }
    }

    #[derive(Default)]
    struct CountingRelay {
        relayed: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl NotificationRelay for CountingRelay {
        async fn relay(&self, notification: &Notification) -> Result<()> {
            self.relayed
                .lock()
                .unwrap()
                .push(notification.target.clone());
            Ok(())
        }
    }

    /// Everything on the channel now; `try_recv` so asserting on nothing fails instead of hanging.
    fn drain(rx: &mut broadcast::Receiver<Notification>) -> Vec<Notification> {
        let mut out = Vec::new();
        while let Ok(n) = rx.try_recv() {
            out.push(n);
        }
        out
    }

    /// Vacuity control for the refusal tests below.
    #[tokio::test]
    async fn a_member_with_two_devices_gets_one_copy_each_and_the_house_gets_none() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let relay = Arc::new(CountingRelay::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), Some(relay.clone()))
            .with_device_attribution(ScriptedAttribution::returning(&["phone-liz", "watch-liz"]));

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert_eq!(report.queued, vec!["phone-liz", "watch-liz"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);

        let published = drain(&mut rx);
        let targets: Vec<&str> = published.iter().map(|n| n.target.as_str()).collect();
        assert_eq!(
            targets,
            vec!["phone-liz", "watch-liz"],
            "each device is addressed by name and the sentinel is never published"
        );
        assert_eq!(
            *relay.relayed.lock().unwrap(),
            vec!["phone-liz".to_string(), "watch-liz".to_string()],
            "background push is attempted per device, not once for the member"
        );

        // Per-device ids: `enqueue` is an INSERT OR REPLACE on the `notifications.id` key.
        let enqueued = queue.enqueued.lock().unwrap();
        let ids: Vec<&str> = enqueued.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert_ne!(
            ids[0], ids[1],
            "two devices sharing one notification id means the second row replaces the first, \
             and the phone that was switched off is the one that loses it"
        );
    }

    #[tokio::test]
    async fn a_member_with_no_attributed_device_reaches_nobody_rather_than_everybody() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None)
            .with_device_attribution(ScriptedAttribution::returning(&[]));

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert_eq!(
            report.plan,
            TargetedDelivery::Undeliverable(Undeliverable::NoAttributedDevice)
        );
        assert!(report.reached_nobody());
        assert!(
            queue.enqueued.lock().unwrap().is_empty(),
            "nothing is queued for a member with no device"
        );
        assert!(
            drain(&mut rx).is_empty(),
            "and NOTHING reaches the broadcast channel -- every connected screen in the house \
             subscribes to it, so one message there is the whole household reading a proposal \
             addressed to one person"
        );
    }

    #[tokio::test]
    async fn a_failed_attribution_read_reaches_nobody() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None)
            .with_device_attribution(ScriptedAttribution::failing());

        let report = sender.send_to_profile("liz", notif("unused")).await;

        match &report.plan {
            TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(why)) => {
                assert!(why.contains("database is locked"), "{why}")
            }
            other => panic!("a failed read must be AttributionUnavailable, got {other:?}"),
        }
        assert!(queue.enqueued.lock().unwrap().is_empty());
        assert!(
            drain(&mut rx).is_empty(),
            "a read failure publishes nothing"
        );
    }

    #[tokio::test]
    async fn an_unwired_attribution_reaches_nobody() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert!(matches!(
            report.plan,
            TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(_))
        ));
        assert!(queue.enqueued.lock().unwrap().is_empty());
        assert!(drain(&mut rx).is_empty());
    }

    /// Device ids are caller-supplied, and sending to "broadcast" would hit every subscriber.
    #[tokio::test]
    async fn a_device_registered_as_the_sentinel_is_not_delivered_to() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender =
            BroadcastNotificationSender::new(tx, queue.clone(), None).with_device_attribution(
                ScriptedAttribution::returning(&[BROADCAST_TARGET, "phone-liz"]),
            );

        let report = sender.send_to_profile("liz", notif("unused")).await;

        assert_eq!(report.queued, vec!["phone-liz"]);
        let published = drain(&mut rx);
        assert_eq!(published.len(), 1);
        assert_eq!(
            published[0].target, "phone-liz",
            "the member's real device is still reached and the sentinel is not"
        );
    }

    #[tokio::test]
    async fn broadcast_does_not_enqueue() {
        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None);

        sender.broadcast(notif("ignored")).await.unwrap();

        assert!(
            queue.enqueued.lock().unwrap().is_empty(),
            "broadcast not queued"
        );
        assert_eq!(rx.recv().await.unwrap().target, BROADCAST_TARGET);
    }
}
