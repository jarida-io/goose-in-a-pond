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

/// How confident the pond is about the name it is using.
///
/// Carried so a caller can phrase itself honestly. "You are in Nairobi" and
/// "your time zone suggests Nairobi" are different claims, and a tool that
/// cannot tell them apart will state a guess as a fact.
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
    /// Whether the coordinates are usable.
    ///
    /// `(0, 0)` is the struct's default and a real point in the Gulf of Guinea.
    /// Treating it as unset is a deliberate trade: a pond moored there gets to
    /// type its coordinates twice, and every other pond stops asking the
    /// weather for a forecast off the coast of Ghana.
    pub fn has_coordinates(&self) -> bool {
        self.latitude != 0.0 || self.longitude != 0.0
    }

    /// Whether there is a name worth saying out loud.
    pub fn is_named(&self) -> bool {
        !self.name.is_empty()
    }

    /// The household's own day containing `now`, as a UTC half-open pair.
    ///
    /// "Today" is a question about where the pond lives, not about UTC. A pond
    /// in `Africa/Nairobi` counting to UTC midnight would see its calendar card
    /// go quiet at 03:00 local and stay quiet until 03:00 the next day -- the
    /// suggestion would be wrong for three hours every morning, in a way that
    /// looks like an empty diary rather than like a bug.
    ///
    /// Half-open `[start, end)`, so an event at exactly midnight belongs to the
    /// day it opens and to that day only.
    ///
    /// # The two days a year this is not a simple lookup
    ///
    /// A local midnight can be **ambiguous** (a DST fold repeats the hour) or
    /// **absent** (a spring-forward jumps over it). `chrono` reports both, and
    /// neither may be answered with "give up and use `now`": that would return
    /// a zero-length window, which every suggestor reads as an empty day. The
    /// fold takes the earlier of the two instants; the gap steps forward an
    /// hour, which is the first local time that exists. Both keep the window a
    /// real day, which is the property that matters.
    pub fn day_bounds(&self, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
        use chrono::TimeZone;
        let tz: chrono_tz::Tz = self.timezone.parse().unwrap_or(chrono_tz::UTC);
        let start_local = now
            .with_timezone(&tz)
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight exists on every date");
        let resolve = |naive: chrono::NaiveDateTime| -> DateTime<Utc> {
            tz.from_local_datetime(&naive)
                .earliest()
                .or_else(|| {
                    tz.from_local_datetime(&(naive + chrono::Duration::hours(1)))
                        .earliest()
                })
                .map(|dt| dt.with_timezone(&Utc))
                // Unreachable for a real zone: an hour past an absent midnight
                // always exists. `now` rather than a panic because a wrong
                // window is a quiet card and a panic is a dead route.
                .unwrap_or(now)
        };
        (
            resolve(start_local),
            resolve(start_local + chrono::Duration::days(1)),
        )
    }

    /// Coordinates and a label for a weather lookup, or `None` when there is
    /// nothing to ask about.
    ///
    /// This decision was written out twice in `main.rs`, once for the HTTP
    /// server and once for voice mode, as two copies of the same six lines —
    /// and both copies read the raw settings fields, so neither of them knew
    /// about the time-zone fallback. A pond that finished onboarding with a
    /// zone and no weather box was told it had no location by a pond holding
    /// the answer, on both surfaces, in the same words.
    ///
    /// The label is the place name when there is one and the coordinates
    /// otherwise, because the provider geocodes a name on demand and a name is
    /// what a person recognises in a log line.
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

    /// The place, phrased for a person, or `None` when there is nothing to say.
    ///
    /// Callers that used to write their own "not configured" string should use
    /// this and say nothing when it is `None` — an interface that reports its
    /// own missing configuration to a household is talking to the wrong person.
    pub fn describe(&self) -> Option<&str> {
        self.is_named().then_some(self.name.as_str())
    }
}

/// The place name a time zone implies.
///
/// `Africa/Nairobi` → `Nairobi`; `America/New_York` → `New York`. Zones without
/// a region part (`UTC`) imply nothing, which is correct — UTC is not a place
/// anybody lives.
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

/// Every IANA zone this build knows, sorted, e.g. `Africa/Nairobi`.
///
/// The IANA database rather than a curated list, because a curated list is a
/// promise that somebody will keep curating it, and nobody did: the three that
/// existed had drifted apart and none of them held `Africa/Kampala`. The cost
/// of the full set is a `<select>` with several hundred entries, which is a UI
/// problem with UI answers (grouping, search) rather than a reason to tell a
/// household its own zone does not exist.
pub fn zones() -> Vec<&'static str> {
    let mut all: Vec<&'static str> = chrono_tz::TZ_VARIANTS.iter().map(|z| z.name()).collect();
    all.sort_unstable();
    all
}

/// Whether `zone` is a real IANA zone.
///
/// The server had NO validation: any string a client sent was stored, and a
/// misspelled zone reached the cron scheduler as a schedule that would never
/// fire, with nothing anywhere saying why.
pub fn is_valid_zone(zone: &str) -> bool {
    zone.trim().parse::<chrono_tz::Tz>().is_ok()
}

/// Canonical spelling for a zone a person or an older client may have typed.
///
/// Accepts the exact name and a case-insensitive match; returns `None` for
/// anything the database does not hold. Case-insensitivity is here because
/// `africa/nairobi` is a reasonable thing to type and an unreasonable thing to
/// reject, not because zone names are case-insensitive — they are not.
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
///
/// Callers were each doing their own `parse::<Tz>()` and each choosing a
/// different thing to do when it failed — one defaulted to UTC, one dropped the
/// schedule, one formatted the error into a prompt. One function, one answer.
pub fn now_in(zone: &str, now: DateTime<Utc>) -> Option<DateTime<chrono_tz::Tz>> {
    let tz: chrono_tz::Tz = zone.trim().parse().ok()?;
    Some(now.with_timezone(&tz))
}

/// Which spelling of an offset a caller needs.
///
/// Both are correct and aimed at different readers, so this is a parameter
/// rather than one being a prettier version of the other. Three hand-rolled
/// implementations existed when this was added — this module's, `world_clock`'s
/// and `get_current_time`'s — and they rendered the same instant three ways in
/// one conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetStyle {
    /// `+03:00`, `-09:30`. Fixed width, always minutes. For pickers and data.
    Iso,
    /// `UTC+3`, `UTC-9:30`. Hours unpadded, minutes only when non-zero. For
    /// prose a model reads aloud.
    Prose,
}

/// Write an already-resolved offset in the requested style.
///
/// Takes the offset rather than a zone so a caller that has parsed a zone does
/// not parse it twice. The sign handling is the part worth centralising: it is
/// integer arithmetic on a possibly-negative second count, and the hand-rolled
/// copies each got the half-hour zones subtly different.
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

/// The UTC offset in `zone` right now, as `+03:00`.
///
/// Computed for an instant rather than stored, because an offset is not a
/// property of a zone: half the world changes its offset twice a year, and a
/// cached `+01:00` for `Europe/London` is wrong for four months of it.
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

/// The whole catalogue, ready to render.
///
/// Offsets are resolved against `now` and handed out together, so a picker does
/// not do several hundred zone lookups of its own and does not have to know
/// that an offset depends on the date.
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

    /// The reason the hand-maintained lists were replaced: none of the three
    /// held this zone, so a household in Kampala could not pick its own.
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

    /// The server stored whatever it was sent, so a typo became a schedule
    /// that would never fire and never explain itself.
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

    /// An offset is a property of an INSTANT, not of a zone. London is +00:00
    /// in January and +01:00 in July, and a cached answer is wrong for months.
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

    /// Not every offset is a whole hour, and a formatter that assumed so would
    /// tell most of India and all of Nepal the wrong time.
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

    /// Both copies of the weather wiring gated on the raw fields, so an
    /// onboarded pond that had only ever been given a time zone was refused
    /// weather by a pond that knew which city it was in.
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

    /// The three spellings that existed before this: the picker's `+03:00`,
    /// world_clock's `UTC+3`, and get_current_time's `UTC+03:00`.
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

    /// Sign handling on a negative half-hour offset — the arithmetic each
    /// hand-rolled copy got subtly different.
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

    /// The defect this service exists for: the pond knew, and said it did not.
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

#[cfg(test)]
mod day_bounds_tests {
    use super::*;
    use chrono::TimeZone;

    fn at(zone: &str, iso: &str) -> (Location, DateTime<Utc>) {
        let loc = Location {
            name: String::new(),
            latitude: 0.0,
            longitude: 0.0,
            timezone: zone.to_string(),
            origin: Origin::Unknown,
        };
        (loc, iso.parse::<DateTime<Utc>>().unwrap())
    }

    #[test]
    fn the_day_is_the_households_own_and_not_utcs() {
        // 02:00 UTC is already 05:00 in Nairobi, so the household's day opened
        // three hours ago at 21:00 UTC the previous date. Counting to UTC
        // midnight would put this instant in yesterday's window.
        let (loc, now) = at("Africa/Nairobi", "2026-09-15T02:00:00Z");
        let (start, end) = loc.day_bounds(now);
        assert_eq!(start.to_rfc3339(), "2026-09-14T21:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-09-15T21:00:00+00:00");
        assert!(start <= now && now < end, "now fell outside its own day");
    }

    #[test]
    fn the_window_is_exactly_one_day_long_in_a_zone_with_no_dst() {
        let (loc, now) = at("Africa/Nairobi", "2026-09-15T12:00:00Z");
        let (start, end) = loc.day_bounds(now);
        assert_eq!(end - start, chrono::Duration::hours(24));
    }

    #[test]
    fn a_spring_forward_that_skips_local_midnight_still_yields_a_real_day() {
        // Lord Howe and Havana skip midnight itself; Havana's 2026 transition
        // moves 00:00 to 01:00 on 8 March, so local midnight does not exist.
        let (loc, now) = at("America/Havana", "2026-03-08T12:00:00Z");
        let (start, end) = loc.day_bounds(now);
        assert!(
            start < end,
            "an absent midnight collapsed the window to nothing"
        );
        assert!(
            end - start >= chrono::Duration::hours(22),
            "the window was not a day: {:?}",
            end - start
        );
        assert!(start <= now && now < end);
    }

    #[test]
    fn an_autumn_fold_that_repeats_local_midnight_takes_the_earlier_instant() {
        // Havana's 2026 fold repeats 00:00 on 1 November.
        let (loc, now) = at("America/Havana", "2026-11-01T12:00:00Z");
        let (start, end) = loc.day_bounds(now);
        let tz: chrono_tz::Tz = "America/Havana".parse().unwrap();
        let naive = start
            .with_timezone(&tz)
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let earliest = tz.from_local_datetime(&naive).earliest().unwrap();
        assert_eq!(start, earliest.with_timezone(&Utc));
        assert!(start < end);
        assert!(start <= now && now < end);
    }

    #[test]
    fn an_unparseable_zone_falls_back_to_utc_rather_than_to_nothing() {
        let (loc, now) = at("Not/AZone", "2026-09-15T12:00:00Z");
        let (start, end) = loc.day_bounds(now);
        assert_eq!(start.to_rfc3339(), "2026-09-15T00:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-09-16T00:00:00+00:00");
    }

    #[test]
    fn the_window_never_has_zero_length() {
        // The failure this guards is the one that reads like an empty diary
        // rather than like a bug: a zero-length window makes every context
        // suggestor silent with a plausible reason.
        for zone in [
            "UTC",
            "Africa/Nairobi",
            "America/Havana",
            "Australia/Lord_Howe",
            "Pacific/Chatham",
            "America/Santiago",
            "Europe/Dublin",
        ] {
            for iso in [
                "2026-03-08T12:00:00Z",
                "2026-11-01T12:00:00Z",
                "2026-09-15T12:00:00Z",
            ] {
                let (loc, now) = at(zone, iso);
                let (start, end) = loc.day_bounds(now);
                assert!(start < end, "{zone} at {iso} produced an empty day");
                assert!(
                    start <= now && now < end,
                    "{zone} at {iso} excluded its own now"
                );
            }
        }
    }
}
