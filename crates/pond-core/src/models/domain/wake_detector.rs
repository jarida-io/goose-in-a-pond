//! Which detector hears the wake word when the ear runs: the "hey goose" CNN only where it hears
//! this household, Whisper on the ear's speech segments everywhere else.

use serde::{Deserialize, Serialize};

/// The one phrase the ear's CNN was trained on.
pub const CNN_PHRASE: &str = "hey goose";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeDetector {
    /// The ear's wake CNN, every 80 ms.
    HeyGoose,
    /// Whisper on each speech segment the ear endpoints, never on a timer.
    Whisper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeDetectorReason {
    /// The household picked it (`voice_wake_detector`).
    Chosen,
    /// The CNN heard the household's own calibration takes.
    Calibrated,
    /// Nobody calibrated, or the CNN missed their takes: it was trained on synthetic voices only.
    NotCalibrated,
    /// The phrase is not "hey goose", the only one the CNN knows.
    CustomPhrase,
    /// The ear is not running (`vad_backend` is `rms`): Whisper listens on a timer.
    NoEar,
}

/// The detector for `setting` (`WAKE_DETECTORS`), the household's `phrase`, and whether the CNN
/// heard the household's calibration takes. An unknown setting is read as `auto`.
pub fn choose_wake_detector(
    setting: &str,
    phrase: &str,
    cnn_hears_household: bool,
) -> (WakeDetector, WakeDetectorReason) {
    if pond_voice::text::normalize_transcript(phrase) != CNN_PHRASE {
        return (WakeDetector::Whisper, WakeDetectorReason::CustomPhrase);
    }
    match setting {
        "hey_goose" => (WakeDetector::HeyGoose, WakeDetectorReason::Chosen),
        "whisper" => (WakeDetector::Whisper, WakeDetectorReason::Chosen),
        _ if cnn_hears_household => (WakeDetector::HeyGoose, WakeDetectorReason::Calibrated),
        _ => (WakeDetector::Whisper, WakeDetectorReason::NotCalibrated),
    }
}

/// The detector the room listens with for these settings: no ear when `vad_backend` is `rms`.
pub fn household_wake_detector(
    settings: &crate::user_data::domain::settings::Settings,
    cnn_hears_household: bool,
) -> (WakeDetector, WakeDetectorReason) {
    if settings.vad_backend.eq_ignore_ascii_case("rms") {
        return (WakeDetector::Whisper, WakeDetectorReason::NoEar);
    }
    choose_wake_detector(
        &settings.voice_wake_detector,
        &settings.voice_wake_word,
        cnn_hears_household,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_trusts_the_cnn_only_once_it_heard_the_household() {
        assert_eq!(
            choose_wake_detector("auto", "hey goose", false),
            (WakeDetector::Whisper, WakeDetectorReason::NotCalibrated)
        );
        assert_eq!(
            choose_wake_detector("auto", "Hey, Goose!", true),
            (WakeDetector::HeyGoose, WakeDetectorReason::Calibrated)
        );
        assert_eq!(
            choose_wake_detector("something new", "hey goose", false).0,
            WakeDetector::Whisper
        );
    }

    #[test]
    fn a_choice_holds_for_hey_goose_but_a_custom_phrase_always_gets_whisper() {
        assert_eq!(
            choose_wake_detector("hey_goose", "hey goose", false),
            (WakeDetector::HeyGoose, WakeDetectorReason::Chosen)
        );
        assert_eq!(
            choose_wake_detector("whisper", "hey goose", true),
            (WakeDetector::Whisper, WakeDetectorReason::Chosen)
        );
        for setting in ["auto", "hey_goose", "whisper"] {
            assert_eq!(
                choose_wake_detector(setting, "goose", true),
                (WakeDetector::Whisper, WakeDetectorReason::CustomPhrase),
                "{setting}"
            );
        }
    }
}
