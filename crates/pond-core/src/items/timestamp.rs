//! The one timestamp format of the `items` tables, and a reader for the four formats the detail
//! tables hold: SQLite's `datetime('now')`, RFC 3339 at any precision and offset, and Unix time.

use chrono::{DateTime, NaiveDateTime, SecondsFormat, TimeZone, Utc};

/// RFC 3339 in UTC with milliseconds and `Z`: sorts as text, and SQLite's date functions read it.
pub fn format(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn now() -> String {
    format(Utc::now())
}

/// `None` for anything it cannot read; a caller decides what an unreadable date means.
pub fn parse_any(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Some(at.with_timezone(&Utc));
    }
    for layout in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, layout) {
            return Some(naive.and_utc());
        }
    }
    if let Ok(at) = DateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f%:z") {
        return Some(at.with_timezone(&Utc));
    }
    let seconds: i64 = raw.parse().ok()?;
    // Past 1e11 is milliseconds: as seconds it would be the year 5138.
    if seconds.abs() >= 100_000_000_000 {
        Utc.timestamp_millis_opt(seconds).single()
    } else {
        Utc.timestamp_opt(seconds, 0).single()
    }
}

/// [`parse_any`], then [`format`].
pub fn normalise(raw: &str) -> Option<String> {
    parse_any(raw).map(format)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_stored_format_reads_as_the_same_instant() {
        let expected = "2026-11-05T09:30:15.000Z";
        for raw in [
            "2026-11-05 09:30:15",
            "2026-11-05T09:30:15",
            "2026-11-05T09:30:15Z",
            "2026-11-05T09:30:15+00:00",
            "2026-11-05T12:30:15+03:00",
            "2026-11-05T09:30:15.000000000+00:00",
            "2026-11-05 12:30:15+03:00",
            "1793871015",
            "1793871015000",
        ] {
            assert_eq!(normalise(raw).as_deref(), Some(expected), "{raw:?}");
        }
    }

    #[test]
    fn fractions_survive_to_the_millisecond() {
        assert_eq!(
            normalise("2026-11-05 09:30:15.123456").as_deref(),
            Some("2026-11-05T09:30:15.123Z")
        );
    }

    #[test]
    fn unreadable_dates_are_none_not_now() {
        for raw in ["", "  ", "yesterday", "2026-13-40 99:00:00"] {
            assert_eq!(parse_any(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn the_format_sorts_as_text() {
        let a = format(Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap());
        let b = format(Utc.with_ymd_and_hms(2026, 10, 2, 3, 4, 5).unwrap());
        assert!(a < b);
        assert_eq!(a.len(), "2026-01-02T03:04:05.000Z".len());
    }
}
