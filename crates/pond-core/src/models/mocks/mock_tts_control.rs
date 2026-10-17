//! In-memory [`TtsControl`] that records what it was asked to apply and to sample.

use std::sync::Mutex;

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::models::ports::tts_control::{TtsApplied, TtsControl};

pub struct MockTtsControl {
    installed: Vec<String>,
    selected: Mutex<String>,
    samples: Mutex<Vec<(String, String)>>,
}

impl MockTtsControl {
    /// `installed[0]` starts selected.
    pub fn new(installed: &[&str]) -> Self {
        Self {
            installed: installed.iter().map(|v| v.to_string()).collect(),
            selected: Mutex::new(installed.first().map(|v| v.to_string()).unwrap_or_default()),
            samples: Mutex::new(Vec::new()),
        }
    }

    pub fn selected(&self) -> String {
        self.selected.lock().unwrap().clone()
    }

    /// Every `(voice, text)` sampled, oldest first.
    pub fn samples(&self) -> Vec<(String, String)> {
        self.samples.lock().unwrap().clone()
    }

    /// The bytes a sample of `voice` comes back as, so a test can tell whose audio played.
    pub fn wav_for(voice: &str) -> Vec<u8> {
        format!("RIFF-mock-{voice}").into_bytes()
    }
}

#[async_trait]
impl TtsControl for MockTtsControl {
    async fn apply(&self, voice: &str, speed: f32, quality: &str) -> Result<TtsApplied> {
        if !self.installed.iter().any(|v| v == voice) {
            bail!("{voice:?} is not installed");
        }
        *self.selected.lock().unwrap() = voice.to_string();
        Ok(TtsApplied {
            voice: voice.to_string(),
            speed_milli: (speed * 1000.0).round() as u32,
            quality: quality.to_string(),
            ..Default::default()
        })
    }

    async fn installed_voices(&self) -> Vec<String> {
        self.installed.clone()
    }

    async fn synthesize_in_voice(&self, voice: &str, text: &str) -> Result<Vec<u8>> {
        if !self.installed.iter().any(|v| v == voice) {
            bail!("{voice:?} is not installed");
        }
        self.samples
            .lock()
            .unwrap()
            .push((voice.to_string(), text.to_string()));
        Ok(Self::wav_for(voice))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn sampling_records_and_never_changes_the_selection() {
        let control = MockTtsControl::new(&["af_heart", "bm_george"]);
        let wav = control
            .synthesize_in_voice("bm_george", "hello")
            .await
            .unwrap();
        assert_eq!(wav, MockTtsControl::wav_for("bm_george"));
        assert_eq!(control.selected(), "af_heart");
        assert_eq!(
            control.samples(),
            vec![("bm_george".to_string(), "hello".to_string())]
        );
        assert!(control.synthesize_in_voice("nope", "hello").await.is_err());
        control.apply("bm_george", 1.0, "q8").await.unwrap();
        assert_eq!(control.selected(), "bm_george");
    }
}
