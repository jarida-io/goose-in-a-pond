//! CalDAV provider presets, so a household picks a name instead of finding a URL. Each needs
//! an app-specific password, not the account password.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalDavProvider {
    /// Google Calendar. Not connectable: its CalDAV requires OAuth 2.0 and rejects Basic auth.
    /// Kept so stored rows can still be read and disconnected.
    Google,
    /// iCloud (CalDAV only; there is no general iCloud API).
    ICloud,
    Fastmail,
    /// Nextcloud or another self-hosted server at the household's own URL.
    Nextcloud {
        base_url: String,
    },
    /// A server named by URL, for everything this list does not cover.
    Custom {
        base_url: String,
    },
}

impl CalDavProvider {
    /// RFC 6764 discovery entry point (not a calendar path: a household may have several).
    pub fn discovery_url(&self) -> String {
        match self {
            Self::Google => "https://apidata.googleusercontent.com/caldav/v2/".to_string(),
            Self::ICloud => "https://caldav.icloud.com/".to_string(),
            Self::Fastmail => "https://caldav.fastmail.com/dav/".to_string(),
            Self::Nextcloud { base_url } | Self::Custom { base_url } => {
                base_url.trim_end_matches('/').to_string()
            }
        }
    }

    /// Persisted in `context_sources.provider`; renaming a value orphans connected sources.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::ICloud => "icloud",
            Self::Fastmail => "fastmail",
            Self::Nextcloud { .. } => "nextcloud",
            Self::Custom { .. } => "custom",
        }
    }

    /// Setup steps the household must do first, shown next to the password box.
    pub fn setup_hint(&self) -> &'static str {
        match self {
            Self::Google => {
                "Google Calendar needs you to sign in with Google, which this pond cannot do \
                 yet. An app password will not work for it — Google refuses those for \
                 calendars. Gmail still works under Mail."
            }
            Self::ICloud => {
                "iCloud needs an app-specific password, created from the Sign-In and Security \
                 section of your Apple account."
            }
            Self::Fastmail => {
                "Fastmail needs an app password with calendar access, created under \
                 Settings, Privacy & Security, Connected apps."
            }
            Self::Nextcloud { .. } => {
                "Nextcloud needs a device password from Settings, Security. The server address \
                 is the one you use in a browser."
            }
            Self::Custom { .. } => {
                "Use the CalDAV address your provider documents, with an app password if the \
                 account has two-factor turned on."
            }
        }
    }

    /// Whether a NEW source may be connected; existing ones must still load to be disconnected.
    pub fn is_connectable(&self) -> bool {
        !matches!(self, Self::Google)
    }

    /// Rebuild from storage; `base_url` can't redirect a preset, only Nextcloud/Custom read it.
    pub fn from_stored(provider: &str, base_url: Option<&str>) -> Option<Self> {
        match provider {
            "google" => Some(Self::Google),
            "icloud" => Some(Self::ICloud),
            "fastmail" => Some(Self::Fastmail),
            "nextcloud" => base_url.map(|b| Self::Nextcloud {
                base_url: b.to_string(),
            }),
            "custom" => base_url.map(|b| Self::Custom {
                base_url: b.to_string(),
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_has_a_discovery_url_and_a_setup_hint() {
        for p in [
            CalDavProvider::Google,
            CalDavProvider::ICloud,
            CalDavProvider::Fastmail,
            CalDavProvider::Nextcloud {
                base_url: "https://cloud.example.org/remote.php/dav".into(),
            },
            CalDavProvider::Custom {
                base_url: "https://dav.example.org".into(),
            },
        ] {
            assert!(p.discovery_url().starts_with("https://"), "{p:?}");
            assert!(!p.setup_hint().is_empty(), "{p:?}");
            assert!(!p.as_str().is_empty(), "{p:?}");
        }
    }

    #[test]
    fn a_self_hosted_base_url_loses_its_trailing_slash_exactly_once() {
        let p = CalDavProvider::Nextcloud {
            base_url: "https://cloud.example.org/dav/".into(),
        };
        assert_eq!(p.discovery_url(), "https://cloud.example.org/dav");
    }

    #[test]
    fn stored_providers_round_trip() {
        for p in [
            CalDavProvider::Google,
            CalDavProvider::ICloud,
            CalDavProvider::Fastmail,
            CalDavProvider::Nextcloud {
                base_url: "https://cloud.example.org".into(),
            },
        ] {
            let base = match &p {
                CalDavProvider::Nextcloud { base_url } | CalDavProvider::Custom { base_url } => {
                    Some(base_url.clone())
                }
                _ => None,
            };
            assert_eq!(
                CalDavProvider::from_stored(p.as_str(), base.as_deref()),
                Some(p)
            );
        }
    }

    /// Guessing a host would send credentials somewhere the household never named.
    #[test]
    fn a_self_hosted_provider_without_its_url_is_not_rebuilt() {
        assert_eq!(CalDavProvider::from_stored("nextcloud", None), None);
        assert_eq!(CalDavProvider::from_stored("custom", None), None);
        assert_eq!(CalDavProvider::from_stored("not-a-provider", None), None);
    }
}

#[cfg(test)]
mod connectability_tests {
    use super::*;

    #[test]
    fn google_calendar_is_not_connectable_with_a_password() {
        assert!(!CalDavProvider::Google.is_connectable());
        assert!(
            CalDavProvider::Google
                .setup_hint()
                .contains("sign in with Google"),
            "the hint must say WHY, or it reads as a bug in the pond"
        );
    }

    #[test]
    fn every_other_preset_is_connectable() {
        for p in [
            CalDavProvider::ICloud,
            CalDavProvider::Fastmail,
            CalDavProvider::Nextcloud {
                base_url: "https://x".into(),
            },
            CalDavProvider::Custom {
                base_url: "https://x".into(),
            },
        ] {
            assert!(p.is_connectable(), "{p:?}");
        }
    }

    #[test]
    fn a_stored_google_source_can_still_be_rebuilt() {
        assert_eq!(
            CalDavProvider::from_stored("google", None),
            Some(CalDavProvider::Google)
        );
    }
}
