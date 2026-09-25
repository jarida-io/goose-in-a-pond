//! Just enough iCalendar to turn a VEVENT into a [`RawItem`]. No RRULE expansion on purpose:
//! the `calendar-query` REPORT has the server expand recurrences in UTC (RFC 4791 §9.6.5).
//!
//! [`RawItem`]: pond_core::context::ingest::RawItem

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use pond_core::context::domain::ItemKind;
use pond_core::context::ingest::RawItem;

/// One property line, after unfolding.
struct Line<'a> {
    name: &'a str,
    params: &'a str,
    value: String,
}

/// Undo RFC 5545 line folding; must run first, as a fold can split a word or an escape.
fn unfold(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in body.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        match line.strip_prefix([' ', '\t']) {
            Some(rest) => {
                if let Some(last) = out.last_mut() {
                    last.push_str(rest);
                }
            }
            None => out.push(line.to_string()),
        }
    }
    out
}

/// Undo RFC 5545 TEXT escaping.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') | Some('N') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(',') => out.push(','),
            Some(';') => out.push(';'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

fn parse_line(line: &str) -> Option<Line<'_>> {
    // A quoted param value may contain ':', so scan instead of `split_once`.
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let (head, value) = line.split_at(colon);
    let value = &value[1..];
    let (name, params) = match head.split_once(';') {
        Some((n, p)) => (n, p),
        None => (head, ""),
    };
    Some(Line {
        name: name.trim(),
        params,
        value: unescape(value),
    })
}

/// Parse DATE-TIME or DATE (midnight) as UTC; a naive value is assumed UTC, not dropped.
fn parse_datetime(value: &str) -> Option<DateTime<Utc>> {
    let v = value.trim();
    if let Some(stripped) = v.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S")
            .ok()
            .map(|n| Utc.from_utc_datetime(&n));
    }
    if let Ok(n) = NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S") {
        return Some(Utc.from_utc_datetime(&n));
    }
    NaiveDate::parse_from_str(v, "%Y%m%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|n| Utc.from_utc_datetime(&n))
}

/// Display name for a calendar address: `CN=` if present, else the address minus `mailto:`.
fn participant(line: &Line<'_>) -> Option<String> {
    for part in line.params.split(';') {
        if let Some(cn) = part.strip_prefix("CN=") {
            let cn = cn.trim_matches('"').trim();
            if !cn.is_empty() {
                return Some(cn.to_string());
            }
        }
    }
    let addr = line
        .value
        .trim()
        .strip_prefix("mailto:")
        .unwrap_or(line.value.trim());
    (!addr.is_empty()).then(|| addr.to_string())
}

/// Every VEVENT as an ingestable item; skips any lacking a UID (the re-sync key) or a start.
pub fn events_from_ics(body: &str) -> Vec<RawItem> {
    let mut items = Vec::new();
    let mut in_event = false;
    let mut uid = None::<String>;
    let mut recurrence_id = None::<String>;
    let mut summary = String::new();
    let mut description = String::new();
    let mut location = String::new();
    let mut start = None::<DateTime<Utc>>;
    let mut end = None::<DateTime<Utc>>;
    let mut participants: Vec<String> = Vec::new();

    for line in unfold(body) {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_event = true;
            uid = None;
            recurrence_id = None;
            summary.clear();
            description.clear();
            location.clear();
            start = None;
            end = None;
            participants.clear();
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VEVENT") {
            in_event = false;
            if let (Some(uid), Some(occurred_at)) = (uid.take(), start) {
                // Expanded recurrences share their parent's UID, so the instance joins the key.
                let external_id = match &recurrence_id {
                    Some(rid) => format!("{uid}:{rid}"),
                    None => uid,
                };
                let title = if summary.trim().is_empty() {
                    "(untitled event)".to_string()
                } else {
                    summary.trim().to_string()
                };
                items.push(RawItem {
                    external_id,
                    kind: ItemKind::Event,
                    occurred_at,
                    title,
                    body: event_body(occurred_at, end, &location, &description),
                    participants: std::mem::take(&mut participants),
                });
            }
            continue;
        }
        if !in_event {
            continue;
        }
        let Some(parsed) = parse_line(trimmed) else {
            continue;
        };
        match parsed.name.to_ascii_uppercase().as_str() {
            "UID" => uid = Some(parsed.value.trim().to_string()),
            "RECURRENCE-ID" => recurrence_id = Some(parsed.value.trim().to_string()),
            "SUMMARY" => summary = parsed.value.clone(),
            "DESCRIPTION" => description = parsed.value.clone(),
            "LOCATION" => location = parsed.value.clone(),
            "DTSTART" => start = parse_datetime(&parsed.value),
            "DTEND" => end = parse_datetime(&parsed.value),
            "ATTENDEE" | "ORGANIZER" => {
                if let Some(p) = participant(&parsed) {
                    if !participants.contains(&p) {
                        participants.push(p);
                    }
                }
            }
            _ => {}
        }
    }
    items
}

/// Item body as prose: it is half of `embedding_text`, so a field dump would hurt retrieval.
fn event_body(
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    location: &str,
    description: &str,
) -> String {
    let mut parts = Vec::new();
    let when = match end {
        Some(e) if e > start => format!(
            "When: {} to {} UTC",
            start.format("%A %-d %B %Y, %H:%M"),
            e.format("%H:%M")
        ),
        _ => format!("When: {} UTC", start.format("%A %-d %B %Y, %H:%M")),
    };
    parts.push(when);
    if !location.trim().is_empty() {
        parts.push(format!("Where: {}", location.trim()));
    }
    if !description.trim().is_empty() {
        parts.push(description.trim().to_string());
    }
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "BEGIN:VCALENDAR\r\n\
BEGIN:VEVENT\r\n\
UID:abc-123\r\n\
SUMMARY:Dentist\r\n\
DTSTART:20260817T093000Z\r\n\
DTEND:20260817T101500Z\r\n\
LOCATION:Riverside Clinic\r\n\
DESCRIPTION:Bring the referral letter\r\n\
ATTENDEE;CN=\"Liz Adera\":mailto:liz@example.org\r\n\
END:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn a_whole_event_becomes_one_item() {
        let items = events_from_ics(ONE);
        assert_eq!(items.len(), 1);
        let e = &items[0];
        assert_eq!(e.external_id, "abc-123");
        assert_eq!(e.kind, ItemKind::Event);
        assert_eq!(e.title, "Dentist");
        assert_eq!(e.participants, vec!["Liz Adera".to_string()]);
        assert_eq!(e.occurred_at.to_rfc3339(), "2026-08-17T09:30:00+00:00");
    }

    #[test]
    fn the_body_reads_like_a_sentence_and_carries_place_and_time() {
        let e = &events_from_ics(ONE)[0];
        assert!(
            e.body.contains("Monday 17 August 2026, 09:30"),
            "{}",
            e.body
        );
        assert!(e.body.contains("to 10:15"), "{}", e.body);
        assert!(e.body.contains("Where: Riverside Clinic"), "{}", e.body);
        assert!(e.body.contains("Bring the referral letter"), "{}", e.body);
    }

    #[test]
    fn folded_lines_are_rejoined_before_anything_reads_them() {
        let ics = "BEGIN:VEVENT\r\nUID:f1\r\nSUMMARY:Quarterly plan\r\n review with the team\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\n";
        let e = &events_from_ics(ics)[0];
        assert_eq!(e.title, "Quarterly planreview with the team");
    }

    #[test]
    fn escapes_are_undone() {
        let ics = "BEGIN:VEVENT\r\nUID:e1\r\nSUMMARY:Lunch\\, then a walk\r\nDESCRIPTION:One\\nTwo\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\n";
        let e = &events_from_ics(ics)[0];
        assert_eq!(e.title, "Lunch, then a walk");
        assert!(e.body.contains("One\nTwo"), "{}", e.body);
    }

    #[test]
    fn expanded_recurrences_do_not_collapse_onto_one_id() {
        let ics = "BEGIN:VEVENT\r\nUID:weekly\r\nRECURRENCE-ID:20260901T080000Z\r\nSUMMARY:Standup\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:weekly\r\nRECURRENCE-ID:20260908T080000Z\r\nSUMMARY:Standup\r\nDTSTART:20260908T080000Z\r\nEND:VEVENT\r\n";
        let items = events_from_ics(ics);
        assert_eq!(items.len(), 2);
        assert_ne!(
            items[0].external_id, items[1].external_id,
            "both occurrences share an external_id, so re-sync would keep one"
        );
    }

    #[test]
    fn an_event_without_a_uid_or_a_start_is_skipped_not_defaulted() {
        let no_uid = "BEGIN:VEVENT\r\nSUMMARY:Ghost\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\n";
        let no_start = "BEGIN:VEVENT\r\nUID:x\r\nSUMMARY:Ghost\r\nEND:VEVENT\r\n";
        assert!(events_from_ics(no_uid).is_empty());
        assert!(events_from_ics(no_start).is_empty());
    }

    #[test]
    fn an_all_day_event_is_midnight_utc() {
        let ics = "BEGIN:VEVENT\r\nUID:d1\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20261225\r\nEND:VEVENT\r\n";
        let e = &events_from_ics(ics)[0];
        assert_eq!(e.occurred_at.to_rfc3339(), "2026-12-25T00:00:00+00:00");
    }

    #[test]
    fn a_colon_inside_a_quoted_parameter_does_not_end_the_name() {
        let ics = "BEGIN:VEVENT\r\nUID:q1\r\nATTENDEE;CN=\"Ochieng: the elder\":mailto:o@example.org\r\nSUMMARY:Call\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\n";
        let e = &events_from_ics(ics)[0];
        assert_eq!(e.participants, vec!["Ochieng: the elder".to_string()]);
    }

    #[test]
    fn an_attendee_without_a_name_falls_back_to_the_address_without_the_scheme() {
        let ics = "BEGIN:VEVENT\r\nUID:a1\r\nATTENDEE:mailto:sam@example.org\r\nSUMMARY:Call\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\n";
        let e = &events_from_ics(ics)[0];
        assert_eq!(e.participants, vec!["sam@example.org".to_string()]);
    }

    #[test]
    fn properties_outside_an_event_are_ignored() {
        let ics = "BEGIN:VCALENDAR\r\nSUMMARY:Not an event\r\nBEGIN:VEVENT\r\nUID:s1\r\nSUMMARY:Real\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\nLOCATION:Nowhere\r\nEND:VCALENDAR\r\n";
        let items = events_from_ics(ics);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Real");
        assert!(!items[0].body.contains("Nowhere"), "{}", items[0].body);
    }

    #[test]
    fn an_empty_summary_gets_a_placeholder_rather_than_an_empty_title() {
        let ics = "BEGIN:VEVENT\r\nUID:n1\r\nDTSTART:20260901T080000Z\r\nEND:VEVENT\r\n";
        assert_eq!(events_from_ics(ics)[0].title, "(untitled event)");
    }
}
