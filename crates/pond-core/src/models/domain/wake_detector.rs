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
    /// Whisper on each segment, and each member's own templates of the phrase on its opening.
    Personal,
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
    /// Members taught the phrase in their own voice: their templates listen beside Whisper.
    Taught,
}

/// The detector for `setting` (`WAKE_DETECTORS`), the household's `phrase`, whether the CNN heard
/// the household's calibration takes, and whether members `taught` the phrase in their own voice.
/// An unknown setting is read as `auto`; `whisper` keeps the taught templates out.
pub fn choose_wake_detector(
    setting: &str,
    phrase: &str,
    cnn_hears_household: bool,
    taught: bool,
) -> (WakeDetector, WakeDetectorReason) {
    let personal = taught && setting != "whisper";
    if pond_voice::text::normalize_transcript(phrase) != CNN_PHRASE {
        return match personal {
            true => (WakeDetector::Personal, WakeDetectorReason::Taught),
            false => (WakeDetector::Whisper, WakeDetectorReason::CustomPhrase),
        };
    }
    match setting {
        "hey_goose" => (WakeDetector::HeyGoose, WakeDetectorReason::Chosen),
        "whisper" => (WakeDetector::Whisper, WakeDetectorReason::Chosen),
        _ if cnn_hears_household => (WakeDetector::HeyGoose, WakeDetectorReason::Calibrated),
        _ if personal => (WakeDetector::Personal, WakeDetectorReason::Taught),
        _ => (WakeDetector::Whisper, WakeDetectorReason::NotCalibrated),
    }
}

/// The detector the room listens with for these settings: no ear when `vad_backend` is `rms`.
pub fn household_wake_detector(
    settings: &crate::user_data::domain::settings::Settings,
    cnn_hears_household: bool,
    taught: bool,
) -> (WakeDetector, WakeDetectorReason) {
    if settings.vad_backend.eq_ignore_ascii_case("rms") {
        return (WakeDetector::Whisper, WakeDetectorReason::NoEar);
    }
    choose_wake_detector(
        &settings.voice_wake_detector,
        &settings.voice_wake_word,
        cnn_hears_household,
        taught,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_trusts_the_cnn_only_once_it_heard_the_household() {
        assert_eq!(
            choose_wake_detector("auto", "hey goose", false, false),
            (WakeDetector::Whisper, WakeDetectorReason::NotCalibrated)
        );
        assert_eq!(
            choose_wake_detector("auto", "Hey, Goose!", true, false),
            (WakeDetector::HeyGoose, WakeDetectorReason::Calibrated)
        );
        assert_eq!(
            choose_wake_detector("something new", "hey goose", false, false).0,
            WakeDetector::Whisper
        );
    }

    #[test]
    fn a_choice_holds_for_hey_goose_but_a_custom_phrase_always_gets_whisper() {
        assert_eq!(
            choose_wake_detector("hey_goose", "hey goose", false, false),
            (WakeDetector::HeyGoose, WakeDetectorReason::Chosen)
        );
        assert_eq!(
            choose_wake_detector("whisper", "hey goose", true, false),
            (WakeDetector::Whisper, WakeDetectorReason::Chosen)
        );
        for setting in ["auto", "hey_goose", "whisper"] {
            assert_eq!(
                choose_wake_detector(setting, "goose", true, false),
                (WakeDetector::Whisper, WakeDetectorReason::CustomPhrase),
                "{setting}"
            );
        }
    }

    #[test]
    fn taught_templates_listen_beside_whisper_unless_whisper_was_chosen() {
        assert_eq!(
            choose_wake_detector("auto", "hey goose", false, true),
            (WakeDetector::Personal, WakeDetectorReason::Taught)
        );
        assert_eq!(
            choose_wake_detector("auto", "hey pond", false, true),
            (WakeDetector::Personal, WakeDetectorReason::Taught),
            "a phrase the CNN never learnt can still be taught"
        );
        assert_eq!(
            choose_wake_detector("whisper", "hey goose", false, true),
            (WakeDetector::Whisper, WakeDetectorReason::Chosen)
        );
        assert_eq!(
            choose_wake_detector("auto", "hey goose", true, true),
            (WakeDetector::HeyGoose, WakeDetectorReason::Calibrated)
        );
    }
}
