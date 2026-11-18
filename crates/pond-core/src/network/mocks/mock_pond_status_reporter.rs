//! Recording [`PondStatusReporter`] standing in for the coordinator.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::network::domain::pond_status::{CoordinatorView, StatusReport};
use crate::network::ports::pond_status_reporter::PondStatusReporter;

pub struct MockPondStatusReporter {
    sent: Mutex<Vec<StatusReport>>,
    answer: Mutex<CoordinatorView>,
    failing: AtomicBool,
}

impl Default for MockPondStatusReporter {
    fn default() -> Self {
        Self {
            sent: Mutex::new(Vec::new()),
            answer: Mutex::new(CoordinatorView {
                state: "online".into(),
                last_outage: None,
            }),
            failing: AtomicBool::new(false),
        }
    }
}

impl MockPondStatusReporter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every report that arrived, in order. A failed attempt is not one.
    pub fn sent(&self) -> Vec<StatusReport> {
        self.sent.lock().unwrap().clone()
    }

    /// Makes every report fail, as an unreachable coordinator would.
    pub fn set_failing(&self, failing: bool) {
        self.failing.store(failing, Ordering::SeqCst);
    }

    /// What the coordinator answers from now on.
    pub fn set_answer(&self, answer: CoordinatorView) {
        *self.answer.lock().unwrap() = answer;
    }
}

#[async_trait]
impl PondStatusReporter for MockPondStatusReporter {
    async fn report(&self, report: &StatusReport) -> Result<CoordinatorView> {
        if self.failing.load(Ordering::SeqCst) {
            anyhow::bail!("the mock coordinator is unreachable");
        }
        self.sent.lock().unwrap().push(report.clone());
        Ok(self.answer.lock().unwrap().clone())
    }
}
