//! Watched links: adds one, checks the ones that are due, and tells the owner what changed. A
//! check reads the page (structured data first; the model only when that finds nothing), keeps
//! the price in the history, writes every change to `hook_events`, and pushes the changes the
//! owner asked for. A refusing shop is checked less often, doubling, and the owner is told once.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

use crate::shared::domain::cart::{
    clamp_every_hours, classify, currency_code, failing_words, money_words, notice_words, price_in,
    wants_push, CartEvent, CartItem, CartStatus, Notify, PricePoint, ProductReading,
    DEFAULT_EVERY_HOURS, FAILURES_BEFORE_FAILING, MAX_ITEMS_PER_MEMBER,
};
use crate::shared::domain::web_watch::backoff_hours;
use crate::shared::ports::cart::{
    CartNotice, CartNotifier, CartRepository, ModelUse, ProductReadError, ProductReader,
};
use crate::shared::ports::hook_event_sink::{HookEventSink, NewHookEvent};

/// Links checked in one pass at most, so a pass stays short.
pub const MAX_DUE_PER_PASS: usize = 8;

/// What a member asks to watch.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewCartItem {
    pub url: String,
    pub target: Option<f64>,
    pub collection: Option<String>,
    pub every_hours: Option<f64>,
    pub notify: Option<Notify>,
}

/// Why a link was not added.
#[derive(Debug, Clone, PartialEq)]
pub enum AddRefusal {
    NotAnAddress,
    TooMany,
    AlreadyWatched(Box<CartItem>),
}

pub struct CartWatch {
    repo: Arc<dyn CartRepository>,
    reader: Arc<dyn ProductReader>,
    notifier: Arc<dyn CartNotifier>,
    events: Arc<dyn HookEventSink>,
    checking: Arc<Mutex<HashSet<String>>>,
    turn_in_flight: fn() -> bool,
}

static INSTALLED: OnceLock<Arc<CartWatch>> = OnceLock::new();

/// Installed once at startup by the binary that can read pages; a Pond without it watches no
/// links.
pub fn install(watch: Arc<CartWatch>) {
    let _ = INSTALLED.set(watch);
}

pub fn installed() -> Option<Arc<CartWatch>> {
    INSTALLED.get().cloned()
}

impl CartWatch {
    pub fn new(
        repo: Arc<dyn CartRepository>,
        reader: Arc<dyn ProductReader>,
        notifier: Arc<dyn CartNotifier>,
        events: Arc<dyn HookEventSink>,
    ) -> Self {
        Self {
            repo,
            reader,
            notifier,
            events,
            checking: Arc::default(),
            turn_in_flight: crate::models::domain::inference_bus::turn_has_the_model,
        }
    }

    /// The same reader and notifier over another store, as a route's own database is.
    pub fn over(&self, repo: Arc<dyn CartRepository>, events: Arc<dyn HookEventSink>) -> Self {
        Self {
            repo,
            reader: self.reader.clone(),
            notifier: self.notifier.clone(),
            events,
            checking: self.checking.clone(),
            turn_in_flight: self.turn_in_flight,
        }
    }

    #[cfg(test)]
    fn with_turn_check(mut self, turn_in_flight: fn() -> bool) -> Self {
        self.turn_in_flight = turn_in_flight;
        self
    }

    pub fn repo(&self) -> &Arc<dyn CartRepository> {
        &self.repo
    }

    /// Keeps the link, unread: its first check reads it.
    pub async fn add(
        &self,
        owner: &str,
        new: NewCartItem,
        now: DateTime<Utc>,
    ) -> Result<std::result::Result<CartItem, AddRefusal>> {
        let Some(url) = web_address(&new.url) else {
            return Ok(Err(AddRefusal::NotAnAddress));
        };
        if let Some(existing) = self.repo.find_by_url(owner, &url).await? {
            return Ok(Err(AddRefusal::AlreadyWatched(Box::new(existing))));
        }
        if self.repo.list_for(owner).await?.len() >= MAX_ITEMS_PER_MEMBER {
            return Ok(Err(AddRefusal::TooMany));
        }
        let item = CartItem {
            id: uuid::Uuid::new_v4().to_string(),
            owner: owner.to_string(),
            site: site_of(&url),
            url,
            target: new.target.filter(|t| t.is_finite() && *t > 0.0),
            every_hours: new
                .every_hours
                .map(clamp_every_hours)
                .unwrap_or(DEFAULT_EVERY_HOURS),
            notify: new.notify.unwrap_or_default(),
            collection: new
                .collection
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty()),
            status: CartStatus::Checking,
            next_check_at: Some(now),
            created_at: now,
            updated_at: now,
            ..Default::default()
        };
        if !self.repo.create(&item).await? {
            return match self.repo.find_by_url(owner, &item.url).await? {
                Some(existing) => Ok(Err(AddRefusal::AlreadyWatched(Box::new(existing)))),
                None => Err(anyhow::anyhow!("the link was neither kept nor found")),
            };
        }
        tracing::info!(target: "giap::trace", kind = "cart_added", cart = %item.id);
        Ok(Ok(item))
    }

    /// Checks every link due at `now`; returns how many were checked.
    pub async fn check_due(&self, now: DateTime<Utc>) -> Result<usize> {
        let due = self.repo.due(now, MAX_DUE_PER_PASS).await?;
        for item in &due {
            if let Err(e) = self.check(item.clone(), ModelUse::Allowed, now).await {
                tracing::warn!(target: "giap::trace", kind = "cart_check_failed", cart = %item.id, error = %e);
            }
        }
        Ok(due.len())
    }

    /// Reads the link now and records what it found; returns the item as saved. A check of a
    /// link already being checked answers the item as it stands.
    pub async fn check(
        &self,
        item: CartItem,
        model: ModelUse,
        now: DateTime<Utc>,
    ) -> Result<CartItem> {
        if !self
            .checking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(item.id.clone())
        {
            return Ok(item);
        }
        let result = self.check_alone(item.clone(), model, now).await;
        self.checking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&item.id);
        result
    }

    async fn check_alone(
        &self,
        item: CartItem,
        model: ModelUse,
        now: DateTime<Utc>,
    ) -> Result<CartItem> {
        // A turn holds the model: reading a page with it would wait behind the person.
        let model = match model {
            ModelUse::Allowed if !(self.turn_in_flight)() => ModelUse::Allowed,
            _ => ModelUse::Never,
        };
        let read = match self.reader.read(&item.url, model).await {
            Ok(reading) if reading.is_useful() => Ok(reading),
            Ok(_) => Err(ProductReadError::NoPrice),
            Err(e) => Err(e),
        };
        let saved = match read {
            Ok(reading) => self.succeeded(item, reading, now).await?,
            Err(why) => self.failed(item, why, model, now).await?,
        };
        Ok(saved)
    }

    async fn succeeded(
        &self,
        mut item: CartItem,
        reading: ProductReading,
        now: DateTime<Utc>,
    ) -> Result<CartItem> {
        let events = classify(&item, &reading);
        let new_currency = reading.currency.as_deref().and_then(currency_code);
        if let (Some(old), Some(new)) = (&item.currency, &new_currency) {
            if !old.eq_ignore_ascii_case(new) {
                // Prices in two currencies are not one history.
                self.repo.clear_history(&item.id).await?;
                item.price = None;
                item.previous = None;
                item.lowest = None;
                item.highest = None;
            }
        }
        if let Some(currency) = new_currency {
            item.currency = Some(currency);
        }
        if let Some(title) = reading.title.filter(|t| !t.trim().is_empty()) {
            item.title = title;
        }
        if let Some(site) = reading.site.filter(|s| !s.trim().is_empty()) {
            item.site = site;
        }
        if reading.picture_key.is_some() {
            item.picture_key = reading.picture_key;
        }
        let stock_moved = reading.in_stock.is_some() && reading.in_stock != item.in_stock;
        if reading.in_stock.is_some() {
            item.in_stock = reading.in_stock;
        }
        let mut price_moved = false;
        if let Some(price) = reading.price {
            price_moved = item.price.is_none_or(|old| (old - price).abs() >= 0.005);
            if price_moved {
                item.previous = item.price;
                item.price = Some(price);
            }
            item.lowest = Some(item.lowest.map_or(price, |l| l.min(price)));
            item.highest = Some(item.highest.map_or(price, |h| h.max(price)));
        }
        if let (true, Some(price)) = (price_moved || stock_moved, item.price) {
            self.repo
                .add_price(
                    &item.id,
                    &PricePoint {
                        at: now,
                        price,
                        in_stock: item.in_stock,
                    },
                )
                .await?;
        }
        item.status = CartStatus::Ok;
        item.failures = 0;
        item.checked_at = Some(now);
        item.next_check_at = Some(now + Duration::hours(item.every_hours.into()));
        item.updated_at = now;
        self.repo.save(&item).await?;
        tracing::info!(
            target: "giap::trace",
            kind = "cart_check",
            cart = %item.id,
            read_by = reading.read_by.map_or("none", |r| r.as_str()),
            events = events.len(),
        );
        self.raise(&item, &events, now).await;
        if wants_push(item.notify, &events) {
            let (title, body) = notice_words(&item, &events);
            let spoken = events.iter().any(|e| {
                matches!(
                    e,
                    CartEvent::PriceDrop { .. } | CartEvent::TargetReached { .. }
                )
            });
            self.tell(&item, title, body, spoken).await;
        }
        Ok(item)
    }

    async fn failed(
        &self,
        mut item: CartItem,
        why: ProductReadError,
        model: ModelUse,
        now: DateTime<Utc>,
    ) -> Result<CartItem> {
        item.failures = item.failures.saturating_add(1);
        let wait = backoff_hours(item.every_hours, item.failures);
        // A page the model read too and found no price on, never priced, is not a passing fault.
        let never_priced =
            why == ProductReadError::NoPrice && model == ModelUse::Allowed && item.price.is_none();
        if item.failures >= FAILURES_BEFORE_FAILING || never_priced {
            item.status = CartStatus::Failing;
        }
        item.next_check_at = Some(now + Duration::hours(wait.into()));
        item.updated_at = now;
        self.repo.save(&item).await?;
        tracing::info!(
            target: "giap::trace",
            kind = "cart_check",
            cart = %item.id,
            failed = ?why,
            failures = item.failures,
        );
        if item.failures == FAILURES_BEFORE_FAILING {
            let (title, body) = failing_words(&item, why.words(), wait);
            self.tell(&item, title, body, false).await;
        }
        Ok(item)
    }

    async fn raise(&self, item: &CartItem, events: &[CartEvent], now: DateTime<Utc>) {
        let money = |amount: Option<f64>| {
            amount.map(
                |a| json!({"amount": a, "currency": item.currency.clone().unwrap_or_default()}),
            )
        };
        for event in events {
            let Some(name) = event.hook_event() else {
                continue;
            };
            let payload = json!({
                "title": item.title,
                "url": item.url,
                "price": money(item.price),
                "previous": money(item.previous),
                "target": money(item.target),
                "in_stock": item.in_stock,
            });
            let row = NewHookEvent::new(Some(&item.owner), "cart", &item.id, name, payload, now);
            if let Err(e) = self.events.record(row).await {
                tracing::warn!(target: "giap::trace", kind = "cart_hook_event_failed", error = %e);
            }
        }
    }

    async fn tell(&self, item: &CartItem, title: String, body: String, spoken: bool) {
        let notice = CartNotice {
            owner: item.owner.clone(),
            cart_id: item.id.clone(),
            title,
            body,
            spoken,
        };
        if let Err(e) = self.notifier.notify(notice).await {
            tracing::warn!(target: "giap::trace", kind = "cart_notice_failed", error = %e);
        }
    }
}

/// `text` as a web address the Pond may read, scheme added when it was left off.
pub fn web_address(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    let with_scheme = if text.contains("://") {
        text.to_string()
    } else {
        format!("https://{text}")
    };
    let url = url::Url::parse(&with_scheme).ok()?;
    let host = url.host_str()?;
    (matches!(url.scheme(), "http" | "https") && host.contains('.')).then(|| url.to_string())
}

/// The site a link is on, as people name it: "jumia.co.ke".
pub fn site_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h.trim_start_matches("www.").to_string())
        })
        .unwrap_or_default()
}

// The model's part: reading a page that names no price in its structured data.

/// Characters of page text the model is shown: a product's title and price sit near the top.
pub const MODEL_PAGE_CHARS: usize = 6_000;

const MODEL_SYSTEM: &str = "\
Using only the page text you are given, report the one product this shop page sells, as one \
JSON object with exactly these properties:
- \"title\": string or null. The product's name as the page states it.
- \"amount\": number or null. What a buyer pays now for one of this product, as a plain number: \
no currency sign, no thousands separators, no words. Only a number the page shows as this \
product's price, beside a currency, is an amount: a model number, a code, a count of ratings or \
a size never is. A sale price beats a crossed-out price. Never a delivery fee, an instalment, a \
deposit or another product's price.
- \"currency\": string or null. The ISO 4217 code of that amount, such as KES or USD; when \
the page shows only a symbol such as KSh or $, give that symbol.
- \"in_stock\": true, false or null. false only when the page says the product is sold out \
or out of stock; true only when it says it can be bought now.
When the page does not state a value, that property is null: never a guess. When the page sells \
no single product, or shows no price for it, amount is null.
Answer with the JSON object only, with no other words and no code fence.";

/// The system prompt and the user message for reading `text`, the page at `url`.
pub fn model_prompt(url: &str, page_title: &str, text: &str) -> (&'static str, String) {
    let shown: String = text.chars().take(MODEL_PAGE_CHARS).collect();
    (
        MODEL_SYSTEM,
        format!("Page address: {url}\nPage title: {page_title}\n\nPage text:\n{shown}"),
    )
}

/// What the model said, read strictly: a price only when it is a positive number.
pub fn parse_model_answer(answer: &str) -> Option<ProductReading> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    let value: serde_json::Value = serde_json::from_str(answer.get(start..=end)?).ok()?;
    let object = value.as_object()?;
    let text = |key: &str| {
        object
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("null"))
            .map(str::to_string)
    };
    let price = match object.get("amount").or_else(|| object.get("price")) {
        Some(serde_json::Value::Number(n)) => n.as_f64().filter(|p| p.is_finite() && *p > 0.0),
        Some(serde_json::Value::String(s)) => price_in(s),
        _ => None,
    };
    let in_stock = object.get("in_stock").and_then(|v| v.as_bool());
    Some(ProductReading {
        title: text("title"),
        price,
        currency: text("currency").and_then(|c| currency_code(&c)),
        in_stock,
        read_by: Some(crate::shared::domain::cart::ReadBy::Model),
        ..Default::default()
    })
}

/// A short line about an item, for the chat model's reply.
pub fn item_words(item: &CartItem) -> String {
    let price = item
        .price
        .map(|p| money_words(p, item.currency.as_deref()))
        .unwrap_or_else(|| "no price read yet".to_string());
    let stock = match item.in_stock {
        Some(false) => ", out of stock",
        _ => "",
    };
    format!("{} on {}: {price}{stock}", item.title, item.site)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::cart::ReadBy;
    use async_trait::async_trait;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Store {
        items: Mutex<Vec<CartItem>>,
        prices: Mutex<HashMap<String, Vec<PricePoint>>>,
    }

    #[async_trait]
    impl CartRepository for Store {
        async fn create(&self, item: &CartItem) -> Result<bool> {
            let mut items = self.items.lock().unwrap();
            if items
                .iter()
                .any(|i| i.owner == item.owner && i.url == item.url)
            {
                return Ok(false);
            }
            items.push(item.clone());
            Ok(true)
        }
        async fn get(&self, owner: &str, id: &str) -> Result<Option<CartItem>> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|i| i.owner == owner && i.id == id)
                .cloned())
        }
        async fn find_by_url(&self, owner: &str, url: &str) -> Result<Option<CartItem>> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .find(|i| i.owner == owner && i.url == url)
                .cloned())
        }
        async fn list_for(&self, owner: &str) -> Result<Vec<CartItem>> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .filter(|i| i.owner == owner)
                .cloned()
                .collect())
        }
        async fn due(&self, now: DateTime<Utc>, limit: usize) -> Result<Vec<CartItem>> {
            Ok(self
                .items
                .lock()
                .unwrap()
                .iter()
                .filter(|i| i.next_check_at.is_some_and(|n| n <= now))
                .take(limit)
                .cloned()
                .collect())
        }
        async fn save(&self, item: &CartItem) -> Result<()> {
            let mut items = self.items.lock().unwrap();
            if let Some(i) = items.iter_mut().find(|i| i.id == item.id) {
                *i = item.clone();
            }
            Ok(())
        }
        async fn add_price(&self, id: &str, point: &PricePoint) -> Result<()> {
            self.prices
                .lock()
                .unwrap()
                .entry(id.to_string())
                .or_default()
                .push(point.clone());
            Ok(())
        }
        async fn history(&self, id: &str, _limit: usize) -> Result<Vec<PricePoint>> {
            Ok(self
                .prices
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_default())
        }
        async fn clear_history(&self, id: &str) -> Result<()> {
            self.prices.lock().unwrap().remove(id);
            Ok(())
        }
        async fn delete(&self, _owner: &str, _id: &str) -> Result<bool> {
            Ok(false)
        }
    }

    /// Answers each read from a queue; an empty queue is a page with nothing on it.
    #[derive(Default)]
    struct Pages {
        answers: Mutex<Vec<std::result::Result<ProductReading, ProductReadError>>>,
        asked: Mutex<Vec<ModelUse>>,
    }

    #[async_trait]
    impl ProductReader for Pages {
        async fn read(
            &self,
            _url: &str,
            model: ModelUse,
        ) -> std::result::Result<ProductReading, ProductReadError> {
            self.asked.lock().unwrap().push(model);
            let mut answers = self.answers.lock().unwrap();
            if answers.is_empty() {
                Ok(ProductReading::default())
            } else {
                answers.remove(0)
            }
        }
    }

    #[derive(Default)]
    struct Told(Mutex<Vec<CartNotice>>);

    #[async_trait]
    impl CartNotifier for Told {
        async fn notify(&self, notice: CartNotice) -> Result<()> {
            self.0.lock().unwrap().push(notice);
            Ok(())
        }
    }

    #[derive(Default)]
    struct Events(Mutex<Vec<NewHookEvent>>);

    #[async_trait]
    impl HookEventSink for Events {
        async fn record(&self, event: NewHookEvent) -> Result<bool> {
            self.0.lock().unwrap().push(event);
            Ok(true)
        }
    }

    struct Rig {
        watch: CartWatch,
        store: Arc<Store>,
        pages: Arc<Pages>,
        told: Arc<Told>,
        events: Arc<Events>,
    }

    fn rig() -> Rig {
        let (store, pages, told, events) = (
            Arc::new(Store::default()),
            Arc::new(Pages::default()),
            Arc::new(Told::default()),
            Arc::new(Events::default()),
        );
        let watch = CartWatch::new(store.clone(), pages.clone(), told.clone(), events.clone())
            .with_turn_check(|| false);
        Rig {
            watch,
            store,
            pages,
            told,
            events,
        }
    }

    fn priced(price: f64, in_stock: bool) -> std::result::Result<ProductReading, ProductReadError> {
        Ok(ProductReading {
            title: Some("Ramtons Cordless Electric Kettle".into()),
            site: Some("Jumia Kenya".into()),
            price: Some(price),
            currency: Some("KES".into()),
            in_stock: Some(in_stock),
            read_by: Some(ReadBy::JsonLd),
            ..Default::default()
        })
    }

    async fn added(rig: &Rig, notify: Notify) -> CartItem {
        rig.watch
            .add(
                "jerry",
                NewCartItem {
                    url: "jumia.co.ke/kettle-177871891.html".into(),
                    notify: Some(notify),
                    ..Default::default()
                },
                Utc::now(),
            )
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn a_drop_is_kept_raised_and_pushed() {
        let rig = rig();
        let item = added(&rig, Notify::Drop).await;
        assert_eq!(item.url, "https://jumia.co.ke/kettle-177871891.html");
        assert_eq!(item.status, CartStatus::Checking);
        rig.pages
            .answers
            .lock()
            .unwrap()
            .extend([priced(1799.0, true), priced(1599.0, true)]);
        let now = Utc::now();
        let first = rig.watch.check(item, ModelUse::Allowed, now).await.unwrap();
        assert_eq!(first.status, CartStatus::Ok);
        assert_eq!(first.price, Some(1799.0));
        assert!(
            rig.told.0.lock().unwrap().is_empty(),
            "a first reading tells nobody"
        );
        let second = rig
            .watch
            .check(first, ModelUse::Allowed, now + Duration::hours(6))
            .await
            .unwrap();
        assert_eq!(second.price, Some(1599.0));
        assert_eq!(second.previous, Some(1799.0));
        assert_eq!(second.lowest, Some(1599.0));
        assert_eq!(second.highest, Some(1799.0));
        assert_eq!(rig.store.history(&second.id, 60).await.unwrap().len(), 2);
        let events: Vec<String> = rig
            .events
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|e| format!("{}/{}", e.source_kind, e.event))
            .collect();
        assert_eq!(events, vec!["cart/price_drop", "cart/price_change"]);
        let told = rig.told.0.lock().unwrap();
        assert_eq!(told.len(), 1);
        assert!(told[0].spoken);
        assert_eq!(
            told[0].title,
            "Cheaper now: Ramtons Cordless Electric Kettle"
        );
    }

    #[tokio::test]
    async fn every_change_is_an_event_whatever_is_pushed() {
        let rig = rig();
        let item = added(&rig, Notify::Stock).await;
        rig.pages
            .answers
            .lock()
            .unwrap()
            .extend([priced(1799.0, true), priced(1599.0, true)]);
        let now = Utc::now();
        let first = rig.watch.check(item, ModelUse::Allowed, now).await.unwrap();
        rig.watch
            .check(first, ModelUse::Allowed, now)
            .await
            .unwrap();
        assert_eq!(rig.events.0.lock().unwrap().len(), 2);
        assert!(rig.told.0.lock().unwrap().is_empty(), "stock only");
    }

    #[tokio::test]
    async fn three_failures_make_it_failing_and_tell_the_owner_once() {
        let rig = rig();
        let mut item = added(&rig, Notify::Drop).await;
        rig.pages
            .answers
            .lock()
            .unwrap()
            .extend((0..4).map(|_| Err::<ProductReading, _>(ProductReadError::Refused)));
        let now = Utc::now();
        for _ in 0..4 {
            item = rig.watch.check(item, ModelUse::Allowed, now).await.unwrap();
        }
        assert_eq!(item.failures, 4);
        assert_eq!(item.status, CartStatus::Failing);
        assert_eq!(item.next_check_at, Some(now + Duration::hours(24)));
        let told = rig.told.0.lock().unwrap();
        assert_eq!(told.len(), 1);
        assert!(told[0].title.starts_with("Can't check the price"));
    }

    #[tokio::test]
    async fn a_page_with_no_price_fails_at_once_only_when_the_model_read_it() {
        let rig = rig();
        let item = added(&rig, Notify::Drop).await;
        let during_a_turn = rig
            .watch
            .check(item, ModelUse::Never, Utc::now())
            .await
            .unwrap();
        assert_eq!(during_a_turn.status, CartStatus::Checking);
        let read = rig
            .watch
            .check(during_a_turn, ModelUse::Allowed, Utc::now())
            .await
            .unwrap();
        assert_eq!(read.status, CartStatus::Failing);
        assert_eq!(
            *rig.pages.asked.lock().unwrap(),
            vec![ModelUse::Never, ModelUse::Allowed]
        );
    }

    #[tokio::test]
    async fn the_model_is_not_asked_while_a_turn_holds_it() {
        let rig = rig();
        let watch = rig.watch.over(rig.store.clone(), rig.events.clone());
        let watch = watch.with_turn_check(|| true);
        let item = added(&rig, Notify::Drop).await;
        watch
            .check(item, ModelUse::Allowed, Utc::now())
            .await
            .unwrap();
        assert_eq!(*rig.pages.asked.lock().unwrap(), vec![ModelUse::Never]);
    }

    #[tokio::test]
    async fn a_link_is_kept_once_and_only_a_web_address() {
        let rig = rig();
        added(&rig, Notify::Drop).await;
        let again = rig
            .watch
            .add(
                "jerry",
                NewCartItem {
                    url: "https://jumia.co.ke/kettle-177871891.html".into(),
                    ..Default::default()
                },
                Utc::now(),
            )
            .await
            .unwrap();
        assert!(matches!(again, Err(AddRefusal::AlreadyWatched(_))));
        let junk = rig
            .watch
            .add(
                "jerry",
                NewCartItem {
                    url: "not a link".into(),
                    ..Default::default()
                },
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(junk, Err(AddRefusal::NotAnAddress));
        assert_eq!(web_address("ftp://x.example/a"), None);
        assert_eq!(
            site_of("https://www.kilimall.co.ke/listing/1"),
            "kilimall.co.ke"
        );
    }

    #[test]
    fn the_models_answer_is_read_strictly() {
        let reading = parse_model_answer(
            "```json\n{\"title\": \"Sonar 2L Kettle\", \"amount\": 449, \"currency\": \"KSh\", \
             \"in_stock\": true}\n```",
        )
        .unwrap();
        assert_eq!(reading.price, Some(449.0));
        assert_eq!(reading.currency.as_deref(), Some("KES"));
        assert_eq!(reading.in_stock, Some(true));
        assert_eq!(reading.read_by, Some(ReadBy::Model));
        let none = parse_model_answer(
            "{\"title\": \"Toyota Fielder\", \"amount\": null, \"currency\": null, \"in_stock\": null}",
        )
        .unwrap();
        assert!(!none.is_useful());
        let words =
            parse_model_answer("{\"price\": \"KSh 1,199,000\", \"currency\": \"KES\"}").unwrap();
        assert_eq!(words.price, Some(1_199_000.0));
        assert!(parse_model_answer("I could not find a price.").is_none());
        let (system, user) = model_prompt("https://x.example/p", "A kettle", &"x".repeat(9_000));
        assert!(system.contains("never a guess"));
        assert!(user.len() < 6_100);
    }
}
