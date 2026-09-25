//! Wake-word detector for keyboard / stdin mode; the default in `ChatService` and all tests.

use crate::models::ports::wake_word::{StreamingWakeWordDetector, WakeWordActivation};
use anyhow::Result;
use async_trait::async_trait;

/// No-op wake-word detector: activates instantly, without waiting.
pub struct InstantActivation;

#[async_trait]
impl StreamingWakeWordDetector for InstantActivation {
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation> {
        Ok(WakeWordActivation {
            captured_audio: None,
        })
    }

    /// Empty, which `run_loop` skips; a `--no-wake-word` mic session already prompts "listening".
    fn activation_prompt(&self) -> &str {
        ""
    }

    /// False: resolving instantly, it would win `run_loop`'s interrupt race and abort every turn.
    fn supports_interruption(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::ports::wake_word::WakeWordDetector;

    #[tokio::test]
    async fn instant_activation_returns_ok_immediately() {
        let detector = InstantActivation;
        let activation = detector.wait_for_activation_with_audio().await.unwrap();
        assert!(activation.captured_audio.is_none());
    }

    #[test]
    fn instant_activation_does_not_support_interruption() {
        let detector = InstantActivation;
        assert!(!detector.supports_interruption());
    }

    #[tokio::test]
    async fn instant_activation_announces_nothing_because_it_never_waits() {
        let detector = InstantActivation;
        // Fully qualified: the deprecated `WakeWordDetector` blanket impl makes the call ambiguous.
        assert_eq!(
            StreamingWakeWordDetector::activation_prompt(&detector),
            "",
            "a detector that returns instantly must not prompt for anything"
        );
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn instant_activation_satisfies_wake_word_detector_via_blanket() {
        let detector = InstantActivation;
        let det: &dyn WakeWordDetector = &detector;
        assert!(det.wait_for_activation().await.is_ok());
    }
}
