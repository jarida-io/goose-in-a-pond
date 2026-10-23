//! `NotificationSender` over a broadcast channel (live clients), an offline queue, and an
//! optional push [`NotificationRelay`]. [`BroadcastNotificationSender::send_to_profile`] never
//! falls back to `broadcast()`: a member it can't address reaches nobody.
//!
//! `broadcast()` is durable for phones: besides the live copy every connected screen sees, each
//! paired phone gets its own copy, id [`per_device_notification_id`], queued and relayed exactly
//! as a targeted `send()`. The notification stream skips the live broadcast copy for a device
//! that has its own queued copy, so a connected phone sees the notice once, under one id.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::notification::{
    per_device_notification_id, MemberDelivery, MemberNotifier, Notification, NotificationSender,
};
use pond_core::mcp::ports::notification_queue::NotificationQueueRepository;
use pond_core::mcp::ports::notification_recipients::NotificationRecipients;
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

    /// The outcome as a member-addressed caller needs it: a phone of theirs, none, or a fault.
    pub fn outcome(&self) -> MemberDelivery {
        match &self.plan {
            TargetedDelivery::ToDevices(_) if !self.queued.is_empty() => {
                MemberDelivery::Reached(self.queued.clone())
            }
            TargetedDelivery::ToDevices(_) => MemberDelivery::Failed(format!(
                "every delivery failed: {}",
                self.failed
                    .iter()
                    .map(|(device, error)| format!("{device}: {error}"))
                    .collect::<Vec<_>>()
                    .join("; ")
            )),
            TargetedDelivery::Undeliverable(
                Undeliverable::NoAttributedDevice | Undeliverable::ReservedTargetOnly,
            ) => MemberDelivery::NoPhone,
            TargetedDelivery::Undeliverable(Undeliverable::NotAMember) => {
                MemberDelivery::Failed("not a household member".to_string())
            }
            TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(why)) => {
                MemberDelivery::Failed(format!("the member's devices could not be read: {why}"))
            }
        }
    }
}

pub struct BroadcastNotificationSender {
    tx: broadcast::Sender<Notification>,
    queue: Arc<dyn NotificationQueueRepository>,
    relay: Option<Arc<dyn NotificationRelay>>,
    /// Device-to-profile lookup; `None` makes [`Self::send_to_profile`] refuse, never fall back.
    attribution: Option<Arc<dyn DeviceAttribution>>,
    /// The paired phones a broadcast is made durable for; `None` keeps broadcasts live-only.
    recipients: Option<Arc<dyn NotificationRecipients>>,
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
            recipients: None,
        }
    }

    /// Make every `broadcast()` durable for the phones `recipients` names: one queued, relayed
    /// copy each, so a household notice reaches a phone that is asleep or offline.
    pub fn with_household_recipients(
        mut self,
        recipients: Arc<dyn NotificationRecipients>,
    ) -> Self {
        self.recipients = Some(recipients);
        self
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

impl BroadcastNotificationSender {
    /// Queue and relay one copy of a household notice per paired phone, through `send()`. Runs
    /// before the live broadcast copy is published, so the stream finds each copy already queued
    /// when it decides whether a phone needs the broadcast copy. A phone whose copy could not be
    /// queued has no row, so the stream still gives it the live copy.
    async fn fan_out_to_phones(&self, notification: &Notification) {
        let Some(recipients) = self.recipients.as_ref() else {
            return;
        };
        let phones = match recipients.paired_phones().await {
            Ok(phones) => phones,
            Err(e) => {
                tracing::warn!(
                    kind = "notification_fanout_failed",
                    notification_id = %notification.id,
                    error = %e,
                    "could not read the paired phones; this notice reaches connected screens only"
                );
                return;
            }
        };
        let mut queued = 0usize;
        // Device ids are caller-supplied; a device named the sentinel would be every subscriber.
        for device_id in phones.iter().filter(|d| d.as_str() != BROADCAST_TARGET) {
            let mut copy = notification.clone();
            copy.id = per_device_notification_id(&notification.id, device_id);
            copy.target = device_id.clone();
            match self.send(copy).await {
                Ok(()) => queued += 1,
                Err(e) => tracing::warn!(
                    kind = "notification_fanout_device_failed",
                    notification_id = %notification.id,
                    device = %device_id,
                    error = %e,
                    "could not queue a household notice for one phone; it reaches it only live"
                ),
            }
        }
        tracing::info!(
            kind = "notification_fanned_out",
            notification_id = %notification.id,
            category = %notification.category,
            phones = phones.len(),
            queued,
            "household notice queued for paired phones"
        );
    }
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
                    tracing::warn!(
                        kind = "push_relay_failed",
                        notification_id = %notification.id,
                        device = %notification.target,
                        error = %e,
                        "notification relay failed"
                    );
                }
            }
        }
        // No subscribers is normal (no device connected) — ignore the error.
        let _ = self.tx.send(notification);
        Ok(())
    }

    async fn broadcast(&self, notification: Notification) -> Result<()> {
        let mut notification = clamp(notification);
        notification.target = BROADCAST_TARGET.to_string();
        // Durable copies first: the stream checks for them when the live copy arrives.
        self.fan_out_to_phones(&notification).await;
        // No subscribers is normal (no screen connected) -- ignore the error.
        let _ = self.tx.send(notification);
        Ok(())
    }
}

#[async_trait]
impl MemberNotifier for BroadcastNotificationSender {
    async fn notify_member(&self, profile_id: &str, notification: Notification) -> MemberDelivery {
        let report = self.send_to_profile(profile_id, notification).await;
        for (device, error) in &report.failed {
            tracing::warn!(device = %device, error = %error, "member notification failed for one device");
        }
        report.outcome()
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
        async fn is_queued_for(&self, id: &str, device_id: &str) -> Result<bool> {
            Ok(self
                .enqueued
                .lock()
                .unwrap()
                .iter()
                .any(|n| n.id == id && n.target == device_id))
        }
        async fn acknowledge(&self, id: &str, device_id: &str) -> Result<bool> {
            self.is_queued_for(id, device_id).await
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
    async fn the_member_port_reports_the_devices_it_reached() {
        let (tx, _rx) = broadcast::channel(8);
        let sender = BroadcastNotificationSender::new(tx, Arc::new(StubQueue::default()), None)
            .with_device_attribution(ScriptedAttribution::returning(&["phone-liz"]));
        let port: &dyn MemberNotifier = &sender;
        assert_eq!(
            port.notify_member("liz", notif("unused")).await,
            MemberDelivery::Reached(vec!["phone-liz".to_string()])
        );
    }

    /// A member without a phone and a pond that failed are different facts to the caller.
    #[tokio::test]
    async fn the_member_port_tells_no_phone_from_a_failure() {
        let port = |attribution: Option<Arc<ScriptedAttribution>>| {
            let (tx, _rx) = broadcast::channel(8);
            let sender = BroadcastNotificationSender::new(tx, Arc::new(StubQueue::default()), None);
            match attribution {
                Some(a) => sender.with_device_attribution(a),
                None => sender,
            }
        };
        assert_eq!(
            port(Some(ScriptedAttribution::returning(&[])))
                .notify_member("liz", notif("unused"))
                .await,
            MemberDelivery::NoPhone
        );
        for (why, sender) in [
            ("unreadable", port(Some(ScriptedAttribution::failing()))),
            ("unwired", port(None)),
        ] {
            assert!(
                matches!(
                    sender.notify_member("liz", notif("unused")).await,
                    MemberDelivery::Failed(_)
                ),
                "{why} attribution must read as a failure, not as no phone"
            );
        }
        assert!(matches!(
            port(Some(ScriptedAttribution::returning(&["phone"])))
                .notify_member("  ", notif("unused"))
                .await,
            MemberDelivery::Failed(_)
        ));
    }

    #[tokio::test]
    async fn every_write_failing_is_a_failure() {
        struct BrokenQueue;
        #[async_trait]
        impl NotificationQueueRepository for BrokenQueue {
            async fn enqueue(&self, _n: Notification) -> Result<()> {
                anyhow::bail!("disk full")
            }
            async fn list_undelivered(&self, _device_id: &str) -> Result<Vec<Notification>> {
                Ok(Vec::new())
            }
            async fn mark_delivered(&self, _ids: &[String]) -> Result<()> {
                Ok(())
            }
            async fn is_queued_for(&self, _id: &str, _device_id: &str) -> Result<bool> {
                Ok(false)
            }
            async fn acknowledge(&self, _id: &str, _device_id: &str) -> Result<bool> {
                Ok(false)
            }
        }
        let (tx, _rx) = broadcast::channel(8);
        let sender = BroadcastNotificationSender::new(tx, Arc::new(BrokenQueue), None)
            .with_device_attribution(ScriptedAttribution::returning(&["phone-liz"]));
        match sender.notify_member("liz", notif("unused")).await {
            MemberDelivery::Failed(why) => assert!(why.contains("disk full"), "{why}"),
            other => panic!("a member whose every copy failed was told {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_broadcast_is_queued_and_relayed_once_per_paired_phone() {
        use pond_core::mcp::mocks::mock_notification_recipients::MockNotificationRecipients;

        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let relay = Arc::new(CountingRelay::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), Some(relay.clone()))
            .with_household_recipients(Arc::new(MockNotificationRecipients::with_phones(&[
                "phone-a", "phone-b",
            ])));

        sender.broadcast(notif("ignored")).await.unwrap();

        let enqueued = queue.enqueued.lock().unwrap().clone();
        let rows: Vec<(&str, &str)> = enqueued
            .iter()
            .map(|n| (n.id.as_str(), n.target.as_str()))
            .collect();
        assert_eq!(
            rows,
            vec![("n1:phone-a", "phone-a"), ("n1:phone-b", "phone-b")],
            "one durable row per phone, each under its own per-device id"
        );
        assert_eq!(
            *relay.relayed.lock().unwrap(),
            vec!["phone-a".to_string(), "phone-b".to_string()],
            "each phone is woken once, for its own copy"
        );

        let published: Vec<(String, String)> = drain(&mut rx)
            .into_iter()
            .map(|n| (n.id, n.target))
            .collect();
        assert_eq!(
            published,
            vec![
                ("n1:phone-a".to_string(), "phone-a".to_string()),
                ("n1:phone-b".to_string(), "phone-b".to_string()),
                ("n1".to_string(), BROADCAST_TARGET.to_string()),
            ],
            "the phones' copies are queued and published before the live copy for other screens"
        );
    }

    #[tokio::test]
    async fn an_unreadable_phone_list_still_reaches_connected_screens() {
        use pond_core::mcp::mocks::mock_notification_recipients::MockNotificationRecipients;

        let (tx, mut rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let relay = Arc::new(CountingRelay::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), Some(relay.clone()))
            .with_household_recipients(Arc::new(MockNotificationRecipients::failing()));

        sender.broadcast(notif("ignored")).await.unwrap();

        assert!(queue.enqueued.lock().unwrap().is_empty());
        assert!(relay.relayed.lock().unwrap().is_empty());
        let published = drain(&mut rx);
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].target, BROADCAST_TARGET);
    }

    #[tokio::test]
    async fn a_phone_registered_as_the_sentinel_gets_no_durable_copy() {
        use pond_core::mcp::mocks::mock_notification_recipients::MockNotificationRecipients;

        let (tx, _rx) = broadcast::channel(8);
        let queue = Arc::new(StubQueue::default());
        let sender = BroadcastNotificationSender::new(tx, queue.clone(), None)
            .with_household_recipients(Arc::new(MockNotificationRecipients::with_phones(&[
                BROADCAST_TARGET,
                "phone-a",
            ])));

        sender.broadcast(notif("ignored")).await.unwrap();

        let targets: Vec<String> = queue
            .enqueued
            .lock()
            .unwrap()
            .iter()
            .map(|n| n.target.clone())
            .collect();
        assert_eq!(targets, vec!["phone-a".to_string()]);
    }

    #[tokio::test]
    async fn a_broadcast_without_recipients_is_live_only() {
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
