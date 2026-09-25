//! Working out where the pond is, from several sources, cheapest first.
//!
//! There were four detections before this and none of them worked. The one in
//! onboarding split the time-zone string on `/`, waited 400 ms so it looked
//! like it had done something, and produced NO COORDINATES — then set
//! `enable_weather`, so a household finished setup with weather switched on and
//! nothing to forecast. The one in Settings asked the browser for coordinates,
//! which a WebView did not reliably answer (the desktop shell is Chromium now,
//! so that is worth re-testing rather than assuming). The server had a fifth path
//! that geocoded on save, so the same question got a different answer depending
//! on which screen you were standing in front of.
//!
//! # The cascade
//!
//! Sources are tried in order of what they cost the household, not of how
//! accurate they are:
//!
//! ```text
//!   1. the zone this device is set to   free, private, always available
//!   2. a name — typed, or implied by 1  one geocoding call, reveals the place
//!   3. the device's own position        precise, needs permission, often absent
//!   4. this connection's apparent home  reveals this household's IP address
//! ```
//!
//! 1 and 2 are enough for weather, which is what this is mostly for, and they
//! ask nobody for anything. 3 is supplied by the caller when a real browser
//! answered. 4 is [`NetworkPlaceLookup`], is not wired unless the household
//! turns it on, and is last because it is the only one that tells a stranger
//! something about this house.
//!
//! # Honesty
//!
//! Every answer carries the [`Source`] it came from, because "you are in
//! Nairobi" and "your time zone suggests Nairobi" are different claims. The old
//! code could not tell them apart, so it stated the guess as a fact.

use crate::user_data::ports::place_lookup::{NetworkPlaceLookup, PlaceLookup};
use crate::user_data::services::location::{is_valid_zone, normalize_zone, place_from_timezone};

/// Which source produced an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The zone alone. A name, no coordinates.
    SystemZone,
    /// A name resolved to coordinates by a geocoder.
    Geocoded,
    /// The device reported its own position.
    Device,
    /// The connection's apparent location.
    Network,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::SystemZone => "timezone",
            Source::Geocoded => "geocoded",
            Source::Device => "device",
            Source::Network => "network",
        }
    }

    /// Whether this source states a fact rather than a guess (a zone-derived name is a guess).
    pub fn is_certain(self) -> bool {
        matches!(self, Source::Device | Source::Geocoded | Source::Network)
    }
}

/// What the pond worked out, and where each part came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Detected {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// Always a real IANA zone; `UTC` when nothing better was found.
    pub timezone: String,
    pub source: Source,
    /// Why there are no coordinates, when there are none. For the screen.
    pub note: Option<String>,
}

impl Detected {
    pub fn has_coordinates(&self) -> bool {
        self.latitude != 0.0 || self.longitude != 0.0
    }
}

/// What the caller already knows before any lookup happens.
#[derive(Debug, Clone, Default)]
pub struct Hints<'a> {
    /// The zone this device is set to, e.g. from `Intl` or the host OS.
    pub system_zone: Option<&'a str>,
    /// A name the household typed. Trusted over anything derived.
    pub typed_name: Option<&'a str>,
    /// Coordinates the device reported, if a browser actually answered.
    pub device_coords: Option<(f64, f64)>,
}

/// Run the cascade; pass `network` only when the household opted in, since it reveals their IP.
pub async fn detect(
    hints: Hints<'_>,
    by_name: Option<&dyn PlaceLookup>,
    network: Option<&dyn NetworkPlaceLookup>,
) -> Detected {
    // 1. The zone. Normalised rather than trusted: this arrives from a client.
    let timezone = hints
        .system_zone
        .and_then(normalize_zone)
        .unwrap_or_else(|| "UTC".to_string());

    // The name to look up: what was typed, else what the zone implies.
    let implied = place_from_timezone(&timezone);
    let candidate = hints
        .typed_name
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(str::to_string)
        .or_else(|| implied.clone());

    // 3. Device coordinates ((0,0) = unset) beat any lookup; the name stays the typed/zone one.
    if let Some((lat, lon)) = hints.device_coords.filter(|(a, b)| *a != 0.0 || *b != 0.0) {
        return Detected {
            name: candidate.unwrap_or_default(),
            latitude: lat,
            longitude: lon,
            timezone,
            source: Source::Device,
            note: None,
        };
    }

    // 2. Geocode the candidate name.
    if let (Some(name), Some(lookup)) = (candidate.as_deref(), by_name) {
        match lookup.by_name(name).await {
            Ok(fix) => {
                return Detected {
                    name: fix.name,
                    latitude: fix.latitude,
                    longitude: fix.longitude,
                    // Only a valid zone: a bad one would be stored and then fail to schedule.
                    timezone: fix
                        .timezone
                        .filter(|z| is_valid_zone(z))
                        .unwrap_or(timezone),
                    source: Source::Geocoded,
                    note: None,
                };
            }
            Err(e) => {
                tracing::debug!(place = name, error = %e, "geocoding the implied place failed");
            }
        }
    }

    // 4. The connection, if the household allowed it.
    if let Some(net) = network {
        match net.by_network().await {
            Ok(fix) => {
                return Detected {
                    name: fix.name,
                    latitude: fix.latitude,
                    longitude: fix.longitude,
                    timezone: fix
                        .timezone
                        .filter(|z| is_valid_zone(z))
                        .unwrap_or(timezone),
                    source: Source::Network,
                    note: None,
                };
            }
            Err(e) => {
                tracing::debug!(error = %e, "network location lookup failed");
            }
        }
    }

    // Nothing placed it; still return the zone and implied city, which needed no network.
    Detected {
        name: candidate.unwrap_or_default(),
        latitude: 0.0,
        longitude: 0.0,
        timezone,
        source: Source::SystemZone,
        note: Some(
            "Worked out the time zone, but not the exact spot. \
             Weather needs a place name — type the nearest town."
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::ports::place_lookup::PlaceFix;
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;

    struct Geo(Option<PlaceFix>);
    #[async_trait]
    impl PlaceLookup for Geo {
        async fn by_name(&self, _q: &str) -> Result<PlaceFix> {
            self.0.clone().ok_or_else(|| anyhow!("no such place"))
        }
    }

    struct Net(Option<PlaceFix>);
    #[async_trait]
    impl NetworkPlaceLookup for Net {
        async fn by_network(&self) -> Result<PlaceFix> {
            self.0.clone().ok_or_else(|| anyhow!("lookup refused"))
        }
    }

    fn fix(name: &str, lat: f64, lon: f64, tz: Option<&str>) -> PlaceFix {
        PlaceFix {
            name: name.into(),
            latitude: lat,
            longitude: lon,
            timezone: tz.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn the_zone_alone_produces_coordinates_via_the_geocoder() {
        let geo = Geo(Some(fix("Nairobi, Kenya", -1.286, 36.817, None)));
        let out = detect(
            Hints {
                system_zone: Some("Africa/Nairobi"),
                ..Default::default()
            },
            Some(&geo),
            None,
        )
        .await;
        assert_eq!(out.source, Source::Geocoded);
        assert!(
            out.has_coordinates(),
            "a detection with no coordinates cannot forecast"
        );
        assert_eq!(out.timezone, "Africa/Nairobi");
        assert_eq!(out.name, "Nairobi, Kenya");
    }

    #[tokio::test]
    async fn a_typed_name_is_looked_up_rather_than_the_zones_guess() {
        let geo = Geo(Some(fix("Kisumu, Kenya", -0.091, 34.768, None)));
        let out = detect(
            Hints {
                system_zone: Some("Africa/Nairobi"),
                typed_name: Some("Kisumu"),
                ..Default::default()
            },
            Some(&geo),
            None,
        )
        .await;
        assert_eq!(out.name, "Kisumu, Kenya");
        assert_eq!(out.source, Source::Geocoded);
    }

    #[tokio::test]
    async fn device_coordinates_win_and_still_carry_a_name() {
        let out = detect(
            Hints {
                system_zone: Some("Africa/Nairobi"),
                device_coords: Some((-1.3, 36.8)),
                ..Default::default()
            },
            None,
            None,
        )
        .await;
        assert_eq!(out.source, Source::Device);
        assert_eq!((out.latitude, out.longitude), (-1.3, 36.8));
        assert_eq!(out.name, "Nairobi", "the zone still supplies the name");
    }

    #[tokio::test]
    async fn null_island_from_a_device_is_not_a_fix() {
        let geo = Geo(Some(fix("Nairobi, Kenya", -1.286, 36.817, None)));
        let out = detect(
            Hints {
                system_zone: Some("Africa/Nairobi"),
                device_coords: Some((0.0, 0.0)),
                ..Default::default()
            },
            Some(&geo),
            None,
        )
        .await;
        assert_eq!(out.source, Source::Geocoded);
    }

    #[tokio::test]
    async fn the_network_is_a_last_resort_not_a_first_choice() {
        let geo = Geo(Some(fix("Nairobi, Kenya", -1.286, 36.817, None)));
        let net = Net(Some(fix("Somewhere Else", 51.5, -0.1, None)));
        let out = detect(
            Hints {
                system_zone: Some("Africa/Nairobi"),
                ..Default::default()
            },
            Some(&geo),
            Some(&net),
        )
        .await;
        assert_eq!(
            out.source,
            Source::Geocoded,
            "the network was asked even though a free source had the answer"
        );
    }

    #[tokio::test]
    async fn the_network_answers_when_geocoding_could_not() {
        let net = Net(Some(fix("London, UK", 51.5, -0.1, Some("Europe/London"))));
        let out = detect(
            Hints {
                system_zone: Some("UTC"),
                ..Default::default()
            },
            Some(&Geo(None)),
            Some(&net),
        )
        .await;
        assert_eq!(out.source, Source::Network);
        assert_eq!(out.timezone, "Europe/London");
    }

    #[tokio::test]
    async fn without_the_network_source_nothing_reaches_for_it() {
        let out = detect(
            Hints {
                system_zone: Some("UTC"),
                ..Default::default()
            },
            Some(&Geo(None)),
            None,
        )
        .await;
        assert_eq!(out.source, Source::SystemZone);
        assert!(!out.has_coordinates());
        assert!(
            out.note.is_some(),
            "a failure with no explanation is the old behaviour"
        );
    }

    #[tokio::test]
    async fn a_nonsense_zone_from_a_source_is_not_stored() {
        let geo = Geo(Some(fix("Nowhere", 1.0, 1.0, Some("Mars/Olympus"))));
        let out = detect(
            Hints {
                system_zone: Some("Africa/Nairobi"),
                ..Default::default()
            },
            Some(&geo),
            None,
        )
        .await;
        assert_eq!(
            out.timezone, "Africa/Nairobi",
            "a bad zone would never schedule"
        );
    }

    #[tokio::test]
    async fn a_zone_the_client_miscased_is_still_understood() {
        let out = detect(
            Hints {
                system_zone: Some("africa/nairobi"),
                ..Default::default()
            },
            None,
            None,
        )
        .await;
        assert_eq!(out.timezone, "Africa/Nairobi");
    }

    #[tokio::test]
    async fn a_pond_with_no_hints_at_all_lands_on_utc_rather_than_failing() {
        let out = detect(Hints::default(), None, None).await;
        assert_eq!(out.timezone, "UTC");
        assert_eq!(out.source, Source::SystemZone);
        assert_eq!(
            out.name, "",
            "UTC is not a place and must not be given a name"
        );
    }
}
