//! A repeat from the words someone would use ("every weekday at 7am", "Mondays and Thursdays at
//! 18:00", "every 2 hours"), and a repeat back into words. Words that name no repeat, or one
//! without its time, are an error to ask about, never a guess.

use chrono::{Datelike, NaiveDate, NaiveTime, Timelike};

use crate::shared::domain::todo::Every;
use crate::shared::domain::when_said::{bare_hour, clock_word, number, ordinal, tokens, weekday};

const MORNING: u32 = 8;
const AFTERNOON: u32 = 14;
const EVENING: u32 = 18;
const NIGHT: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Repeat {
    Minutes(u32),
    Hours {
        every: u32,
        minute: u32,
    },
    Daily {
        at: NaiveTime,
    },
    /// ISO weekdays, 1 is Monday, sorted.
    Weekly {
        days: Vec<u32>,
        at: NaiveTime,
    },
    /// Days of the month, sorted.
    Monthly {
        days: Vec<u32>,
        at: NaiveTime,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatUnclear {
    /// No word says how often.
    NoCadence,
    /// How often is clear, the time of day is not.
    NoTime,
    /// An hour from 1 to 7 with neither am, pm nor a part of the day.
    AmOrPm,
    /// "Every other day", "every 3 weeks": a step a calendar repeat cannot hold.
    Step,
}

impl RepeatUnclear {
    /// What to tell the model, so its next call or its question to the person is the right one.
    pub fn advice(self) -> &'static str {
        match self {
            Self::NoCadence => {
                "That does not say how often. Say it as a person would: \"every day at 7am\", \
                 \"every weekday at 18:00\", \"every Monday and Thursday at 9am\", \"every 2 \
                 hours\", \"on the 1st of every month at 9am\"."
            }
            Self::NoTime => {
                "That says how often but not the time of day. Ask them what time, then send it \
                 again with the time: \"every Monday at 9am\"."
            }
            Self::AmOrPm => {
                "That hour could be morning or evening. Send it with am or pm, or as 24-hour \
                 time: \"7am\", \"19:00\". Ask them if they did not say."
            }
            Self::Step => {
                "A repeat can be every so many minutes or hours, every day, on named weekdays, \
                 or on days of the month. \"Every other day\" or \"every 3 weeks\" cannot be \
                 kept; offer the nearest of those instead."
            }
        }
    }
}

const WEEKDAYS: [u32; 5] = [1, 2, 3, 4, 5];
const DAY_NAMES: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

/// A weekday a token names, counting "mondays" and, only after a word that leads into a day,
/// the three-letter forms: "sun" and "sat" are also ordinary words.
fn day_named(word: &str, previous: &str) -> Option<u32> {
    let singular = word.strip_suffix('s').filter(|w| w.len() >= 6);
    let day = weekday(word).or_else(|| singular.and_then(weekday))?;
    let short = word.len() <= 5 && !matches!(word, "ijumaa");
    let led_in = matches!(previous, "every" | "each" | "on" | "and" | "or" | "kila")
        || weekday(previous).is_some();
    (!short || led_in).then(|| day.number_from_monday())
}

fn hour(h: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, 0, 0).expect("an hour of the day")
}

fn sorted(mut days: Vec<u32>) -> Vec<u32> {
    days.sort_unstable();
    days.dedup();
    days
}

/// The repeat `words` describe. `today` anchors "every week" and "every month" said without a
/// day: the day they were said on.
pub fn repeat_from_words(words: &str, today: NaiveDate) -> Result<Repeat, RepeatUnclear> {
    let words = tokens(words);
    let word = |i: usize| words.get(i).map(String::as_str).unwrap_or("");

    let mut cadence = false;
    let mut days: Vec<u32> = Vec::new();
    let mut month_days: Vec<u32> = Vec::new();
    let (mut daily, mut weekly, mut monthly) = (false, false, false);
    let mut time: Option<NaiveTime> = None;
    let mut part: Option<u32> = None;
    let mut unclear_hour = false;

    let mut i = 0;
    while i < words.len() {
        let w = word(i);
        let previous = if i > 0 { word(i - 1) } else { "" };
        match w {
            "every" | "each" | "kila" => {
                cadence = true;
                let next = word(i + 1);
                if next == "other" || next == "second" || next == "alternate" {
                    return Err(RepeatUnclear::Step);
                }
                if next == "half" {
                    return Ok(Repeat::Minutes(30));
                }
                let (n, unit) = match number(next) {
                    Some(n) => (n, word(i + 2)),
                    None => (1, next),
                };
                match unit.trim_end_matches('s') {
                    "minute" | "min" => {
                        return u32::try_from(n)
                            .ok()
                            .filter(|n| (1..=59).contains(n))
                            .map(Repeat::Minutes)
                            .ok_or(RepeatUnclear::Step);
                    }
                    "hour" | "hr" => {
                        return u32::try_from(n)
                            .ok()
                            .filter(|n| (1..=23).contains(n))
                            .map(|every| Repeat::Hours { every, minute: 0 })
                            .ok_or(RepeatUnclear::Step);
                    }
                    "day" | "week" | "month" | "year" if n > 1 => return Err(RepeatUnclear::Step),
                    _ => {}
                }
            }
            "hourly" => {
                return Ok(Repeat::Hours {
                    every: 1,
                    minute: 0,
                })
            }
            "daily" | "day" | "days" | "siku" => daily = true,
            "nightly" => {
                cadence = true;
                daily = true;
                part = Some(NIGHT);
            }
            "weekly" | "week" | "weeks" => weekly = true,
            "monthly" | "month" | "months" => monthly = true,
            "weekday" | "weekdays" => {
                cadence = true;
                days.extend(WEEKDAYS);
            }
            "weekend" | "weekends" => {
                cadence = true;
                days.extend([6, 7]);
            }
            "morning" | "mornings" => {
                daily = true;
                part = Some(MORNING);
            }
            "afternoon" | "afternoons" => {
                daily = true;
                part = Some(AFTERNOON);
            }
            "evening" | "evenings" => {
                daily = true;
                part = Some(EVENING);
            }
            "night" | "nights" => {
                daily = true;
                part = Some(NIGHT);
            }
            "noon" | "midday" | "lunchtime" => time = Some(hour(12)),
            "midnight" => time = Some(hour(0)),
            "at" => {
                let next = word(i + 1);
                if let Some(t) = clock_word(next) {
                    time = Some(t);
                    i += 1;
                } else if let Some(n) = number(next).filter(|_| next.parse::<u64>().is_ok()) {
                    if (1..=7).contains(&n) {
                        unclear_hour = true;
                        time = NaiveTime::from_hms_opt(n as u32, 0, 0);
                    } else {
                        time = bare_hour(n).or(time);
                    }
                    i += 1;
                }
            }
            _ => {
                if let Some(day) = day_named(w, previous) {
                    // "Mondays" repeats by itself; "Monday" needs "every" or "each" somewhere.
                    if w.ends_with('s') && w.len() >= 7 {
                        cadence = true;
                    }
                    days.push(day);
                } else if let Some(d) = ordinal(w) {
                    month_days.push(d);
                } else if let Some(t) = clock_word(w) {
                    time = Some(t);
                }
            }
        }
        i += 1;
    }
    cadence |= words
        .iter()
        .any(|w| matches!(w.as_str(), "daily" | "weekly" | "monthly"));
    if !cadence {
        return Err(RepeatUnclear::NoCadence);
    }

    // "At 7" is the morning when they said morning, and the evening when they said evening.
    let at = match (time, part) {
        (Some(t), Some(p)) if unclear_hour => {
            Some(if p == MORNING { t } else { hour(t.hour() + 12) })
        }
        (Some(_), None) if unclear_hour => return Err(RepeatUnclear::AmOrPm),
        (Some(t), _) => Some(t),
        (None, Some(p)) => Some(hour(p)),
        (None, None) => None,
    };

    let days = sorted(days);
    let which = if monthly {
        let days = if month_days.is_empty() {
            vec![today.day()]
        } else {
            sorted(month_days)
        };
        Some(Every::Month).zip(Some(days))
    } else if days.len() == 7 {
        Some((Every::Day, Vec::new()))
    } else if !days.is_empty() {
        Some((Every::Week, days))
    } else if weekly {
        Some((Every::Week, vec![today.weekday().number_from_monday()]))
    } else if daily {
        Some((Every::Day, Vec::new()))
    } else {
        None
    };
    let (every, days) = which.ok_or(RepeatUnclear::NoCadence)?;
    let at = at.ok_or(RepeatUnclear::NoTime)?;
    Ok(match every {
        Every::Month => Repeat::Monthly { days, at },
        Every::Week | Every::Weekday => Repeat::Weekly { days, at },
        Every::Day => Repeat::Daily { at },
    })
}

fn clock(at: NaiveTime) -> String {
    at.format("%H:%M").to_string()
}

fn listed(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

fn nth(day: u32) -> String {
    let suffix = match (day % 10, day % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{day}{suffix}")
}

impl Repeat {
    /// The repeat as a person would say it, with 24-hour times.
    pub fn in_words(&self) -> String {
        match self {
            Self::Minutes(1) => "every minute".to_string(),
            Self::Minutes(n) => format!("every {n} minutes"),
            Self::Hours {
                every: 1,
                minute: 0,
            } => "every hour".to_string(),
            Self::Hours { every: 1, minute } => {
                format!("every hour at {minute} minutes past")
            }
            Self::Hours { every, .. } => format!("every {every} hours"),
            Self::Daily { at } => format!("every day at {}", clock(*at)),
            Self::Weekly { days, at } if days.as_slice() == WEEKDAYS => {
                format!("every weekday at {}", clock(*at))
            }
            Self::Weekly { days, at } => {
                let names: Vec<String> = days
                    .iter()
                    .filter_map(|d| DAY_NAMES.get(*d as usize - 1))
                    .map(|n| n.to_string())
                    .collect();
                format!("every {} at {}", listed(&names), clock(*at))
            }
            Self::Monthly { days, at } => {
                let names: Vec<String> = days.iter().map(|d| nth(*d)).collect();
                format!("on the {} of every month at {}", listed(&names), clock(*at))
            }
        }
    }

    /// The scheduler's six fields: sec min hr dom mon dow, Sunday as 0.
    pub fn to_cron(&self) -> String {
        match self {
            Self::Minutes(1) => "0 * * * * *".to_string(),
            Self::Minutes(n) => format!("0 */{n} * * * *"),
            Self::Hours { every: 1, minute } => format!("0 {minute} * * * *"),
            Self::Hours { every, minute } => format!("0 {minute} */{every} * * *"),
            Self::Daily { at } => format!("0 {} {} * * *", at.minute(), at.hour()),
            Self::Weekly { days, at } => {
                let dow = if days.as_slice() == WEEKDAYS {
                    "1-5".to_string()
                } else {
                    let mut numbers: Vec<u32> = days.iter().map(|d| d % 7).collect();
                    numbers.sort_unstable();
                    numbers
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                };
                format!("0 {} {} * * {dow}", at.minute(), at.hour())
            }
            Self::Monthly { days, at } => {
                let dom = days
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",");
                format!("0 {} {} {dom} * *", at.minute(), at.hour())
            }
        }
    }

    /// The repeat as a to-do's hook keeps it; `None` for one within a day, which a to-do
    /// cannot hold.
    pub fn as_todo_repeat(&self) -> Option<(Every, NaiveTime, Vec<u32>)> {
        match self {
            Self::Minutes(_) | Self::Hours { .. } => None,
            Self::Daily { at } => Some((Every::Day, *at, Vec::new())),
            Self::Weekly { days, at } if days.as_slice() == WEEKDAYS => {
                Some((Every::Weekday, *at, Vec::new()))
            }
            Self::Weekly { days, at } => Some((Every::Week, *at, days.clone())),
            Self::Monthly { days, at } => Some((Every::Month, *at, days.clone())),
        }
    }
}

/// Whether `text` has the shape of a cron expression: five or six fields of cron characters.
pub fn looks_like_cron(text: &str) -> bool {
    let fields: Vec<&str> = text.split_whitespace().collect();
    matches!(fields.len(), 5 | 6)
        && fields.iter().all(|f| {
            f.chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '*' | '/' | '-' | ','))
        })
}

/// `cron` with its seconds field, as the scheduler takes it.
pub fn six_field_cron(cron: &str) -> String {
    let cron = cron.split_whitespace().collect::<Vec<_>>().join(" ");
    if cron.split(' ').count() == 5 {
        format!("0 {cron}")
    } else {
        cron
    }
}

fn numbers_in(field: &str, max: u32) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for piece in field.split(',') {
        match piece.split_once('-') {
            Some((from, to)) => {
                let (from, to) = (from.parse::<u32>().ok()?, to.parse::<u32>().ok()?);
                if from > to || to > max {
                    return None;
                }
                out.extend(from..=to);
            }
            None => out.push(piece.parse::<u32>().ok().filter(|n| *n <= max)?),
        }
    }
    Some(out)
}

/// The repeat a stored cron stands for, when it is one of the shapes [`Repeat::to_cron`] writes.
pub fn repeat_from_cron(cron: &str) -> Option<Repeat> {
    let cron = six_field_cron(cron);
    let fields: Vec<&str> = cron.split(' ').collect();
    let [sec, min, hr, dom, mon, dow] = fields.as_slice() else {
        return None;
    };
    if *sec != "0" || *mon != "*" {
        return None;
    }
    let every_day = *dom == "*" && (*dow == "*" || *dow == "?");
    if *hr == "*" {
        if !every_day {
            return None;
        }
        return match *min {
            "*" => Some(Repeat::Minutes(1)),
            m => match m.strip_prefix("*/") {
                Some(step) => step.parse().ok().map(Repeat::Minutes),
                None => m
                    .parse()
                    .ok()
                    .map(|minute| Repeat::Hours { every: 1, minute }),
            },
        };
    }
    let minute: u32 = min.parse().ok().filter(|m| *m < 60)?;
    if let Some(step) = hr.strip_prefix("*/") {
        let every = step.parse().ok()?;
        return every_day.then_some(Repeat::Hours { every, minute });
    }
    let at = NaiveTime::from_hms_opt(hr.parse().ok()?, minute, 0)?;
    match (*dom, *dow) {
        ("*", "*" | "?") => Some(Repeat::Daily { at }),
        ("*", dow) => {
            let days = sorted(
                numbers_in(dow, 7)?
                    .into_iter()
                    .map(|d| if d == 0 { 7 } else { d })
                    .collect(),
            );
            Some(if days.len() == 7 {
                Repeat::Daily { at }
            } else {
                Repeat::Weekly { days, at }
            })
        }
        (dom, "*" | "?") => {
            let days = sorted(numbers_in(dom, 31)?);
            (!days.is_empty() && !days.contains(&0)).then_some(Repeat::Monthly { days, at })
        }
        _ => None,
    }
}

/// A stored cron as a person would say it, or the cron itself when it is not a plain repeat.
pub fn cron_in_words(cron: &str) -> String {
    repeat_from_cron(cron).map_or_else(|| cron.to_string(), |r| r.in_words())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Thursday, the 12th.
    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 12).unwrap()
    }

    fn at(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn read(words: &str) -> Result<Repeat, RepeatUnclear> {
        repeat_from_words(words, today())
    }

    #[test]
    fn everyday_phrasings_become_the_repeat_they_mean() {
        let cases: [(&str, Repeat, &str); 14] = [
            (
                "every day at 7am",
                Repeat::Daily { at: at(7, 0) },
                "0 0 7 * * *",
            ),
            (
                "daily at 10:30 am",
                Repeat::Daily { at: at(10, 30) },
                "0 30 10 * * *",
            ),
            (
                "every morning",
                Repeat::Daily { at: at(8, 0) },
                "0 0 8 * * *",
            ),
            (
                "every evening at 9 pm",
                Repeat::Daily { at: at(21, 0) },
                "0 0 21 * * *",
            ),
            (
                "every weekday at 18:00",
                Repeat::Weekly {
                    days: vec![1, 2, 3, 4, 5],
                    at: at(18, 0),
                },
                "0 0 18 * * 1-5",
            ),
            (
                "every Monday and Thursday at 6pm",
                Repeat::Weekly {
                    days: vec![1, 4],
                    at: at(18, 0),
                },
                "0 0 18 * * 1,4",
            ),
            (
                "Sundays at 9am",
                Repeat::Weekly {
                    days: vec![7],
                    at: at(9, 0),
                },
                "0 0 9 * * 0",
            ),
            (
                "weekends at 10:00",
                Repeat::Weekly {
                    days: vec![6, 7],
                    at: at(10, 0),
                },
                "0 0 10 * * 0,6",
            ),
            (
                "every week at 8am",
                Repeat::Weekly {
                    days: vec![4],
                    at: at(8, 0),
                },
                "0 0 8 * * 4",
            ),
            (
                "on the 1st and 15th of every month at 9am",
                Repeat::Monthly {
                    days: vec![1, 15],
                    at: at(9, 0),
                },
                "0 0 9 1,15 * *",
            ),
            (
                "monthly at noon",
                Repeat::Monthly {
                    days: vec![12],
                    at: at(12, 0),
                },
                "0 0 12 12 * *",
            ),
            (
                "every 2 hours",
                Repeat::Hours {
                    every: 2,
                    minute: 0,
                },
                "0 0 */2 * * *",
            ),
            ("every 15 minutes", Repeat::Minutes(15), "0 */15 * * * *"),
            ("every half hour", Repeat::Minutes(30), "0 */30 * * * *"),
        ];
        for (words, repeat, cron) in cases {
            assert_eq!(read(words), Ok(repeat.clone()), "{words}");
            assert_eq!(repeat.to_cron(), cron, "{words}");
        }
    }

    #[test]
    fn a_bare_small_hour_takes_its_half_of_the_day_from_the_words_or_is_asked_about() {
        assert_eq!(
            read("every morning at 7"),
            Ok(Repeat::Daily { at: at(7, 0) })
        );
        assert_eq!(
            read("every evening at 7"),
            Ok(Repeat::Daily { at: at(19, 0) })
        );
        assert_eq!(read("every day at 7"), Err(RepeatUnclear::AmOrPm));
        assert_eq!(read("every day at 9"), Ok(Repeat::Daily { at: at(9, 0) }));
    }

    #[test]
    fn words_that_name_no_repeat_or_no_time_are_not_guessed() {
        assert_eq!(read("at 3 pm"), Err(RepeatUnclear::NoCadence));
        assert_eq!(read("tomorrow at 6pm"), Err(RepeatUnclear::NoCadence));
        assert_eq!(read("hello world"), Err(RepeatUnclear::NoCadence));
        assert_eq!(read("every Monday"), Err(RepeatUnclear::NoTime));
        assert_eq!(read("every day"), Err(RepeatUnclear::NoTime));
        assert_eq!(read("every other day at 9am"), Err(RepeatUnclear::Step));
        assert_eq!(read("every 3 weeks at 9am"), Err(RepeatUnclear::Step));
        assert_eq!(read("every 90 minutes"), Err(RepeatUnclear::Step));
    }

    #[test]
    fn a_short_day_name_counts_only_where_a_day_is_being_named() {
        assert_eq!(
            read("every day at 6am tell me when the sun rises"),
            Ok(Repeat::Daily { at: at(6, 0) })
        );
        assert_eq!(
            read("every sat and sun at 10am"),
            Ok(Repeat::Weekly {
                days: vec![6, 7],
                at: at(10, 0)
            })
        );
    }

    #[test]
    fn a_repeat_reads_back_in_words_and_survives_its_cron() {
        let cases = [
            ("every weekday at 7am", "every weekday at 07:00"),
            (
                "every Monday and Thursday at 6pm",
                "every Monday and Thursday at 18:00",
            ),
            (
                "mondays, wednesdays and fridays at 06:30",
                "every Monday, Wednesday and Friday at 06:30",
            ),
            (
                "on the 1st and 22nd of every month at 9am",
                "on the 1st and 22nd of every month at 09:00",
            ),
            ("every 2 hours", "every 2 hours"),
            ("hourly", "every hour"),
            ("every minute", "every minute"),
            ("every night", "every day at 20:00"),
        ];
        for (words, said) in cases {
            let repeat = read(words).unwrap();
            assert_eq!(repeat.in_words(), said, "{words}");
            assert_eq!(repeat_from_cron(&repeat.to_cron()), Some(repeat), "{words}");
        }
    }

    #[test]
    fn a_stored_cron_is_said_in_words_when_it_is_a_plain_repeat() {
        assert_eq!(cron_in_words("0 0 8 * * *"), "every day at 08:00");
        assert_eq!(cron_in_words("30 7 * * 1-5"), "every weekday at 07:30");
        assert_eq!(cron_in_words("0 0 9 * * 0"), "every Sunday at 09:00");
        assert_eq!(
            cron_in_words("0 15 * * * *"),
            "every hour at 15 minutes past"
        );
        assert_eq!(cron_in_words("0 0 9 1 1 *"), "0 0 9 1 1 *");
        assert_eq!(cron_in_words("@once"), "@once");
    }

    #[test]
    fn a_to_do_keeps_a_calendar_repeat_and_refuses_one_within_a_day() {
        assert_eq!(
            read("every weekday at 18:00").unwrap().as_todo_repeat(),
            Some((Every::Weekday, at(18, 0), vec![]))
        );
        assert_eq!(
            read("every tuesday at 9am").unwrap().as_todo_repeat(),
            Some((Every::Week, at(9, 0), vec![2]))
        );
        assert_eq!(read("every 2 hours").unwrap().as_todo_repeat(), None);
    }

    #[test]
    fn cron_shaped_text_is_told_apart_from_words() {
        assert!(looks_like_cron("0 0 8 * * *"));
        assert!(looks_like_cron("*/5 * * * *"));
        assert!(!looks_like_cron("every day at 8"));
        assert!(!looks_like_cron("8"));
        assert_eq!(six_field_cron("30 7 * * 1-5"), "0 30 7 * * 1-5");
        assert_eq!(six_field_cron("0 30 7 * * 1-5"), "0 30 7 * * 1-5");
    }
}
