use std::fmt;

/// Symbols on an admin card; each carries five bits.
pub const ADMIN_PASSWORD_LEN: usize = 16;

/// Crockford base32: no `I`, `L`, `O` or `U`, so a printed card reads back without ambiguity.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// The one administrator's name, as the OS account and the audit trail spell it.
pub const ADMIN_PRINCIPAL: &str = "agamemnon";

/// Why text is not an admin password. No variant carries the text, so an error is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdminPasswordError {
    #[error(
        "an admin password has {} characters, this has {found}",
        ADMIN_PASSWORD_LEN
    )]
    WrongLength { found: usize },
    #[error("an admin password uses only the card's letters and digits")]
    NotOnTheCard,
    #[error("each symbol of an admin password is 0 to 31")]
    SymbolOutOfRange,
}

/// The secret on a pond's admin card, held normalised. `Debug` redacts and there is no
/// `Display`, so a log line or an error can never carry it by accident (PAI-2).
#[derive(Clone, PartialEq, Eq)]
pub struct AdminPassword(String);

impl AdminPassword {
    /// Takes the random symbols rather than an RNG: pond-core has none, so the store draws them.
    pub fn from_symbols(symbols: [u8; ADMIN_PASSWORD_LEN]) -> Result<Self, AdminPasswordError> {
        symbols
            .iter()
            .map(|&s| {
                ALPHABET
                    .get(usize::from(s))
                    .map(|&c| char::from(c))
                    .ok_or(AdminPasswordError::SymbolOutOfRange)
            })
            .collect::<Result<String, _>>()
            .map(Self)
    }

    /// Accepts what a person types from the card: any case, with or without dashes and spaces,
    /// and `O`, `I` or `L` where the card has `0` or `1`.
    pub fn parse(input: &str) -> Result<Self, AdminPasswordError> {
        let mut out = String::with_capacity(ADMIN_PASSWORD_LEN);
        let mut found = 0usize;
        for c in input.chars() {
            if c == '-' || c.is_whitespace() {
                continue;
            }
            let c = match c.to_ascii_uppercase() {
                'O' => '0',
                'I' | 'L' => '1',
                other => other,
            };
            if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) {
                return Err(AdminPasswordError::NotOnTheCard);
            }
            found += 1;
            if found <= ADMIN_PASSWORD_LEN {
                out.push(c);
            }
        }
        if found != ADMIN_PASSWORD_LEN {
            return Err(AdminPasswordError::WrongLength { found });
        }
        Ok(Self(out))
    }

    /// The normalised sixteen characters a verifier is made from.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// As printed on the card: `XXXX-XXXX-XXXX-XXXX`.
    pub fn grouped(&self) -> String {
        self.0
            .as_bytes()
            .chunks(4)
            .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl fmt::Debug for AdminPassword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AdminPassword(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> AdminPassword {
        AdminPassword::from_symbols([0, 1, 2, 3, 10, 18, 22, 27, 31, 30, 29, 28, 9, 8, 7, 6])
            .unwrap()
    }

    #[test]
    fn a_card_prints_in_four_groups_and_reads_back() {
        let password = card();
        assert_eq!(password.grouped(), "0123-AJPV-ZYXW-9876");
        assert_eq!(AdminPassword::parse(&password.grouped()).unwrap(), password);
    }

    #[test]
    fn what_a_person_types_is_forgiven() {
        let password = card();
        for typed in [
            "0123ajpvzyxw9876",
            " o123 AJPV zyxw 9876 ",
            "O123-ajpv-ZYXW-9876",
            "0123-AJPV-ZYXW-9876\n",
        ] {
            assert_eq!(AdminPassword::parse(typed).unwrap(), password, "{typed:?}");
        }
        let ones = AdminPassword::parse("IL11-0000-0000-0000").unwrap();
        assert_eq!(ones.as_str(), "1111000000000000");
    }

    #[test]
    fn anything_else_is_refused_without_echoing_it() {
        assert_eq!(
            AdminPassword::parse("0123-AJPV-ZYXW-987"),
            Err(AdminPasswordError::WrongLength { found: 15 })
        );
        assert_eq!(
            AdminPassword::parse("0123-AJPV-ZYXW-98765"),
            Err(AdminPasswordError::WrongLength { found: 17 })
        );
        assert_eq!(
            AdminPassword::parse("0123-AJPV-ZYXW-987U"),
            Err(AdminPasswordError::NotOnTheCard)
        );
        assert_eq!(
            AdminPassword::parse("0123-AJPV-ZYXW-987é"),
            Err(AdminPasswordError::NotOnTheCard)
        );
        assert_eq!(
            AdminPassword::from_symbols([32; ADMIN_PASSWORD_LEN]),
            Err(AdminPasswordError::SymbolOutOfRange)
        );
    }

    #[test]
    fn debug_never_shows_the_password() {
        let shown = format!("{:?}", card());
        assert!(!shown.contains("0123"), "{shown}");
    }
}
