//! `WhisperKeywordDetector` tests via a canned-transcript mock backend; no model or microphone.

// ── Mock backend ──────────────────────────────────────────────────────────────

use anyhow::Result;
use pond_adapters_whisper::{WhisperBackend, WhisperKeywordDetector};
use pond_core::models::ports::wake_word::WakeWordDetector;
use std::sync::Arc;

/// Minimal `WhisperBackend` for tests — always returns the canned transcript.
struct MockBackend(&'static str);
impl WhisperBackend for MockBackend {
    fn transcribe_pcm_blocking(&self, _samples: &[f32]) -> Result<String> {
        Ok(self.0.to_string())
    }
}

/// A no-hardware `MicHandle`, for tests that only need one to construct, not to capture.
fn test_mic() -> pond_audio::MicHandle {
    let (mic, _join) = pond_audio::spawn(
        Box::new(pond_audio::testing::ScriptedCapture::silence(0, 20)),
        pond_audio::CAPTURE_RATE_HZ,
        5_000,
        true,
    );
    mic
}

#[test]
fn wake_word_detector_prompt_mentions_goose() {
    let detector = WhisperKeywordDetector::new(
        Arc::new(MockBackend("")) as Arc<dyn WhisperBackend>,
        "goose",
        test_mic(),
    );
    assert!(
        detector
            .activation_prompt()
            .to_lowercase()
            .contains("goose"),
        "expected 'goose' in prompt: {}",
        detector.activation_prompt()
    );
}

#[test]
fn wake_word_detector_is_wake_word_detector_trait_object() {
    let _: Arc<dyn WakeWordDetector> = Arc::new(WhisperKeywordDetector::new(
        Arc::new(MockBackend("")) as Arc<dyn WhisperBackend>,
        "goose",
        test_mic(),
    ));
}
