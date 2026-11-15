//! Goes back for notices no phone came for: wakes the phone again, says a due reminder aloud,
//! and hands over what quiet hours held once they end.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::mcp::domain::notice_delivery::{
    marked, morning_notice, passes_quiet_hours, RewakePolicy, Wake, QUIET_PASS_KEY,
};
use crate::mcp::ports::notification::{Notification, NotificationSender};
use crate::mcp::ports::notification_queue::NotificationQueueRepository;
use crate::mcp::ports::notification_relay::NotificationRelay;
use crate::mcp::ports::room_speaker::RoomSpeaker;
use crate::shared::services::chat::household_quiet_at;
use crate::user_data::ports::settings::SettingsRepository;

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FollowUpReport {
    pub woken: usize,
    pub spoken: usize,
    /// Held notices handed over, counted before they were rolled into one per phone.
    pub released: usize,
}

impl FollowUpReport {
    pub fn did_nothing(&self) -> bool {
        *self == Self::default()
    }
}

pub struct NoticeFollowUp {
    queue: Arc<dyn NotificationQueueRepository>,
    sender: Arc<dyn NotificationSender>,
    settings: Arc<dyn SettingsRepository>,
    relay: Option<Arc<dyn NotificationRelay>>,
    speaker: Option<Arc<dyn RoomSpeaker>>,
    policy: RewakePolicy,
}

impl NoticeFollowUp {
    pub fn new(
        queue: Arc<dyn NotificationQueueRepository>,
        sender: Arc<dyn NotificationSender>,
        settings: Arc<dyn SettingsRepository>,
    ) -> Self {
        Self {
            queue,
            sender,
            settings,
            relay: None,
            speaker: None,
            policy: RewakePolicy::default(),
        }
    }

    pub fn with_relay(mut self, relay: Arc<dyn NotificationRelay>) -> Self {
        self.relay = Some(relay);
        self
    }

    pub fn with_speaker(mut self, speaker: Arc<dyn RoomSpeaker>) -> Self {
        self.speaker = Some(speaker);
        self
    }

    pub async fn pass(&self, now: DateTime<Utc>) -> FollowUpReport {
        let settings = match self.settings.get().await {
            Ok(settings) => settings,
            Err(error) => {
                tracing::warn!(%error, "settings unreadable; notices are not followed up this pass");
                return FollowUpReport::default();
            }
        };
        // Unreadable bounds hold nothing, as in the sender.
        let quiet = settings.notices_wait_in_quiet_hours
            && household_quiet_at(&settings, now) == Some(true);
        let mut report = FollowUpReport::default();
        if !quiet {
            report.released = self.release_held(now).await;
        }

        let waiting = match self
            .queue
            .list_unfetched(now - self.policy.gives_up_after)
            .await
        {
            Ok(waiting) => waiting,
            Err(error) => {
                tracing::warn!(%error, "could not list the notices no phone has fetched");
                return report;
            }
        };
        for notice in &waiting {
            let may_go_out = !quiet || passes_quiet_hours(&notice.notification);
            if may_go_out
                && self.policy.wake_is_due(notice, now)
                && self.wake(&notice.notification).await
            {
                report.woken += 1;
            }
            if settings.speak_unfetched_reminders
                && self.policy.speaking_is_due(notice, now)
                && self.say(&notice.notification).await
            {
                report.spoken += 1;
            }
        }
        if !report.did_nothing() {
            tracing::info!(
                target: "giap::trace",
                kind = "notice_follow_up",
                woken = report.woken,
                spoken = report.spoken,
                released = report.released,
                waiting = waiting.len(),
                "notices no phone had fetched were followed up"
            );
        }
        report
    }

    async fn wake(&self, notification: &Notification) -> bool {
        let Some(relay) = &self.relay else {
            return false;
        };
        match relay.relay(notification).await {
            Ok(Wake::Sent) => {
                if let Err(error) = self.queue.note_wake(&notification.id).await {
                    tracing::warn!(%error, "could not record a repeated wake");
                }
                true
            }
            Ok(Wake::NotSent(_)) => false,
            Err(error) => {
                tracing::debug!(%error, notification_id = %notification.id, "a repeated wake failed");
                false
            }
        }
    }

    async fn say(&self, notification: &Notification) -> bool {
        let Some(speaker) = &self.speaker else {
            return false;
        };
        match speaker.say(&notification.title).await {
            Ok(true) => {
                if let Err(error) = self.queue.note_spoken(&notification.id).await {
                    tracing::warn!(%error, "could not record that a notice was spoken");
                }
                true
            }
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(%error, notification_id = %notification.id, "could not say a notice aloud");
                false
            }
        }
    }

    /// Each phone's held notices become one, sent now.
    async fn release_held(&self, now: DateTime<Utc>) -> usize {
        let held = match self.queue.take_held().await {
            Ok(held) => held,
            Err(error) => {
                tracing::warn!(%error, "could not take the notices quiet hours held");
                return 0;
            }
        };
        let mut by_phone: Vec<(String, Vec<Notification>)> = Vec::new();
        for notification in &held {
            match by_phone
                .iter_mut()
                .find(|(phone, _)| *phone == notification.target)
            {
                Some((_, theirs)) => theirs.push(notification.clone()),
                None => by_phone.push((notification.target.clone(), vec![notification.clone()])),
            }
        }
        for (phone, theirs) in &by_phone {
            let mut morning = morning_notice(phone, theirs, now);
            // Sent the moment quiet hours end; the mark keeps a clock a second behind from holding it.
            morning.data = marked(morning.data, QUIET_PASS_KEY);
            if let Err(error) = self.sender.send(morning).await {
                tracing::warn!(%error, phone, held = theirs.len(), "the notice for quiet hours could not be sent");
            }
        }
        held.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::domain::notice_delivery::{UnfetchedNotice, SPEAK_IF_UNFETCHED_KEY};
    use crate::mcp::mocks::mock_notification_sender::MockNotificationSender;
    use crate::user_data::domain::settings::Settings;
    use anyhow::Result;
    use async_trait::async_trait;
    use chrono::Duration;
    use serde_json::json;
    use std::sync::Mutex;

    fn noon() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn reminder(id: &str, phone: &str) -> Notification {
        Notification {
            id: id.into(),
            target: phone.into(),
            category: "info".into(),
            title: "Reminder: get superglue".into(),
            body: String::new(),
            timestamp: noon().to_rfc3339(),
            data: Some(json!({ SPEAK_IF_UNFETCHED_KEY: true, QUIET_PASS_KEY: true })),
        }
    }

    #[derive(Default)]
    struct Queue {
        waiting: Mutex<Vec<UnfetchedNotice>>,
        held: Mutex<Vec<Notification>>,
        wakes: Mutex<Vec<String>>,
        spoken: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl NotificationQueueRepository for Queue {
        async fn enqueue(&self, _: Notification) -> Result<()> {
            Ok(())
        }
        async fn list_undelivered(&self, _: &str) -> Result<Vec<Notification>> {
            Ok(Vec::new())
        }
        async fn mark_delivered(&self, _: &[String]) -> Result<()> {
            Ok(())
        }
        async fn is_queued_for(&self, _: &str, _: &str) -> Result<bool> {
            Ok(false)
        }
        async fn acknowledge(&self, _: &str, _: &str) -> Result<bool> {
            Ok(false)
        }
        async fn take_held(&self) -> Result<Vec<Notification>> {
            Ok(std::mem::take(&mut *self.held.lock().unwrap()))
        }
        async fn note_wake(&self, id: &str) -> Result<()> {
            self.wakes.lock().unwrap().push(id.to_string());
            Ok(())
        }
        async fn note_spoken(&self, id: &str) -> Result<()> {
            self.spoken.lock().unwrap().push(id.to_string());
            Ok(())
        }
        async fn list_unfetched(&self, _: DateTime<Utc>) -> Result<Vec<UnfetchedNotice>> {
            Ok(self.waiting.lock().unwrap().clone())
        }
    }

    struct Relay(Wake);
    #[async_trait]
    impl NotificationRelay for Relay {
        async fn relay(&self, _: &Notification) -> Result<Wake> {
            Ok(self.0)
        }
    }

    struct Speaker {
        allowed: bool,
        said: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl RoomSpeaker for Speaker {
        async fn say(&self, text: &str) -> Result<bool> {
            if self.allowed {
                self.said.lock().unwrap().push(text.to_string());
            }
            Ok(self.allowed)
        }
    }

    struct Household(Settings);
    #[async_trait]
    impl SettingsRepository for Household {
        async fn get(&self) -> Result<Settings> {
            Ok(self.0.clone())
        }
        async fn update(&self, _: &Settings) -> Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _: &str, _: String) -> Result<()> {
            Ok(())
        }
    }

    fn settings(quiet: bool) -> Arc<dyn SettingsRepository> {
        let mut s = Settings::default();
        s.timezone = "UTC".into();
        // Noon UTC is inside 11:00 to 13:00 and outside 01:00 to 02:00.
        (s.quiet_hours_start, s.quiet_hours_end) = if quiet {
            ("11:00".into(), "13:00".into())
        } else {
            ("01:00".into(), "02:00".into())
        };
        Arc::new(Household(s))
    }

    fn waiting(id: &str, minutes_ago: i64, notification: Notification) -> UnfetchedNotice {
        UnfetchedNotice {
            notification: Notification {
                id: id.into(),
                ..notification
            },
            queued_at: noon() - Duration::minutes(minutes_ago),
            wakes: 1,
            last_wake_at: Some(noon() - Duration::minutes(minutes_ago)),
            spoken: false,
        }
    }

    #[tokio::test]
    async fn an_unfetched_reminder_is_woken_again_and_then_said_once() {
        let queue = Arc::new(Queue::default());
        queue
            .waiting
            .lock()
            .unwrap()
            .push(waiting("r1", 6, reminder("r1", "phone")));
        let speaker = Arc::new(Speaker {
            allowed: true,
            said: Mutex::default(),
        });
        let follow_up = NoticeFollowUp::new(
            queue.clone(),
            Arc::new(MockNotificationSender::new()),
            settings(false),
        )
        .with_relay(Arc::new(Relay(Wake::Sent)))
        .with_speaker(speaker.clone());

        let report = follow_up.pass(noon()).await;
        assert_eq!((report.woken, report.spoken), (1, 1));
        assert_eq!(*queue.wakes.lock().unwrap(), ["r1"]);
        assert_eq!(*queue.spoken.lock().unwrap(), ["r1"]);
        assert_eq!(*speaker.said.lock().unwrap(), ["Reminder: get superglue"]);
    }

    #[tokio::test]
    async fn a_wake_that_did_not_leave_the_pond_is_not_counted() {
        let queue = Arc::new(Queue::default());
        queue
            .waiting
            .lock()
            .unwrap()
            .push(waiting("r1", 3, reminder("r1", "phone")));
        let follow_up = NoticeFollowUp::new(
            queue.clone(),
            Arc::new(MockNotificationSender::new()),
            settings(false),
        )
        .with_relay(Arc::new(Relay(Wake::NotSent(
            crate::mcp::domain::notice_delivery::WakeGap::NoKey,
        ))));

        assert!(follow_up.pass(noon()).await.did_nothing());
        assert!(queue.wakes.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn in_quiet_hours_only_what_passes_is_woken_and_nothing_is_released() {
        let queue = Arc::new(Queue::default());
        let mut plain = reminder("p1", "phone");
        plain.data = None;
        queue.waiting.lock().unwrap().extend([
            waiting("p1", 3, plain.clone()),
            waiting("r1", 3, reminder("r1", "phone")),
        ]);
        queue.held.lock().unwrap().push(plain);
        let sender = Arc::new(MockNotificationSender::new());
        let follow_up = NoticeFollowUp::new(queue.clone(), sender.clone(), settings(true))
            .with_relay(Arc::new(Relay(Wake::Sent)));

        let report = follow_up.pass(noon()).await;
        assert_eq!((report.woken, report.released), (1, 0));
        assert_eq!(*queue.wakes.lock().unwrap(), ["r1"]);
        assert_eq!(queue.held.lock().unwrap().len(), 1);
        assert!(sender.sent().is_empty());
    }

    #[tokio::test]
    async fn when_quiet_hours_end_each_phone_gets_one_notice_for_what_was_held() {
        let queue = Arc::new(Queue::default());
        let mut plain = reminder("x", "phone-a");
        plain.data = None;
        queue.held.lock().unwrap().extend([
            Notification {
                id: "h1".into(),
                title: "Schedule: movies".into(),
                ..plain.clone()
            },
            Notification {
                id: "h2".into(),
                title: "A suggestion".into(),
                ..plain.clone()
            },
            Notification {
                id: "h3".into(),
                target: "phone-b".into(),
                title: "Schedule: movies".into(),
                ..plain
            },
        ]);
        let sender = Arc::new(MockNotificationSender::new());
        let follow_up = NoticeFollowUp::new(queue.clone(), sender.clone(), settings(false));

        assert_eq!(follow_up.pass(noon()).await.released, 3);
        let sent = sender.sent();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].1.target, "phone-a");
        assert_eq!(sent[0].1.title, "2 notices from quiet hours");
        assert_eq!(sent[0].1.body, "Schedule: movies\nA suggestion");
        assert!(passes_quiet_hours(&sent[0].1));
        assert_eq!(sent[1].1.target, "phone-b");
        assert_eq!(follow_up.pass(noon()).await.released, 0, "handed over once");
    }
}
