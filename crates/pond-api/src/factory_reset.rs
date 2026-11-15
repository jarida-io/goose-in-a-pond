//! What a factory reset quiets in this process while it erases: background model work, the
//! room's voice, and the turns already being answered.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use pond_core::network::services::factory_reset::FactoryResetQuiet;

use crate::runs::TurnInFlight;
use crate::AppState;

/// Longest the reset waits for turns and background work already running to finish.
const SETTLE: Duration = Duration::from_secs(30);
/// The room stays deaf at most this long if the reset never resumes it.
const ROOM_PAUSE: Duration = Duration::from_secs(10 * 60);

pub struct QuietPond {
    state: Arc<AppState>,
    /// Held as a turn would be, so no lane job or embedding pass starts while the reset runs.
    turn: Mutex<Option<TurnInFlight>>,
}

impl QuietPond {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            turn: Mutex::new(None),
        }
    }

    async fn busy(&self) -> bool {
        // Ours is one of them.
        if self.state.runs.turns_in_flight() > 1 {
            return true;
        }
        match &self.state.lane {
            Some(lane) => lane.snapshot().await.slot_busy,
            None => false,
        }
    }
}

#[async_trait]
impl FactoryResetQuiet for QuietPond {
    async fn quiet(&self) {
        self.state.note_user_activity().await;
        if let Some(room) = &self.state.room_voice {
            room.interrupt();
            room.pause(ROOM_PAUSE);
        }
        let turn = self.state.runs.turn_started();
        *self.turn.lock().unwrap_or_else(|e| e.into_inner()) = Some(turn);

        let deadline = tokio::time::Instant::now() + SETTLE;
        while self.busy().await {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    turns = self.state.runs.turns_in_flight().saturating_sub(1),
                    "factory reset: work still running after the wait; erasing beside it"
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    fn resume(&self) {
        self.turn.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(room) = &self.state.room_voice {
            room.resume();
        }
    }
}
