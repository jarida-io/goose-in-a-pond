//! A web watch: a page a member asked the pond to keep checking, the items it last listed, and
//! what counts as a match. A check compares items, never text, so ads, dates and layout don't
//! read as changes; and the comparison, the filters and the notice are all code, so a check never
//! needs the model.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Hours between checks unless the member sets another.
pub const DEFAULT_INTERVAL_HOURS: u32 = 6;
/// The shortest interval a member may set: a site is checked as a person would, not hammered.
pub const MIN_INTERVAL_HOURS: u32 = 1;
pub const MAX_INTERVAL_HOURS: u32 = 7 * 24;

/// Watches one member may keep.
pub const MAX_WATCHES_PER_MEMBER: usize = 20;
/// Days a watch keeps the changes it found.
pub const CHANGE_HISTORY_DAYS: i64 = 30;
/// Failed checks in a row before the owner is told the site is refusing.
pub const FAILURES_BEFORE_TELLING: u32 = 3;
/// The longest back-off after failures: a day.
pub const MAX_BACKOFF_HOURS: u32 = 24;
/// The local hour the daily digest goes out.
pub const DIGEST_HOUR: u32 = 18;

/// One thing a watched page lists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchItem {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mileage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

impl WatchItem {
    /// What names the item across checks: its link, else its title.
    pub fn key(&self) -> String {
        match self
            .link
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty())
        {
            Some(link) => link.trim_end_matches('/').to_string(),
            None => self.title.trim().to_lowercase(),
        }
    }

    pub fn price_value(&self) -> Option<f64> {
        self.price.as_deref().and_then(number_in)
    }

    pub fn mileage_value(&self) -> Option<f64> {
        self.mileage.as_deref().and_then(number_in)
    }

    /// The year it states, else the first year in its title.
    pub fn year_value(&self) -> Option<u32> {
        self.year
            .as_deref()
            .and_then(year_in)
            .or_else(|| year_in(&self.title))
    }

    /// Its price as people read it: "KES 1,450,000".
    pub fn price_words(&self) -> Option<String> {
        let price = self.price.as_deref()?;
        let shown = match number_in(price) {
            Some(n) if price.chars().all(|c| c.is_ascii_digit() || c == '.') => grouped(n),
            _ => price.to_string(),
        };
        Some(match &self.currency {
            Some(c) if !shown.contains(c.as_str()) => format!("{c} {shown}"),
            _ => shown,
        })
    }

    fn mileage_words(&self) -> Option<String> {
        let mileage = self.mileage.as_deref()?;
        Some(match number_in(mileage) {
            Some(n) if mileage.chars().all(|c| c.is_ascii_digit() || c == '.') => {
                format!("{} km", grouped(n))
            }
            _ => mileage.to_string(),
        })
    }
}

/// What a watch tells its owner about. The default is a listing's: a new item, an update or a
/// price drop that fits the filters. The others are read off the whole page, in code.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "when", rename_all = "snake_case")]
pub enum TellWhen {
    #[default]
    NewOrChanged,
    /// The phrase is on the page when it was not at the last check.
    Appears { phrase: String },
    /// The phrase was on the page at the last check and is not now.
    Disappears { phrase: String },
    /// The number beside the phrase is under `value` when it was not at the last check.
    Below { phrase: String, value: f64 },
    /// The number beside the phrase is over `value` when it was not at the last check.
    Above { phrase: String, value: f64 },
}

/// Characters after a phrase in which its number is looked for.
const NUMBER_WITHIN: usize = 60;
/// The longest line of a page a notice quotes.
const QUOTED_CHARS: usize = 160;

fn plain(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// An item as the page showed it, on one line.
fn item_text(item: &WatchItem) -> String {
    [
        Some(item.title.as_str()),
        item.price.as_deref(),
        item.snippet.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

/// The first item whose text has `phrase`, as the page showed it.
fn item_with(items: &[WatchItem], phrase: &str) -> Option<String> {
    let phrase = plain(phrase);
    if phrase.is_empty() {
        return None;
    }
    items
        .iter()
        .map(item_text)
        .find(|text| text.to_lowercase().contains(&phrase))
}

/// The number written just after `phrase` on the page; with no phrase, the first price.
pub fn number_beside(items: &[WatchItem], phrase: &str) -> Option<f64> {
    let phrase = plain(phrase);
    if phrase.is_empty() {
        return items.iter().find_map(WatchItem::price_value);
    }
    items.iter().map(item_text).find_map(|text| {
        let lower = text.to_lowercase();
        let at = lower.find(&phrase)? + phrase.len();
        let after: String = lower[at..].chars().take(NUMBER_WITHIN).collect();
        number_in(&after)
    })
}

fn quoted(text: &str) -> String {
    if text.chars().count() <= QUOTED_CHARS {
        return text.to_string();
    }
    let cut: String = text.chars().take(QUOTED_CHARS).collect();
    format!(
        "{} ...",
        cut[..cut.rfind(' ').unwrap_or(cut.len())].trim_end()
    )
}

impl TellWhen {
    pub fn is_default(&self) -> bool {
        *self == TellWhen::NewOrChanged
    }

    /// The rule as people read it: `when "sold out" is gone`, `when USD goes above 130`.
    pub fn in_words(&self) -> Option<String> {
        Some(match self {
            TellWhen::NewOrChanged => return None,
            TellWhen::Appears { phrase } => format!("when \"{}\" appears", phrase.trim()),
            TellWhen::Disappears { phrase } => format!("when \"{}\" is gone", phrase.trim()),
            TellWhen::Below { phrase, value } => {
                format!("when {} goes below {}", subject(phrase), shown(*value))
            }
            TellWhen::Above { phrase, value } => {
                format!("when {} goes above {}", subject(phrase), shown(*value))
            }
        })
    }

    /// How the page stands against the rule now, for the reply when a watch is set up.
    /// `None` when the rule cannot be read off this page at all.
    pub fn standing(&self, items: &[WatchItem]) -> Option<String> {
        match self {
            TellWhen::NewOrChanged => None,
            TellWhen::Appears { phrase } | TellWhen::Disappears { phrase } => {
                Some(match item_with(items, phrase) {
                    Some(_) => format!("\"{}\" is on the page now", phrase.trim()),
                    None => format!("\"{}\" is not on the page now", phrase.trim()),
                })
            }
            TellWhen::Below { phrase, .. } | TellWhen::Above { phrase, .. } => {
                number_beside(items, phrase)
                    .map(|n| format!("{} is {} now", subject(phrase), shown(n)))
            }
        }
    }

    /// The one change the rule reports from `before` to `now`, if it came true between them.
    fn came_true(&self, before: &[WatchItem], now: &[WatchItem]) -> Option<String> {
        match self {
            TellWhen::NewOrChanged => None,
            TellWhen::Appears { phrase } => {
                match (item_with(before, phrase), item_with(now, phrase)) {
                    (None, Some(line)) => Some(format!(
                        "\"{}\" is on the page now: {}",
                        phrase.trim(),
                        quoted(&line)
                    )),
                    _ => None,
                }
            }
            TellWhen::Disappears { phrase } => {
                match (item_with(before, phrase), item_with(now, phrase)) {
                    (Some(_), None) => {
                        Some(format!("\"{}\" is no longer on the page", phrase.trim()))
                    }
                    _ => None,
                }
            }
            TellWhen::Below { phrase, value } => {
                let now = number_beside(now, phrase)?;
                let was_under = number_beside(before, phrase).is_some_and(|b| b < *value);
                (now < *value && !was_under).then(|| {
                    format!(
                        "{} is {} now, below {}",
                        subject(phrase),
                        shown(now),
                        shown(*value)
                    )
                })
            }
            TellWhen::Above { phrase, value } => {
                let now = number_beside(now, phrase)?;
                let was_over = number_beside(before, phrase).is_some_and(|b| b > *value);
                (now > *value && !was_over).then(|| {
                    format!(
                        "{} is {} now, above {}",
                        subject(phrase),
                        shown(now),
                        shown(*value)
                    )
                })
            }
        }
    }
}

/// A number as a notice shows it: whole numbers grouped, others to two places.
fn shown(n: f64) -> String {
    if n.fract().abs() < 1e-9 {
        return grouped(n);
    }
    let fixed = format!("{n:.2}");
    fixed
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

/// What a number rule calls its number.
fn subject(phrase: &str) -> String {
    match phrase.trim() {
        "" => "the price".to_string(),
        phrase => phrase.to_string(),
    }
}

/// Parts of a page that change on every visit without anything having happened: a clock time,
/// "3 hours ago", a count of views.
static PASSING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        \b\d{1,2}:\d{2}(:\d{2})?(\s?[ap]\.?m\.?)?
        | \b(\d+|an?|one)\s+(second|sec|minute|min|hour|hr|day|week|month|year)s?\s+ago\b
        | \bjust\s+now\b
        | \b[\d.,]+[km]?\s+(views?|likes?|comments?|shares?|replies|reply|reads?|reactions?)\b
        ",
    )
    .expect("the pattern is a constant")
});

/// `text` without what changes on every visit, for telling a real update from a passing one.
pub fn steady(text: &str) -> String {
    PASSING
        .replace_all(text, "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the member is looking for, as the model wrote it from their words and they may edit.
/// An empty filter matches every item.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WatchFilters {
    /// Words every matching title has, such as ["subaru", "forester"].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_price: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_year: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mileage: Option<f64>,
    /// What to tell them about; the default is a new or changed item that fits the rest.
    #[serde(default, skip_serializing_if = "TellWhen::is_default")]
    pub tell_when: TellWhen,
}

impl WatchFilters {
    /// Whether `item` is what they asked for. A filter on a value the item doesn't state is not
    /// met: an alert has to be one they can trust.
    pub fn matches(&self, item: &WatchItem) -> bool {
        let title = item.title.to_lowercase();
        let snippet = item.snippet.as_deref().unwrap_or("").to_lowercase();
        self.words
            .iter()
            .map(|w| w.trim().to_lowercase())
            .filter(|w| !w.is_empty())
            .all(|w| title.contains(&w) || snippet.contains(&w))
            && self
                .max_price
                .is_none_or(|max| item.price_value().is_some_and(|p| p <= max))
            && self
                .min_year
                .is_none_or(|min| item.year_value().is_some_and(|y| y >= min))
            && self
                .max_mileage
                .is_none_or(|max| item.mileage_value().is_some_and(|m| m <= max))
    }

    pub fn is_empty(&self) -> bool {
        self == &WatchFilters::default()
    }

    /// The filters in words, for a list or a notice: "subaru forester, up to 1,500,000".
    pub fn words_for_people(&self) -> String {
        if let Some(rule) = self.tell_when.in_words() {
            return rule;
        }
        let mut parts = Vec::new();
        if !self.words.is_empty() {
            parts.push(self.words.join(" "));
        }
        if let Some(max) = self.max_price {
            parts.push(format!("up to {}", grouped(max)));
        }
        if let Some(min) = self.min_year {
            parts.push(format!("{min} or newer"));
        }
        if let Some(max) = self.max_mileage {
            parts.push(format!("under {} km", grouped(max)));
        }
        if parts.is_empty() {
            "anything".to_string()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeKind {
    New,
    Gone,
    PriceChanged {
        from: String,
        to: String,
    },
    Updated {
        from: String,
        to: String,
    },
    /// What the watch was set to tell came true; the item's title says what.
    Condition,
}

/// One change a check found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WatchChange {
    #[serde(flatten)]
    pub kind: ChangeKind,
    pub item: WatchItem,
    /// Whether it is what the member asked for, and so pushed at once rather than in the digest.
    pub matched: bool,
}

impl WatchChange {
    /// The change as one line: "New: 2017 Subaru Forester, KES 1,450,000, 98,000 km".
    pub fn line(&self) -> String {
        let item = &self.item;
        if self.kind == ChangeKind::Condition {
            return item.title.clone();
        }
        let mut facts = vec![item.title.clone()];
        match &self.kind {
            ChangeKind::PriceChanged { from, to } => {
                facts.push(format!(
                    "{} to {}",
                    price_with(item, from),
                    price_with(item, to)
                ));
            }
            ChangeKind::Updated { from: _, to } => {
                if !to.trim().is_empty() {
                    facts.push(to.clone());
                }
            }
            _ => facts.extend(item.price_words()),
        }
        if !matches!(self.kind, ChangeKind::Gone | ChangeKind::Updated { .. }) {
            facts.extend(item.mileage_words());
        }
        let what = match &self.kind {
            ChangeKind::New => "New",
            ChangeKind::Gone => "Gone",
            ChangeKind::PriceChanged { from, to } => match (number_in(from), number_in(to)) {
                (Some(a), Some(b)) if b < a => "Price down",
                (Some(a), Some(b)) if b > a => "Price up",
                _ => "Price changed",
            },
            ChangeKind::Updated { .. } => "Updated",
            ChangeKind::Condition => "Now",
        };
        format!("{what}: {}", facts.join(", "))
    }

    fn is_price_drop(&self) -> bool {
        match &self.kind {
            ChangeKind::PriceChanged { from, to } => {
                matches!((number_in(from), number_in(to)), (Some(a), Some(b)) if b < a)
            }
            _ => false,
        }
    }
}

fn price_with(item: &WatchItem, price: &str) -> String {
    WatchItem {
        price: Some(price.to_string()),
        ..item.clone()
    }
    .price_words()
    .unwrap_or_else(|| price.to_string())
}

/// What one check found against the items before it.
#[derive(Debug, Clone, PartialEq)]
pub enum Comparison {
    /// The first check: these are the items from now on, and nothing is reported.
    First,
    Changes(Vec<WatchChange>),
    /// Every item went at once: more likely the page moved or changed its layout than that
    /// everything sold, so it is reported as that, not as a list of gone items.
    Emptied,
}

/// The changes from `before` to `now`, each marked matched or not by `filters`. A new item or a
/// price drop that matches is a match; anything else goes to the digest.
pub fn compare(
    before: Option<&[WatchItem]>,
    now: &[WatchItem],
    filters: &WatchFilters,
) -> Comparison {
    let Some(before) = before else {
        return Comparison::First;
    };
    if now.is_empty() && !before.is_empty() {
        return Comparison::Emptied;
    }
    // A rule read off the whole page reports only itself: its owner asked for one thing.
    if !filters.tell_when.is_default() {
        let told = filters
            .tell_when
            .came_true(before, now)
            .map(|what| WatchChange {
                kind: ChangeKind::Condition,
                item: WatchItem {
                    title: what,
                    ..WatchItem::default()
                },
                matched: true,
            });
        return Comparison::Changes(told.into_iter().collect());
    }
    let mut changes = Vec::new();
    for item in now {
        let key = item.key();
        match before.iter().find(|b| b.key() == key) {
            None => changes.push(WatchChange {
                kind: ChangeKind::New,
                item: item.clone(),
                matched: false,
            }),
            Some(old) => {
                let (from, to) = (old.price.clone(), item.price.clone());
                let moved = match (old.price_value(), item.price_value()) {
                    (Some(a), Some(b)) => (a - b).abs() > f64::EPSILON,
                    _ => from != to,
                };
                if let (true, Some(from), Some(to)) = (moved, from, to) {
                    changes.push(WatchChange {
                        kind: ChangeKind::PriceChanged { from, to },
                        item: item.clone(),
                        matched: false,
                    });
                } else if old.snippet != item.snippet {
                    let from = old.snippet.clone().unwrap_or_default();
                    let to = item.snippet.clone().unwrap_or_default();
                    // A clock time or a view count moving is not an update.
                    if steady(&from) != steady(&to) {
                        changes.push(WatchChange {
                            kind: ChangeKind::Updated { from, to },
                            item: item.clone(),
                            matched: false,
                        });
                    }
                }
            }
        }
    }
    for old in before {
        let key = old.key();
        if !now.iter().any(|n| n.key() == key) {
            changes.push(WatchChange {
                kind: ChangeKind::Gone,
                item: old.clone(),
                matched: false,
            });
        }
    }
    for change in &mut changes {
        let wanted = matches!(change.kind, ChangeKind::New | ChangeKind::Updated { .. })
            || change.is_price_drop();
        change.matched = wanted && filters.matches(&change.item);
    }
    Comparison::Changes(changes)
}

/// Hours to wait after `failures` failed checks in a row: the interval, doubled for each
/// failure, up to a day.
pub fn backoff_hours(interval_hours: u32, failures: u32) -> u32 {
    let doubled = interval_hours.saturating_mul(1u32 << failures.min(8));
    doubled.min(MAX_BACKOFF_HOURS.max(interval_hours))
}

/// The number written in `text`: "KES 1,450,000" is 1450000, "1.45M" is 1450000, "98k km" is
/// 98000. `None` when there is none.
pub fn number_in(text: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    let start = lower.find(|c: char| c.is_ascii_digit())?;
    let mut digits = String::new();
    let mut end = start;
    for (i, c) in lower[start..].char_indices() {
        match c {
            '0'..='9' => digits.push(c),
            '.' => digits.push('.'),
            ',' | ' ' | '\u{a0}' | '_' | '\'' => {}
            _ => {
                end = start + i;
                break;
            }
        }
        end = start + i + c.len_utf8();
    }
    let digits = digits.trim_end_matches('.');
    let value: f64 = digits.parse().ok()?;
    let rest = lower[end..].trim_start();
    let scale = if rest.starts_with("million") || rest.starts_with('m') && !rest.starts_with("mi") {
        1_000_000.0
    } else if rest.starts_with('k') && !rest.starts_with("km") {
        1_000.0
    } else {
        1.0
    };
    Some(value * scale)
}

fn year_in(text: &str) -> Option<u32> {
    let chars: Vec<char> = text.chars().collect();
    chars.windows(4).enumerate().find_map(|(i, w)| {
        let before = i.checked_sub(1).and_then(|j| chars.get(j));
        let after = chars.get(i + 4);
        let alone =
            !before.is_some_and(char::is_ascii_digit) && !after.is_some_and(char::is_ascii_digit);
        let year: String = w.iter().collect();
        (alone
            && w.iter().all(char::is_ascii_digit)
            && (year.starts_with("19") || year.starts_with("20")))
        .then(|| year.parse().ok())
        .flatten()
    })
}

/// `n` with thousands grouped: 1450000 is "1,450,000".
fn grouped(n: f64) -> String {
    let whole = n.round() as i64;
    let digits = whole.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if whole < 0 {
        format!("-{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn car(title: &str, price: &str, link: &str) -> WatchItem {
        WatchItem {
            title: title.into(),
            price: Some(price.into()),
            currency: Some("KES".into()),
            link: Some(link.into()),
            ..Default::default()
        }
    }

    fn forester_under(max: f64) -> WatchFilters {
        WatchFilters {
            words: vec!["subaru".into(), "forester".into()],
            max_price: Some(max),
            ..Default::default()
        }
    }

    #[test]
    fn numbers_are_read_as_people_write_them() {
        assert_eq!(number_in("KES 1,450,000"), Some(1_450_000.0));
        assert_eq!(number_in("KSh 1.45M"), Some(1_450_000.0));
        assert_eq!(number_in("1.5 million"), Some(1_500_000.0));
        assert_eq!(number_in("98k km"), Some(98_000.0));
        assert_eq!(number_in("98,000 km"), Some(98_000.0));
        assert_eq!(number_in("120000 mi"), Some(120_000.0));
        assert_eq!(number_in("Call for price"), None);
    }

    #[test]
    fn the_first_check_reports_nothing() {
        let now = [car(
            "2017 Subaru Forester",
            "1450000",
            "https://c.example/1",
        )];
        assert_eq!(
            compare(None, &now, &WatchFilters::default()),
            Comparison::First
        );
    }

    #[test]
    fn a_new_match_and_a_price_drop_are_matches_and_the_rest_digest() {
        let before = [
            car("2017 Subaru Forester", "1600000", "https://c.example/1"),
            car("2015 Toyota RAV4", "1200000", "https://c.example/2"),
        ];
        let now = [
            car("2017 Subaru Forester", "1450000", "https://c.example/1/"),
            car("2016 Subaru Forester XT", "1400000", "https://c.example/3"),
            car("2018 Subaru Forester", "1900000", "https://c.example/4"),
        ];
        let Comparison::Changes(changes) =
            compare(Some(&before), &now, &forester_under(1_500_000.0))
        else {
            panic!("changes expected");
        };
        let lines: Vec<(String, bool)> = changes.iter().map(|c| (c.line(), c.matched)).collect();
        assert_eq!(
            lines,
            vec![
                (
                    "Price down: 2017 Subaru Forester, KES 1,600,000 to KES 1,450,000".into(),
                    true
                ),
                ("New: 2016 Subaru Forester XT, KES 1,400,000".into(), true),
                ("New: 2018 Subaru Forester, KES 1,900,000".into(), false),
                ("Gone: 2015 Toyota RAV4, KES 1,200,000".into(), false),
            ]
        );
    }

    #[test]
    fn a_page_that_lists_nothing_at_once_is_reported_as_emptied() {
        let before = [car("A car", "1", "https://c.example/1")];
        assert_eq!(
            compare(Some(&before), &[], &WatchFilters::default()),
            Comparison::Emptied
        );
    }

    #[test]
    fn a_filter_on_a_value_the_item_lacks_is_not_met() {
        let mut item = car("2017 Subaru Forester", "1450000", "https://c.example/1");
        let filters = WatchFilters {
            min_year: Some(2016),
            max_mileage: Some(100_000.0),
            ..Default::default()
        };
        assert!(!filters.matches(&item), "no mileage stated");
        item.mileage = Some("98,000 km".into());
        assert!(filters.matches(&item), "the year comes from the title");
        assert!(WatchFilters::default().matches(&item));
        assert_eq!(
            forester_under(1_500_000.0).words_for_people(),
            "subaru forester, up to 1,500,000"
        );
    }

    #[test]
    fn backoff_doubles_up_to_a_day() {
        assert_eq!(backoff_hours(6, 1), 12);
        assert_eq!(backoff_hours(6, 2), 24);
        assert_eq!(backoff_hours(6, 5), 24);
        assert_eq!(
            backoff_hours(48, 3),
            48,
            "never sooner than the member's interval"
        );
    }

    #[test]
    fn an_item_is_named_by_its_link_else_its_title() {
        assert_eq!(
            car("X", "1", "https://c.example/1/").key(),
            "https://c.example/1"
        );
        let mut nameless = car("Big Kettle", "1", "");
        nameless.link = None;
        assert_eq!(nameless.key(), "big kettle");
    }

    #[test]
    fn a_content_snippet_update_is_reported_and_matches_keywords() {
        let before = [WatchItem {
            title: "Application Deadlines".into(),
            link: Some("https://uni.example/admissions#deadlines".into()),
            snippet: Some("Applications close November 15".into()),
            ..Default::default()
        }];
        let now = [WatchItem {
            title: "Application Deadlines".into(),
            link: Some("https://uni.example/admissions#deadlines".into()),
            snippet: Some("Applications extended to December 1".into()),
            ..Default::default()
        }];
        let filters = WatchFilters {
            words: vec!["december".into()],
            ..Default::default()
        };
        let Comparison::Changes(changes) = compare(Some(&before), &now, &filters) else {
            panic!("changes expected");
        };
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes[0].line(),
            "Updated: Application Deadlines, Applications extended to December 1"
        );
        assert!(changes[0].matched);
    }

    fn section(title: &str, text: &str) -> WatchItem {
        WatchItem {
            title: title.into(),
            link: Some(format!("https://shop.example/#{}", title.to_lowercase())),
            snippet: Some(text.into()),
            ..Default::default()
        }
    }

    fn told(before: &[WatchItem], now: &[WatchItem], rule: TellWhen) -> Vec<String> {
        let filters = WatchFilters {
            tell_when: rule,
            ..Default::default()
        };
        match compare(Some(before), now, &filters) {
            Comparison::Changes(changes) => {
                assert!(changes
                    .iter()
                    .all(|c| c.matched && c.kind == ChangeKind::Condition));
                changes.iter().map(WatchChange::line).collect()
            }
            other => panic!("changes expected, got {other:?}"),
        }
    }

    #[test]
    fn a_phrase_appearing_is_told_once_and_only_when_it_was_not_there() {
        let closed = [section("Tickets", "Registration opens soon")];
        let open = [section("Tickets", "Registration is now open for 2027")];
        let rule = TellWhen::Appears {
            phrase: "now open".into(),
        };
        assert_eq!(
            told(&closed, &open, rule.clone()),
            vec!["\"now open\" is on the page now: Tickets Registration is now open for 2027"]
        );
        assert!(
            told(&open, &open, rule.clone()).is_empty(),
            "still there is not news"
        );
        assert!(
            told(&open, &closed, rule).is_empty(),
            "going away is not what was asked"
        );
    }

    #[test]
    fn a_phrase_going_away_is_told_and_other_changes_on_the_page_are_not() {
        let sold_out = [
            section("Bike", "Trail bike 29er. Sold out"),
            section("News", "Old post"),
        ];
        let back = [
            section("Bike", "Trail bike 29er. In stock"),
            section("News", "A new post nobody asked about"),
        ];
        let rule = TellWhen::Disappears {
            phrase: "Sold  Out".into(),
        };
        assert_eq!(
            told(&sold_out, &back, rule.clone()),
            vec!["\"Sold  Out\" is no longer on the page"]
        );
        assert!(told(&back, &back, rule).is_empty());
    }

    #[test]
    fn a_number_is_told_when_it_crosses_and_not_again_while_it_stays_across() {
        let at = |rate: &str| [section("Rates", &format!("USD/KES {rate} EUR/KES 151.20"))];
        let rule = TellWhen::Above {
            phrase: "usd/kes".into(),
            value: 130.0,
        };
        assert_eq!(number_beside(&at("129.40"), "USD/KES"), Some(129.4));
        assert!(told(&at("129.40"), &at("129.90"), rule.clone()).is_empty());
        assert_eq!(
            told(&at("129.90"), &at("130.25"), rule.clone()),
            vec!["usd/kes is 130.25 now, above 130"]
        );
        assert!(told(&at("130.25"), &at("131"), rule.clone()).is_empty());
        // The number gone from the page is no crossing.
        let gone = [section("Rates", "Rates are being updated")];
        assert!(told(&at("129.90"), &gone, rule).is_empty());

        let cheap = TellWhen::Below {
            phrase: String::new(),
            value: 4_000.0,
        };
        let kettle = |price: &str| [car("Kettle", price, "https://shop.example/kettle")];
        assert_eq!(
            told(&kettle("4,500"), &kettle("3,800"), cheap.clone()),
            vec!["the price is 3,800 now, below 4,000"]
        );
        assert_eq!(
            cheap.standing(&kettle("4,500")).as_deref(),
            Some("the price is 4,500 now")
        );
        assert_eq!(cheap.standing(&[section("About", "No prices here")]), None);
    }

    #[test]
    fn a_rule_reads_as_people_say_it_and_survives_storage() {
        let filters = WatchFilters {
            tell_when: TellWhen::Disappears {
                phrase: "sold out".into(),
            },
            ..Default::default()
        };
        assert_eq!(filters.words_for_people(), "when \"sold out\" is gone");
        let stored = serde_json::to_string(&filters).unwrap();
        assert_eq!(
            stored,
            r#"{"tell_when":{"when":"disappears","phrase":"sold out"}}"#
        );
        assert_eq!(
            serde_json::from_str::<WatchFilters>(&stored).unwrap(),
            filters
        );
        // A watch kept before rules existed reads as the default.
        let old: WatchFilters = serde_json::from_str(r#"{"words":["subaru"]}"#).unwrap();
        assert!(old.tell_when.is_default());
        assert_eq!(
            serde_json::to_string(&old).unwrap(),
            r#"{"words":["subaru"]}"#
        );
    }

    #[test]
    fn a_clock_or_a_counter_moving_is_not_an_update() {
        assert_eq!(
            steady("Updated 14:05, 3 hours ago. 1,204 views. Road closed"),
            steady("Updated 09:12, just now. 1.3k views. Road closed")
        );
        let before = [section(
            "Status",
            "Last checked 09:12. All lines running. 120 views",
        )];
        let later = [section(
            "Status",
            "Last checked 14:05. All lines running. 133 views",
        )];
        let changed = [section(
            "Status",
            "Last checked 14:05. Line 2 suspended. 140 views",
        )];
        let filters = WatchFilters::default();
        assert_eq!(
            compare(Some(&before), &later, &filters),
            Comparison::Changes(Vec::new())
        );
        let Comparison::Changes(changes) = compare(Some(&before), &changed, &filters) else {
            panic!("changes expected");
        };
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].kind, ChangeKind::Updated { .. }));
        // A version number is not a clock: a release still shows.
        assert_ne!(
            steady("Version 1.2.3 released"),
            steady("Version 1.2.4 released")
        );
    }
}
