//! What a pond says about itself on the LAN as `_pond._tcp` (`docs/architecture/pond-networking.md`,
//! "Discovery"), whichever responder on the host publishes it.

use crate::network::domain::setup_record::SetupRecord;

/// The DNS-SD service type without its domain: dns_sd takes it bare, mdns-sd appends `local.`.
pub const POND_SERVICE_TYPE: &str = "_pond._tcp";

/// A DNS label's limit (RFC 1035 2.3.4); dns_sd refuses a longer instance name outright.
pub const MAX_INSTANCE_NAME_BYTES: usize = 63;

/// One TXT character-string's limit (RFC 6763 6.1): its length is a single byte.
pub const MAX_TXT_STRING_BYTES: usize = 255;

/// What the TXT record says about setup: the pond's id, and whether its card is still accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupAdvert {
    /// The discriminator printed on the card, so a phone connects to the pond it scanned.
    pub id: String,
    pub unclaimed: bool,
}

impl SetupAdvert {
    pub fn from_record(record: &SetupRecord) -> Self {
        Self {
            id: record.discriminator.to_string(),
            unclaimed: !record.claim_state().is_claimed(),
        }
    }
}

/// The pond's `_pond._tcp` instance: its name, HTTPS port and TXT record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PondAdvert {
    /// The host label without `.local`. The instance is named after it; a responder that owns
    /// the host's address records (mDNSResponder) uses its own name for the SRV target instead.
    pub host: String,
    pub port: u16,
    pub version: String,
    /// Public, and only a convenience: mDNS is spoofable, so clients must confirm the pin.
    pub pin: String,
    pub setup: Option<SetupAdvert>,
}

impl PondAdvert {
    pub fn new(host: &str, port: u16, version: &str, pin: &str) -> Self {
        Self {
            host: host.to_string(),
            port,
            version: version.to_string(),
            pin: pin.to_string(),
            setup: None,
        }
    }

    pub fn with_setup(mut self, setup: Option<SetupAdvert>) -> Self {
        self.setup = setup;
        self
    }

    /// `Pond Hub @ <host>`, cut at a character boundary to fit one DNS label.
    pub fn instance(&self) -> String {
        let mut name = format!("Pond Hub @ {}", self.host);
        if name.len() > MAX_INSTANCE_NAME_BYTES {
            let mut end = MAX_INSTANCE_NAME_BYTES;
            while !name.is_char_boundary(end) {
                end -= 1;
            }
            name.truncate(end);
        }
        name
    }

    /// Whether a phone holding the card should look at this pond.
    pub fn setup_open(&self) -> bool {
        self.setup.as_ref().is_some_and(|s| s.unclaimed)
    }

    /// The TXT record in wire order. `setup=1` only while the card is accepted, so a phone holding
    /// one looks only at ponds that will take it.
    pub fn txt(&self) -> Vec<(&'static str, String)> {
        let mut entries = vec![
            ("v", self.version.clone()),
            ("scheme", "https".to_string()),
            ("pin", self.pin.clone()),
        ];
        if let Some(setup) = &self.setup {
            entries.push(("id", setup.id.clone()));
            if setup.unclaimed {
                entries.push(("setup", "1".to_string()));
            }
        }
        entries
    }

    /// The TXT rdata (RFC 6763 6.1): each `key=value` prefixed by its one-byte length.
    pub fn txt_rdata(&self) -> Result<Vec<u8>, AdvertiseError> {
        let mut rdata = Vec::new();
        for (key, value) in self.txt() {
            let len = key.len() + 1 + value.len();
            let len = u8::try_from(len).map_err(|_| {
                AdvertiseError::Refused(format!("TXT entry `{key}` is {len} bytes"))
            })?;
            rdata.push(len);
            rdata.extend_from_slice(key.as_bytes());
            rdata.push(b'=');
            rdata.extend_from_slice(value.as_bytes());
        }
        Ok(rdata)
    }
}

/// Why a pond is not advertised. Discovery is a convenience, so neither stops the pond.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdvertiseError {
    /// The host's responder is not running, or will not talk to this process yet (macOS local
    /// network permission). The adapter keeps retrying.
    #[error("the host's mDNS responder is unavailable: {0}")]
    Unavailable(String),
    /// The responder refused this advertisement as written; the same call would fail again.
    #[error("the mDNS responder refused the advertisement: {0}")]
    Refused(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIN: &str = "sha256/31n0ucNeHtTIeBFSEzKFw1HyQ4GtsurULcV5/IQYYXY=";

    fn advert() -> PondAdvert {
        PondAdvert::new("J3RRA4", 4543, "0.1.0", PIN)
    }

    fn get<'a>(txt: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
        txt.iter().find(|(k, _)| *k == key).map(|(_, v)| v.as_str())
    }

    #[test]
    fn the_txt_record_names_the_pond_and_opens_setup_only_while_unclaimed() {
        let base = advert().txt();
        assert_eq!(get(&base, "id"), None);
        assert_eq!(get(&base, "setup"), None);
        assert_eq!(get(&base, "pin"), Some(PIN));
        assert_eq!(get(&base, "scheme"), Some("https"));
        assert_eq!(get(&base, "v"), Some("0.1.0"));

        let open = SetupAdvert {
            id: "7f3a".into(),
            unclaimed: true,
        };
        let txt = advert().with_setup(Some(open.clone())).txt();
        assert_eq!(get(&txt, "id"), Some("7f3a"));
        assert_eq!(get(&txt, "setup"), Some("1"));

        let claimed = SetupAdvert {
            unclaimed: false,
            ..open
        };
        let txt = advert().with_setup(Some(claimed)).txt();
        assert_eq!(get(&txt, "id"), Some("7f3a"));
        assert_eq!(
            get(&txt, "setup"),
            None,
            "a claimed pond must not invite a card"
        );
    }

    #[test]
    fn the_txt_keys_are_exactly_the_published_contract_in_a_fixed_order() {
        let open = advert().with_setup(Some(SetupAdvert {
            id: "7f3a".into(),
            unclaimed: true,
        }));
        let keys: Vec<&str> = open.txt().iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["v", "scheme", "pin", "id", "setup"]);
        assert!(open.setup_open());
        assert!(!advert().setup_open());
    }

    #[test]
    fn the_rdata_is_length_prefixed_key_value_strings() {
        let open = advert().with_setup(Some(SetupAdvert {
            id: "7f3a".into(),
            unclaimed: true,
        }));
        let rdata = open.txt_rdata().unwrap();
        let mut strings = Vec::new();
        let mut rest = rdata.as_slice();
        while let Some((&len, tail)) = rest.split_first() {
            let (s, after) = tail.split_at(len as usize);
            strings.push(std::str::from_utf8(s).unwrap().to_string());
            rest = after;
        }
        assert_eq!(
            strings,
            [
                "v=0.1.0".to_string(),
                "scheme=https".into(),
                format!("pin={PIN}"),
                "id=7f3a".into(),
                "setup=1".into(),
            ]
        );
    }

    #[test]
    fn an_entry_over_one_length_byte_is_refused_not_truncated() {
        let mut long = advert();
        long.pin = "x".repeat(MAX_TXT_STRING_BYTES);
        assert!(matches!(
            long.txt_rdata(),
            Err(AdvertiseError::Refused(reason)) if reason.contains("pin")
        ));
    }

    #[test]
    fn the_instance_is_named_after_the_host_and_fits_one_label() {
        assert_eq!(advert().instance(), "Pond Hub @ J3RRA4");

        // Three-byte characters, so the cut lands mid-character and has to step back.
        let long = PondAdvert::new(&"名".repeat(30), 4543, "0.1.0", PIN).instance();
        assert_eq!(long.len(), 62);
        assert!(long.starts_with("Pond Hub @ 名"));
    }

    #[test]
    fn the_setup_advert_follows_the_records_claim() {
        use crate::network::domain::discriminator::Discriminator;
        use crate::network::domain::setup_code::SetupCode;
        use chrono::{TimeZone, Utc};

        let mut record = SetupRecord::new(
            SetupCode::from_digits([7, 9, 9, 2, 7, 3, 9, 8, 7, 1]).unwrap(),
            Discriminator::new(0x7f3a),
            Utc.with_ymd_and_hms(2026, 10, 16, 9, 0, 0).unwrap(),
        );
        assert_eq!(
            SetupAdvert::from_record(&record),
            SetupAdvert {
                id: "7f3a".into(),
                unclaimed: true
            }
        );
        record.claimed_at = Some(Utc.with_ymd_and_hms(2026, 10, 16, 10, 0, 0).unwrap());
        assert!(!SetupAdvert::from_record(&record).unclaimed);
    }
}
