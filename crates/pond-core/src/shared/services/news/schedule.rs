//! When a member's news is written and read aloud, in their own day: stories three times a day,
//! the digest ready by their digest time, and the speaker at that time.

use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate, NaiveTime, TimeZone, Utc};

use crate::shared::domain::news::{NewsBook, NewsSettings};

/// The longest one story pass may write for. On the Jetson a story's summary takes 20-40 s, so
/// this is about a dozen stories.
pub const STORIES_PASS_BUDGET: Duration = Duration::from_secs(10 * 60);
/// The longest one digest pass may write for: a section per interest, the intro, and its audio.
pub const DIGEST_PASS_BUDGET: Duration = Duration::from_secs(10 * 60);
/// How long before the digest time the morning's stories start: both passes three times over,
/// so the lane can be busy, or wait for quiet, and the digest is still ready.
pub const DIGEST_LEAD: Duration =
    Duration::from_secs(3 * (STORIES_PASS_BUDGET.as_secs() + DIGEST_PASS_BUDGET.as_secs()));
/// The digest starts without the morning's stories this long before its time, so a story pass
/// that never ran cannot leave the member with nothing.
pub const DIGEST_FALLBACK: Duration = Duration::from_secs(2 * DIGEST_PASS_BUDGET.as_secs());
/// A digest the speaker could not read at its time is read up to this long after it.
pub const SPEAK_WINDOW: Duration = Duration::from_secs(2 * 3600);
/// A pass marked writing for longer than this ended with the process.
pub const WRITING_STALE: Duration = Duration::from_secs(45 * 60);
/// A late digest is still written this long after its time; past it, the next day's is due.
const DIGEST_LATE_FOR: Duration = Duration::from_secs(20 * 3600);

const MIDDAY: (u32, u32) = (12, 30);
const EVENING: (u32, u32) = (17, 30);

/// What a story pass is for, which decides the interests it covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryPass {
    /// Before the digest: tiles and digest interests both.
    Morning,
    Midday,
    Evening,
    /// The member asked for fresh news.
    Refresh,
}

impl StoryPass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Morning => "morning",
            Self::Midday => "midday",
            Self::Evening => "evening",
            Self::Refresh => "refresh",
        }
    }

    /// Whether this pass writes stories for interests that feed only the digest.
    pub fn feeds_digest(self) -> bool {
        !matches!(self, Self::Midday | Self::Evening)
    }
}

/// `time` on `day` in `zone`, as UTC. A time a clock change skips reads as an hour later.
pub fn local_at<Tz: TimeZone>(zone: &Tz, day: NaiveDate, time: NaiveTime) -> DateTime<Utc> {
    let naive = day.and_time(time);
    zone.from_local_datetime(&naive)
        .earliest()
        .or_else(|| {
            zone.from_local_datetime(&(naive + chrono::Duration::hours(1)))
                .earliest()
        })
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
}

fn local_day<Tz: TimeZone>(zone: &Tz, at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(zone).date_naive()
}

fn chrono_of(d: Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::zero())
}

/// When the digest for `day` is due to be ready.
pub fn ready_at<Tz: TimeZone>(zone: &Tz, settings: &NewsSettings, day: NaiveDate) -> DateTime<Utc> {
    local_at(zone, day, settings.digest_time_of_day())
}

/// The story passes of `day` that apply under `settings`, as instants.
fn slots_on<Tz: TimeZone>(
    zone: &Tz,
    settings: &NewsSettings,
    day: NaiveDate,
) -> Vec<(StoryPass, DateTime<Utc>)> {
    let mut slots = Vec::new();
    if settings.tiles || settings.digest {
        slots.push((
            StoryPass::Morning,
            ready_at(zone, settings, day) - chrono_of(DIGEST_LEAD),
        ));
    }
    if settings.tiles {
        for (pass, (h, m)) in [(StoryPass::Midday, MIDDAY), (StoryPass::Evening, EVENING)] {
            let time = NaiveTime::from_hms_opt(h, m, 0).expect("a fixed time of day");
            slots.push((pass, local_at(zone, day, time)));
        }
    }
    slots
}

/// The story pass due now, if one is: a refresh the member asked for, else the latest of the
/// day's passes that has started since the last pass finished.
pub fn story_pass_due<Tz: TimeZone>(
    now: DateTime<Utc>,
    zone: &Tz,
    settings: &NewsSettings,
    book: &NewsBook,
) -> Option<StoryPass> {
    if book.refresh_requested_at.is_some() && (settings.tiles || settings.digest) {
        return Some(StoryPass::Refresh);
    }
    let today = local_day(zone, now);
    let (pass, started) = [today.pred_opt(), Some(today), today.succ_opt()]
        .into_iter()
        .flatten()
        .flat_map(|day| slots_on(zone, settings, day))
        .filter(|(_, at)| *at <= now)
        .max_by_key(|(_, at)| *at)?;
    match book.stories_written_at {
        Some(done) if done >= started => None,
        _ => Some(pass),
    }
}

/// The member's day whose digest is being written for now: the next digest time within
/// [`DIGEST_LEAD`], else the last one, while it is less than a day late.
pub fn digest_day<Tz: TimeZone>(
    now: DateTime<Utc>,
    zone: &Tz,
    settings: &NewsSettings,
) -> Option<NaiveDate> {
    let today = local_day(zone, now);
    [today.succ_opt(), Some(today), today.pred_opt()]
        .into_iter()
        .flatten()
        .find(|day| {
            let ready = ready_at(zone, settings, *day);
            now >= ready - chrono_of(DIGEST_LEAD) && now < ready + chrono_of(DIGEST_LATE_FOR)
        })
}

/// Whether the digest for `day` should be written now: the morning's stories are in, or its time
/// is close enough that it starts without them.
pub fn digest_due<Tz: TimeZone>(
    now: DateTime<Utc>,
    zone: &Tz,
    settings: &NewsSettings,
    book: &NewsBook,
    day: NaiveDate,
    already_written: bool,
) -> bool {
    if !settings.digest || already_written {
        return false;
    }
    let ready = ready_at(zone, settings, day);
    let morning = ready - chrono_of(DIGEST_LEAD);
    let stories_in = book.stories_written_at.is_some_and(|done| done >= morning);
    now >= morning && (stories_in || now >= ready - chrono_of(DIGEST_FALLBACK))
}

/// When the next digest will be ready: `day`'s, unless it is written, then the day after's.
pub fn next_digest_at<Tz: TimeZone>(
    now: DateTime<Utc>,
    zone: &Tz,
    settings: &NewsSettings,
    written: impl Fn(NaiveDate) -> bool,
) -> Option<DateTime<Utc>> {
    if !settings.digest {
        return None;
    }
    let day = digest_day(now, zone, settings).unwrap_or_else(|| local_day(zone, now));
    let day = if written(day) {
        day.checked_add_days(Days::new(1))?
    } else {
        day
    };
    Some(ready_at(zone, settings, day))
}

/// Whether the house speaker should read `day`'s digest now: the member asked for it, the digest
/// is written, its time has come and not long passed, and it has not been read yet.
pub fn speak_due<Tz: TimeZone>(
    now: DateTime<Utc>,
    zone: &Tz,
    settings: &NewsSettings,
    book: &NewsBook,
    day: NaiveDate,
    written: bool,
) -> bool {
    if !settings.spoken_on_speaker || !written || book.spoken_on == Some(day) {
        return false;
    }
    let ready = ready_at(zone, settings, day);
    now >= ready && now < ready + chrono_of(SPEAK_WINDOW)
}

/// Whether the member's news is being written, or waits for the lane to write it.
pub fn writing(now: DateTime<Utc>, book: &NewsBook) -> bool {
    book.refresh_requested_at.is_some()
        || book
            .writing_since
            .is_some_and(|since| now - since < chrono_of(WRITING_STALE))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Africa::Nairobi;

    fn at(day: u32, h: u32, m: u32) -> DateTime<Utc> {
        local_at(
            &Nairobi,
            NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
            NaiveTime::from_hms_opt(h, m, 0).unwrap(),
        )
    }

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, day).unwrap()
    }

    #[test]
    fn the_lead_is_three_times_both_budgets() {
        assert_eq!(DIGEST_LEAD, Duration::from_secs(60 * 60));
        assert!(DIGEST_FALLBACK < DIGEST_LEAD);
    }

    #[test]
    fn three_story_passes_a_day_each_once() {
        let s = NewsSettings::default();
        let mut book = NewsBook::default();
        assert_eq!(
            story_pass_due(at(9, 5, 59), &Nairobi, &s, &book),
            Some(StoryPass::Evening)
        );
        book.stories_written_at = Some(at(8, 18, 0));
        assert_eq!(story_pass_due(at(9, 5, 59), &Nairobi, &s, &book), None);
        assert_eq!(
            story_pass_due(at(9, 6, 0), &Nairobi, &s, &book),
            Some(StoryPass::Morning)
        );
        book.stories_written_at = Some(at(9, 6, 9));
        assert_eq!(story_pass_due(at(9, 12, 29), &Nairobi, &s, &book), None);
        assert_eq!(
            story_pass_due(at(9, 12, 31), &Nairobi, &s, &book),
            Some(StoryPass::Midday)
        );
        book.stories_written_at = Some(at(9, 13, 0));
        assert_eq!(
            story_pass_due(at(9, 17, 45), &Nairobi, &s, &book),
            Some(StoryPass::Evening)
        );
    }

    #[test]
    fn a_refresh_goes_first_and_tiles_off_leaves_only_the_morning() {
        let mut s = NewsSettings::default();
        let book = NewsBook {
            refresh_requested_at: Some(at(9, 9, 0)),
            stories_written_at: Some(at(9, 8, 0)),
            ..Default::default()
        };
        assert_eq!(
            story_pass_due(at(9, 9, 1), &Nairobi, &s, &book),
            Some(StoryPass::Refresh)
        );
        s.tiles = false;
        let book = NewsBook {
            stories_written_at: Some(at(9, 6, 5)),
            ..Default::default()
        };
        assert_eq!(story_pass_due(at(9, 18, 0), &Nairobi, &s, &book), None);
        s.digest = false;
        assert_eq!(story_pass_due(at(10, 6, 30), &Nairobi, &s, &book), None);
    }

    #[test]
    fn the_digest_waits_for_the_morning_stories_then_starts_without_them() {
        let s = NewsSettings::default();
        let mut book = NewsBook {
            stories_written_at: Some(at(8, 18, 0)),
            ..Default::default()
        };
        let day = digest_day(at(9, 6, 10), &Nairobi, &s).unwrap();
        assert_eq!(day, date(9));
        assert!(!digest_due(at(9, 6, 10), &Nairobi, &s, &book, day, false));
        assert!(digest_due(at(9, 6, 40), &Nairobi, &s, &book, day, false));
        book.stories_written_at = Some(at(9, 6, 12));
        assert!(digest_due(at(9, 6, 13), &Nairobi, &s, &book, day, false));
        assert!(!digest_due(at(9, 6, 13), &Nairobi, &s, &book, day, true));
        assert_eq!(digest_day(at(9, 2, 30), &Nairobi, &s), Some(date(8)));
        assert_eq!(digest_day(at(9, 3, 30), &Nairobi, &s), None);
    }

    #[test]
    fn a_digest_time_just_after_midnight_belongs_to_the_next_day() {
        let s = NewsSettings {
            digest_time: "00:30".into(),
            ..Default::default()
        };
        assert_eq!(digest_day(at(8, 23, 45), &Nairobi, &s), Some(date(9)));
        assert_eq!(
            story_pass_due(at(8, 23, 45), &Nairobi, &s, &NewsBook::default()),
            Some(StoryPass::Morning)
        );
    }

    #[test]
    fn the_next_digest_is_tomorrows_once_todays_is_written() {
        let s = NewsSettings::default();
        let now = at(9, 9, 0);
        assert_eq!(
            next_digest_at(now, &Nairobi, &s, |_| false),
            Some(at(9, 7, 0))
        );
        assert_eq!(
            next_digest_at(now, &Nairobi, &s, |d| d == date(9)),
            Some(at(10, 7, 0))
        );
        let off = NewsSettings {
            digest: false,
            ..Default::default()
        };
        assert_eq!(next_digest_at(now, &Nairobi, &off, |_| false), None);
    }

    #[test]
    fn the_speaker_reads_once_from_the_digest_time_for_a_while() {
        let s = NewsSettings {
            spoken_on_speaker: true,
            ..Default::default()
        };
        let mut book = NewsBook::default();
        assert!(!speak_due(at(9, 6, 59), &Nairobi, &s, &book, date(9), true));
        assert!(speak_due(at(9, 7, 0), &Nairobi, &s, &book, date(9), true));
        assert!(!speak_due(at(9, 7, 0), &Nairobi, &s, &book, date(9), false));
        assert!(!speak_due(at(9, 9, 1), &Nairobi, &s, &book, date(9), true));
        book.spoken_on = Some(date(9));
        assert!(!speak_due(at(9, 7, 5), &Nairobi, &s, &book, date(9), true));
        let quiet = NewsSettings::default();
        assert!(!speak_due(
            at(9, 7, 0),
            &Nairobi,
            &quiet,
            &NewsBook::default(),
            date(9),
            true
        ));
    }

    #[test]
    fn a_pass_that_died_stops_reading_as_writing() {
        let now = at(9, 12, 0);
        let mut book = NewsBook {
            writing_since: Some(now - chrono::Duration::minutes(5)),
            ..Default::default()
        };
        assert!(writing(now, &book));
        book.writing_since = Some(now - chrono::Duration::hours(2));
        assert!(!writing(now, &book));
        book.refresh_requested_at = Some(now);
        assert!(writing(now, &book));
    }
}
