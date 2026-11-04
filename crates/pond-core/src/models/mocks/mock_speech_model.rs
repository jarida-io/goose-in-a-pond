use std::sync::Mutex;

use async_trait::async_trait;

use crate::models::ports::speech_model::{SpeechModelSwitch, SpeechSwitch};

/// Remembers every model it was asked to switch to and switches to each.
#[derive(Default)]
pub struct MockSpeechModelSwitch {
    pub asked: Mutex<Vec<String>>,
}

#[async_trait]
impl SpeechModelSwitch for MockSpeechModelSwitch {
    async fn switch_to(&self, model: &str) -> SpeechSwitch {
        self.asked
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(model.to_string());
        SpeechSwitch::Switched {
            model: model.to_string(),
            fetched: false,
        }
    }
}
