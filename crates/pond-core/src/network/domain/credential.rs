use serde::{Deserialize, Serialize};

/// Which secret a phone says it holds in `pake_init`, so the pond knows which limits apply.
/// Absent means `Pairing`, so a phone built before setup codes pairs as it always did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Credential {
    /// The printed card's static code, accepted only while the pond is unclaimed.
    Setup,
    /// A short-lived six-digit pairing code, issued on the console or read aloud.
    #[default]
    Pairing,
    /// A paired phone's own refresh token, proven by its hash; the setup channel's only, since
    /// HTTPS already has `/handshake/refresh`.
    Device,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_names_are_setup_pairing_and_device() {
        assert_eq!(
            serde_json::to_string(&Credential::Setup).unwrap(),
            "\"setup\""
        );
        assert_eq!(
            serde_json::to_string(&Credential::Pairing).unwrap(),
            "\"pairing\""
        );
        assert_eq!(
            serde_json::to_string(&Credential::Device).unwrap(),
            "\"device\""
        );
        assert_eq!(
            serde_json::from_str::<Credential>("\"setup\"").unwrap(),
            Credential::Setup
        );
        assert_eq!(
            serde_json::from_str::<Credential>("\"device\"").unwrap(),
            Credential::Device
        );
        assert!(serde_json::from_str::<Credential>("\"Setup\"").is_err());
        assert!(serde_json::from_str::<Credential>("\"Device\"").is_err());
        assert!(serde_json::from_str::<Credential>("\"token\"").is_err());
    }

    #[test]
    fn an_absent_credential_means_pairing() {
        assert_eq!(Credential::default(), Credential::Pairing);
    }
}
