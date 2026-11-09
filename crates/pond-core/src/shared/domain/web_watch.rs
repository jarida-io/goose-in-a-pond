//! A web watch: a page a member asked the pond to keep checking, the items it last listed, and
//! what counts as a match. A check compares items, never text, so ads, dates and layout don't
//! read as changes; and the comparison, the filters and the notice are all code, so a check never
//! needs the model.

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
}

impl WatchFilters {
    /// Whether `item` is what they asked for. A filter on a value the item doesn't state is not
    /// met: an alert has to be one they can trust.
    pub fn matches(&self, item: &WatchItem) -> bool {
        let title = item.title.to_lowercase();
        self.words
            .iter()
            .map(|w| w.trim().to_lowercase())
            .filter(|w| !w.is_empty())
            .all(|w| title.contains(&w))
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
    PriceChanged { from: String, to: String },
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
        let mut facts = vec![item.title.clone()];
        match &self.kind {
            ChangeKind::PriceChanged { from, to } => {
                facts.push(format!(
                    "{} to {}",
                    price_with(item, from),
                    price_with(item, to)
                ));
            }
            _ => facts.extend(item.price_words()),
        }
        if !matches!(self.kind, ChangeKind::Gone) {
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
        let wanted = matches!(change.kind, ChangeKind::New) || change.is_price_drop();
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
}
