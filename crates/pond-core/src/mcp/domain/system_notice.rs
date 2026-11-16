//! What the Pond tells the household about itself: the system half of the notification
//! catalogue (`docs/architecture/notifications.md`). Each [`SystemEvent`] becomes exactly one
//! [`Notification`], with its title, body, category and route hint decided here and nowhere else.

use std::time::Duration;

use crate::mcp::ports::notification::{Notification, OpenScreen};

/// Which part of a model a download fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPart {
    /// The model's weights.
    Model,
    /// The add-on that lets a model read pictures.
    Pictures,
}

/// The parts of the Pond that did not start. Empty means a clean start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupProblems {
    /// Why speech recognition did not start, when it was configured and did not.
    pub speech_recognition: Option<String>,
    /// Why spoken replies did not start.
    pub speech_output: Option<String>,
    /// Saved, enabled extensions that failed to start, by name.
    pub extensions: Vec<String>,
}

impl StartupProblems {
    /// Whether anything failed to start.
    pub fn is_empty(&self) -> bool {
        self.speech_recognition.is_none()
            && self.speech_output.is_none()
            && self.extensions.is_empty()
    }

    /// Which parts failed, without the reasons: two starts that failed the same parts in different
    /// words are the same news, so a restart loop is announced once.
    pub fn fingerprint(&self) -> String {
        let mut extensions = self.extensions.clone();
        extensions.sort();
        extensions.dedup();
        format!(
            "asr={};tts={};extensions={}",
            self.speech_recognition.is_some(),
            self.speech_output.is_some(),
            extensions.join(",")
        )
    }
}

/// Something in the Pond a household member away from the dashboard would want to know about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemEvent {
    /// A model download finished.
    ModelDownloaded { model: String, part: ModelPart },
    /// A model download stopped with an error. A pause or a cancel is not one.
    ModelDownloadFailed {
        model: String,
        part: ModelPart,
        reason: String,
    },
    /// The Pond started, but some of it did not.
    StartupProblems(StartupProblems),
    /// Free space on the data directory's disk fell below the low-water mark.
    DiskLow { free_bytes: u64, total_bytes: u64 },
    /// Free space is back, after a [`SystemEvent::DiskLow`] was announced.
    DiskRecovered { free_bytes: u64 },
    /// Remote access stopped working after it had worked.
    RemoteAccessDown,
    /// Remote access was signed out and needs the household to sign it in again.
    RemoteAccessNeedsSignIn,
    /// Remote access works again, after an outage was announced.
    RemoteAccessRestored,
    /// The Pond started again after its host lost power. `off_for` is how long it was off, as
    /// the remote-access coordinator timed it; `None` when no coordinator could say.
    PondLostPower { off_for: Option<Duration> },
    /// A paired device was removed from the Pond and lost its access.
    DeviceRemoved { name: String },
    /// A provider refused an account's saved sign-in, so the Pond forgot it.
    SignInExpired { provider: String },
}

impl SystemEvent {
    /// The stable name of this notice in the catalogue, logs and `data.kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ModelDownloaded { .. } => "model_downloaded",
            Self::ModelDownloadFailed { .. } => "model_download_failed",
            Self::StartupProblems(_) => "startup_problems",
            Self::DiskLow { .. } => "disk_low",
            Self::DiskRecovered { .. } => "disk_recovered",
            Self::RemoteAccessDown => "remote_access_down",
            Self::RemoteAccessNeedsSignIn => "remote_access_sign_in",
            Self::RemoteAccessRestored => "remote_access_restored",
            Self::PondLostPower { .. } => "pond_lost_power",
            Self::DeviceRemoved { .. } => "device_removed",
            Self::SignInExpired { .. } => "sign_in_expired",
        }
    }

    /// A security notice reaches the household even when system notices are switched off:
    /// muting "things that need attention" must not hide who can reach the Pond.
    pub fn is_security(&self) -> bool {
        matches!(self, Self::DeviceRemoved { .. })
    }

    /// The notification for this event. `id` and `timestamp` come from the caller so the
    /// wording can be tested on its own.
    pub fn notification(&self, id: String, timestamp: String) -> Notification {
        let (category, title, body, open) = self.wording();
        let mut data = serde_json::Map::new();
        data.insert("kind".into(), self.kind().into());
        let data = match open {
            Some(open) => open.merged_into(data),
            None => serde_json::Value::Object(data),
        };
        Notification {
            id,
            target: "broadcast".to_string(),
            category: category.to_string(),
            title,
            body,
            timestamp,
            data: Some(data),
        }
    }

    fn wording(&self) -> (&'static str, String, String, Option<OpenScreen>) {
        match self {
            Self::ModelDownloaded {
                model,
                part: ModelPart::Model,
            } => (
                "info",
                "Model downloaded".to_string(),
                format!("\"{model}\" finished downloading and is ready to use."),
                Some(OpenScreen::Models),
            ),
            Self::ModelDownloaded {
                model,
                part: ModelPart::Pictures,
            } => (
                "info",
                "Picture support downloaded".to_string(),
                format!("\"{model}\" can now read pictures."),
                Some(OpenScreen::Models),
            ),
            Self::ModelDownloadFailed {
                model,
                part,
                reason,
            } => {
                let what = match part {
                    ModelPart::Model => format!("\"{model}\""),
                    ModelPart::Pictures => format!("Picture support for \"{model}\""),
                };
                (
                    "alert",
                    "Model download failed".to_string(),
                    format!(
                        "{what} did not finish downloading: {reason}. Try again from the Models \
                         screen."
                    ),
                    Some(OpenScreen::Models),
                )
            }
            Self::StartupProblems(problems) => {
                let mut lines = Vec::new();
                if let Some(why) = &problems.speech_recognition {
                    lines.push(format!("Speech recognition did not start: {why}."));
                }
                if let Some(why) = &problems.speech_output {
                    lines.push(format!("Spoken replies did not start: {why}."));
                }
                if !problems.extensions.is_empty() {
                    lines.push(format!(
                        "These extensions did not start: {}.",
                        problems.extensions.join(", ")
                    ));
                }
                let open = if problems.extensions.is_empty() {
                    OpenScreen::Models
                } else {
                    OpenScreen::Extensions
                };
                (
                    "alert",
                    "The Pond started with problems".to_string(),
                    lines.join(" "),
                    Some(open),
                )
            }
            Self::DiskLow {
                free_bytes,
                total_bytes,
            } => (
                "alert",
                "The Pond is running out of space".to_string(),
                format!(
                    "Only {} of {} is free on the Pond's disk. Downloads and recordings fail \
                     when it fills; remove models you no longer use.",
                    human_bytes(*free_bytes),
                    human_bytes(*total_bytes)
                ),
                Some(OpenScreen::Models),
            ),
            Self::DiskRecovered { free_bytes } => (
                "info",
                "The Pond has space again".to_string(),
                format!(
                    "{} is free on the Pond's disk again.",
                    human_bytes(*free_bytes)
                ),
                Some(OpenScreen::Models),
            ),
            Self::RemoteAccessDown => (
                "alert",
                "Remote access stopped".to_string(),
                "This Pond cannot be reached from outside the home. Phones at home still reach \
                 it."
                .to_string(),
                Some(OpenScreen::RemoteAccess),
            ),
            Self::RemoteAccessNeedsSignIn => (
                "action_required",
                "Remote access needs signing in".to_string(),
                "This Pond's remote access was signed out. Sign it in again to reach the Pond \
                 from outside the home."
                    .to_string(),
                Some(OpenScreen::RemoteAccess),
            ),
            Self::RemoteAccessRestored => (
                "info",
                "Remote access is working again".to_string(),
                "This Pond can be reached from outside the home again.".to_string(),
                Some(OpenScreen::RemoteAccess),
            ),
            Self::PondLostPower { off_for } => (
                "info",
                "Your Pond lost power".to_string(),
                match off_for {
                    Some(off_for) => format!(
                        "Your Pond lost power and was off for {}. It is running again.",
                        about(*off_for)
                    ),
                    None => "Your Pond lost power and has started again.".to_string(),
                },
                None,
            ),
            Self::DeviceRemoved { name } => (
                "alert",
                "Device removed".to_string(),
                format!(
                    "\"{name}\" was removed from this Pond and can no longer reach it. If nobody \
                     in your household did this, check the paired devices."
                ),
                Some(OpenScreen::Devices),
            ),
            Self::SignInExpired { provider } => (
                "action_required",
                format!("Sign in to {provider} again"),
                format!(
                    "{provider} refused this Pond's saved sign-in, so the Pond has forgotten it. \
                     Sign in again to keep using {provider}."
                ),
                None,
            ),
        }
    }
}

/// A duration as a person says it roughly: minutes up to an hour and a half, then hours, then
/// days.
fn about(duration: Duration) -> String {
    let minutes = (duration.as_secs() + 30) / 60;
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("about 1 {unit}")
        } else {
            format!("about {n} {unit}s")
        }
    };
    match minutes {
        0 => "less than a minute".to_string(),
        1..=89 => plural(minutes, "minute"),
        _ if minutes < 36 * 60 => plural((minutes + 30) / 60, "hour"),
        _ => plural((minutes + 12 * 60) / (24 * 60), "day"),
    }
}

/// Bytes as a person reads them: one decimal place, in GB at or above a gigabyte, else MB.
fn human_bytes(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const MB: f64 = 1_000_000.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notify(event: &SystemEvent) -> Notification {
        event.notification("n-1".into(), "2026-10-27T00:00:00Z".into())
    }

    #[test]
    fn every_notice_is_a_household_broadcast_that_names_its_kind() {
        let events = [
            SystemEvent::ModelDownloaded {
                model: "qwen".into(),
                part: ModelPart::Model,
            },
            SystemEvent::StartupProblems(StartupProblems {
                extensions: vec!["weather".into()],
                ..Default::default()
            }),
            SystemEvent::RemoteAccessDown,
            SystemEvent::SignInExpired {
                provider: "Spotify".into(),
            },
        ];
        for event in &events {
            let n = notify(event);
            assert_eq!(n.target, "broadcast", "{event:?}");
            assert_eq!(n.data.as_ref().unwrap()["kind"], json!(event.kind()));
            assert!(!n.title.is_empty() && !n.body.is_empty(), "{event:?}");
        }
    }

    #[test]
    fn a_finished_model_opens_the_models_screen_and_a_failure_is_an_alert() {
        let done = notify(&SystemEvent::ModelDownloaded {
            model: "qwen3-4b".into(),
            part: ModelPart::Model,
        });
        assert_eq!(done.category, "info");
        assert_eq!(done.data.unwrap()["open"], json!("models"));
        assert!(done.body.contains("qwen3-4b"));

        let failed = notify(&SystemEvent::ModelDownloadFailed {
            model: "qwen3-4b".into(),
            part: ModelPart::Pictures,
            reason: "the connection dropped".into(),
        });
        assert_eq!(failed.category, "alert");
        assert!(failed.body.starts_with("Picture support for \"qwen3-4b\""));
        assert!(failed.body.contains("the connection dropped"));
    }

    #[test]
    fn startup_problems_list_every_failed_part_and_open_extensions_when_one_failed() {
        let n = notify(&SystemEvent::StartupProblems(StartupProblems {
            speech_recognition: Some("the speech model is missing".into()),
            speech_output: None,
            extensions: vec!["weather".into(), "calendar".into()],
        }));
        assert!(n.body.contains("Speech recognition did not start"));
        assert!(!n.body.contains("Spoken replies"));
        assert!(n.body.contains("weather, calendar"));
        assert_eq!(n.data.unwrap()["open"], json!("extensions"));

        let speech_only = notify(&SystemEvent::StartupProblems(StartupProblems {
            speech_output: Some("the voice engine is not installed".into()),
            ..Default::default()
        }));
        assert_eq!(speech_only.data.unwrap()["open"], json!("models"));
    }

    #[test]
    fn the_fingerprint_ignores_wording_and_order_but_not_which_parts_failed() {
        let a = StartupProblems {
            speech_recognition: Some("one wording".into()),
            speech_output: None,
            extensions: vec!["weather".into(), "calendar".into()],
        };
        let b = StartupProblems {
            speech_recognition: Some("another wording".into()),
            speech_output: None,
            extensions: vec!["calendar".into(), "weather".into()],
        };
        assert_eq!(a.fingerprint(), b.fingerprint());
        let c = StartupProblems {
            extensions: vec!["calendar".into()],
            ..b.clone()
        };
        assert_ne!(b.fingerprint(), c.fingerprint());
        assert!(StartupProblems::default().is_empty());
        assert!(!a.is_empty());
    }

    #[test]
    fn disk_sizes_read_as_a_person_would_say_them() {
        let n = notify(&SystemEvent::DiskLow {
            free_bytes: 1_500_000_000,
            total_bytes: 64_000_000_000,
        });
        assert!(n.body.contains("1.5 GB of 64.0 GB"), "{}", n.body);
        assert_eq!(human_bytes(250_000_000), "250 MB");
    }

    #[test]
    fn only_a_removed_device_is_a_security_notice() {
        assert!(SystemEvent::DeviceRemoved { name: "x".into() }.is_security());
        assert!(!SystemEvent::RemoteAccessDown.is_security());
        let n = notify(&SystemEvent::DeviceRemoved {
            name: "Liz's phone".into(),
        });
        assert_eq!(n.data.unwrap()["open"], json!("devices"));
    }

    #[test]
    fn a_power_loss_says_how_long_the_pond_was_off_when_that_is_known() {
        let timed = notify(&SystemEvent::PondLostPower {
            off_for: Some(Duration::from_secs(92 * 60)),
        });
        assert_eq!(timed.title, "Your Pond lost power");
        assert_eq!(
            timed.body,
            "Your Pond lost power and was off for about 2 hours. It is running again."
        );
        assert_eq!(timed.category, "info");
        assert_eq!(timed.data.unwrap()["kind"], json!("pond_lost_power"));
        let untimed = notify(&SystemEvent::PondLostPower { off_for: None });
        assert_eq!(untimed.body, "Your Pond lost power and has started again.");
        assert!(!SystemEvent::PondLostPower { off_for: None }.is_security());
    }

    #[test]
    fn durations_read_as_a_person_would_say_them() {
        let said = |secs: u64| about(Duration::from_secs(secs));
        assert_eq!(said(20), "less than a minute");
        assert_eq!(said(60), "about 1 minute");
        assert_eq!(said(72 * 60), "about 72 minutes");
        assert_eq!(said(90 * 60), "about 2 hours");
        assert_eq!(said(60 * 60 + 89 * 60), "about 2 hours");
        assert_eq!(said(3 * 24 * 3600), "about 3 days");
    }

    #[test]
    fn a_sign_in_notice_carries_no_route_hint() {
        let n = notify(&SystemEvent::SignInExpired {
            provider: "Spotify".into(),
        });
        assert_eq!(n.category, "action_required");
        assert_eq!(n.data.unwrap().get("open"), None);
    }
}
