//! Runs members' web watches: every few minutes the due ones are checked, in code, and once a day
//! at the digest hour, household time, each owner gets one summary of the changes that did not
//! match. No check takes the model.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::web_page::{read_watched, FetchFailure};
use async_trait::async_trait;
use chrono::{Timelike, Utc};
use pond_core::mcp::ports::notification::{
    MemberDelivery, MemberNotifier, Notification, OpenScreen,
};
use pond_core::shared::domain::todo::HookEvent;
use pond_core::shared::domain::web_watch::{WatchItem, DIGEST_HOUR};
use pond_core::shared::ports::todo::HookEventQueue;
use pond_core::shared::ports::web_watch::{
    NoticeKind, WatchNotice, WatchNotifier, WatchPageReader, WatchReadError,
};
use pond_core::shared::services::spoken_watch_alerts::SpokenWatchAlerts;
use pond_core::shared::services::web_watch_checker::WebWatchChecker;
use pond_core::user_data::ports::settings::SettingsRepository;

/// How often due watches are looked for. The interval a member sets is hours; this only bounds
/// how late a check runs.
const PASS_EVERY: Duration = Duration::from_secs(5 * 60);
/// The first pass waits for startup's own work to settle.
const FIRST_PASS_AFTER: Duration = Duration::from_secs(90);

/// Where a match is also queued for the member's to-do hooks; unset, a match is only told.
static HOOK_EVENTS: OnceLock<Arc<dyn HookEventQueue>> = OnceLock::new();

/// Install the hook queue once, at startup; later calls are ignored.
pub fn install_hook_events(queue: Arc<dyn HookEventQueue>) {
    let _ = HOOK_EVENTS.set(queue);
}

/// Reads a watched page through the page tool's own reader.
pub struct PageReader;

#[async_trait]
impl WatchPageReader for PageReader {
    async fn read(&self, address: &str) -> Result<Vec<WatchItem>, WatchReadError> {
        match read_watched(address).await {
            Ok(page) => Ok(page
                .items
                .into_iter()
                .map(crate::web_watch::watch_item)
                .collect()),
            Err(FetchFailure::Status(401 | 403)) => Err(WatchReadError::Refused),
            Err(FetchFailure::Status(429)) => Err(WatchReadError::RateLimited),
            Err(FetchFailure::Status(404 | 410)) => Err(WatchReadError::Gone),
            Err(other) => Err(WatchReadError::Other(format!("{other:?}"))),
        }
    }
}

/// Tells a watch's owner on their own devices, never the household, and says a match aloud to
/// an owner who asked for that.
pub struct MemberWatchNotifier {
    members: Arc<dyn MemberNotifier>,
    speech: Option<Arc<SpokenWatchAlerts>>,
}

impl MemberWatchNotifier {
    pub fn new(members: Arc<dyn MemberNotifier>) -> Self {
        Self {
            members,
            speech: None,
        }
    }

    /// Says matches aloud through `speech`; `None` on a pond with no voice.
    pub fn with_speech(mut self, speech: Option<Arc<SpokenWatchAlerts>>) -> Self {
        self.speech = speech;
        self
    }
}

#[async_trait]
impl WatchNotifier for MemberWatchNotifier {
    async fn notify(&self, notice: WatchNotice) -> anyhow::Result<()> {
        let now = Utc::now();
        // Spawned: speech waits its turn for the speaker, and the checks must not wait with it.
        if let (NoticeKind::Match, Some(speech)) = (notice.kind, &self.speech) {
            let (speech, owner) = (speech.clone(), notice.owner.clone());
            let (title, body) = (notice.title.clone(), notice.body.clone());
            tokio::spawn(async move {
                speech.speak(&owner, &title, &body, now).await;
            });
        }
        if let (NoticeKind::Match, Some(watch_id), Some(queue)) =
            (notice.kind, notice.watch_id.as_deref(), HOOK_EVENTS.get())
        {
            let event = HookEvent::new(
                Some(&notice.owner),
                "web_watch",
                watch_id,
                "match",
                serde_json::json!({"title": notice.title}),
                now,
            );
            if let Err(e) = queue.raise(&event).await {
                tracing::warn!(target: "giap::trace", kind = "web_watch_hook_event_failed", error = %e);
            }
        }
        pond_core::shared::services::goose_thread::post(
            Some(&notice.owner),
            &format!("{}\n{}", notice.title, notice.body),
            pond_core::user_data::domain::message_note::MessageNote::from_source(
                pond_core::user_data::domain::message_note::SourceKind::Watch,
                notice.title.clone(),
                notice.watch_id.clone(),
            ),
        )
        .await;
        let notification = Notification {
            id: format!(
                "web-watch-{}-{}",
                notice.watch_id.as_deref().unwrap_or("digest"),
                now.timestamp_millis()
            ),
            target: String::new(),
            category: "info".to_string(),
            title: notice.title,
            body: notice.body,
            timestamp: now.to_rfc3339(),
            data: Some(
                OpenScreen::Watch {
                    watch_id: notice.watch_id,
                }
                .data(),
            ),
        };
        let delivery = self
            .members
            .notify_member(&notice.owner, notification)
            .await;
        let outcome = match &delivery {
            MemberDelivery::Reached(devices) => format!("queued for {}", devices.len()),
            MemberDelivery::NoPhone => "no_phone".to_string(),
            MemberDelivery::Failed(_) => "failed".to_string(),
        };
        tracing::info!(target: "giap::trace", kind = "web_watch_notice", outcome = %outcome);
        Ok(())
    }
}

/// Starts the watch loop.
pub fn spawn(
    checker: Arc<WebWatchChecker>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(FIRST_PASS_AFTER).await;
        let mut clock = tokio::time::interval(PASS_EVERY);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut digested_on = None;
        loop {
            clock.tick().await;
            let now = Utc::now();
            if let Err(e) = checker.check_due(now).await {
                tracing::warn!(target: "giap::trace", kind = "web_watch_pass_failed", error = %e);
            }
            let timezone = settings
                .get()
                .await
                .ok()
                .and_then(|s| s.timezone.parse::<chrono_tz::Tz>().ok())
                .unwrap_or(chrono_tz::UTC);
            let local = now.with_timezone(&timezone);
            let today = local.date_naive();
            if local.hour() >= DIGEST_HOUR && digested_on != Some(today) {
                digested_on = Some(today);
                match checker.send_digests(now).await {
                    Ok(sent) => {
                        tracing::info!(target: "giap::trace", kind = "web_watch_digests", sent)
                    }
                    Err(e) => {
                        tracing::warn!(target: "giap::trace", kind = "web_watch_digest_failed", error = %e)
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use pond_core::shared::domain::todo::EventKey;
    use pond_core::shared::mocks::mock_todo::MockTodoRepository;

    fn notice(kind: NoticeKind) -> WatchNotice {
        WatchNotice {
            owner: "jerry".into(),
            kind,
            title: "A match on Jiji".into(),
            body: "Toyota Fielder 2016, KSh 1,150,000".into(),
            watch_id: Some("w1".into()),
        }
    }

    #[tokio::test]
    async fn a_match_is_queued_for_the_owners_todo_hooks_and_trouble_is_not() {
        let queue = Arc::new(MockTodoRepository::new());
        install_hook_events(queue.clone());
        let notifier = MemberWatchNotifier::new(Arc::new(
            MockMemberNotifier::new().with_devices("jerry", &["p"]),
        ));
        notifier.notify(notice(NoticeKind::Trouble)).await.unwrap();
        notifier.notify(notice(NoticeKind::Match)).await.unwrap();
        let raised = queue.events();
        assert_eq!(raised.len(), 1);
        assert_eq!(raised[0].0.key(), EventKey::new("web_watch", "w1", "match"));
        assert_eq!(raised[0].0.profile_id.as_deref(), Some("jerry"));
    }
}
