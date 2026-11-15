//! Asking the model which of a watch's changes are what its owner asked to be told about. A
//! watch set up with a rule or filters is judged by code; one set up in the owner's words alone
//! ("tell me when a story about Zig appears") has nothing else to go by. Nothing here calls a
//! model.

use chrono::{DateTime, Duration, Utc};

use crate::shared::domain::web_watch::WatchChange;
use crate::shared::ports::web_watch::{NoticeKind, StoredChange, WatchNotice, WebWatch};
use crate::user_data::services::model_json::object_in;

/// Changes one model call judges: few enough that a small model answers for every one.
pub const CHANGES_PER_CALL: usize = 12;
/// How long a change waits for a model before it is told anyway: an alert is never lost.
pub const HELD_FOR: Duration = Duration::hours(2);
/// How often the job looks for changes still to judge.
pub const JUDGE_POLL: std::time::Duration = std::time::Duration::from_secs(5 * 60);
/// Lines one notice carries.
const NOTICE_LINES: usize = 4;

const SYSTEM: &str = "\
{name} asked the Pond to watch a web page. Below: what they asked for, in \
their own words, and the numbered changes the latest check found on the \
page. Decide which of the changes {name} asked to be told about.

Reply with JSON only, exactly this shape:
{\"tell\":[1,3]}

- tell: the numbers of the changes that are what they asked for; [] when \
none is.
- When they asked to hear of anything new on the page, every new item counts.
- When they named a subject, a word, a price or a condition, only a change \
that meets it counts. A change about something else is never told, however \
interesting it is.";

pub fn render_system(name: &str) -> String {
    SYSTEM.replace("{name}", name)
}

/// Whether a watch's changes go to the model: it has its owner's words and nothing code can
/// check them against.
pub fn needs_judging(watch: &WebWatch) -> bool {
    !watch.looking_for.trim().is_empty() && watch.filters.is_empty()
}

pub fn build_user_message(watch: &WebWatch, changes: &[WatchChange]) -> String {
    let mut out = format!(
        "They asked: \"{}\"\nThe page: {}\n\nChanges:\n",
        watch.looking_for.trim().replace('"', "'"),
        watch.title.trim()
    );
    for (i, change) in changes.iter().enumerate() {
        out.push_str(&format!("{}. {}\n", i + 1, change.line()));
    }
    out.push_str("\nJSON:");
    out
}

/// The changes to tell, by position among `count`; `None` when the reply says nothing usable,
/// so the changes are asked about again.
pub fn parse_reply(raw: &str, count: usize) -> Option<Vec<usize>> {
    let map = object_in(raw)?;
    let told = map.get("tell")?.as_array()?;
    let mut picked: Vec<usize> = told
        .iter()
        .filter_map(|n| n.as_u64().or_else(|| n.as_str()?.trim().parse().ok()))
        .filter_map(|n| usize::try_from(n).ok())
        .filter(|n| (1..=count).contains(n))
        .map(|n| n - 1)
        .collect();
    picked.sort_unstable();
    picked.dedup();
    Some(picked)
}

/// Whether a held change has waited long enough to be told without a judgement.
pub fn waited_out(change: &StoredChange, now: DateTime<Utc>) -> bool {
    now - change.found_at >= HELD_FOR
}

/// The push for the changes that were what the owner asked for.
pub fn notice(watch: &WebWatch, told: &[&WatchChange]) -> Option<WatchNotice> {
    if told.is_empty() {
        return None;
    }
    let mut lines: Vec<String> = told
        .iter()
        .take(NOTICE_LINES)
        .map(|change| change.line())
        .collect();
    if told.len() > NOTICE_LINES {
        lines.push(format!("and {} more", told.len() - NOTICE_LINES));
    }
    Some(WatchNotice {
        owner: watch.owner.clone(),
        kind: NoticeKind::Match,
        title: watch.title.clone(),
        body: lines.join("\n"),
        watch_id: Some(watch.id.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::web_watch::{ChangeKind, TellWhen, WatchFilters, WatchItem};

    fn watch(looking_for: &str, filters: WatchFilters) -> WebWatch {
        let now = Utc::now();
        WebWatch {
            id: "w1".into(),
            owner: "m1".into(),
            address: "https://lobste.rs/rss".into(),
            title: "Lobsters".into(),
            looking_for: looking_for.into(),
            filters,
            interval_hours: 6,
            paused: false,
            items: None,
            failures: 0,
            last_checked_at: None,
            next_check_at: now,
            created_at: now,
            session_id: None,
        }
    }

    fn new(title: &str) -> WatchChange {
        WatchChange {
            kind: ChangeKind::New,
            item: WatchItem {
                title: title.to_string(),
                ..WatchItem::default()
            },
            matched: false,
        }
    }

    #[test]
    fn only_a_watch_with_words_and_no_rule_goes_to_the_model() {
        let asked = "tell me when a story mentioning Zig appears";
        assert!(needs_judging(&watch(asked, WatchFilters::default())));
        assert!(!needs_judging(&watch("  ", WatchFilters::default())));
        let ruled = WatchFilters {
            tell_when: TellWhen::Appears {
                phrase: "zig".into(),
            },
            ..WatchFilters::default()
        };
        assert!(!needs_judging(&watch(asked, ruled)));
        let filtered = WatchFilters {
            words: vec!["forester".into()],
            ..WatchFilters::default()
        };
        assert!(!needs_judging(&watch(asked, filtered)));
    }

    #[test]
    fn the_model_is_shown_their_words_and_the_numbered_changes() {
        let message = build_user_message(
            &watch("a story about \"Zig\"", WatchFilters::default()),
            &[new("Zig 0.16 released"), new("A history of Lisp")],
        );
        assert_eq!(
            message,
            "They asked: \"a story about 'Zig'\"\nThe page: Lobsters\n\nChanges:\n\
             1. New: Zig 0.16 released\n2. New: A history of Lisp\n\nJSON:"
        );
        assert!(render_system("Jerry").contains("Jerry asked the Pond"));
    }

    #[test]
    fn a_reply_names_changes_by_number_and_an_unusable_one_is_asked_again() {
        assert_eq!(
            parse_reply("{\"tell\":[2,1,2,9,\"3\"]}", 3),
            Some(vec![0, 1, 2])
        );
        assert_eq!(parse_reply("ok {\"tell\":[]} done", 3), Some(vec![]));
        assert_eq!(parse_reply("{\"told\":[1]}", 3), None);
        assert_eq!(parse_reply("none of them", 3), None);
    }

    #[test]
    fn what_was_asked_for_is_told_in_one_notice_and_a_long_wait_tells_anyway() {
        let w = watch("zig", WatchFilters::default());
        assert_eq!(notice(&w, &[]), None);
        let zig = new("Zig 0.16 released");
        let told = notice(&w, &[&zig]).unwrap();
        assert_eq!(told.title, "Lobsters");
        assert_eq!(told.body, "New: Zig 0.16 released");
        assert_eq!(told.kind, NoticeKind::Match);

        let now = Utc::now();
        let held = |hours: i64| StoredChange {
            id: 1,
            watch_id: "w1".into(),
            found_at: now - Duration::hours(hours),
            change: zig.clone(),
            digested: true,
        };
        assert!(!waited_out(&held(1), now));
        assert!(waited_out(&held(2), now));
    }
}
