//! Test double for [`NotificationRecipients`]: answers the phones it is given, or fails.

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::mcp::ports::notification_recipients::NotificationRecipients;

pub struct MockNotificationRecipients {
    phones: Option<Vec<String>>,
}

impl MockNotificationRecipients {
    /// These phones are paired.
    pub fn with_phones(phones: &[&str]) -> Self {
        Self {
            phones: Some(phones.iter().map(|p| p.to_string()).collect()),
        }
    }

    /// Every read fails, as a locked database would.
    pub fn failing() -> Self {
        Self { phones: None }
    }
}

#[async_trait]
impl NotificationRecipients for MockNotificationRecipients {
    async fn paired_phones(&self) -> Result<Vec<String>> {
        self.phones
            .clone()
            .ok_or_else(|| anyhow!("database is locked"))
    }
}
