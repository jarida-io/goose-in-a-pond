//! What became of a queued notice: whether its phone was woken, fetched it, showed it and had it
//! opened. "Queued" says only that a row exists; these say whether anybody got it.

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::mcp::ports::notification::Notification;

/// The `data` key a producer sets on a notice that should be said aloud in the room when no phone
/// fetches it, such as a reminder that has come due.
pub const SPEAK_IF_UNFETCHED_KEY: &str = "speak_if_unfetched";

/// The `data` key a producer sets on a notice that must not wait for quiet hours to end: a
/// reminder set for that time, or an answer to something the person just asked for.
pub const QUIET_PASS_KEY: &str = "quiet_pass";

/// Whether a notice goes out in quiet hours: an alert, or one its producer marked.
pub fn passes_quiet_hours(notification: &Notification) -> bool {
    notification.category == "alert" || flag(notification, QUIET_PASS_KEY)
}

fn flag(notification: &Notification, key: &str) -> bool {
    notification
        .data
        .as_ref()
        .and_then(|d| d.get(key))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// `data` with `key` set, for a producer marking a notice. A payload that is not an object is
/// replaced: there is nowhere in it to put a key.
pub fn marked(data: Option<serde_json::Value>, key: &str) -> Option<serde_json::Value> {
    let mut fields = match data {
        Some(serde_json::Value::Object(fields)) => fields,
        _ => serde_json::Map::new(),
    };
    fields.insert(key.to_string(), serde_json::Value::Bool(true));
    Some(serde_json::Value::Object(fields))
}

/// The one notice a phone gets for what was held in quiet hours.
pub fn morning_notice(device_id: &str, held: &[Notification], at: DateTime<Utc>) -> Notification {
    const NAMED: usize = 6;
    let mut lines: Vec<String> = held.iter().take(NAMED).map(|n| n.title.clone()).collect();
    if held.len() > NAMED {
        lines.push(format!("and {} more", held.len() - NAMED));
    }
    Notification {
        id: format!("quiet-{}:{device_id}", at.format("%Y%m%d%H%M")),
        target: device_id.to_string(),
        category: "info".to_string(),
        title: match held.len() {
            1 => "1 notice from quiet hours".to_string(),
            n => format!("{n} notices from quiet hours"),
        },
        body: lines.join("\n"),
        timestamp: at.to_rfc3339(),
        data: held.last().and_then(|n| n.data.clone()),
    }
}

/// How asking a phone's push service to wake it went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    Sent,
    NotSent(WakeGap),
}

/// Why no wake left the pond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeGap {
    /// The phone never registered a push token.
    NoToken,
    /// This pond has no key for the phone's push service.
    NoKey,
    /// The token is of a kind the pond does not send to.
    NoSender,
}

impl WakeGap {
    pub fn as_str(self) -> &'static str {
        match self {
            WakeGap::NoToken => "no_token",
            WakeGap::NoKey => "no_key",
            WakeGap::NoSender => "no_sender",
        }
    }
}

/// A copy queued for a phone that the phone has not fetched.
#[derive(Debug, Clone)]
pub struct UnfetchedNotice {
    pub notification: Notification,
    pub queued_at: DateTime<Utc>,
    pub wakes: u32,
    pub last_wake_at: Option<DateTime<Utc>>,
    pub spoken: bool,
}

impl UnfetchedNotice {
    pub fn wants_speaking(&self) -> bool {
        flag(&self.notification, SPEAK_IF_UNFETCHED_KEY)
    }
}

/// When an unfetched copy is woken again, and when it is given up on.
#[derive(Debug, Clone, Copy)]
pub struct RewakePolicy {
    /// How long a copy sits unfetched before the first repeat.
    pub first_after: Duration,
    /// The longest gap between repeats; each gap doubles up to this.
    pub longest_gap: Duration,
    /// Wakes in all, the first included.
    pub most_wakes: u32,
    /// A copy older than this is left for the phone to find when it next opens.
    pub gives_up_after: Duration,
    /// How long a copy that asks to be spoken sits unfetched before it is said in the room.
    pub speaks_after: Duration,
    /// A copy older than this is no longer worth saying: the moment has passed.
    pub speaks_until: Duration,
}

impl Default for RewakePolicy {
    fn default() -> Self {
        Self {
            first_after: Duration::minutes(2),
            longest_gap: Duration::minutes(60),
            most_wakes: 6,
            gives_up_after: Duration::hours(24),
            speaks_after: Duration::minutes(5),
            speaks_until: Duration::minutes(30),
        }
    }
}

impl RewakePolicy {
    /// The gap that must pass after wake number `wakes` before the next one.
    fn gap_after(&self, wakes: u32) -> Duration {
        let doubled = self
            .first_after
            .checked_mul(1 << wakes.saturating_sub(1).min(16))
            .unwrap_or(self.longest_gap);
        doubled.min(self.longest_gap)
    }

    pub fn wake_is_due(&self, notice: &UnfetchedNotice, now: DateTime<Utc>) -> bool {
        if notice.wakes >= self.most_wakes || now - notice.queued_at > self.gives_up_after {
            return false;
        }
        let since = notice.last_wake_at.unwrap_or(notice.queued_at);
        now - since >= self.gap_after(notice.wakes.max(1))
    }

    pub fn speaking_is_due(&self, notice: &UnfetchedNotice, now: DateTime<Utc>) -> bool {
        notice.wants_speaking()
            && !notice.spoken
            && now - notice.queued_at >= self.speaks_after
            && now - notice.queued_at <= self.speaks_until
    }
}

/// The record's totals over a period, for the reliability report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DeliveryTotals {
    /// Copies queued for phones.
    pub queued: u64,
    /// Copies a push wake was sent for.
    pub woken: u64,
    /// Copies their phone fetched.
    pub fetched: u64,
    /// Copies fetched within a minute of being queued.
    pub fetched_within_a_minute: u64,
    /// Copies fetched more than an hour after being queued.
    pub fetched_after_an_hour: u64,
    /// Copies the phone said it showed.
    pub shown: u64,
    /// Copies the person opened.
    pub opened: u64,
    /// Copies said aloud in the room because no phone fetched them.
    pub spoken: u64,
}

impl DeliveryTotals {
    pub fn never_fetched(&self) -> u64 {
        self.queued.saturating_sub(self.fetched)
    }

    /// Whole percent of queued copies fetched within a minute; `None` when nothing was queued.
    pub fn percent_within_a_minute(&self) -> Option<u64> {
        (self.queued > 0).then(|| self.fetched_within_a_minute * 100 / self.queued)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notice(
        queued_mins_ago: i64,
        wakes: u32,
        last_wake_mins_ago: Option<i64>,
    ) -> UnfetchedNotice {
        let now = now();
        UnfetchedNotice {
            notification: Notification {
                id: "n1:phone".into(),
                target: "phone".into(),
                category: "info".into(),
                title: "Reminder: get superglue".into(),
                body: String::new(),
                timestamp: now.to_rfc3339(),
                data: Some(json!({ SPEAK_IF_UNFETCHED_KEY: true })),
            },
            queued_at: now - Duration::minutes(queued_mins_ago),
            wakes,
            last_wake_at: last_wake_mins_ago.map(|m| now - Duration::minutes(m)),
            spoken: false,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-13T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn repeats_wait_longer_each_time_and_stop() {
        let policy = RewakePolicy::default();
        assert!(
            !policy.wake_is_due(&notice(1, 1, Some(1)), now()),
            "too soon"
        );
        assert!(policy.wake_is_due(&notice(2, 1, Some(2)), now()));
        assert!(
            !policy.wake_is_due(&notice(5, 2, Some(3)), now()),
            "second gap is four minutes"
        );
        assert!(policy.wake_is_due(&notice(6, 2, Some(4)), now()));
        assert!(
            !policy.wake_is_due(&notice(600, 6, Some(120)), now()),
            "six wakes is the most"
        );
        assert!(
            !policy.wake_is_due(&notice(25 * 60, 2, Some(600)), now()),
            "a day old is left"
        );
    }

    #[test]
    fn a_copy_never_woken_is_woken_once_it_has_waited() {
        let policy = RewakePolicy::default();
        assert!(!policy.wake_is_due(&notice(1, 0, None), now()));
        assert!(policy.wake_is_due(&notice(3, 0, None), now()));
    }

    #[test]
    fn only_a_notice_that_asks_is_spoken_and_only_once() {
        let policy = RewakePolicy::default();
        assert!(
            !policy.speaking_is_due(&notice(4, 1, Some(4)), now()),
            "not five minutes yet"
        );
        assert!(policy.speaking_is_due(&notice(5, 1, Some(5)), now()));

        assert!(
            !policy.speaking_is_due(&notice(31, 1, Some(9)), now()),
            "the moment has passed"
        );

        let mut said = notice(9, 1, Some(9));
        said.spoken = true;
        assert!(!policy.speaking_is_due(&said, now()));

        let mut quiet = notice(9, 1, Some(9));
        quiet.notification.data = Some(json!({"open": "devices"}));
        assert!(!policy.speaking_is_due(&quiet, now()));
    }

    #[test]
    fn only_alerts_and_marked_notices_pass_quiet_hours() {
        let mut n = notice(0, 0, None).notification;
        n.data = None;
        assert!(!passes_quiet_hours(&n), "a plain notice waits");
        n.data = marked(Some(json!({"open": "todos"})), QUIET_PASS_KEY);
        assert!(passes_quiet_hours(&n));
        assert_eq!(
            n.data.as_ref().unwrap()["open"],
            "todos",
            "marking keeps the rest"
        );
        n.data = None;
        n.category = "alert".into();
        assert!(passes_quiet_hours(&n));
    }

    #[test]
    fn the_morning_notice_names_what_was_held() {
        let held: Vec<Notification> = (1..=8)
            .map(|i| {
                let mut n = notice(0, 0, None).notification;
                n.title = format!("Notice {i}");
                n.data = Some(json!({"open": "chat", "session_id": format!("s{i}")}));
                n
            })
            .collect();
        let morning = morning_notice("phone", &held, now());
        assert_eq!(morning.title, "8 notices from quiet hours");
        assert_eq!(morning.body.lines().count(), 7);
        assert!(morning.body.ends_with("and 2 more"));
        assert_eq!(morning.target, "phone");
        assert_eq!(
            morning.data.unwrap()["session_id"],
            "s8",
            "it opens the newest"
        );
        assert_eq!(
            morning_notice("phone", &held[..1], now()).title,
            "1 notice from quiet hours"
        );
    }

    #[test]
    fn totals_say_what_share_arrived_in_a_minute() {
        let totals = DeliveryTotals {
            queued: 20,
            fetched: 15,
            fetched_within_a_minute: 9,
            ..DeliveryTotals::default()
        };
        assert_eq!(totals.never_fetched(), 5);
        assert_eq!(totals.percent_within_a_minute(), Some(45));
        assert_eq!(DeliveryTotals::default().percent_within_a_minute(), None);
    }
}
