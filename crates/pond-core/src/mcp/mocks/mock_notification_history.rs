//! Test double for [`NotificationHistory`]: keeps every record in memory, in order, and answers
//! reads with [`HistoryAudience::admits`], the rule the SQLite store applies in its query.

use std::collections::HashSet;
use std::sync::Mutex;

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::mcp::domain::notification_history::{HistoryAudience, HistoryEntry};
use crate::mcp::ports::notification::Notification;
use crate::mcp::ports::notification_history::NotificationHistory;

/// An in-memory history; see the module documentation.
#[derive(Default)]
pub struct MockNotificationHistory {
    records: Mutex<Vec<(Notification, HistoryAudience)>>,
    read: Mutex<HashSet<(String, String)>>,
    failing: bool,
}

impl MockNotificationHistory {
    /// An empty history whose writes and reads succeed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Every write and read fails, as a locked database would.
    pub fn failing() -> Self {
        Self {
            failing: true,
            ..Self::default()
        }
    }

    /// Every `(notification, audience)` recorded, oldest first.
    pub fn recorded(&self) -> Vec<(Notification, HistoryAudience)> {
        self.records.lock().unwrap().clone()
    }

    fn refuse(&self) -> Result<()> {
        if self.failing {
            return Err(anyhow!("database is locked"));
        }
        Ok(())
    }
}

#[async_trait]
impl NotificationHistory for MockNotificationHistory {
    async fn record(&self, notification: &Notification, audience: &HistoryAudience) -> Result<()> {
        self.refuse()?;
        let mut records = self.records.lock().unwrap();
        match records.iter_mut().find(|(n, _)| n.id == notification.id) {
            Some((existing, _)) => *existing = notification.clone(),
            None => records.push((notification.clone(), audience.clone())),
        }
        Ok(())
    }

    async fn list_for_device(&self, device_id: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        self.refuse()?;
        let read = self.read.lock().unwrap();
        Ok(self
            .records
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|(_, audience)| audience.admits(device_id))
            .take(limit)
            .map(|(n, _)| HistoryEntry {
                id: n.id.clone(),
                category: n.category.clone(),
                title: n.title.clone(),
                body: n.body.clone(),
                timestamp: n.timestamp.clone(),
                data: n.data.clone(),
                read: read.contains(&(n.id.clone(), device_id.to_string())),
            })
            .collect())
    }

    async fn mark_read(&self, device_id: &str, ids: &[String]) -> Result<u64> {
        self.refuse()?;
        let records = self.records.lock().unwrap();
        let mut read = self.read.lock().unwrap();
        let mut marked = 0;
        for id in ids {
            let visible = records
                .iter()
                .any(|(n, audience)| &n.id == id && audience.admits(device_id));
            if visible && read.insert((id.clone(), device_id.to_string())) {
                marked += 1;
            }
        }
        Ok(marked)
    }
}
