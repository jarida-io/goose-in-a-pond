//! Where and when the pond is — asked once, answered the same way everywhere.
//!
//! Location was spread across four settings (`weather_location_name`, the two
//! coordinates, `timezone`) and read directly in seven places across five
//! crates. Each call site invented its own fallback, which is how the same pond
//! came to describe itself two different ways in the same breath: the system
//! prompt omitted the line entirely when the name was blank, while the device
//! tool reported "not configured".
//!
//! Worse, neither of them looked at the time zone. A household that picked
//! `Africa/Nairobi` during setup and never filled the weather box was told the
//! pond did not know where it was, by a pond holding the answer.
//!
//! So the fallbacks live here, once, and callers ask a question instead of
//! reading fields:
//!
//! ```text
//!   name   configured name → the time zone's own place → nothing
//!   coords as stored; (0, 0) is the unset sentinel, not the Atlantic
//!   zone   as stored, defaulting to UTC
//! ```
//!
//! This is a pure function over [`Settings`], not a port. Nothing here reaches
//! for a network or a device — deciding where the pond is and *discovering* it
//! are different jobs, and only the second one needs permission from anybody.
//! Discovery lives in [`crate::user_data::services::place_detection`].
//!
//! # The zone half
//!
//! Time zones were hand-maintained in four places that disagreed: three lists
//! in the desktop (16, 18 and 13 zones, no two the same) and nothing at all on
//! the server, which accepted any string a client sent. A household in
//! `Africa/Kampala` could not pick its own zone from any of the three, and a
//! typo reached the scheduler as a zone that would silently never fire.
//!
//! So the catalogue comes from the IANA database via `chrono-tz` — every zone
//! that exists, spelled the way the database spells it — and the same function
//! answers "is this real?" for the settings route, the scheduler and the UI.

use crate::user_data::domain::settings::Settings;
use chrono::{DateTime, FixedOffset, Offset, Utc};

/// How confident the pond is about its place name, so callers never state a guess as fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Somebody typed it, or a detection wrote it.
    Configured,
    /// Derived from the time zone, which is a good guess and only a guess.
    Timezone,
    /// The pond does not know.
    Unknown,
}

/// Where the pond believes it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// Place name, empty when [`Origin::Unknown`].
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    /// IANA zone, e.g. `Africa/Nairobi`. Never empty — falls back to `UTC`.
    pub timezone: String,
    pub origin: Origin,
}

impl Location {
    /// Whether the coordinates are usable; `(0, 0)` (the default, out at sea) counts as unset.
    pub fn has_coordinates(&self) -> bool {
        self.latitude != 0.0 || self.longitude != 0.0
    }

    /// Whether there is a name worth saying out loud.
    pub fn is_named(&self) -> bool {
        !self.name.is_empty()
    }

    /// Coordinates and a label for a weather lookup, or `None` when there is nothing to ask.
    /// The label prefers the name: the provider geocodes it, so a name alone is enough.
    pub fn weather_target(&self) -> Option<(f64, f64, String)> {
        if !self.has_coordinates() && !self.is_named() {
            return None;
        }
        let label = if self.is_named() {
            self.name.clone()
        } else {
            format!("{:.3}, {:.3}", self.latitude, self.longitude)
        };
        Some((self.latitude, self.longitude, label))
    }

    /// The place, phrased for a person; on `None` say nothing rather than "not configured".
    pub fn describe(&self) -> Option<&str> {
        self.is_named().then_some(self.name.as_str())
    }
}

/// The place a zone implies: `America/New_York` → `New York`; `UTC` implies none.
pub fn place_from_timezone(zone: &str) -> Option<String> {
    let leaf = zone.rsplit('/').next()?;
    if leaf == zone || leaf.is_empty() {
        return None;
    }
    Some(leaf.replace('_', " "))
}

/// Resolve the pond's location from its settings.
pub fn resolve(settings: &Settings) -> Location {
    let timezone = if settings.timezone.trim().is_empty() {
        "UTC".to_string()
    } else {
        settings.timezone.trim().to_string()
    };

    let configured = settings.weather_location_name.trim();
    let (name, origin) = if !configured.is_empty() {
        (configured.to_string(), Origin::Configured)
    } else {
        match place_from_timezone(&timezone) {
            Some(p) => (p, Origin::Timezone),
            None => (String::new(), Origin::Unknown),
        }
    };

    Location {
        name,
        latitude: settings.weather_latitude,
        longitude: settings.weather_longitude,
        timezone,
        origin,
    }
}

// ── Zones ───────────────────────────────────────────────────────────────────

/// Every IANA zone this build knows, sorted; the full set, as curated lists drift and miss zones.
pub fn zones() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = chrono_tz::TZ_VARIANTS.iter().map(|z| z.name()).collect();
    all.sort_unstable();
    all
}

/// Whether `zone` is a real IANA zone.
pub fn is_valid_zone(zone: &str) -> bool {
    zone.trim().parse::<chrono_tz::Tz>().is_ok()
}

/// Canonical spelling of a typed zone (case-insensitive match); `None` if the database lacks it.
pub fn normalize_zone(zone: &str) -> Option<String> {
    let t = zone.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(tz) = t.parse::<chrono_tz::Tz>() {
        return Some(tz.name().to_string());
    }
    let lower = t.to_ascii_lowercase();
    chrono_tz::TZ_VARIANTS
        .iter()
        .find(|z| z.name().to_ascii_lowercase() == lower)
        .map(|z| z.name().to_string())
}

// ── Time ────────────────────────────────────────────────────────────────────

/// The current local time in `zone`, or `None` if the zone is not real.
pub fn now_in(zone: &str, now: DateTime<Utc>) -> Option<DateTime<chrono_tz::Tz>> {
    let tz: chrono_tz::Tz = zone.trim().parse().ok()?;
    Some(now.with_timezone(&tz))
}

/// Which spelling of an offset a caller needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetStyle {
    /// `+03:00`, `-09:30`. Fixed width, always minutes. For pickers and data.
    Iso,
    /// `UTC+3`, `UTC-9:30`: unpadded hours, minutes only when non-zero. For prose read aloud.
    Prose,
}

/// Write an already-resolved offset in the requested style.
pub fn format_offset(offset: FixedOffset, style: OffsetStyle) -> String {
    let total = offset.local_minus_utc();
    let (sign, secs) = if total < 0 {
        ('-', -total)
    } else {
        ('+', total)
    };
    let (h, m) = (secs / 3600, (secs % 3600) / 60);
    match style {
        OffsetStyle::Iso => format!("{sign}{h:02}:{m:02}"),
        OffsetStyle::Prose if m == 0 => format!("UTC{sign}{h}"),
        OffsetStyle::Prose => format!("UTC{sign}{h}:{m:02}"),
    }
}

/// The offset in `zone` at `now`, in the requested style.
pub fn offset_label_styled(zone: &str, now: DateTime<Utc>, style: OffsetStyle) -> Option<String> {
    Some(format_offset(now_in(zone, now)?.offset().fix(), style))
}

/// The UTC offset in `zone` at `now`, as `+03:00`; never cache it, since DST moves it.
pub fn offset_label(zone: &str, now: DateTime<Utc>) -> Option<String> {
    offset_label_styled(zone, now, OffsetStyle::Iso)
}

/// A zone, its current offset, and the place it implies — what a picker shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneChoice {
    /// IANA name, e.g. `Africa/Nairobi`.
    pub zone: String,
    /// Current offset, e.g. `+03:00`.
    pub offset: String,
    /// The place the name implies, e.g. `Nairobi`. Empty for zones like `UTC`.
    pub place: String,
}

/// The whole catalogue, ready to render, with offsets resolved for `now`.
pub fn zone_catalogue(now: DateTime<Utc>) -> Vec<ZoneChoice> {
    zones()
        .into_iter()
        .map(|zone| ZoneChoice {
            offset: offset_label(zone, now).unwrap_or_else(|| "+00:00".into()),
            place: place_from_timezone(zone).unwrap_or_default(),
            zone: zone.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn with(name: &str, zone: &str, lat: f64, lon: f64) -> Settings {
        Settings {
            weather_location_name: name.into(),
            timezone: zone.into(),
            weather_latitude: lat,
            weather_longitude: lon,
            ..Default::default()
        }
    }

    // ── Zones ───────────────────────────────────────────────────────────

    #[test]
    fn the_catalogue_holds_zones_no_hand_list_did() {
        let all = zones();
        for zone in [
            "Africa/Kampala",
            "Africa/Nairobi",
            "America/Argentina/Buenos_Aires",
            "Asia/Kathmandu",
            "Pacific/Chatham",
            "UTC",
        ] {
            assert!(all.contains(&zone), "{zone} is missing from the catalogue");
        }
        assert!(
            all.len() > 300,
            "only {} zones — that is a curated list",
            all.len()
        );
    }

    #[test]
    fn the_catalogue_is_sorted_and_unique() {
        let all = zones();
        let mut sorted = all.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(all, sorted, "a picker renders this in order");
    }

    #[test]
    fn a_zone_that_does_not_exist_is_refused() {
        assert!(is_valid_zone("Africa/Nairobi"));
        assert!(is_valid_zone("UTC"));
        assert!(!is_valid_zone("Africa/Nairobbi"));
        assert!(!is_valid_zone("EST5EDT_typo"));
        assert!(!is_valid_zone(""));
    }

    #[test]
    fn a_reasonable_spelling_is_accepted_and_canonicalised() {
        assert_eq!(
            normalize_zone("africa/nairobi").as_deref(),
            Some("Africa/Nairobi")
        );
        assert_eq!(normalize_zone("  UTC  ").as_deref(), Some("UTC"));
        assert_eq!(normalize_zone("Mars/Olympus"), None);
    }

    // ── Time ────────────────────────────────────────────────────────────

    #[test]
    fn an_offset_follows_the_date_not_the_zone() {
        let jan = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let jul = Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap();
        assert_eq!(
            offset_label("Europe/London", jan).as_deref(),
            Some("+00:00")
        );
        assert_eq!(
            offset_label("Europe/London", jul).as_deref(),
            Some("+01:00")
        );
        // A zone that does not observe it stays put.
        assert_eq!(
            offset_label("Africa/Nairobi", jan).as_deref(),
            Some("+03:00")
        );
        assert_eq!(
            offset_label("Africa/Nairobi", jul).as_deref(),
            Some("+03:00")
        );
    }

    #[test]
    fn a_half_hour_offset_is_formatted_correctly() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        assert_eq!(offset_label("Asia/Kolkata", now).as_deref(), Some("+05:30"));
        assert_eq!(
            offset_label("Asia/Kathmandu", now).as_deref(),
            Some("+05:45")
        );
        assert_eq!(
            offset_label("Pacific/Marquesas", now).as_deref(),
            Some("-09:30")
        );
    }

    #[test]
    fn a_zone_that_is_not_real_has_no_time_and_no_offset() {
        let now = Utc::now();
        assert!(now_in("Mars/Olympus", now).is_none());
        assert!(offset_label("Mars/Olympus", now).is_none());
    }

    #[test]
    fn the_catalogue_carries_the_offset_and_the_place_together() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let all = zone_catalogue(now);
        let nairobi = all.iter().find(|c| c.zone == "Africa/Nairobi").unwrap();
        assert_eq!(nairobi.offset, "+03:00");
        assert_eq!(nairobi.place, "Nairobi");
        // UTC is a zone but not a place, and must not be given a made-up one.
        let utc = all.iter().find(|c| c.zone == "UTC").unwrap();
        assert_eq!(utc.place, "");
    }

    #[test]
    fn a_zone_alone_is_enough_to_ask_about_the_weather() {
        let target = resolve(&with("", "Africa/Nairobi", 0.0, 0.0)).weather_target();
        assert_eq!(target, Some((0.0, 0.0, "Nairobi".to_string())));
    }

    #[test]
    fn coordinates_without_a_name_are_labelled_by_position() {
        let target = resolve(&with("", "UTC", -1.286, 36.817)).weather_target();
        assert_eq!(target, Some((-1.286, 36.817, "-1.286, 36.817".to_string())));
    }

    #[test]
    fn a_pond_that_knows_nothing_has_nothing_to_ask() {
        assert_eq!(resolve(&with("", "UTC", 0.0, 0.0)).weather_target(), None);
    }

    #[test]
    fn the_two_styles_are_both_correct_and_differ_only_in_shape() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        let iso = offset_label_styled("Africa/Nairobi", now, OffsetStyle::Iso);
        let prose = offset_label_styled("Africa/Nairobi", now, OffsetStyle::Prose);
        assert_eq!(iso.as_deref(), Some("+03:00"));
        assert_eq!(prose.as_deref(), Some("UTC+3"));
        assert_eq!(
            offset_label_styled("UTC", now, OffsetStyle::Prose).as_deref(),
            Some("UTC+0")
        );
    }

    #[test]
    fn a_negative_half_hour_offset_keeps_its_sign_in_both_styles() {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap();
        assert_eq!(
            offset_label_styled("Pacific/Marquesas", now, OffsetStyle::Iso).as_deref(),
            Some("-09:30")
        );
        assert_eq!(
            offset_label_styled("Pacific/Marquesas", now, OffsetStyle::Prose).as_deref(),
            Some("UTC-9:30")
        );
        assert_eq!(
            offset_label_styled("Asia/Kathmandu", now, OffsetStyle::Prose).as_deref(),
            Some("UTC+5:45")
        );
    }

    // ── Place ───────────────────────────────────────────────────────────

    #[test]
    fn a_typed_name_wins() {
        let l = resolve(&with("Kisumu", "Africa/Nairobi", 0.0, 0.0));
        assert_eq!(l.name, "Kisumu");
        assert_eq!(l.origin, Origin::Configured);
    }

    #[test]
    fn an_empty_name_falls_back_to_the_timezone() {
        let l = resolve(&with("", "Africa/Nairobi", 0.0, 0.0));
        assert_eq!(l.name, "Nairobi");
        assert_eq!(l.origin, Origin::Timezone);
        assert_eq!(l.describe(), Some("Nairobi"));
    }

    #[test]
    fn underscores_are_not_shown_to_anybody() {
        assert_eq!(
            place_from_timezone("America/New_York").as_deref(),
            Some("New York")
        );
    }

    #[test]
    fn utc_is_not_a_place() {
        let l = resolve(&with("", "UTC", 0.0, 0.0));
        assert_eq!(l.origin, Origin::Unknown);
        assert!(!l.is_named());
        assert_eq!(l.describe(), None);
    }

    #[test]
    fn an_empty_timezone_is_utc_rather_than_blank() {
        assert_eq!(resolve(&with("", "   ", 0.0, 0.0)).timezone, "UTC");
    }

    #[test]
    fn null_island_counts_as_unset() {
        assert!(!resolve(&with("Nairobi", "Africa/Nairobi", 0.0, 0.0)).has_coordinates());
        assert!(resolve(&with("Nairobi", "Africa/Nairobi", -1.286, 36.817)).has_coordinates());
        // One axis is enough — the prime meridian runs through inhabited places.
        assert!(resolve(&with("Accra", "Africa/Accra", 5.6, 0.0)).has_coordinates());
    }

    #[test]
    fn whitespace_is_not_a_location() {
        let l = resolve(&with("   ", "Africa/Lagos", 0.0, 0.0));
        assert_eq!(l.name, "Lagos");
        assert_eq!(l.origin, Origin::Timezone);
    }
}
