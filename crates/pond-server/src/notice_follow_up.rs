//! Runs the follow-up of notices no phone fetched once a minute, and says a due reminder in the
//! room on the pond's own speaker.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use pond_core::mcp::ports::room_speaker::RoomSpeaker;
use pond_core::mcp::services::notice_follow_up::NoticeFollowUp;
use pond_core::shared::services::chat::household_quiet_at;
use pond_core::user_data::ports::settings::SettingsRepository;

const EVERY: Duration = Duration::from_secs(60);

/// Speaks through the audio bus, so it queues behind a turn like every other sound.
pub struct AnnouncerSpeaker {
    announcer: pond_audio_out::Announcer,
    settings: Arc<dyn SettingsRepository>,
}

impl AnnouncerSpeaker {
    pub fn new(
        announcer: pond_audio_out::Announcer,
        settings: Arc<dyn SettingsRepository>,
    ) -> Self {
        Self {
            announcer,
            settings,
        }
    }
}

#[async_trait]
impl RoomSpeaker for AnnouncerSpeaker {
    async fn say(&self, text: &str) -> Result<bool> {
        let settings = self.settings.get().await?;
        // Speech reads an unreadable bound as quiet, unlike a held notice.
        if household_quiet_at(&settings, chrono::Utc::now()) != Some(false) {
            return Ok(false);
        }
        self.announcer.speak_text(text).await?;
        Ok(true)
    }
}

pub fn spawn(follow_up: NoticeFollowUp) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            follow_up.pass(chrono::Utc::now()).await;
        }
    });
}
