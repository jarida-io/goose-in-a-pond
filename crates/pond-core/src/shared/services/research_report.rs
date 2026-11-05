//! A long task's report, put together by the pond rather than written in one reply: the summary,
//! then each section a step drafted, in plan order, then the sources they cite. So a report is as
//! long as its plan, never as long as one context window allows.
use crate::shared::domain::long_task::{ArtifactKind, ArtifactSummary, PlanStep};
use crate::shared::services::long_task_runner::{result_artifact, RESULT_TITLE};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Utc};

/// The data artifact a plan's dated events are saved in, which the calendar export reads.
pub const CALENDAR_TITLE: &str = "Calendar";

/// Longest title a report takes from its goal.
const TITLE_CHARS: usize = 80;

/// Which stored artifacts make up a task's report, by id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReportParts {
    pub result: Option<String>,
    /// Drafted sections in the order their steps were planned.
    pub sections: Vec<String>,
    pub calendars: Vec<String>,
}

/// A report's sections are its drafts, ordered by their steps' place in the plan; a draft whose
/// step a replan deleted keeps its place by when it was written, after the planned ones.
pub fn report_parts(steps: &[PlanStep], artifacts: &[ArtifactSummary]) -> ReportParts {
    let result = result_artifact(artifacts).map(|a| a.id.clone());
    let ordinal = |a: &ArtifactSummary| {
        a.step_key
            .as_ref()
            .and_then(|key| steps.iter().find(|s| s.key() == key))
            .map(|s| s.ordinal())
    };
    let mut drafts: Vec<&ArtifactSummary> = artifacts
        .iter()
        // A failed try's Result stays stored; no summary is ever a section.
        .filter(|a| {
            a.kind == ArtifactKind::Draft && !a.title.trim().eq_ignore_ascii_case(RESULT_TITLE)
        })
        .collect();
    drafts.sort_by_key(|a| (ordinal(a).is_none(), ordinal(a), a.created_at));
    ReportParts {
        result,
        sections: drafts.into_iter().map(|a| a.id.clone()).collect(),
        calendars: artifacts
            .iter()
            .filter(|a| {
                a.kind == ArtifactKind::Data && a.title.eq_ignore_ascii_case(CALENDAR_TITLE)
            })
            .map(|a| a.id.clone())
            .collect(),
    }
}

/// The report's title: the goal's first line, cut to a title's length.
pub fn report_title(goal: &str) -> String {
    let line = goal
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("Report")
        .trim();
    // A question's first clause names it ("Plan a trip to Mombasa, from Friday...").
    let line = line
        .find([',', ':', ';', '.', '?'])
        .map(|at| line[..at].trim_end())
        .filter(|clause| clause.chars().count() >= 20)
        .unwrap_or(line);
    if line.chars().count() <= TITLE_CHARS {
        return line.to_string();
    }
    let cut: String = line.chars().take(TITLE_CHARS - 3).collect();
    format!("{}...", cut.trim_end())
}

/// The title an assembled report leads with.
pub fn assembled_title(markdown: &str) -> String {
    markdown
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("Report")
        .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub title: String,
    pub url: String,
}

/// The title the summary gives itself on a leading `# ` line, and the summary without it.
fn titled(summary: &str) -> (Option<&str>, &str) {
    let summary = summary.trim();
    match summary.strip_prefix("# ") {
        Some(rest) => {
            let (title, body) = rest.split_once('\n').unwrap_or((rest, ""));
            let title = title.trim();
            ((!title.is_empty()).then_some(title), body.trim())
        }
        None => (None, summary),
    }
}

/// Whether the summary is already the whole report: a last step that compiled the sections into
/// its Result, though told not to, wrote them once, and appending them would say it all twice.
pub fn holds_its_sections(summary: &str, sections: &[(String, String)]) -> bool {
    let words = |text: &str| text.split_whitespace().count();
    let drafted: usize = sections.iter().map(|(_, body)| words(body)).sum();
    drafted > 0 && words(summary) * 5 >= drafted * 4
}

/// The whole report in Markdown: title, summary, sections, then the sources they link to. The
/// title is the one the summary gives itself, else the goal's first line.
pub fn assemble(goal: &str, summary: Option<&str>, sections: &[(String, String)]) -> String {
    let (own_title, summary) = summary.map(titled).unwrap_or((None, ""));
    let title = own_title
        .map(report_title)
        .unwrap_or_else(|| report_title(goal));
    let mut out = format!("# {title}\n\n");
    if !summary.is_empty() {
        out.push_str(summary);
        out.push_str("\n\n");
    }
    let compiled = holds_its_sections(summary, sections);
    for (title, body) in sections.iter().filter(|_| !compiled) {
        let body = body.trim();
        if body.is_empty() {
            continue;
        }
        let body = below_the_title(body);
        // A section that brings its own heading keeps it; one that does not gets its title.
        if !body.starts_with('#') {
            out.push_str(&format!("## {}\n\n", title.trim()));
        }
        out.push_str(&body);
        out.push_str("\n\n");
    }
    let cited = sources(&out);
    if !cited.is_empty() {
        out.push_str("## Sources\n\n");
        for source in cited {
            out.push_str(&format!("- [{}]({})\n", source.title, source.url));
        }
    }
    out.trim_end().to_string() + "\n"
}

/// A section's headings moved down a level when it uses the report's own level for itself, so the
/// title stays the only `# ` heading. Lines inside code fences are left alone.
fn below_the_title(body: &str) -> String {
    let level = |line: &str| {
        let hashes = line.chars().take_while(|c| *c == '#').count();
        (hashes > 0 && line[hashes..].starts_with(' ')).then_some(hashes)
    };
    let mut fenced = false;
    let mut top = usize::MAX;
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            top = top.min(level(line).unwrap_or(usize::MAX));
        }
    }
    if top != 1 {
        return body.to_string();
    }
    fenced = false;
    let mut out = String::with_capacity(body.len() + 16);
    for line in body.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if !fenced && level(line).is_some() {
            out.push('#');
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// Every web link the text cites as `[title](url)`, once each, in the order first cited.
pub fn sources(markdown: &str) -> Vec<Source> {
    let mut found: Vec<Source> = Vec::new();
    let mut rest = markdown;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find("](") else { break };
        let title = &after[..close];
        let target = &after[close + 2..];
        // Wikipedia's titles bracket their disambiguation, so the link ends at the `)` that
        // closes it, not the first one inside it.
        let mut depth = 0usize;
        let Some(end) = target.char_indices().find_map(|(i, c)| match c {
            '(' => {
                depth += 1;
                None
            }
            ')' if depth == 0 => Some(i),
            ')' => {
                depth -= 1;
                None
            }
            _ => None,
        }) else {
            break;
        };
        let url = target[..end].trim();
        let is_web = url.starts_with("https://") || url.starts_with("http://");
        // A map link names a place in the plan; it is not something the report read.
        let is_map = url.contains("openstreetmap.org/") || url.contains("google.com/maps");
        if is_web && !is_map && !title.contains('[') && !found.iter().any(|s| s.url == url) {
            let title = title.trim();
            found.push(Source {
                title: if title.is_empty() {
                    url.to_string()
                } else {
                    title.to_string()
                },
                url: url.to_string(),
            });
        }
        rest = &target[end..];
    }
    found
}

/// One dated event a plan saved for the calendar export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarEvent {
    pub title: String,
    pub when: EventTime,
    pub location: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventTime {
    /// A day with no time: a whole-day event ending after `last`.
    Days { first: NaiveDate, last: NaiveDate },
    At {
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    },
    /// A wall-clock time with no zone, as an itinerary writes it: a calendar shows it as the
    /// time where its owner is, which is where the traveller will be.
    Local {
        start: NaiveDateTime,
        end: NaiveDateTime,
    },
}

/// The events in a calendar artifact: a JSON array or one JSON object per line, each with
/// `title`, `start` and optional `end`, `location` and `notes`. An unreadable entry is skipped,
/// because a model wrote these and one bad line must not lose the rest.
pub fn calendar_events(body: &str) -> Vec<CalendarEvent> {
    let values: Vec<serde_json::Value> =
        match serde_json::from_str::<serde_json::Value>(body.trim()) {
            Ok(serde_json::Value::Array(items)) => items,
            Ok(single @ serde_json::Value::Object(_)) => vec![single],
            _ => body
                .lines()
                .filter_map(|line| serde_json::from_str(line.trim().trim_end_matches(',')).ok())
                .collect(),
        };
    values.iter().filter_map(event_from).collect()
}

fn text_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn event_from(value: &serde_json::Value) -> Option<CalendarEvent> {
    let title = text_field(value, "title")?;
    let start = text_field(value, "start")?;
    let end = text_field(value, "end");
    let when = if let Ok(start) = DateTime::parse_from_rfc3339(&start) {
        let start = start.with_timezone(&Utc);
        let end = end
            .and_then(|e| DateTime::parse_from_rfc3339(&e).ok())
            .map(|e| e.with_timezone(&Utc))
            .filter(|e| *e > start)
            .unwrap_or(start + chrono::Duration::hours(1));
        EventTime::At { start, end }
    } else {
        let first = NaiveDate::parse_from_str(&start, "%Y-%m-%d").ok()?;
        let last = end
            .and_then(|e| NaiveDate::parse_from_str(&e, "%Y-%m-%d").ok())
            .filter(|l| *l >= first)
            .unwrap_or(first);
        EventTime::Days { first, last }
    };
    Some(CalendarEvent {
        title,
        when,
        location: text_field(value, "location"),
        notes: text_field(value, "notes"),
    })
}

/// A plan's timed lines as events, for a plan whose steps saved no calendar: each heading that
/// names a date opens a day, and each `- HH:MM what` line under it is an event lasting until the
/// next one that day, or an hour. A date with no year takes the first one on or after `since`.
pub fn itinerary_events(markdown: &str, since: NaiveDate) -> Vec<CalendarEvent> {
    let mut events: Vec<CalendarEvent> = Vec::new();
    let mut day: Option<NaiveDate> = None;
    let mut day_starts = 0;
    let close_day = |events: &mut Vec<CalendarEvent>, from: usize| {
        for i in from..events.len() {
            let next_start = events.get(i + 1).map(|e| e.when);
            if let EventTime::Local { start, end } = &mut events[i].when {
                if let Some(EventTime::Local { start: next, .. }) = next_start {
                    if next > *start {
                        *end = next;
                    }
                }
                if *end <= *start {
                    *end = *start + chrono::Duration::hours(1);
                }
            }
        }
    };
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            if let Some(date) = heading_date(trimmed.trim_start_matches('#'), since) {
                close_day(&mut events, day_starts);
                day = Some(date);
                day_starts = events.len();
            }
            continue;
        }
        let (Some(date), Some(item)) = (
            day,
            trimmed.strip_prefix("- ").or(trimmed.strip_prefix("* ")),
        ) else {
            continue;
        };
        let item = item.trim_start_matches(['*', ' ']);
        let Some((clock, what)) = item.split_once(' ') else {
            continue;
        };
        let Ok(time) = NaiveTime::parse_from_str(clock.trim_end_matches([':', '*']), "%H:%M")
        else {
            continue;
        };
        let (title, location) = plain_text(what);
        if title.is_empty() {
            continue;
        }
        let start = date.and_time(time);
        events.push(CalendarEvent {
            title,
            when: EventTime::Local { start, end: start },
            location,
            notes: None,
        });
    }
    close_day(&mut events, day_starts);
    events
}

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];

/// The date a heading names, such as "Day 1: Friday, November 13" or "Saturday 14 November 2026".
fn heading_date(heading: &str, since: NaiveDate) -> Option<NaiveDate> {
    let words: Vec<String> = heading
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let at = words.iter().position(|w| {
        MONTHS
            .iter()
            .any(|m| w == m || (w.len() >= 3 && m.starts_with(w.as_str()) && w.len() <= m.len()))
    })?;
    let month = MONTHS
        .iter()
        .position(|m| m.starts_with(words[at].as_str()))? as u32
        + 1;
    let day_of = |w: &String| w.parse::<u32>().ok().filter(|d| (1..=31).contains(d));
    let day = words
        .get(at + 1)
        .and_then(day_of)
        .or_else(|| at.checked_sub(1).and_then(|i| day_of(&words[i])))?;
    let year = words
        .iter()
        .filter_map(|w| w.parse::<i32>().ok())
        .find(|y| (2000..=2100).contains(y));
    match year {
        Some(year) => NaiveDate::from_ymd_opt(year, month, day),
        None => {
            let this = NaiveDate::from_ymd_opt(since.year(), month, day)?;
            if this >= since {
                Some(this)
            } else {
                NaiveDate::from_ymd_opt(since.year() + 1, month, day)
            }
        }
    }
}

/// A line's words without its Markdown, and the first linked place in it.
fn plain_text(text: &str) -> (String, Option<String>) {
    let mut out = String::new();
    let mut place = None;
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after
            .find("](")
            .and_then(|close| after[close + 2..].find(')').map(|end| (close, end)))
        {
            Some((close, end)) => {
                let label = after[..close].trim();
                place.get_or_insert_with(|| label.to_string());
                out.push_str(label);
                rest = &after[close + 2 + end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let words = out.replace(['*', '_', '`'], "");
    let title = words
        .trim()
        .trim_end_matches(['.', ',', ';'])
        .trim()
        .to_string();
    (title, place.filter(|p| !p.is_empty()))
}

/// The events as an iCalendar file (RFC 5545), for a phone's or a desktop's calendar.
pub fn to_ics(task_id: &str, events: &[CalendarEvent], now: DateTime<Utc>) -> String {
    let stamp = now.format("%Y%m%dT%H%M%SZ").to_string();
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Goose In A Pond//Deep research//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
    ];
    for (n, event) in events.iter().enumerate() {
        lines.push("BEGIN:VEVENT".to_string());
        lines.push(format!("UID:{task_id}-{n}@goose-in-a-pond"));
        lines.push(format!("DTSTAMP:{stamp}"));
        match event.when {
            EventTime::At { start, end } => {
                lines.push(format!("DTSTART:{}", start.format("%Y%m%dT%H%M%SZ")));
                lines.push(format!("DTEND:{}", end.format("%Y%m%dT%H%M%SZ")));
            }
            EventTime::Local { start, end } => {
                lines.push(format!("DTSTART:{}", start.format("%Y%m%dT%H%M%S")));
                lines.push(format!("DTEND:{}", end.format("%Y%m%dT%H%M%S")));
            }
            EventTime::Days { first, last } => {
                // DTEND is exclusive for whole days.
                let after = last.succ_opt().unwrap_or(last);
                lines.push(format!("DTSTART;VALUE=DATE:{}", first.format("%Y%m%d")));
                lines.push(format!("DTEND;VALUE=DATE:{}", after.format("%Y%m%d")));
            }
        }
        lines.push(format!("SUMMARY:{}", ics_text(&event.title)));
        if let Some(location) = &event.location {
            lines.push(format!("LOCATION:{}", ics_text(location)));
        }
        if let Some(notes) = &event.notes {
            lines.push(format!("DESCRIPTION:{}", ics_text(notes)));
        }
        lines.push("END:VEVENT".to_string());
    }
    lines.push("END:VCALENDAR".to_string());
    lines
        .iter()
        .map(|l| fold(l))
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

fn ics_text(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace("\r\n", "\\n")
        .replace('\n', "\\n")
}

/// Lines longer than 75 octets continue on the next line after a space, never inside a character.
fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut width = 0;
    for ch in line.chars() {
        let len = ch.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(ch);
        width += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::{PlanStepParts, StepKey, StepState};

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    fn step(key: &str, ordinal: u32) -> PlanStep {
        PlanStep::from_parts(PlanStepParts {
            task_id: "task-1".into(),
            key: key.into(),
            ordinal,
            role: "long-task-worker".into(),
            instructions: "do it".into(),
            inputs: Vec::new(),
            depends_on: Vec::new(),
            acceptance: None,
            state: StepState::Pending,
            attempts: 0,
            summary: None,
            failure: None,
            started_at: None,
            finished_at: None,
            agent: None,
        })
        .unwrap()
    }

    fn artifact(
        id: &str,
        step: Option<&str>,
        kind: ArtifactKind,
        title: &str,
        at: i64,
    ) -> ArtifactSummary {
        ArtifactSummary {
            id: id.into(),
            task_id: "task-1".into(),
            step_key: step.map(|k| StepKey::parse(k).unwrap()),
            kind,
            title: title.into(),
            summary: String::new(),
            tokens: 10,
            created_at: t(at),
        }
    }

    #[test]
    fn sections_follow_the_plan_and_the_summary_and_notes_stay_out_of_them() {
        let steps = [step("intro", 0), step("costs", 1), step("routes", 2)];
        let artifacts = [
            artifact("a-routes", Some("routes"), ArtifactKind::Draft, "Routes", 1),
            artifact(
                "a-notes",
                Some("intro"),
                ArtifactKind::Findings,
                "What I found",
                2,
            ),
            artifact("a-costs", Some("costs"), ArtifactKind::Draft, "Costs", 3),
            artifact("a-orphan", Some("gone"), ArtifactKind::Draft, "Extra", 4),
            artifact("a-tried", Some("routes"), ArtifactKind::Draft, "Result", 5),
            artifact("a-result", Some("routes"), ArtifactKind::Draft, "Result", 5),
            artifact("a-cal", None, ArtifactKind::Data, "calendar", 6),
        ];
        let parts = report_parts(&steps, &artifacts);
        assert_eq!(parts.result.as_deref(), Some("a-result"));
        assert_eq!(parts.sections, ["a-costs", "a-routes", "a-orphan"]);
        assert_eq!(parts.calendars, ["a-cal"]);
    }

    #[test]
    fn a_report_is_the_summary_then_its_sections_then_what_they_cite() {
        let report = assemble(
            "Plan our Mombasa trip\nfor four friends",
            Some("Four days, mostly by the sea."),
            &[
                ("Day one".into(), "Land at [Moi airport](https://example.org/moi).".into()),
                ("Ignored title".into(), "## Day two\n\nFort Jesus, see [Moi airport](https://example.org/moi) and [the fort](https://example.org/fort).".into()),
                ("Empty".into(), "   ".into()),
            ],
        );
        assert!(report.starts_with("# Plan our Mombasa trip\n\nFour days"));
        assert!(report.contains("## Day one\n\nLand at"));
        assert!(
            !report.contains("Ignored title"),
            "a section's own heading wins"
        );
        assert!(!report.contains("## Empty"));
        let sources = report.split("## Sources").nth(1).unwrap();
        assert_eq!(
            sources.matches("https://example.org/moi").count(),
            1,
            "cited once, listed once"
        );
        assert!(sources.contains("[the fort](https://example.org/fort)"));
    }

    #[test]
    fn only_web_links_are_sources() {
        let found = sources(
            "[a](https://a.example) [b](javascript:alert(1)) [c](mailto:x@y) [](http://d.example) \
             [Fort Jesus](https://en.wikipedia.org/wiki/Fort_Jesus_(Mombasa)).",
        );
        assert_eq!(
            found,
            [
                Source {
                    title: "a".into(),
                    url: "https://a.example".into()
                },
                Source {
                    title: "http://d.example".into(),
                    url: "http://d.example".into()
                },
                Source {
                    title: "Fort Jesus".into(),
                    url: "https://en.wikipedia.org/wiki/Fort_Jesus_(Mombasa)".into()
                },
            ]
        );
    }

    #[test]
    fn the_summary_names_the_report_and_map_links_are_not_sources() {
        let report = assemble(
            "Plan a four-day trip to Mombasa for me and three friends from Friday 13 November",
            Some("# Four days in Mombasa\n\nBy the sea, mostly."),
            &[(
                "Itinerary".into(),
                "See [Fort Jesus](https://www.openstreetmap.org/search?query=Fort%20Jesus) and \
                 [its history](https://example.org/fort-jesus)."
                    .into(),
            )],
        );
        assert!(
            report.starts_with("# Four days in Mombasa\n\nBy the sea, mostly."),
            "{report}"
        );
        assert_eq!(assembled_title(&report), "Four days in Mombasa");
        assert_eq!(
            sources(&report),
            [Source {
                title: "its history".into(),
                url: "https://example.org/fort-jesus".into()
            }],
            "a place's map link is not a source"
        );
        assert_eq!(
            assembled_title(&assemble("Compare heat pumps", Some("No title here."), &[])),
            "Compare heat pumps"
        );
    }

    #[test]
    fn an_itinerary_is_a_calendar_when_no_step_saved_one() {
        let plan = "## Day-by-Day Itinerary: Mombasa (November 13 - 16, 2026)\n\n\
                    ### Day 1: Friday, November 13\n\
                    - 14:00 Check into accommodation.\n\
                    - 16:00 Visit [Fort Jesus](https://example.org/fort), a Portuguese fort.\n\
                    - Transportation: a tuk-tuk.\n\
                    - **19:00** Dinner near the Old Town\n\n\
                    ### Day 2: Saturday 14 November\n\
                    - 09:00 Haller Park\n\n\
                    # Meal Plan for Mombasa (November 13-16, 2026)\n\
                    - **Breakfast:** Mahamri with tea\n";
        let since = NaiveDate::from_ymd_opt(2026, 10, 28).unwrap();
        let events = itinerary_events(plan, since);
        let at = |d: u32, h: u32, m: u32| {
            NaiveDate::from_ymd_opt(2026, 11, d)
                .unwrap()
                .and_hms_opt(h, m, 0)
                .unwrap()
        };
        let times: Vec<_> = events.iter().map(|e| (e.title.as_str(), e.when)).collect();
        assert_eq!(
            times,
            [
                (
                    "Check into accommodation",
                    EventTime::Local {
                        start: at(13, 14, 0),
                        end: at(13, 16, 0)
                    }
                ),
                (
                    "Visit Fort Jesus, a Portuguese fort",
                    EventTime::Local {
                        start: at(13, 16, 0),
                        end: at(13, 19, 0)
                    }
                ),
                (
                    "Dinner near the Old Town",
                    EventTime::Local {
                        start: at(13, 19, 0),
                        end: at(13, 20, 0)
                    }
                ),
                (
                    "Haller Park",
                    EventTime::Local {
                        start: at(14, 9, 0),
                        end: at(14, 10, 0)
                    }
                ),
            ],
            "untimed lines and the meal plan's meals are not events"
        );
        assert_eq!(events[1].location.as_deref(), Some("Fort Jesus"));
        let ics = to_ics("task-1", &events, t(0));
        assert!(
            ics.contains("DTSTART:20261113T140000\r\nDTEND:20261113T160000\r\n"),
            "local times carry no zone"
        );

        let january = itinerary_events("### Tuesday 5 January\n- 10:00 Fly home\n", since);
        assert_eq!(
            january[0].when,
            EventTime::Local {
                start: NaiveDate::from_ymd_opt(2027, 1, 5)
                    .unwrap()
                    .and_hms_opt(10, 0, 0)
                    .unwrap(),
                end: NaiveDate::from_ymd_opt(2027, 1, 5)
                    .unwrap()
                    .and_hms_opt(11, 0, 0)
                    .unwrap(),
            },
            "a date with no year is the next one after the task began"
        );
        assert!(itinerary_events("- 09:00 No day above this\n", since).is_empty());
    }

    #[test]
    fn a_section_that_titles_itself_at_the_top_level_moves_down_one() {
        let report = assemble(
            "Plan a trip",
            None,
            &[(
                "Meal plan".into(),
                "# Meal Plan\n\n## Friday\n\n```\n# not a heading\n```".into(),
            )],
        );
        assert!(
            report.contains("\n## Meal Plan\n\n### Friday\n"),
            "{report}"
        );
        assert!(
            report.contains("```\n# not a heading\n```"),
            "code is left alone"
        );
        assert_eq!(
            report.matches("\n# ").count(),
            1,
            "only the title is level one, beside the fenced line that is not a heading"
        );
    }

    #[test]
    fn a_long_goal_makes_a_short_title() {
        assert_eq!(
            report_title(
                "Plan a four-day trip to Mombasa for me and three friends, from Friday 13"
            ),
            "Plan a four-day trip to Mombasa for me and three friends",
            "the first clause names it"
        );
        assert_eq!(
            report_title("Tea, please"),
            "Tea, please",
            "too short to cut"
        );
        assert_eq!(
            report_title("  \nCompare heat pumps\nmore"),
            "Compare heat pumps"
        );
        let title = report_title(&"word ".repeat(40));
        assert!(title.chars().count() <= TITLE_CHARS && title.ends_with("..."));
    }

    #[test]
    fn calendar_entries_read_from_a_list_or_lines_and_a_bad_one_is_skipped() {
        let lines = "{\"title\": \"Flight\", \"start\": \"2026-11-14T09:00:00+03:00\", \"end\": \"2026-11-14T10:10:00+03:00\", \"location\": \"JKIA\"},\nnot json\n{\"title\": \"Beach day\", \"start\": \"2026-11-15\"}\n{\"start\": \"2026-11-16\"}";
        let events = calendar_events(lines);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0].when,
            EventTime::At {
                start: DateTime::parse_from_rfc3339("2026-11-14T06:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                end: DateTime::parse_from_rfc3339("2026-11-14T07:10:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            }
        );
        assert_eq!(events[0].location.as_deref(), Some("JKIA"));
        let day = NaiveDate::from_ymd_opt(2026, 11, 15).unwrap();
        assert_eq!(
            events[1].when,
            EventTime::Days {
                first: day,
                last: day
            }
        );
        assert_eq!(
            calendar_events("[{\"title\": \"A\", \"start\": \"2026-11-15\"}]").len(),
            1
        );
    }

    #[test]
    fn the_calendar_file_escapes_text_and_folds_long_lines() {
        let events = calendar_events("{\"title\": \"Dinner; seafood, at Tamarind\", \"start\": \"2026-11-15\", \"end\": \"2026-11-16\", \"notes\": \"Book ahead\\nfor four\"}");
        let ics = to_ics("task-1", &events, t(0));
        assert!(ics.starts_with("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n"));
        assert!(ics.contains("SUMMARY:Dinner\\; seafood\\, at Tamarind\r\n"));
        assert!(
            ics.contains("DTSTART;VALUE=DATE:20261115\r\nDTEND;VALUE=DATE:20261117\r\n"),
            "the end day is included"
        );
        assert!(ics.contains("DESCRIPTION:Book ahead\\nfor four"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        let long = fold(&"x".repeat(200));
        assert!(long.split("\r\n").all(|l| l.len() <= 75));
        assert_eq!(long.replace("\r\n ", ""), "x".repeat(200));
    }

    #[test]
    fn a_result_that_already_holds_its_sections_is_not_followed_by_them_again() {
        let section = "The camera saw Jerry at 16:10 and an unknown person at 15:55. ".repeat(10);
        let sections = vec![("Events".to_string(), section.clone())];
        let compiled = format!("# Vision report\n\n{section}");
        let report = assemble("vision events", Some(&compiled), &sections);
        assert_eq!(
            report.matches("unknown person at 15:55").count(),
            10,
            "{report}"
        );
        assert!(!report.contains("## Events"));

        let brief = "# Vision report\n\nTwo people were seen today.";
        let report = assemble("vision events", Some(brief), &sections);
        assert!(
            report.contains("## Events"),
            "a short summary keeps its sections"
        );
    }
}
