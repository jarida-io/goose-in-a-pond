use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The SoftAP's SSID is this followed by the discriminator, so the printed card names the network.
pub const SOFTAP_SSID_PREFIX: &str = "Pond-Setup-";

/// Hex digits in a discriminator.
const DISCRIMINATOR_HEX_LEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a pond's setup id is four hex digits")]
pub struct DiscriminatorError;

/// Random and public: tells apart the unclaimed ponds a phone can reach, so it connects to the one
/// on its card. Written as four lowercase hex digits, `7f3a`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Discriminator(u16);

impl Discriminator {
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    pub const fn value(self) -> u16 {
        self.0
    }

    /// Exactly four hex digits, in either case, since a person may type the SSID's uppercase form.
    pub fn parse(input: &str) -> Result<Self, DiscriminatorError> {
        if input.len() != DISCRIMINATOR_HEX_LEN || !input.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DiscriminatorError);
        }
        u16::from_str_radix(input, 16)
            .map(Self)
            .map_err(|_| DiscriminatorError)
    }

    /// `Pond-Setup-7F3A`: uppercase so it reads clearly in a phone's Wi-Fi list.
    pub fn softap_ssid(self) -> String {
        format!("{SOFTAP_SSID_PREFIX}{:04X}", self.0)
    }
}

impl fmt::Display for Discriminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}", self.0)
    }
}

impl fmt::Debug for Discriminator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Discriminator({self})")
    }
}

impl Serialize for Discriminator {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Discriminator {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_is_four_lowercase_hex_digits_zero_padded() {
        assert_eq!(Discriminator::new(0x7f3a).to_string(), "7f3a");
        assert_eq!(Discriminator::new(0x000a).to_string(), "000a");
        assert_eq!(
            format!("{:?}", Discriminator::new(0x7f3a)),
            "Discriminator(7f3a)"
        );
    }

    #[test]
    fn parse_takes_either_case_and_writes_lowercase() {
        assert_eq!(Discriminator::parse("7f3a"), Ok(Discriminator::new(0x7f3a)));
        assert_eq!(Discriminator::parse("7F3A"), Ok(Discriminator::new(0x7f3a)));
        assert_eq!(Discriminator::parse("0000"), Ok(Discriminator::new(0)));
    }

    #[test]
    fn parse_refuses_anything_but_four_hex_digits() {
        for bad in ["7f3", "7f3a0", "7g3a", "+f3a", " 7f3", "", "7f\u{e9}"] {
            assert_eq!(
                Discriminator::parse(bad),
                Err(DiscriminatorError),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn the_softap_ssid_names_the_discriminator_in_uppercase() {
        assert_eq!(Discriminator::new(0x7f3a).softap_ssid(), "Pond-Setup-7F3A");
        assert_eq!(Discriminator::new(0x00be).softap_ssid(), "Pond-Setup-00BE");
    }

    #[test]
    fn serde_round_trips_the_hex_string() {
        let id = Discriminator::new(0x7f3a);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"7f3a\"");
        assert_eq!(serde_json::from_str::<Discriminator>(&json).unwrap(), id);
        assert!(serde_json::from_str::<Discriminator>("\"7f3\"").is_err());
        assert!(serde_json::from_str::<Discriminator>("32570").is_err());
    }
}
