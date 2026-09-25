use anyhow::Result;
use async_trait::async_trait;

// ── StreamingWakeWordDetector (primary interface) ─────────────────────────────

/// Audio caught with the wake word; Listen transcribes it instead of recording afresh.
pub struct WakeWordActivation {
    /// WAV, 16-bit mono 16 kHz; `None` if the detector has no audio hand-off.
    pub captured_audio: Option<Vec<u8>>,
}

/// Primary wake-word port (a blanket impl provides the deprecated `WakeWordDetector`).
#[async_trait]
pub trait StreamingWakeWordDetector: Send + Sync {
    /// Block until the wake word is heard, then return any captured command audio.
    async fn wait_for_activation_with_audio(&self) -> Result<WakeWordActivation>;

    /// Short label shown in the Wait state UI (e.g. `"Say \"Goose\"..."`).
    fn activation_prompt(&self) -> &str {
        "Waiting for activation..."
    }

    /// Whether this detector can interrupt an in-flight turn. `run_loop` races the turn against
    /// activation, so one that resolves immediately must say `false` or it aborts every turn.
    fn supports_interruption(&self) -> bool {
        true
    }
}

// ── WakeWordDetector (deprecated, provided via blanket impl) ──────────────────

/// Wake-word port without audio capture.
#[async_trait]
#[deprecated(
    since = "0.2.0",
    note = "Implement `StreamingWakeWordDetector` instead. \
            `WakeWordDetector` is provided automatically via blanket impl."
)]
pub trait WakeWordDetector: Send + Sync {
    /// Block until the assistant should activate.
    async fn wait_for_activation(&self) -> Result<()>;

    /// Short label shown in the Wait state.
    fn activation_prompt(&self) -> &str {
        "Waiting for activation..."
    }
}

#[async_trait]
#[allow(deprecated)]
impl<T: StreamingWakeWordDetector + 'static> WakeWordDetector for T {
    async fn wait_for_activation(&self) -> Result<()> {
        self.wait_for_activation_with_audio().await.map(|_| ())
    }

    fn activation_prompt(&self) -> &str {
        StreamingWakeWordDetector::activation_prompt(self)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::services::instant_activation::InstantActivation;

    #[test]
    fn wakeword_activation_no_audio_field_is_none() {
        let a = WakeWordActivation {
            captured_audio: None,
        };
        assert!(a.captured_audio.is_none());
    }

    #[test]
    fn wakeword_activation_with_audio_carries_bytes() {
        let wav = vec![b'R', b'I', b'F', b'F'];
        let a = WakeWordActivation {
            captured_audio: Some(wav.clone()),
        };
        assert_eq!(a.captured_audio.unwrap(), wav);
    }

    #[tokio::test]
    async fn instant_activation_satisfies_streaming_interface() {
        let det = InstantActivation;
        let activation = det.wait_for_activation_with_audio().await.unwrap();
        assert!(activation.captured_audio.is_none());
        // Empty by contract: it never waits, so it has nothing to prompt for.
        assert_eq!(StreamingWakeWordDetector::activation_prompt(&det), "");
    }

    #[tokio::test]
    #[allow(deprecated)]
    async fn instant_activation_satisfies_legacy_interface_via_blanket() {
        let det: &dyn WakeWordDetector = &InstantActivation;
        assert!(det.wait_for_activation().await.is_ok());
        // The blanket impl must forward the streaming value verbatim, not its own default.
        assert_eq!(<dyn WakeWordDetector>::activation_prompt(det), "");
    }
}
