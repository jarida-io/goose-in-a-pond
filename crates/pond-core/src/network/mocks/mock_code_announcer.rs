//! Recording [`CodeAnnouncer`] standing in for the speaker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::network::ports::code_announcer::CodeAnnouncer;

#[derive(Default)]
pub struct MockCodeAnnouncer {
    announced: Mutex<Vec<String>>,
    failing: AtomicBool,
    unavailable: AtomicBool,
}

impl MockCodeAnnouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every code read aloud, in order.
    pub fn announced(&self) -> Vec<String> {
        self.announced.lock().unwrap().clone()
    }

    /// Makes every announcement fail, as a pond with no working speaker would.
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    /// Reports no speaker up front, as a pond with no output device does.
    pub fn set_unavailable(&self, unavailable: bool) {
        self.unavailable.store(unavailable, Ordering::SeqCst);
    }
}

#[async_trait]
impl CodeAnnouncer for MockCodeAnnouncer {
    async fn announce_code(&self, digits: &str) -> Result<()> {
        if self.failing.load(Ordering::SeqCst) {
            anyhow::bail!("the mock speaker is failing");
        }
        self.announced.lock().unwrap().push(digits.to_string());
        Ok(())
    }

    async fn is_available(&self) -> bool {
        !self.unavailable.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn it_records_what_it_reads_aloud_and_nothing_when_failing() {
        let announcer = MockCodeAnnouncer::new();
        announcer.announce_code("123456").await.unwrap();
        announcer.announce_code("654321").await.unwrap();
        announcer.set_failing(true);
        assert!(announcer.announce_code("000000").await.is_err());
        assert_eq!(announcer.announced(), vec!["123456", "654321"]);
        assert!(announcer.is_available().await);
        announcer.set_unavailable(true);
        assert!(!announcer.is_available().await);
    }
}
