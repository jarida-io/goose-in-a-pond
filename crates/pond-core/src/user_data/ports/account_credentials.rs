//! How a connector gets permission to read an account, whichever provider holds it.
//! Callers may branch on the [`AccountAuth`] shape, never on which provider answered.

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Proof a connector may read an account.
#[derive(Clone, PartialEq, Eq)]
pub enum AccountAuth {
    /// Password or app-specific password: IMAP `LOGIN`, CalDAV HTTP Basic.
    Password { username: String, password: String },
    /// OAuth 2.0 token the pond neither mints nor refreshes: IMAP SASL `XOAUTH2`, CalDAV Bearer.
    Bearer {
        username: String,
        token: String,
        /// Provider-stated expiry; `None` means unstated, not "never expires".
        expires_at: Option<DateTime<Utc>>,
    },
}

// Hand-written so a token or password cannot reach a log through `{:?}`.
impl std::fmt::Debug for AccountAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Password { username, .. } => f
                .debug_struct("AccountAuth::Password")
                .field("username", username)
                .field("password", &"<redacted>")
                .finish(),
            Self::Bearer {
                username,
                expires_at,
                ..
            } => f
                .debug_struct("AccountAuth::Bearer")
                .field("username", username)
                .field("token", &"<redacted>")
                .field("expires_at", expires_at)
                .finish(),
        }
    }
}

impl AccountAuth {
    /// The account this proof is for; IMAP `XOAUTH2` needs it alongside a token too.
    pub fn username(&self) -> &str {
        match self {
            Self::Password { username, .. } | Self::Bearer { username, .. } => username,
        }
    }

    /// Whether this is known to have aged out at `now`. An unknown expiry is not one:
    /// treating it as expired would re-authenticate on every sync.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        match self {
            Self::Password { .. } => false,
            Self::Bearer { expires_at, .. } => expires_at.is_some_and(|at| at <= now),
        }
    }
}

/// Where a source's credentials come from, parsed from its `secret_ref`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSource<'a> {
    /// The encrypted secret store, under this key.
    SecretStore(&'a str),
    /// A desktop account manager's account, e.g. `desktop:goa:<account-id>`.
    DesktopAccount { provider: &'a str, id: &'a str },
}

/// The prefix marking a ref that a desktop account manager owns.
const DESKTOP_PREFIX: &str = "desktop:";

impl<'a> CredentialSource<'a> {
    /// Parse a stored `secret_ref`; anything unrecognised is a legacy [`Self::SecretStore`] key.
    pub fn parse(secret_ref: &'a str) -> Self {
        match secret_ref.strip_prefix(DESKTOP_PREFIX) {
            Some(rest) => match rest.split_once(':') {
                Some((provider, id)) if !provider.is_empty() && !id.is_empty() => {
                    Self::DesktopAccount { provider, id }
                }
                // Malformed: the store key is the whole ref, never a stripped fragment
                // that could name another account's secret.
                _ => Self::SecretStore(secret_ref),
            },
            None => Self::SecretStore(secret_ref),
        }
    }

    /// The `secret_ref` to store for a desktop-managed account.
    pub fn desktop_ref(provider: &str, id: &str) -> String {
        format!("{DESKTOP_PREFIX}{provider}:{id}")
    }
}

/// Resolves a source's `secret_ref` into something an adapter can use.
#[async_trait]
pub trait AccountCredentialProvider: Send + Sync {
    /// Whether this provider owns `secret_ref`.
    fn owns(&self, secret_ref: &str) -> bool;

    /// The proof for `secret_ref`, refreshed if that is this provider's job. `Ok(None)` is
    /// "not mine or not present"; `Err` is "mine, and something is wrong".
    async fn auth_for(&self, secret_ref: &str) -> Result<Option<AccountAuth>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ref_written_before_this_port_existed_still_resolves_to_the_store() {
        // The live value on a real pond.
        assert_eq!(
            CredentialSource::parse("caldav:mail:gmail:d21b2618-96aa-4be6-8077-5cc256aadf2c"),
            CredentialSource::SecretStore("caldav:mail:gmail:d21b2618-96aa-4be6-8077-5cc256aadf2c")
        );
    }

    #[test]
    fn a_desktop_ref_round_trips_through_its_own_scheme() {
        let r = CredentialSource::desktop_ref("goa", "account_1712345");
        assert_eq!(r, "desktop:goa:account_1712345");
        assert_eq!(
            CredentialSource::parse(&r),
            CredentialSource::DesktopAccount {
                provider: "goa",
                id: "account_1712345"
            }
        );
    }

    #[test]
    fn a_malformed_desktop_ref_is_not_silently_a_store_key() {
        for bad in ["desktop:", "desktop:goa", "desktop::id", "desktop:goa:"] {
            assert_eq!(
                CredentialSource::parse(bad),
                CredentialSource::SecretStore(bad)
            );
        }
    }

    #[test]
    fn both_shapes_name_the_account_because_xoauth2_needs_it() {
        let pw = AccountAuth::Password {
            username: "jerry@example.com".into(),
            password: "app-specific".into(),
        };
        let tok = AccountAuth::Bearer {
            username: "jerry@example.com".into(),
            token: "ya29...".into(),
            expires_at: None,
        };
        assert_eq!(pw.username(), "jerry@example.com");
        assert_eq!(tok.username(), "jerry@example.com");
    }

    #[test]
    fn only_a_token_with_a_stated_expiry_can_be_expired() {
        let now = Utc::now();
        let pw = AccountAuth::Password {
            username: "u".into(),
            password: "p".into(),
        };
        assert!(!pw.is_expired_at(now));

        let no_expiry = AccountAuth::Bearer {
            username: "u".into(),
            token: "t".into(),
            expires_at: None,
        };
        assert!(!no_expiry.is_expired_at(now));

        let expired = AccountAuth::Bearer {
            username: "u".into(),
            token: "t".into(),
            expires_at: Some(now - chrono::Duration::seconds(1)),
        };
        assert!(expired.is_expired_at(now));

        let live = AccountAuth::Bearer {
            username: "u".into(),
            token: "t".into(),
            expires_at: Some(now + chrono::Duration::hours(1)),
        };
        assert!(!live.is_expired_at(now));
    }

    #[test]
    fn neither_shape_prints_its_secret() {
        let pw = AccountAuth::Password {
            username: "u".into(),
            password: "hunter2".into(),
        };
        let tok = AccountAuth::Bearer {
            username: "u".into(),
            token: "ya29.super-secret".into(),
            expires_at: None,
        };
        assert!(!format!("{pw:?}").contains("hunter2"), "{pw:?}");
        assert!(!format!("{tok:?}").contains("super-secret"), "{tok:?}");
        // The account name is not a secret and is what makes a log useful.
        assert!(format!("{pw:?}").contains('u'));
    }
}
