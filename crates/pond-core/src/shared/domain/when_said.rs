//! A due time from the words someone used about when ("on Friday", "tomorrow at 6pm", "kesho
//! jioni"), read from the moment they said them, in their timezone. Words it cannot place are
//! `None`, never a guess: a reminder on the wrong day is worse than asking for a time.

use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, NaiveTime, Timelike, Utc, Weekday};
use chrono_tz::Tz;

use crate::shared::domain::todo::local_instant;

/// The hour a day said without a time is reminded at.
const MORNING: u32 = 9;
const AFTERNOON: u32 = 14;
const EVENING: u32 = 18;
const NIGHT: u32 = 20;
/// "Today" said without a time is reminded at this hour, or an hour on if that has passed.
const LATE_TODAY: u32 = 17;

/// Words that make the timing a condition or a range, which no one date stands for.
const VAGUE: &[&str] = &[
    "sometime",
    "someday",
    "soon",
    "eventually",
    "every",
    "after",
    "before",
    "until",
    "till",
    "when",
    "once",
    "maybe",
    "probably",
    "year",
    "years",
    "rains",
    "between",
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Day {
    Today,
    Ahead(u64),
    Weekday(Weekday),
    ThisWeekday(Weekday),
    NextWeekday(Weekday),
    NextWeek,
    NextMonth,
    Weekend,
    OfMonth(u32),
    Date(u32, u32),
}

pub(crate) fn weekday(word: &str) -> Option<Weekday> {
    Some(match word {
        "monday" | "mon" | "jumatatu" => Weekday::Mon,
        "tuesday" | "tue" | "tues" | "jumanne" => Weekday::Tue,
        "wednesday" | "wed" | "jumatano" => Weekday::Wed,
        "thursday" | "thu" | "thur" | "thurs" | "alhamisi" => Weekday::Thu,
        "friday" | "fri" | "ijumaa" => Weekday::Fri,
        "saturday" | "sat" | "jumamosi" => Weekday::Sat,
        "sunday" | "sun" | "jumapili" => Weekday::Sun,
        _ => return None,
    })
}

fn month(word: &str) -> Option<u32> {
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
    MONTHS
        .iter()
        .position(|m| word == *m || (word.len() >= 3 && m.starts_with(word)))
        .map(|i| i as u32 + 1)
}

pub(crate) fn number(word: &str) -> Option<u64> {
    if let Ok(n) = word.parse::<u64>() {
        return Some(n);
    }
    Some(match word {
        "a" | "an" | "one" => 1,
        "two" | "couple" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        _ => return None,
    })
}

/// "19th", "1st", "22nd": a day of the month.
pub(crate) fn ordinal(word: &str) -> Option<u32> {
    let digits = ["st", "nd", "rd", "th"]
        .iter()
        .find_map(|s| word.strip_suffix(s))?;
    let n = digits.parse::<u32>().ok()?;
    (1..=31).contains(&n).then_some(n)
}

/// "6pm", "6:30pm", "18:00", "6.30am".
pub(crate) fn clock_word(word: &str) -> Option<NaiveTime> {
    let (body, half) = if let Some(b) = word.strip_suffix("pm") {
        (b, Some(true))
    } else if let Some(b) = word.strip_suffix("am") {
        (b, Some(false))
    } else {
        (word, None)
    };
    let (h, m) = match body.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None if half.is_some() => (body.parse::<u32>().ok()?, 0),
        None => return None,
    };
    let hour = match half {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            match (h, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (h, true) => h + 12,
                (h, false) => h,
            }
        }
        None => h,
    };
    NaiveTime::from_hms_opt(hour, m, 0)
}

/// "at 3" with no am or pm: an afternoon hour for 1 to 7, as appointments are spoken.
pub(crate) fn bare_hour(n: u64) -> Option<NaiveTime> {
    let hour = match n {
        1..=7 => n + 12,
        8..=23 => n,
        _ => return None,
    };
    NaiveTime::from_hms_opt(hour as u32, 0, 0)
}

fn hour(h: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, 0, 0).expect("an hour of the day")
}

pub(crate) fn tokens(words: &str) -> Vec<String> {
    let lower = words.to_lowercase().replace(['\'', '\u{2019}'], "");
    let cleaned: String = lower
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ':' || c == '.' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut out: Vec<String> = Vec::new();
    for raw in cleaned.split_whitespace() {
        let word = raw.trim_matches('.').to_string();
        if word.is_empty() {
            continue;
        }
        // "6 pm" is one time.
        if matches!(word.as_str(), "am" | "pm" | "a.m" | "p.m") {
            if let Some(last) = out.last_mut() {
                if last
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == ':' || c == '.')
                {
                    last.push_str(&word.replace('.', ""));
                    continue;
                }
            }
        }
        out.push(match word.as_str() {
            "leo" => "today".into(),
            "kesho" => "tomorrow".into(),
            "asubuhi" => "morning".into(),
            "mchana" => "afternoon".into(),
            "jioni" => "evening".into(),
            "usiku" => "night".into(),
            "wiki" => "week".into(),
            "ijayo" => "following".into(),
            _ => word,
        });
    }
    // "the day after tomorrow" and "kesho kutwa" are one day, and "after" in them is no condition.
    let mut joined: Vec<String> = Vec::with_capacity(out.len());
    for word in out {
        let n = joined.len();
        if word == "tomorrow" && n >= 2 && joined[n - 2] == "day" && joined[n - 1] == "after" {
            joined.truncate(n - 2);
            joined.push("overmorrow".into());
        } else if word == "kutwa" && joined.last().is_some_and(|w| w == "tomorrow") {
            joined.pop();
            joined.push("overmorrow".into());
        } else {
            joined.push(word);
        }
    }
    joined
}

/// What the words name: a day, a clock time, a part of the day, or a wait from now.
#[derive(Debug, Default, Clone, Copy)]
struct Said {
    day: Option<Day>,
    time: Option<NaiveTime>,
    part: Option<u32>,
    within: Option<Duration>,
}

/// `None` for words that make the timing a condition or a range.
fn read(words: &[String]) -> Option<Said> {
    if words.iter().any(|w| VAGUE.contains(&w.as_str())) {
        return None;
    }
    let word = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let mut day: Option<Day> = None;
    let mut time: Option<NaiveTime> = None;
    let mut part: Option<u32> = None;

    let mut i = 0;
    while i < words.len() {
        let w = word(i);
        let previous = if i > 0 { word(i - 1) } else { "" };
        match w {
            "today" => day = day.or(Some(Day::Today)),
            "tonight" => {
                day = day.or(Some(Day::Today));
                part = Some(NIGHT);
            }
            "tomorrow" => day = Some(Day::Ahead(1)),
            "overmorrow" => day = Some(Day::Ahead(2)),
            "morning" => part = Some(MORNING),
            "afternoon" => part = Some(AFTERNOON),
            "evening" => part = Some(EVENING),
            "night" => part = Some(NIGHT),
            "noon" | "midday" | "lunchtime" => time = Some(hour(12)),
            "week" if previous == "next" || word(i + 1) == "following" => day = Some(Day::NextWeek),
            "month" if previous == "next" => day = Some(Day::NextMonth),
            "weekend" => day = Some(Day::Weekend),
            "in" => {
                if let (Some(n), unit) = (number(word(i + 1)), word(i + 2)) {
                    let unit = unit.trim_end_matches('s');
                    match unit {
                        "minute" | "min" => {
                            return Some(Said {
                                within: Some(Duration::minutes(n as i64)),
                                ..Said::default()
                            })
                        }
                        "hour" | "hr" => {
                            return Some(Said {
                                within: Some(Duration::hours(n as i64)),
                                ..Said::default()
                            })
                        }
                        "day" => day = Some(Day::Ahead(n)),
                        "week" => day = Some(Day::Ahead(n * 7)),
                        _ => {}
                    }
                    i += 2;
                }
            }
            "at" => {
                if let Some(t) =
                    clock_word(word(i + 1)).or_else(|| number(word(i + 1)).and_then(bare_hour))
                {
                    time = Some(t);
                    i += 1;
                }
            }
            _ => {
                if let Some(wd) = weekday(w) {
                    day = Some(match previous {
                        "next" => Day::NextWeekday(wd),
                        "this" => Day::ThisWeekday(wd),
                        _ => Day::Weekday(wd),
                    });
                } else if let Some(m) = month(w) {
                    // "19 November", "November 19th", "19th of November".
                    let before = if previous == "of" && i >= 2 {
                        word(i - 2)
                    } else {
                        previous
                    };
                    let d = ordinal(before)
                        .or_else(|| before.parse().ok())
                        .or_else(|| ordinal(word(i + 1)))
                        .or_else(|| word(i + 1).parse().ok());
                    if let Some(d) = d.filter(|d| (1..=31).contains(d)) {
                        day = Some(Day::Date(m, d));
                    }
                } else if let Some(d) = ordinal(w) {
                    if !matches!(day, Some(Day::Date(..))) && month(word(i + 1)).is_none() {
                        let next = if word(i + 1) == "of" { word(i + 2) } else { "" };
                        if month(next).is_none() {
                            day = Some(Day::OfMonth(d));
                        }
                    }
                } else if let Some(t) = clock_word(w) {
                    time = Some(t);
                } else if word(i + 1) == "oclock" {
                    time = number(w).and_then(bare_hour).or(time);
                }
            }
        }
        i += 1;
    }
    Some(Said {
        day,
        time,
        part,
        within: None,
    })
}

/// When `words` mean, said at `said_at` by someone in `tz`; `None` when they name no one time.
pub fn due_from_words(words: &str, said_at: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    let Said {
        day,
        time,
        part,
        within,
    } = read(&tokens(words))?;
    if let Some(wait) = within {
        return Some(said_at + wait);
    }
    if day.is_none() && time.is_none() && part.is_none() {
        return None;
    }

    let local = said_at.with_timezone(&tz);
    let today = local.date_naive();
    let date = match day {
        None if time.is_none() => today,
        None => {
            let t = time?;
            let at = local_instant(tz, today, t)?;
            return Some(if at > said_at {
                at
            } else {
                local_instant(tz, today.checked_add_days(Days::new(1))?, t)?
            });
        }
        Some(d) => resolve_day(d, today)?,
    };
    let at = match (time, part) {
        (Some(t), _) => t,
        (None, Some(h)) => hour(h),
        (None, None) if date == today => {
            let late = local_instant(tz, today, hour(LATE_TODAY))?;
            let soon = said_at + Duration::hours(1);
            return Some(if late > soon {
                late
            } else {
                soon_on_the_minute(soon)
            });
        }
        (None, None) => hour(MORNING),
    };
    local_instant(tz, date, at).filter(|due| *due > said_at)
}

fn soon_on_the_minute(at: DateTime<Utc>) -> DateTime<Utc> {
    at.with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(at)
}

/// The calendar day `words` name, seen from `today`: "today", "tomorrow", "Saturday", "19
/// November", or a date written `2026-11-14`. `None` when they name no day.
pub fn day_from_words(words: &str, today: NaiveDate) -> Option<NaiveDate> {
    if let Ok(day) = NaiveDate::parse_from_str(words.trim(), "%Y-%m-%d") {
        return Some(day);
    }
    resolve_day(read(&tokens(words))?.day?, today)
}

/// The moment `words` point back to, said at `now` by someone in `tz`: "yesterday", "6 hours
/// ago", "the last 3 days", "Monday", "this morning", "yesterday at 6pm". `None` when they
/// name no earlier moment.
pub fn earlier_from_words(words: &str, now: DateTime<Utc>, tz: Tz) -> Option<DateTime<Utc>> {
    let words = tokens(words);
    let word = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    let span = |n: u64, unit: &str| -> Option<Duration> {
        let n = i64::try_from(n).ok()?;
        match unit.trim_end_matches('s') {
            "minute" | "min" => Some(Duration::minutes(n)),
            "hour" | "hr" => Some(Duration::hours(n)),
            "day" => Some(Duration::days(n)),
            "week" => Some(Duration::weeks(n)),
            "month" => Some(Duration::days(30 * n)),
            _ => None,
        }
    };
    let today = now.with_timezone(&tz).date_naive();
    let mut date: Option<NaiveDate> = None;
    let mut time: Option<NaiveTime> = None;

    for i in 0..words.len() {
        let w = word(i);
        let previous = if i > 0 { word(i - 1) } else { "" };
        match w {
            "now" => return Some(now),
            "ago" => {
                // "6 hours ago", "an hour ago".
                let n = if i >= 2 { number(word(i - 2)) } else { None };
                return span(n?, previous).map(|back| now - back);
            }
            "last" | "past" => {
                let next = word(i + 1);
                if let Some(n) = number(next).filter(|_| span(1, word(i + 2)).is_some()) {
                    return span(n, word(i + 2)).map(|back| now - back);
                }
                if next == "night" {
                    date = today.checked_sub_days(Days::new(1));
                    time = Some(hour(EVENING));
                } else if let Some(back) = span(1, next) {
                    return Some(now - back);
                }
            }
            "today" | "morning" | "afternoon" | "evening" | "tonight" => {
                date = date.or(Some(today));
                time = time.or(match w {
                    "afternoon" => Some(hour(12)),
                    "evening" | "tonight" => Some(hour(EVENING)),
                    _ => None,
                });
            }
            "yesterday" => date = today.checked_sub_days(Days::new(1)),
            "week" if previous == "this" => {
                date = today
                    .checked_sub_days(Days::new(u64::from(today.weekday().num_days_from_monday())));
            }
            "month" if previous == "this" => date = today.with_day(1),
            "at" => {
                if let Some(t) =
                    clock_word(word(i + 1)).or_else(|| number(word(i + 1)).and_then(bare_hour))
                {
                    time = Some(t);
                }
            }
            _ => {
                if let Some(wd) = weekday(w) {
                    let back = (today.weekday().num_days_from_monday() + 7
                        - wd.num_days_from_monday())
                        % 7;
                    date = today.checked_sub_days(Days::new(u64::from(back)));
                } else if let Some(t) = clock_word(w) {
                    time = Some(t);
                }
            }
        }
    }
    match (date, time) {
        (None, None) => None,
        (Some(date), time) => local_instant(tz, date, time.unwrap_or(hour(0))),
        // A bare clock time is today's, or yesterday's when today's has not come yet.
        (None, Some(t)) => {
            let at = local_instant(tz, today, t)?;
            if at <= now {
                Some(at)
            } else {
                local_instant(tz, today.checked_sub_days(Days::new(1))?, t)
            }
        }
    }
}

fn resolve_day(day: Day, today: NaiveDate) -> Option<NaiveDate> {
    let ahead = |n: u64| today.checked_add_days(Days::new(n));
    let to_weekday = |wd: Weekday| {
        u64::from((wd.num_days_from_monday() + 7 - today.weekday().num_days_from_monday()) % 7)
    };
    match day {
        Day::Today => Some(today),
        Day::Ahead(n) => ahead(n),
        // A weekday said on that weekday is the next one: today would have been "today".
        Day::Weekday(wd) => match to_weekday(wd) {
            0 => ahead(7),
            n => ahead(n),
        },
        Day::ThisWeekday(wd) => ahead(to_weekday(wd)),
        // The one in the coming week, as "next Tuesday" is said on a Monday.
        Day::NextWeekday(wd) => {
            let monday = ahead(u64::from(7 - today.weekday().num_days_from_monday()))?;
            monday.checked_add_days(Days::new(u64::from(wd.num_days_from_monday())))
        }
        Day::NextWeek => ahead(u64::from(7 - today.weekday().num_days_from_monday())),
        Day::NextMonth => {
            let (y, m) = if today.month() == 12 {
                (today.year() + 1, 1)
            } else {
                (today.year(), today.month() + 1)
            };
            NaiveDate::from_ymd_opt(y, m, 1)
        }
        Day::Weekend => ahead(to_weekday(Weekday::Sat)).map(|sat| {
            if today.weekday() == Weekday::Sun {
                today
            } else {
                sat
            }
        }),
        Day::OfMonth(d) => {
            let this = NaiveDate::from_ymd_opt(today.year(), today.month(), d);
            match this {
                Some(date) if date >= today => Some(date),
                _ => {
                    let (y, m) = if today.month() == 12 {
                        (today.year() + 1, 1)
                    } else {
                        (today.year(), today.month() + 1)
                    };
                    NaiveDate::from_ymd_opt(y, m, d)
                }
            }
        }
        Day::Date(m, d) => match NaiveDate::from_ymd_opt(today.year(), m, d) {
            Some(date) if date >= today => Some(date),
            _ => NaiveDate::from_ymd_opt(today.year() + 1, m, d),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    /// Monday 9 November 2026, 10:00 in Nairobi.
    fn said() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn due(words: &str) -> Option<String> {
        due_from_words(words, said(), NAIROBI).map(crate::shared::domain::todo::rfc3339)
    }

    fn at(local: &str) -> Option<String> {
        let naive = chrono::NaiveDateTime::parse_from_str(local, "%Y-%m-%d %H:%M").unwrap();
        local_instant(NAIROBI, naive.date(), naive.time()).map(crate::shared::domain::todo::rfc3339)
    }

    #[test]
    fn days_said_plainly_fall_where_people_mean_them() {
        let cases = [
            ("today", "2026-11-09 17:00"),
            ("tonight", "2026-11-09 20:00"),
            ("this evening", "2026-11-09 18:00"),
            ("later today", "2026-11-09 17:00"),
            ("tomorrow", "2026-11-10 09:00"),
            ("tomorrow morning", "2026-11-10 09:00"),
            ("Tomorrow night", "2026-11-10 20:00"),
            ("the day after tomorrow", "2026-11-11 09:00"),
            ("on Friday", "2026-11-13 09:00"),
            ("by Friday.", "2026-11-13 09:00"),
            ("Fri", "2026-11-13 09:00"),
            ("this Tuesday", "2026-11-10 09:00"),
            ("next Tuesday", "2026-11-17 09:00"),
            ("Monday", "2026-11-16 09:00"),
            ("next week", "2026-11-16 09:00"),
            ("next month", "2026-12-01 09:00"),
            ("this weekend", "2026-11-14 09:00"),
            ("on the 19th", "2026-11-19 09:00"),
            ("the 5th", "2026-12-05 09:00"),
            ("19 November", "2026-11-19 09:00"),
            ("November 19th", "2026-11-19 09:00"),
            ("the 19th of November", "2026-11-19 09:00"),
            ("3 March", "2027-03-03 09:00"),
            ("in three days", "2026-11-12 09:00"),
            ("in two weeks", "2026-11-23 09:00"),
        ];
        for (words, local) in cases {
            assert_eq!(due(words), at(local), "{words:?}");
        }
    }

    #[test]
    fn a_time_said_with_or_without_a_day_is_kept() {
        let cases = [
            ("at 6pm", "2026-11-09 18:00"),
            ("6 pm", "2026-11-09 18:00"),
            ("at 18:00", "2026-11-09 18:00"),
            ("at 6", "2026-11-09 18:00"),
            ("tomorrow at 6pm", "2026-11-10 18:00"),
            ("6:30pm on Thursday", "2026-11-12 18:30"),
            ("Friday at 3", "2026-11-13 15:00"),
            ("tomorrow at 7 o'clock", "2026-11-10 19:00"),
            ("noon tomorrow", "2026-11-10 12:00"),
            // 9am has passed by 10:00, so tomorrow's.
            ("at 9am", "2026-11-10 09:00"),
        ];
        for (words, local) in cases {
            assert_eq!(due(words), at(local), "{words:?}");
        }
        assert_eq!(due("in 2 hours").as_deref(), Some("2026-11-09T09:00:00Z"));
        assert_eq!(
            due("in 30 minutes").as_deref(),
            Some("2026-11-09T07:30:00Z")
        );
    }

    #[test]
    fn swahili_days_and_times_are_read_too() {
        assert_eq!(due("kesho jioni"), at("2026-11-10 18:00"));
        assert_eq!(due("leo usiku"), at("2026-11-09 20:00"));
        assert_eq!(due("kesho kutwa"), at("2026-11-11 09:00"));
        assert_eq!(due("Ijumaa"), at("2026-11-13 09:00"));
        assert_eq!(due("wiki ijayo"), at("2026-11-16 09:00"));
    }

    #[test]
    fn timing_that_names_no_one_moment_is_none() {
        for words in [
            "after the rains",
            "soon",
            "sometime next year",
            "every Friday",
            "before Friday",
            "when the car is fixed",
            "later",
            "",
            "whenever",
        ] {
            assert_eq!(due(words), None, "{words:?} names no one time");
        }
    }

    #[test]
    fn today_said_late_is_an_hour_on_and_a_passed_time_today_is_none() {
        // 16:30 in Nairobi: 17:00 is too close to be useful, so 17:30.
        let late = DateTime::parse_from_rfc3339("2026-11-09T13:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            due_from_words("today", late, NAIROBI).map(crate::shared::domain::todo::rfc3339),
            Some("2026-11-09T14:30:00Z".to_string())
        );
        assert_eq!(due("today at 8am"), None);
        assert_eq!(due("this morning"), None);
    }

    #[test]
    fn the_day_is_counted_in_the_speakers_timezone() {
        // 23:30 Sunday in Nairobi is still Sunday there, though Monday has not come in UTC.
        let late_sunday = DateTime::parse_from_rfc3339("2026-11-08T20:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            due_from_words("tomorrow", late_sunday, NAIROBI)
                .map(crate::shared::domain::todo::rfc3339),
            at("2026-11-09 09:00")
        );
    }
    #[test]
    fn a_day_is_read_on_its_own() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 9).unwrap();
        let day = |words: &str| day_from_words(words, today).map(|d| d.to_string());
        assert_eq!(day("today").as_deref(), Some("2026-11-09"));
        assert_eq!(day("tomorrow").as_deref(), Some("2026-11-10"));
        assert_eq!(day("kesho kutwa").as_deref(), Some("2026-11-11"));
        assert_eq!(day("Saturday").as_deref(), Some("2026-11-14"));
        assert_eq!(
            day("on the 19th of November").as_deref(),
            Some("2026-11-19")
        );
        assert_eq!(day("2026-11-21").as_deref(), Some("2026-11-21"));
        assert_eq!(day("at 6pm"), None);
        assert_eq!(day("whenever"), None);
    }

    #[test]
    fn words_that_point_back_name_an_earlier_moment() {
        let back = |words: &str| {
            earlier_from_words(words, said(), NAIROBI).map(crate::shared::domain::todo::rfc3339)
        };
        let ago = |d: Duration| Some(crate::shared::domain::todo::rfc3339(said() - d));
        assert_eq!(back("6 hours ago"), ago(Duration::hours(6)));
        assert_eq!(back("an hour ago"), ago(Duration::hours(1)));
        assert_eq!(back("the last 3 days"), ago(Duration::days(3)));
        assert_eq!(back("last week"), ago(Duration::weeks(1)));
        assert_eq!(back("past 30 minutes"), ago(Duration::minutes(30)));
        assert_eq!(back("today"), at("2026-11-09 00:00"));
        assert_eq!(back("this morning"), at("2026-11-09 00:00"));
        assert_eq!(back("yesterday"), at("2026-11-08 00:00"));
        assert_eq!(back("yesterday at 6pm"), at("2026-11-08 18:00"));
        assert_eq!(back("last night"), at("2026-11-08 18:00"));
        assert_eq!(back("Friday"), at("2026-11-06 00:00"));
        assert_eq!(back("this week"), at("2026-11-09 00:00"));
        assert_eq!(back("8am"), at("2026-11-09 08:00"));
        assert_eq!(back("at 21:00"), at("2026-11-08 21:00"));
        assert_eq!(back("whenever"), None);
    }
}
