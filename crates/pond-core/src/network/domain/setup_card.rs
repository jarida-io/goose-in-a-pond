use std::borrow::Cow;
use std::collections::HashSet;

use url::Url;

use super::discriminator::{Discriminator, DiscriminatorError};
use super::setup_code::{SetupCode, SetupCodeError};

pub const SETUP_QR_SCHEME: &str = "pond";
pub const SETUP_QR_HOST: &str = "setup";
/// A card printed today must still scan after the format grows, so the version is on the card.
pub const SETUP_QR_VERSION: &str = "1";

/// Why scanned text is not a setup card. No variant carries the code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SetupCardError {
    #[error("not a URL")]
    NotAUrl,
    #[error("not a pond link")]
    WrongScheme,
    #[error("a pond link, but not a setup card")]
    WrongHost,
    #[error("a setup card carries no path, port, user or fragment")]
    UnexpectedComponent,
    #[error("a setup card names each parameter once")]
    RepeatedParameter,
    #[error("the setup card has no {0} parameter")]
    MissingParameter(&'static str),
    #[error("this setup card is a newer version than this app reads")]
    UnsupportedVersion,
    #[error(transparent)]
    Code(#[from] SetupCodeError),
    #[error(transparent)]
    Discriminator(#[from] DiscriminatorError),
}

/// What a pond's printed card carries: its setup code and the discriminator that finds it. No TLS
/// pin: the pin arrives authenticated by the exchange the code feeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCard {
    pub code: SetupCode,
    pub discriminator: Discriminator,
}

impl SetupCard {
    pub fn new(code: SetupCode, discriminator: Discriminator) -> Self {
        Self {
            code,
            discriminator,
        }
    }

    /// `pond://setup?v=1&code=<11 digits>&id=<4 hex>`. Formatted, not built with `Url`: every
    /// value is digits or hex, so nothing needs escaping and nothing can fail.
    pub fn qr_url(&self) -> String {
        format!(
            "{SETUP_QR_SCHEME}://{SETUP_QR_HOST}?v={SETUP_QR_VERSION}&code={}&id={}",
            self.code.digits(),
            self.discriminator
        )
    }

    /// A repeated parameter is refused so two readers cannot disagree on which value counts; an
    /// unknown one is ignored so a later card still scans here.
    pub fn parse_qr(raw: &str) -> Result<Self, SetupCardError> {
        let url = Url::parse(raw.trim()).map_err(|_| SetupCardError::NotAUrl)?;
        if url.scheme() != SETUP_QR_SCHEME {
            return Err(SetupCardError::WrongScheme);
        }
        if url.host_str() != Some(SETUP_QR_HOST) {
            return Err(SetupCardError::WrongHost);
        }
        if !url.path().is_empty()
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(SetupCardError::UnexpectedComponent);
        }

        let mut seen: HashSet<Cow<'_, str>> = HashSet::new();
        let (mut version, mut code, mut id) = (None, None, None);
        for (name, value) in url.query_pairs() {
            match name.as_ref() {
                "v" => version = Some(value),
                "code" => code = Some(value),
                "id" => id = Some(value),
                _ => {}
            }
            if !seen.insert(name) {
                return Err(SetupCardError::RepeatedParameter);
            }
        }

        match version.as_deref() {
            None => return Err(SetupCardError::MissingParameter("v")),
            Some(SETUP_QR_VERSION) => {}
            Some(_) => return Err(SetupCardError::UnsupportedVersion),
        }
        let code = code.ok_or(SetupCardError::MissingParameter("code"))?;
        // The card prints bare digits; separators are for people typing, not for a scanned code.
        if !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SetupCodeError::NotDigits.into());
        }
        let code = SetupCode::parse(&code)?;
        let id = id.ok_or(SetupCardError::MissingParameter("id"))?;
        let discriminator = Discriminator::parse(&id)?;
        Ok(Self::new(code, discriminator))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QR: &str = "pond://setup?v=1&code=79927398713&id=7f3a";

    fn card() -> SetupCard {
        SetupCard::new(
            SetupCode::from_digits([7, 9, 9, 2, 7, 3, 9, 8, 7, 1]).unwrap(),
            Discriminator::new(0x7f3a),
        )
    }

    #[test]
    fn the_qr_url_is_exactly_the_documented_shape_and_parses_back() {
        assert_eq!(card().qr_url(), QR);
        assert_eq!(SetupCard::parse_qr(QR).unwrap(), card());
        assert_eq!(SetupCard::parse_qr(&format!("  {QR}\n")).unwrap(), card());
    }

    #[test]
    fn parameter_order_does_not_matter_and_unknown_parameters_are_ignored() {
        let reordered = "pond://setup?id=7f3a&code=79927398713&v=1";
        assert_eq!(SetupCard::parse_qr(reordered).unwrap(), card());
        let extended = "pond://setup?v=1&code=79927398713&id=7f3a&label=Kitchen&future=x";
        assert_eq!(SetupCard::parse_qr(extended).unwrap(), card());
    }

    #[test]
    fn another_scheme_or_host_is_not_a_setup_card() {
        assert_eq!(
            SetupCard::parse_qr("https://setup?v=1&code=79927398713&id=7f3a"),
            Err(SetupCardError::WrongScheme)
        );
        assert_eq!(
            SetupCard::parse_qr("pond://pair?v=1&code=79927398713&id=7f3a"),
            Err(SetupCardError::WrongHost)
        );
        assert_eq!(
            SetupCard::parse_qr("pond:setup?v=1&code=79927398713&id=7f3a"),
            Err(SetupCardError::WrongHost)
        );
        assert_eq!(
            SetupCard::parse_qr("3497-011-2339"),
            Err(SetupCardError::NotAUrl)
        );
    }

    #[test]
    fn a_path_port_user_or_fragment_is_refused() {
        for bad in [
            "pond://setup/?v=1&code=79927398713&id=7f3a",
            "pond://setup:80?v=1&code=79927398713&id=7f3a",
            "pond://me@setup?v=1&code=79927398713&id=7f3a",
            "pond://setup?v=1&code=79927398713&id=7f3a#x",
        ] {
            assert_eq!(
                SetupCard::parse_qr(bad),
                Err(SetupCardError::UnexpectedComponent),
                "{bad}"
            );
        }
    }

    #[test]
    fn another_or_missing_version_is_refused() {
        assert_eq!(
            SetupCard::parse_qr("pond://setup?v=2&code=79927398713&id=7f3a"),
            Err(SetupCardError::UnsupportedVersion)
        );
        assert_eq!(
            SetupCard::parse_qr("pond://setup?code=79927398713&id=7f3a"),
            Err(SetupCardError::MissingParameter("v"))
        );
    }

    #[test]
    fn any_repeated_parameter_is_refused() {
        for bad in [
            "pond://setup?v=1&code=79927398713&code=79927398713&id=7f3a",
            "pond://setup?v=1&v=1&code=79927398713&id=7f3a",
            "pond://setup?v=1&code=79927398713&id=7f3a&id=0001",
            "pond://setup?v=1&code=79927398713&id=7f3a&x=1&x=2",
        ] {
            assert_eq!(
                SetupCard::parse_qr(bad),
                Err(SetupCardError::RepeatedParameter),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_bad_code_or_id_is_refused() {
        assert_eq!(
            SetupCard::parse_qr("pond://setup?v=1&code=79927398714&id=7f3a"),
            Err(SetupCardError::Code(SetupCodeError::CheckDigitMismatch))
        );
        assert_eq!(
            SetupCard::parse_qr("pond://setup?v=1&code=7992-739-8713&id=7f3a"),
            Err(SetupCardError::Code(SetupCodeError::NotDigits))
        );
        assert_eq!(
            SetupCard::parse_qr("pond://setup?v=1&code=7992739871&id=7f3a"),
            Err(SetupCardError::Code(SetupCodeError::WrongLength {
                found: 10
            }))
        );
        assert_eq!(
            SetupCard::parse_qr("pond://setup?v=1&code=79927398713&id=7f3"),
            Err(SetupCardError::Discriminator(DiscriminatorError))
        );
        assert_eq!(
            SetupCard::parse_qr("pond://setup?v=1&code=79927398713"),
            Err(SetupCardError::MissingParameter("id"))
        );
        assert_eq!(
            SetupCard::parse_qr("pond://setup?v=1&id=7f3a"),
            Err(SetupCardError::MissingParameter("code"))
        );
    }

    #[test]
    fn debug_never_shows_the_code() {
        let card = card();
        let debug = format!("{card:?}");
        assert!(!debug.contains(card.code.digits()), "{debug}");
        assert!(!debug.contains(&card.code.display()), "{debug}");
    }
}
