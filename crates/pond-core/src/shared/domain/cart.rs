//! A watched link: one product page a member asked Goose to keep an eye on, its price over time,
//! and which changes they want to hear about. Comparing two readings is code; only reading a page
//! with no structured price may take the model.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub use crate::shared::domain::web_watch::number_in;

pub const DEFAULT_EVERY_HOURS: u32 = 6;
pub const MIN_EVERY_HOURS: u32 = 1;
pub const MAX_EVERY_HOURS: u32 = 7 * 24;
/// Links one member may watch.
pub const MAX_ITEMS_PER_MEMBER: usize = 50;
/// Failed checks in a row before the link reads as failing and its owner is told.
pub const FAILURES_BEFORE_FAILING: u32 = 3;
/// Price points an item shows.
pub const HISTORY_SHOWN: usize = 60;

/// Which changes are pushed to the member. Every change is still an event a to-do can hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Notify {
    /// The price fell.
    #[default]
    Drop,
    /// The price is at or below the member's target.
    Target,
    /// It came back in stock.
    Stock,
    Any,
}

impl Notify {
    pub fn as_str(self) -> &'static str {
        match self {
            Notify::Drop => "drop",
            Notify::Target => "target",
            Notify::Stock => "stock",
            Notify::Any => "any",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "drop" => Some(Notify::Drop),
            "target" => Some(Notify::Target),
            "stock" => Some(Notify::Stock),
            "any" => Some(Notify::Any),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CartStatus {
    Ok,
    /// Added, and not yet read once.
    #[default]
    Checking,
    Failing,
}

impl CartStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CartStatus::Ok => "ok",
            CartStatus::Checking => "checking",
            CartStatus::Failing => "failing",
        }
    }

    pub fn parse(text: &str) -> Self {
        match text {
            "ok" => CartStatus::Ok,
            "failing" => CartStatus::Failing,
            _ => CartStatus::Checking,
        }
    }
}

/// One watched link, as kept. Prices are in `currency`, the page's own.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CartItem {
    pub id: String,
    pub owner: String,
    pub url: String,
    pub title: String,
    pub site: String,
    /// A kept picture's key, served at `/api/v1/report-images/{key}`.
    pub picture_key: Option<String>,
    pub currency: Option<String>,
    pub price: Option<f64>,
    pub previous: Option<f64>,
    pub lowest: Option<f64>,
    pub highest: Option<f64>,
    pub target: Option<f64>,
    pub in_stock: Option<bool>,
    pub status: CartStatus,
    pub failures: u32,
    pub every_hours: u32,
    pub notify: Notify,
    pub collection: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
    pub next_check_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One price a check found.
#[derive(Debug, Clone, PartialEq)]
pub struct PricePoint {
    pub at: DateTime<Utc>,
    pub price: f64,
    pub in_stock: Option<bool>,
}

/// Which reader found the price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadBy {
    /// schema.org JSON-LD: a Product and its Offer.
    JsonLd,
    /// Price meta tags (`product:price:amount`, `og:price:*`) or `itemprop="price"` microdata.
    PageTags,
    /// The model, reading the page's text.
    Model,
}

impl ReadBy {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadBy::JsonLd => "json_ld",
            ReadBy::PageTags => "page_tags",
            ReadBy::Model => "model",
        }
    }
}

/// What one reading of a product page found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProductReading {
    pub title: Option<String>,
    pub site: Option<String>,
    pub price: Option<f64>,
    /// ISO 4217, upper case.
    pub currency: Option<String>,
    pub in_stock: Option<bool>,
    /// A kept picture's key.
    pub picture_key: Option<String>,
    pub read_by: Option<ReadBy>,
}

impl ProductReading {
    /// Whether it says anything a check can compare: a price, or whether it is in stock.
    pub fn is_useful(&self) -> bool {
        self.price.is_some() || self.in_stock.is_some()
    }
}

/// A change one check found. Each is also a `hook_events` row under the cart's vocabulary.
#[derive(Debug, Clone, PartialEq)]
pub enum CartEvent {
    PriceDrop {
        from: f64,
        to: f64,
    },
    TargetReached {
        price: f64,
        target: f64,
    },
    BackInStock,
    PriceChange {
        from: f64,
        to: f64,
    },
    /// Pushed under `any`, but not a hook event: the vocabulary has no word for it.
    OutOfStock,
}

impl CartEvent {
    /// The `hook_events.event` it is written as, if any.
    pub fn hook_event(&self) -> Option<&'static str> {
        match self {
            CartEvent::PriceDrop { .. } => Some("price_drop"),
            CartEvent::TargetReached { .. } => Some("target_reached"),
            CartEvent::BackInStock => Some("back_in_stock"),
            CartEvent::PriceChange { .. } => Some("price_change"),
            CartEvent::OutOfStock => None,
        }
    }
}

/// The changes from `item`'s last reading to `now`. A first reading, or one in another currency,
/// changes nothing: there is nothing to compare it with.
pub fn classify(item: &CartItem, now: &ProductReading) -> Vec<CartEvent> {
    let mut events = Vec::new();
    let comparable = item.checked_at.is_some()
        && match (&item.currency, &now.currency) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => true,
        };
    if !comparable {
        return events;
    }
    if let (Some(from), Some(to)) = (item.price, now.price) {
        if moved(from, to) {
            if to < from {
                events.push(CartEvent::PriceDrop { from, to });
            }
            if let Some(target) = item.target {
                if to <= target && from > target {
                    events.push(CartEvent::TargetReached { price: to, target });
                }
            }
            events.push(CartEvent::PriceChange { from, to });
        }
    }
    match (item.in_stock, now.in_stock) {
        (Some(false), Some(true)) => events.push(CartEvent::BackInStock),
        (Some(true), Some(false)) => events.push(CartEvent::OutOfStock),
        _ => {}
    }
    events
}

/// Whether `events` are what a member who asked for `notify` wants pushed. A target reached is
/// a drop too, so `drop` hears it.
pub fn wants_push(notify: Notify, events: &[CartEvent]) -> bool {
    events.iter().any(|e| match notify {
        Notify::Drop => matches!(
            e,
            CartEvent::PriceDrop { .. } | CartEvent::TargetReached { .. }
        ),
        Notify::Target => matches!(e, CartEvent::TargetReached { .. }),
        Notify::Stock => matches!(e, CartEvent::BackInStock),
        Notify::Any => true,
    })
}

/// The push for `events`: a title and a one-line body, in plain words.
pub fn notice_words(item: &CartItem, events: &[CartEvent]) -> (String, String) {
    let name = short_title(&item.title, &item.site);
    let money = |amount: f64| money_words(amount, item.currency.as_deref());
    let target = events.iter().find_map(|e| match e {
        CartEvent::TargetReached { price, target } => Some((*price, *target)),
        _ => None,
    });
    let change = events.iter().find_map(|e| match e {
        CartEvent::PriceChange { from, to } => Some((*from, *to)),
        _ => None,
    });
    let back = events.iter().any(|e| matches!(e, CartEvent::BackInStock));
    let gone = events.iter().any(|e| matches!(e, CartEvent::OutOfStock));
    let lowest = item
        .lowest
        .filter(|l| change.is_some_and(|(_, to)| *l < to))
        .map(|l| format!(" The lowest it has been is {}.", money(l)))
        .unwrap_or_default();
    if let Some((price, target)) = target {
        return (
            format!("At your price: {name}"),
            format!(
                "{} now, at or under the {} you wanted.",
                money(price),
                money(target)
            ),
        );
    }
    match change {
        Some((from, to)) if to < from => (
            format!("Cheaper now: {name}"),
            format!(
                "{}, down from {}{}.{lowest}",
                money(to),
                money(from),
                if back { ", and back in stock" } else { "" }
            ),
        ),
        Some((from, to)) => (
            format!("Price up: {name}"),
            format!("{}, up from {}.", money(to), money(from)),
        ),
        None if back => (
            format!("Back in stock: {name}"),
            match item.price {
                Some(p) => format!("It is for sale again, at {}.", money(p)),
                None => "It is for sale again.".to_string(),
            },
        ),
        None if gone => (
            format!("Sold out: {name}"),
            "The shop says it is out of stock.".to_string(),
        ),
        None => (format!("A change on {name}"), String::new()),
    }
}

/// What the owner is told when a link has failed [`FAILURES_BEFORE_FAILING`] times.
pub fn failing_words(item: &CartItem, why: &str, wait_hours: u32) -> (String, String) {
    (
        format!(
            "Can't check the price of {}",
            short_title(&item.title, &item.site)
        ),
        format!("{why} Goose will keep trying every {wait_hours} hours."),
    )
}

/// The title as a notice shows it: cut at a word near 60 characters, else the site.
pub fn short_title(title: &str, site: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        return if site.is_empty() {
            "your link".to_string()
        } else {
            site.to_string()
        };
    }
    if title.chars().count() <= 60 {
        return title.to_string();
    }
    let cut: String = title.chars().take(60).collect();
    let cut = cut.rsplit_once(' ').map(|(head, _)| head).unwrap_or(&cut);
    format!("{}...", cut.trim_end_matches([',', ';', ':', '-', ' ']))
}

/// A price as people read it: "KES 1,450,000", "USD 19.99".
pub fn money_words(amount: f64, currency: Option<&str>) -> String {
    let whole = (amount - amount.trunc()).abs() < 0.005;
    let digits = if whole {
        grouped(amount.round() as i64)
    } else {
        let cents = (amount * 100.0).round() as i64;
        format!("{}.{:02}", grouped(cents / 100), (cents % 100).abs())
    };
    match currency.filter(|c| !c.is_empty()) {
        Some(c) => format!("{c} {digits}"),
        None => digits,
    }
}

fn grouped(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

fn moved(from: f64, to: f64) -> bool {
    (from - to).abs() >= 0.005
}

/// The ISO 4217 code a page's currency text means: "KSh" and "Kshs" are KES, "$" is USD.
pub fn currency_code(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    let code = match lower.trim_end_matches('.') {
        "ksh" | "kshs" | "ks" | "kes" | "sh" | "shs" => "KES",
        "ush" | "ugx" => "UGX",
        "tsh" | "tzs" => "TZS",
        "$" | "us$" | "usd" => "USD",
        "€" | "eur" => "EUR",
        "£" | "gbp" => "GBP",
        "₦" | "ngn" => "NGN",
        "r" | "zar" => "ZAR",
        _ if t.len() == 3 && t.chars().all(|c| c.is_ascii_alphabetic()) => {
            return Some(t.to_ascii_uppercase())
        }
        _ => return None,
    };
    Some(code.to_string())
}

/// The currency a price text names before or after its number: "KSh 1,799" is KES.
pub fn currency_in(price_text: &str) -> Option<String> {
    let words: Vec<&str> = price_text
        .split(|c: char| c.is_ascii_digit() || c.is_whitespace() || c == ',' || c == '.')
        .filter(|w| !w.is_empty())
        .collect();
    words.iter().find_map(|w| currency_code(w)).or_else(|| {
        ["$", "€", "£", "₦"]
            .iter()
            .find(|s| price_text.contains(*s))
            .and_then(|s| currency_code(s))
    })
}

/// The price in `text`, when it is one a shopper pays: positive and finite.
pub fn price_in(text: &str) -> Option<f64> {
    number_in(text).filter(|p| p.is_finite() && *p > 0.0)
}

/// Whether schema.org or a shop's own words say it can be bought now. `None` when they say
/// neither.
pub fn availability_of(text: &str) -> Option<bool> {
    let word = text
        .trim()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace([' ', '_', '-'], "");
    match word.as_str() {
        "instock"
        | "limitedavailability"
        | "onlineonly"
        | "instoreonly"
        | "presale"
        | "preorder"
        | "available" => Some(true),
        "outofstock" | "soldout" | "discontinued" | "backorder" | "oos" | "unavailable" => {
            Some(false)
        }
        _ => None,
    }
}

/// A member's interval, kept to what a site can bear.
pub fn clamp_every_hours(hours: f64) -> u32 {
    if !hours.is_finite() {
        return DEFAULT_EVERY_HOURS;
    }
    (hours.round().max(0.0) as u32).clamp(MIN_EVERY_HOURS, MAX_EVERY_HOURS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kettle(price: Option<f64>, in_stock: Option<bool>) -> CartItem {
        CartItem {
            id: "c1".into(),
            title: "Ramtons Cordless Electric Kettle".into(),
            site: "Jumia Kenya".into(),
            currency: Some("KES".into()),
            price,
            lowest: price,
            highest: price,
            in_stock,
            checked_at: Some(Utc::now()),
            ..Default::default()
        }
    }

    fn reading(price: Option<f64>, in_stock: Option<bool>) -> ProductReading {
        ProductReading {
            price,
            currency: Some("KES".into()),
            in_stock,
            ..Default::default()
        }
    }

    #[test]
    fn a_drop_is_a_drop_and_a_change() {
        let events = classify(
            &kettle(Some(1799.0), Some(true)),
            &reading(Some(1599.0), Some(true)),
        );
        assert_eq!(
            events,
            vec![
                CartEvent::PriceDrop {
                    from: 1799.0,
                    to: 1599.0
                },
                CartEvent::PriceChange {
                    from: 1799.0,
                    to: 1599.0
                },
            ]
        );
        assert!(wants_push(Notify::Drop, &events));
        assert!(!wants_push(Notify::Stock, &events));
        assert!(!wants_push(Notify::Target, &events));
        assert!(wants_push(Notify::Any, &events));
    }

    #[test]
    fn a_rise_is_only_a_change() {
        let events = classify(&kettle(Some(1599.0), None), &reading(Some(1799.0), None));
        assert_eq!(
            events,
            vec![CartEvent::PriceChange {
                from: 1599.0,
                to: 1799.0
            }]
        );
        assert!(!wants_push(Notify::Drop, &events));
        assert!(wants_push(Notify::Any, &events));
    }

    #[test]
    fn a_target_is_reached_once_when_the_price_crosses_it() {
        let mut item = kettle(Some(1_250_000.0), None);
        item.target = Some(1_200_000.0);
        let events = classify(&item, &reading(Some(1_190_000.0), None));
        assert!(events.contains(&CartEvent::TargetReached {
            price: 1_190_000.0,
            target: 1_200_000.0
        }));
        assert!(wants_push(Notify::Target, &events));
        assert!(
            wants_push(Notify::Drop, &events),
            "a target reached is a drop"
        );
        item.price = Some(1_190_000.0);
        let lower = classify(&item, &reading(Some(1_180_000.0), None));
        assert!(
            !lower
                .iter()
                .any(|e| matches!(e, CartEvent::TargetReached { .. })),
            "already under the target"
        );
        assert!(!wants_push(Notify::Target, &lower));
    }

    #[test]
    fn back_in_stock_and_sold_out() {
        let back = classify(
            &kettle(Some(1799.0), Some(false)),
            &reading(None, Some(true)),
        );
        assert_eq!(back, vec![CartEvent::BackInStock]);
        assert!(wants_push(Notify::Stock, &back));
        assert!(!wants_push(Notify::Drop, &back));
        let gone = classify(
            &kettle(Some(1799.0), Some(true)),
            &reading(Some(1799.0), Some(false)),
        );
        assert_eq!(gone, vec![CartEvent::OutOfStock]);
        assert_eq!(gone[0].hook_event(), None);
        assert!(!wants_push(Notify::Stock, &gone));
        assert!(wants_push(Notify::Any, &gone));
    }

    #[test]
    fn a_first_reading_or_another_currency_changes_nothing() {
        let mut first = kettle(None, None);
        first.checked_at = None;
        assert!(classify(&first, &reading(Some(1799.0), Some(true))).is_empty());
        let mut dollars = reading(Some(14.0), None);
        dollars.currency = Some("USD".into());
        assert!(classify(&kettle(Some(1799.0), None), &dollars).is_empty());
        assert!(classify(&kettle(Some(1799.0), None), &reading(Some(1799.004), None)).is_empty());
    }

    #[test]
    fn notices_read_as_people_say_them() {
        let mut item = kettle(Some(1599.0), Some(true));
        item.lowest = Some(1499.0);
        let events = vec![
            CartEvent::PriceDrop {
                from: 1799.0,
                to: 1599.0,
            },
            CartEvent::PriceChange {
                from: 1799.0,
                to: 1599.0,
            },
        ];
        assert_eq!(
            notice_words(&item, &events),
            (
                "Cheaper now: Ramtons Cordless Electric Kettle".to_string(),
                "KES 1,599, down from KES 1,799. The lowest it has been is KES 1,499.".to_string()
            )
        );
        let target = vec![CartEvent::TargetReached {
            price: 1_190_000.0,
            target: 1_200_000.0,
        }];
        assert_eq!(
            notice_words(&item, &target).1,
            "KES 1,190,000 now, at or under the KES 1,200,000 you wanted."
        );
    }

    #[test]
    fn currencies_and_prices_are_read_as_shops_write_them() {
        assert_eq!(currency_code("KSh").as_deref(), Some("KES"));
        assert_eq!(currency_code("kes").as_deref(), Some("KES"));
        assert_eq!(currency_in("KSh 1,799").as_deref(), Some("KES"));
        assert_eq!(currency_in("$19.99").as_deref(), Some("USD"));
        assert_eq!(currency_in("1,799"), None);
        assert_eq!(price_in("1799.00"), Some(1799.0));
        assert_eq!(price_in("KSh 1,199,000"), Some(1_199_000.0));
        assert_eq!(price_in("0"), None);
        assert_eq!(money_words(19.5, Some("USD")), "USD 19.50");
        assert_eq!(money_words(1_450_000.0, Some("KES")), "KES 1,450,000");
        assert_eq!(availability_of("http://schema.org/InStock"), Some(true));
        assert_eq!(
            availability_of("https://schema.org/OutOfStock"),
            Some(false)
        );
        assert_eq!(availability_of("out of stock"), Some(false));
        assert_eq!(availability_of("NewCondition"), None);
    }

    #[test]
    fn long_titles_are_cut_at_a_word() {
        let long = "LYONS 1.8L/2.2L Cordless Stainless Steel Electric Kettle Fast Boil Electric \
                    Kettle 1500W Large Capacity";
        let short = short_title(long, "Kilimall");
        assert!(short.ends_with("..."), "{short}");
        assert!(short.chars().count() <= 63, "{short}");
        assert_eq!(short_title("", "Kilimall"), "Kilimall");
    }
}
