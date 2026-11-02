//! An interrupt watch a test fires by hand.

use std::sync::Mutex;

use async_trait::async_trait;
use tokio::sync::watch;

use crate::models::ports::interrupt::{Interrupt, InterruptWatch};

/// Each [`fire`](Self::fire) reaches every watch waiting at that moment, like the ear's.
pub struct MockInterruptWatch {
    fired: watch::Sender<(u64, Option<Interrupt>)>,
    /// How many `wait_for_interrupt` calls are waiting right now.
    waiting: Mutex<usize>,
}

impl Default for MockInterruptWatch {
    fn default() -> Self {
        Self {
            fired: watch::channel((0, None)).0,
            waiting: Mutex::new(0),
        }
    }
}

impl MockInterruptWatch {
    pub fn fire(&self, interrupt: Interrupt) {
        self.fired.send_modify(|(n, last)| {
            *n += 1;
            *last = Some(interrupt);
        });
    }

    pub fn waiting(&self) -> usize {
        *self.waiting.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct Waiting<'a>(&'a Mutex<usize>);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
    }
}

#[async_trait]
impl InterruptWatch for MockInterruptWatch {
    async fn wait_for_interrupt(&self) -> Interrupt {
        let mut rx = self.fired.subscribe();
        let start = rx.borrow_and_update().0;
        *self.waiting.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        let _waiting = Waiting(&self.waiting);
        loop {
            if rx.changed().await.is_err() {
                return std::future::pending().await;
            }
            if let (n, Some(interrupt)) = *rx.borrow_and_update() {
                if n > start {
                    return interrupt;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ports::interrupt::InterruptReason;
    use std::sync::Arc;

    #[tokio::test]
    async fn a_fire_reaches_the_watch_waiting_and_not_a_later_one() {
        let watch = Arc::new(MockInterruptWatch::default());
        let waiting = {
            let watch = watch.clone();
            tokio::spawn(async move { watch.wait_for_interrupt().await })
        };
        while watch.waiting() == 0 {
            tokio::task::yield_now().await;
        }
        watch.fire(Interrupt {
            onset: std::time::Instant::now(),
            why: InterruptReason::WakeWord,
        });
        assert_eq!(waiting.await.unwrap().why, InterruptReason::WakeWord);
        assert_eq!(watch.waiting(), 0);
        let late = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            watch.wait_for_interrupt(),
        )
        .await;
        assert!(
            late.is_err(),
            "an earlier interrupt must not end a later wait"
        );
    }
}
