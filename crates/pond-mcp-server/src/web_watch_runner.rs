//! Runs members' web watches: every few minutes the due ones are checked, in code, and once a day
//! at the digest hour, household time, each owner gets one summary of the changes that did not
//! match. No check takes the model.

use std::sync::Arc;
use std::time::Duration;

use crate::web_page::{read_watched, FetchFailure};
use async_trait::async_trait;
use chrono::{Timelike, Utc};
use pond_core::mcp::ports::notification::{
    MemberDelivery, MemberNotifier, Notification, OpenScreen,
};
use pond_core::shared::domain::web_watch::{WatchItem, DIGEST_HOUR};
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
            MemberDelivery::Reached(devices) => format!("reached {}", devices.len()),
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
