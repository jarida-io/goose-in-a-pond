//! IMAP provider presets, like the CalDAV ones; each needs an app-specific password.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImapProvider {
    /// Gmail: needs 2FA, an app password, and IMAP enabled in Gmail's settings.
    Gmail,
    /// iCloud Mail. IMAP is the honest ceiling here, as CalDAV is for calendar.
    ICloud,
    Fastmail,
    /// Anything self-hosted or unlisted.
    Custom {
        host: String,
        port: u16,
    },
}

impl ImapProvider {
    pub fn host(&self) -> &str {
        match self {
            Self::Gmail => "imap.gmail.com",
            Self::ICloud => "imap.mail.me.com",
            Self::Fastmail => "imap.fastmail.com",
            Self::Custom { host, .. } => host,
        }
    }

    /// Implicit TLS on 993; STARTTLS is not offered, as its downgrade would be invisible.
    pub fn port(&self) -> u16 {
        match self {
            Self::Custom { port, .. } => *port,
            _ => 993,
        }
    }

    /// Persisted in `context_sources.provider`; renaming a value orphans connected sources.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::ICloud => "icloud",
            Self::Fastmail => "fastmail",
            Self::Custom { .. } => "custom",
        }
    }

    pub fn setup_hint(&self) -> &'static str {
        match self {
            Self::Gmail => {
                "Gmail needs two-factor turned on, then an app password from your Google \
                 account's security page. IMAP also has to be switched on in Gmail's own \
                 settings, under Forwarding and POP/IMAP."
            }
            Self::ICloud => {
                "iCloud needs an app-specific password, created from the Sign-In and Security \
                 section of your Apple account."
            }
            Self::Fastmail => {
                "Fastmail needs an app password with mail access, created under Settings, \
                 Privacy & Security, Connected apps."
            }
            Self::Custom { .. } => {
                "Use your provider's IMAP server address. This pond only connects over TLS on \
                 port 993."
            }
        }
    }

    /// Rebuild from storage; only `Custom` reads `host_port` (`host:port`), so presets stay put.
    pub fn from_stored(provider: &str, host_port: Option<&str>) -> Option<Self> {
        match provider {
            "gmail" => Some(Self::Gmail),
            "icloud" => Some(Self::ICloud),
            "fastmail" => Some(Self::Fastmail),
            "custom" => {
                let raw = host_port?;
                let (host, port) = match raw.rsplit_once(':') {
                    Some((h, p)) => (h.to_string(), p.parse().ok()?),
                    None => (raw.to_string(), 993),
                };
                (!host.is_empty()).then_some(Self::Custom { host, port })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_implicit_tls_on_993() {
        for p in [
            ImapProvider::Gmail,
            ImapProvider::ICloud,
            ImapProvider::Fastmail,
        ] {
            assert_eq!(p.port(), 993, "{p:?}");
            assert!(!p.host().is_empty());
            assert!(!p.setup_hint().is_empty(), "{p:?}");
        }
    }

    #[test]
    fn a_custom_host_round_trips_with_its_port() {
        let p = ImapProvider::from_stored("custom", Some("mail.example.org:1993")).unwrap();
        assert_eq!(p.host(), "mail.example.org");
        assert_eq!(p.port(), 1993);
    }

    #[test]
    fn a_custom_host_without_a_port_defaults_to_993() {
        let p = ImapProvider::from_stored("custom", Some("mail.example.org")).unwrap();
        assert_eq!(p.port(), 993);
    }

    /// Guessing a host would send credentials somewhere the household never named.
    #[test]
    fn a_custom_provider_without_a_host_is_not_rebuilt() {
        assert_eq!(ImapProvider::from_stored("custom", None), None);
        assert_eq!(ImapProvider::from_stored("custom", Some("")), None);
        assert_eq!(ImapProvider::from_stored("not-a-provider", None), None);
    }
}
