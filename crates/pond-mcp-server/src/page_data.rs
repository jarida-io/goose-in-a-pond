//! What a page carries beside its text: the items its structured data lists (schema.org JSON-LD,
//! or the page data a Next.js or Nuxt app ships in its first HTML), the links in its content, and
//! its forms. Many listing sites build their text with scripts but still send their items in
//! the first HTML, so items are read before any browser would be needed.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use pond_core::mcp::services::page_addresses::{FormField, FormMethod, SearchForm};
use pond_core::shared::services::passages;
use reqwest::Url;
use serde_json::{Map, Value};

use crate::web_search;

/// Items kept from one page.
pub const MAX_ITEMS: usize = 40;

/// Links kept from one page's content, in page order.
pub const MAX_LINKS: usize = 120;

const MAX_FORMS: usize = 6;
const MAX_FIELDS: usize = 24;
const MAX_OPTIONS: usize = 60;

const MAX_TEXT_BYTES: usize = 120;
const MAX_LINK_TEXT_BYTES: usize = 80;

/// JSON values visited at most in one page's data: a big app state is megabytes.
const MAX_JSON_NODES: usize = 200_000;
const MAX_JSON_DEPTH: usize = 48;

/// schema.org types that are a thing on offer, an article, or an announcement.
pub(crate) const ITEM_TYPES: &[&str] = &[
    "Car",
    "Vehicle",
    "Motorcycle",
    "Product",
    "IndividualProduct",
    "ProductModel",
    "Offer",
    "Article",
    "NewsArticle",
    "BlogPosting",
    "TechArticle",
    "SocialMediaPosting",
    "Event",
    "JobPosting",
    "SoftwareApplication",
    "SoftwareSourceCode",
];

const TITLE_KEYS: &[&str] = &[
    "title",
    "name",
    "heading",
    "displayname",
    "listingtitle",
    "headline",
];
const PRICE_KEYS: &[&str] = &[
    "price",
    "pricevalue",
    "price_value",
    "amount",
    "displayprice",
    "formattedprice",
    "priceformatted",
    "listprice",
    "saleprice",
];
const CURRENCY_KEYS: &[&str] = &["currency", "pricecurrency", "currencycode", "currency_code"];
const MILEAGE_KEYS: &[&str] = &[
    "mileage",
    "odometer",
    "kilometers",
    "kilometres",
    "mileagekm",
    "km",
    "mileagefromodometer",
];
const YEAR_KEYS: &[&str] = &[
    "year",
    "modelyear",
    "model_year",
    "registrationyear",
    "manufactureyear",
    "vehiclemodeldate",
    "modeldate",
    "productiondate",
];
const LINK_KEYS: &[&str] = &[
    "url",
    "href",
    "link",
    "permalink",
    "canonicalurl",
    "path",
    "slug",
];
const IMAGE_KEYS: &[&str] = &[
    "image",
    "imageurl",
    "image_url",
    "thumbnail",
    "thumbnailurl",
    "photo",
    "mainimage",
    "images",
    "photos",
];

/// One thing a page lists: a car, a product, an article, or an update.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Item {
    pub title: String,
    pub price: Option<String>,
    pub currency: Option<String>,
    pub mileage: Option<String>,
    pub year: Option<String>,
    pub link: Option<String>,
    pub image: Option<String>,
    pub snippet: Option<String>,
}

/// Where a page's items came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemSource {
    JsonLd,
    NextData,
    NuxtData,
    HtmlArticles,
    ContentSections,
    /// A run of links that share a shape: a forum's front page, search results, an index.
    LinkList,
}

impl ItemSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemSource::JsonLd => "json_ld",
            ItemSource::NextData => "next_data",
            ItemSource::NuxtData => "nuxt_data",
            ItemSource::HtmlArticles => "html_articles",
            ItemSource::ContentSections => "content_sections",
            ItemSource::LinkList => "link_list",
        }
    }
}

/// A link in a page's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub address: String,
    pub text: String,
}

#[derive(Debug, Default)]
pub struct PageData {
    pub items: Vec<Item>,
    pub items_from: Option<ItemSource>,
    /// What the page lists as plain links when nothing marks them as items. Kept apart from
    /// `items`, so a watch that compares a page's sections is not handed a different list.
    pub listed_links: Vec<Item>,
    pub links: Vec<Link>,
    pub forms: Vec<SearchForm>,
    /// The picture the page names for itself when shared (`og:image`), for a report to show.
    pub preview_image: Option<String>,
}

pub fn page_data(document: &scraper::Html, base: &Url) -> PageData {
    let (items, items_from) = items_of(document, base);
    let listed_links = match items.is_empty() {
        true => link_list(document, base),
        false => Vec::new(),
    };
    PageData {
        items,
        items_from,
        listed_links,
        links: links_of(document, base),
        forms: forms_of(document, base),
        preview_image: preview_of(document, base),
    }
}

pub(crate) fn preview_of(document: &scraper::Html, base: &Url) -> Option<String> {
    static META: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"meta[property="og:image"], meta[property="og:image:url"], meta[name="twitter:image"]"#,
        )
    });
    document
        .select(&META)
        .filter_map(|m| m.value().attr("content"))
        .find_map(|c| absolute(c, base))
}

fn selector(css: &'static str) -> scraper::Selector {
    scraper::Selector::parse(css).expect("a fixed selector")
}

fn script_text(element: scraper::ElementRef<'_>) -> String {
    element.text().collect::<String>()
}

// ── Items ──────────────────────────────────────────────────────────────────

fn items_of(document: &scraper::Html, base: &Url) -> (Vec<Item>, Option<ItemSource>) {
    static LD: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"script[type="application/ld+json"]"#));
    static NEXT: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"script#__NEXT_DATA__"#));
    static NUXT: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"script#__NUXT_DATA__"#));

    let mut items = Vec::new();
    for script in document.select(&LD) {
        if let Ok(value) = serde_json::from_str::<Value>(script_text(script).trim()) {
            let mut budget = MAX_JSON_NODES;
            ld_items(&value, base, &mut items, 0, &mut budget);
        }
    }
    let items = distinct(items);
    if !items.is_empty() {
        return (items, Some(ItemSource::JsonLd));
    }
    if let Some(value) = document
        .select(&NEXT)
        .next()
        .and_then(|s| serde_json::from_str::<Value>(script_text(s).trim()).ok())
    {
        let items = distinct(listing_items(&value, base));
        if !items.is_empty() {
            return (items, Some(ItemSource::NextData));
        }
    }
    if let Some(Value::Array(values)) = document
        .select(&NUXT)
        .next()
        .and_then(|s| serde_json::from_str::<Value>(script_text(s).trim()).ok())
    {
        let state = unflatten(&values);
        let items = distinct(listing_items(&state, base));
        if !items.is_empty() {
            return (items, Some(ItemSource::NuxtData));
        }
    }
    let articles = html_articles(document, base);
    if !articles.is_empty() {
        return (articles, Some(ItemSource::HtmlArticles));
    }
    (Vec::new(), None)
}

fn html_articles(document: &scraper::Html, base: &Url) -> Vec<Item> {
    static ARTICLE: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"article, [role="article"], .post, .blog-post, .entry, .news-item, .release-entry, .release"#,
        )
    });
    static TITLE: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"h1 a, h2 a, h3 a, h4 a, h1, h2, h3, h4, .entry-title a, .post-title a, .title a"#,
        )
    });
    static LINK: LazyLock<scraper::Selector> = LazyLock::new(|| selector(r#"a[href]"#));
    static TIME: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"time, .date, [datetime]"#));
    static SNIPPET: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"p, .summary, .excerpt, .description, .entry-summary"#));
    static IMG: LazyLock<scraper::Selector> = LazyLock::new(|| selector(r#"img[src]"#));

    let mut items = Vec::new();
    for element in document.select(&ARTICLE).take(MAX_ITEMS) {
        let title_elem = element.select(&TITLE).next();
        let title = title_elem
            .map(|e| e.text().collect::<String>())
            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|t| !t.is_empty());
        let Some(title) = title else { continue };

        let link = title_elem
            .and_then(|e| e.value().attr("href"))
            .or_else(|| element.select(&LINK).find_map(|e| e.value().attr("href")))
            .and_then(|l| absolute(l, base));

        let time_text = element.select(&TIME).next().and_then(|e| {
            e.value()
                .attr("datetime")
                .map(str::to_string)
                .or_else(|| Some(e.text().collect::<String>()))
        });
        let year = time_text.as_deref().and_then(year_in);

        let snippet = element
            .select(&SNIPPET)
            .map(|e| e.text().collect::<String>())
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .find(|s| !s.is_empty() && s != &title)
            .map(|s| clip(&s));

        let image = element
            .select(&IMG)
            .find_map(|e| e.value().attr("src"))
            .and_then(|s| absolute(s, base));

        items.push(Item {
            title: clip(&title),
            price: None,
            currency: None,
            mileage: None,
            year,
            link,
            image,
            snippet,
        });
    }
    distinct(items)
}

/// Fewest links of one shape that make a list rather than a few related links.
const MIN_LISTED: usize = 5;
/// Fewest words a listed link's text has: a menu's entries are one or two.
const MIN_LISTED_WORDS: usize = 3;
/// Ancestors of a link that name its shape.
const SHAPE_DEPTH: usize = 3;

/// A page that is mostly a list of links to its own things, with nothing that marks them as
/// articles: the largest run of links that sit in the same place in the same kind of row. A
/// menu, a footer and a tag cloud are left out by where they are and how short their words are.
fn link_list(document: &scraper::Html, base: &Url) -> Vec<Item> {
    static ANCHOR: LazyLock<scraper::Selector> = LazyLock::new(|| selector("a[href]"));
    let shape = |anchor: scraper::ElementRef<'_>| -> String {
        let mut parts: Vec<String> = anchor
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .take(SHAPE_DEPTH)
            .map(|e| {
                let class = e.value().classes().next().unwrap_or_default();
                format!("{}.{class}", e.value().name())
            })
            .collect();
        parts.push(format!(
            "a.{}",
            anchor.value().classes().next().unwrap_or_default()
        ));
        parts.join(">")
    };

    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<(scraper::ElementRef<'_>, String, String)>> =
        HashMap::new();
    for anchor in document.select(&ANCHOR) {
        let in_furniture = anchor
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|e| web_search::is_boilerplate(e.value(), false));
        if in_furniture {
            continue;
        }
        let text = anchor
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !text.chars().any(char::is_alphanumeric) {
            continue;
        }
        let Some(link) = anchor
            .value()
            .attr("href")
            .and_then(|h| absolute(h.trim(), base))
        else {
            continue;
        };
        let key = shape(anchor);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push((anchor, text, link));
    }
    // A run is a list when most of its links are worded like titles; a menu's are not. Of
    // the lists, the one with the most words wins, so a column of "3 hours ago" links loses
    // to the titles beside it; of equals, the one that comes first on the page.
    let titled = |key: &String| -> usize {
        groups[key]
            .iter()
            .filter(|(_, text, _)| text.split_whitespace().count() >= MIN_LISTED_WORDS)
            .count()
    };
    let words = |key: &String| -> usize { groups[key].iter().map(|(_, text, _)| text.len()).sum() };
    let listed: Vec<&String> = order
        .iter()
        .filter(|key| titled(key) >= MIN_LISTED && titled(key) * 2 > groups[*key].len())
        .collect();
    let Some(most) = listed.iter().map(|key| words(key)).max() else {
        return Vec::new();
    };
    let Some(best) = listed.into_iter().find(|key| words(key) == most) else {
        return Vec::new();
    };

    let rows = &groups[best];
    let mut items = Vec::new();
    for (anchor, title, link) in rows.iter().take(MAX_ITEMS) {
        // The row is the nearest ancestor that holds this link and no other of the run.
        let row = anchor
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .take(SHAPE_DEPTH + 1)
            .take_while(|e| {
                rows.iter()
                    .filter(|(other, _, _)| other.ancestors().any(|a| a.id() == e.id()))
                    .count()
                    == 1
            })
            .last();
        let snippet = row
            .map(|e| {
                e.text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .map(|all| all.replacen(title.as_str(), "", 1).trim().to_string())
            .filter(|rest| rest.chars().any(char::is_alphanumeric))
            .map(|rest| clip(&rest));
        items.push(Item {
            title: clip(title),
            link: Some(link.clone()),
            snippet,
            ..Item::default()
        });
    }
    distinct(items)
}

fn distinct(items: Vec<Item>) -> Vec<Item> {
    let mut seen = HashSet::new();
    items
        .into_iter()
        .filter(|i| seen.insert((i.title.clone(), i.link.clone())))
        .take(MAX_ITEMS)
        .collect()
}

pub(crate) fn types_of(object: &Map<String, Value>) -> Vec<String> {
    match object.get("@type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// The items a JSON-LD value describes, `@graph`s and `ItemList`s opened.
fn ld_items(value: &Value, base: &Url, out: &mut Vec<Item>, depth: usize, budget: &mut usize) {
    if depth > MAX_JSON_DEPTH || *budget == 0 || out.len() >= MAX_ITEMS * 2 {
        return;
    }
    *budget -= 1;
    match value {
        Value::Array(values) => {
            for v in values {
                ld_items(v, base, out, depth + 1, budget);
            }
        }
        Value::Object(object) => {
            if let Some(graph) = object.get("@graph") {
                ld_items(graph, base, out, depth + 1, budget);
            }
            let types = types_of(object);
            let is = |t: &str| types.iter().any(|x| x.eq_ignore_ascii_case(t));
            if is("ItemList") || is("OfferCatalog") {
                let elements = object
                    .get("itemListElement")
                    .or_else(|| object.get("itemlistelement"));
                if let Some(elements) = elements {
                    ld_items(elements, base, out, depth + 1, budget);
                }
            } else if is("ListItem") {
                match object.get("item") {
                    Some(item @ Value::Object(_)) => ld_items(item, base, out, depth + 1, budget),
                    _ => {
                        if let Some(item) = ld_item(object, base) {
                            out.push(item);
                        }
                    }
                }
            } else if ITEM_TYPES.iter().any(|t| is(t)) {
                if let Some(item) = ld_item(object, base) {
                    out.push(item);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn ld_item(object: &Map<String, Value>, base: &Url) -> Option<Item> {
    let offered = object.get("itemOffered").and_then(Value::as_object);
    let title = text_of(object.get("name").or_else(|| object.get("headline")))
        .or_else(|| offered.and_then(|o| text_of(o.get("name"))))?;
    let offer = match object.get("offers") {
        Some(Value::Array(offers)) => offers.first().and_then(Value::as_object),
        Some(Value::Object(offer)) => Some(offer),
        _ => None,
    };
    let field = |key: &str| {
        offer
            .and_then(|o| text_of(o.get(key)))
            .or_else(|| text_of(object.get(key)))
    };
    let price = field("price").or_else(|| field("lowPrice"));
    let currency = field("priceCurrency");
    let mileage = object.get("mileageFromOdometer").and_then(|m| match m {
        Value::Object(q) => {
            let value = text_of(q.get("value"))?;
            let unit = match text_of(q.get("unitCode")).as_deref() {
                Some("KMT") => " km".to_string(),
                Some("SMI") => " mi".to_string(),
                Some(other) => format!(" {other}"),
                None => String::new(),
            };
            Some(format!("{value}{unit}"))
        }
        other => text_of(Some(other)),
    });
    let year = [
        "vehicleModelDate",
        "modelDate",
        "productionDate",
        "dateVehicleFirstRegistered",
        "datePublished",
        "dateCreated",
        "dateModified",
        "startDate",
    ]
    .iter()
    .find_map(|k| text_of(object.get(*k)))
    .and_then(|d| year_in(&d));
    let link = text_of(object.get("url"))
        .or_else(|| offer.and_then(|o| text_of(o.get("url"))))
        .or_else(|| text_of(object.get("@id")).filter(|id| id.starts_with("http")))
        .and_then(|u| absolute(&u, base));
    let image = object
        .get("image")
        .and_then(image_of)
        .and_then(|u| absolute(&u, base));
    let snippet = text_of(
        object
            .get("description")
            .or_else(|| object.get("articleBody"))
            .or_else(|| object.get("summary")),
    );
    Some(Item {
        title: clip(&title),
        price,
        currency,
        mileage,
        year,
        link,
        image,
        snippet,
    })
}

/// The longest array of objects in an app's state that reads as a listing: most of its objects
/// have a name and a price.
fn listing_items(state: &Value, base: &Url) -> Vec<Item> {
    let mut best: Option<&Vec<Value>> = None;
    let mut best_score = 0usize;
    let mut budget = MAX_JSON_NODES;
    let mut stack: Vec<(&Value, usize)> = vec![(state, 0)];
    while let Some((value, depth)) = stack.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        if depth > MAX_JSON_DEPTH {
            continue;
        }
        match value {
            Value::Array(values) => {
                let score = values
                    .iter()
                    .filter_map(Value::as_object)
                    .filter(|o| looks_listed(o))
                    .count();
                if score >= 2 && score * 2 >= values.len() && score > best_score {
                    best = Some(values);
                    best_score = score;
                }
                stack.extend(values.iter().map(|v| (v, depth + 1)));
            }
            Value::Object(object) => stack.extend(object.values().map(|v| (v, depth + 1))),
            _ => {}
        }
    }
    best.map(|values| {
        values
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|o| state_item(o, base))
            .collect()
    })
    .unwrap_or_default()
}

/// A field by any of `keys`, matched without case.
fn key<'a>(object: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a Value> {
    object
        .iter()
        .find(|(k, v)| !v.is_null() && keys.contains(&k.to_ascii_lowercase().as_str()))
        .map(|(_, v)| v)
}

fn looks_listed(object: &Map<String, Value>) -> bool {
    let named = key(object, TITLE_KEYS).is_some_and(|v| v.is_string())
        || (key(object, &["make"]).is_some() && key(object, &["model"]).is_some());
    named && key(object, PRICE_KEYS).is_some()
}

fn state_item(object: &Map<String, Value>, base: &Url) -> Option<Item> {
    let title = text_of(key(object, TITLE_KEYS)).or_else(|| {
        let part = |k: &str| text_of(key(object, &[k]));
        let words: Vec<String> = ["year", "make", "model"]
            .iter()
            .filter_map(|k| part(k))
            .collect();
        (!words.is_empty()).then(|| words.join(" "))
    })?;
    let (price, mut currency) = match key(object, PRICE_KEYS) {
        Some(Value::Object(p)) => (
            text_of(key(
                p,
                &["amount", "value", "raw", "formatted", "display", "price"],
            )),
            text_of(key(p, CURRENCY_KEYS)),
        ),
        other => (text_of(other), None),
    };
    if currency.is_none() {
        currency = text_of(key(object, CURRENCY_KEYS));
    }
    let mileage = match key(object, MILEAGE_KEYS) {
        Some(Value::Object(m)) => text_of(key(m, &["value", "amount", "formatted"])),
        other => text_of(other),
    };
    let year = text_of(key(object, YEAR_KEYS)).and_then(|y| year_in(&y));
    let link = text_of(key(object, LINK_KEYS))
        .filter(|l| l.starts_with('/') || l.starts_with("http"))
        .and_then(|l| absolute(&l, base));
    let image = key(object, IMAGE_KEYS)
        .and_then(image_of)
        .and_then(|u| absolute(&u, base));
    let snippet = text_of(key(
        object,
        &["snippet", "description", "summary", "excerpt", "text"],
    ));
    Some(Item {
        title: clip(&title),
        price,
        currency,
        mileage,
        year,
        link,
        image,
        snippet,
    })
}

/// A short text from a string or number; `None` for anything else, or a blank.
fn text_of(value: Option<&Value>) -> Option<String> {
    let text = match value? {
        Value::String(s) => s.split_whitespace().collect::<Vec<_>>().join(" "),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    (!text.is_empty()).then(|| clip(&text))
}

fn clip(text: &str) -> String {
    passages::clip(&web_search::defang(text), MAX_TEXT_BYTES)
}

fn image_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Array(values) => values.iter().find_map(image_of),
        Value::Object(o) => key(o, &["url", "contenturl", "src", "href"]).and_then(image_of),
        _ => None,
    }
}

fn year_in(text: &str) -> Option<String> {
    let digits: Vec<char> = text.chars().collect();
    digits.windows(4).find_map(|w| {
        let year: String = w.iter().collect();
        (w.iter().all(char::is_ascii_digit) && (year.starts_with("19") || year.starts_with("20")))
            .then_some(year)
    })
}

fn absolute(address: &str, base: &Url) -> Option<String> {
    let url = base.join(address.trim()).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// Nuxt's `__NUXT_DATA__`, a devalue payload, as the value it encodes: a flat array whose
/// entries name other entries by index, with tagged arrays for dates, sets and Vue's refs.
fn unflatten(values: &[Value]) -> Value {
    fn hydrate(values: &[Value], index: &Value, depth: usize, budget: &mut usize) -> Value {
        let Some(index) = index.as_i64().filter(|i| *i >= 0) else {
            return Value::Null;
        };
        if depth > MAX_JSON_DEPTH || *budget == 0 {
            return Value::Null;
        }
        *budget -= 1;
        let Some(value) = values.get(index as usize) else {
            return Value::Null;
        };
        match value {
            Value::Array(entries) => match entries.first() {
                Some(Value::String(tag)) => match tag.as_str() {
                    "Date" | "RegExp" | "BigInt" | "Object" | "URL" => {
                        entries.get(1).cloned().unwrap_or(Value::Null)
                    }
                    "Set" => Value::Array(
                        entries[1..]
                            .iter()
                            .map(|i| hydrate(values, i, depth + 1, budget))
                            .collect(),
                    ),
                    "Map" | "null" => {
                        let mut object = Map::new();
                        for pair in entries[1..].chunks(2) {
                            if let [k, v] = pair {
                                let key = match hydrate(values, k, depth + 1, budget) {
                                    Value::String(s) => s,
                                    _ => k.as_str().map(str::to_string).unwrap_or_default(),
                                };
                                object.insert(key, hydrate(values, v, depth + 1, budget));
                            }
                        }
                        Value::Object(object)
                    }
                    // A reducer such as Vue's Reactive or Ref wraps the one value it holds.
                    _ => entries
                        .get(1)
                        .map(|i| hydrate(values, i, depth + 1, budget))
                        .unwrap_or(Value::Null),
                },
                _ => Value::Array(
                    entries
                        .iter()
                        .map(|i| hydrate(values, i, depth + 1, budget))
                        .collect(),
                ),
            },
            Value::Object(object) => Value::Object(
                object
                    .iter()
                    .map(|(k, i)| (k.clone(), hydrate(values, i, depth + 1, budget)))
                    .collect(),
            ),
            primitive => primitive.clone(),
        }
    }
    let mut budget = MAX_JSON_NODES;
    hydrate(values, &Value::from(0), 0, &mut budget)
}

// ── Links ──────────────────────────────────────────────────────────────────

/// The links in the page's content, furniture left out, each once, in page order.
fn links_of(document: &scraper::Html, base: &Url) -> Vec<Link> {
    static ANCHOR: LazyLock<scraper::Selector> = LazyLock::new(|| selector("a[href]"));
    let page = base
        .as_str()
        .split('#')
        .next()
        .unwrap_or_default()
        .to_string();
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for anchor in document.select(&ANCHOR) {
        if links.len() >= MAX_LINKS {
            break;
        }
        let in_furniture = anchor
            .ancestors()
            .filter_map(scraper::ElementRef::wrap)
            .any(|e| web_search::is_boilerplate(e.value(), false));
        if in_furniture {
            continue;
        }
        let Some(mut url) = anchor
            .value()
            .attr("href")
            .and_then(|h| base.join(h.trim()).ok())
        else {
            continue;
        };
        if !matches!(url.scheme(), "http" | "https") {
            continue;
        }
        url.set_fragment(None);
        let address = url.to_string();
        if address == page || !seen.insert(address.clone()) {
            continue;
        }
        let text = anchor
            .text()
            .collect::<Vec<_>>()
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !text.chars().any(char::is_alphanumeric) {
            continue;
        }
        links.push(Link {
            address,
            text: passages::clip(&web_search::defang(&text), MAX_LINK_TEXT_BYTES),
        });
    }
    links
}

// ── Forms ──────────────────────────────────────────────────────────────────

/// Words in a field's name that mark a form as asking for a secret, a payment or the person's
/// contact. Matched as whole words, so a "hotel" field is not a telephone.
const PRIVATE_NAME_WORDS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "pin",
    "card",
    "cardnumber",
    "cvv",
    "cvc",
    "iban",
    "payment",
    "email",
    "phone",
    "tel",
    "mobile",
];

/// Input types that ask for a secret, the person's contact, or a file of theirs.
const PRIVATE_TYPES: &[&str] = &["password", "email", "tel", "file"];

/// Whether a field asks for something private: by its type, its autofill hint, or its name.
fn is_private(name: &str, kind: &str, autocomplete: &str) -> bool {
    let autocomplete = autocomplete.to_ascii_lowercase();
    PRIVATE_TYPES.contains(&kind)
        || autocomplete.starts_with("cc-")
        || ["email", "tel", "password"]
            .iter()
            .any(|h| autocomplete.contains(h))
        || name
            .to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| PRIVATE_NAME_WORDS.contains(&word))
}

/// The page's GET and POST forms that ask for nothing private, as fields a reader may fill.
pub fn forms_of(document: &scraper::Html, base: &Url) -> Vec<SearchForm> {
    static FORM: LazyLock<scraper::Selector> = LazyLock::new(|| selector("form"));
    static FIELD: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector("input[name], select[name], textarea[name]"));
    static OPTION: LazyLock<scraper::Selector> = LazyLock::new(|| selector("option"));

    let mut forms = Vec::new();
    for form in document.select(&FORM) {
        if forms.len() >= MAX_FORMS {
            break;
        }
        let method = form.value().attr("method").unwrap_or("get").trim();
        let method = if method.eq_ignore_ascii_case("get") || method.is_empty() {
            FormMethod::Get
        } else if method.eq_ignore_ascii_case("post") {
            FormMethod::Post
        } else {
            continue;
        };
        // A file upload sends the person's own file, never a field the reader fills.
        if form
            .value()
            .attr("enctype")
            .is_some_and(|e| e.trim().eq_ignore_ascii_case("multipart/form-data"))
        {
            continue;
        }
        let Some(mut action) = base
            .join(form.value().attr("action").unwrap_or_default().trim())
            .ok()
        else {
            continue;
        };
        if !matches!(action.scheme(), "http" | "https") {
            continue;
        }
        action.set_query(None);
        action.set_fragment(None);
        let mut fields: Vec<FormField> = Vec::new();
        let mut private = false;
        for field in form.select(&FIELD) {
            let element = field.value();
            let name = element.attr("name").unwrap_or_default().trim().to_string();
            let kind = element
                .attr("type")
                .unwrap_or("text")
                .trim()
                .to_ascii_lowercase();
            if is_private(
                &name,
                &kind,
                element.attr("autocomplete").unwrap_or_default(),
            ) {
                private = true;
                break;
            }
            if name.is_empty() || matches!(kind.as_str(), "submit" | "button" | "image" | "reset") {
                continue;
            }
            let value = element.attr("value").unwrap_or_default().to_string();
            let new = match element.name() {
                "select" => FormField::Choice {
                    name: name.clone(),
                    options: field
                        .select(&OPTION)
                        .map(|o| {
                            o.value()
                                .attr("value")
                                .map(str::to_string)
                                .unwrap_or_else(|| o.text().collect::<String>().trim().to_string())
                        })
                        .take(MAX_OPTIONS)
                        .collect(),
                },
                _ => match kind.as_str() {
                    "hidden" => FormField::Fixed {
                        name: name.clone(),
                        value,
                    },
                    "checkbox" | "radio" => FormField::Choice {
                        name: name.clone(),
                        options: vec![if value.is_empty() { "on".into() } else { value }],
                    },
                    _ => FormField::Text(name.clone()),
                },
            };
            match (fields.iter_mut().find(|f| f.name() == name), new) {
                // Radio buttons and checkboxes of one name are one choice.
                (
                    Some(FormField::Choice { options, .. }),
                    FormField::Choice { options: more, .. },
                ) => {
                    for option in more {
                        if !options.contains(&option) {
                            options.push(option);
                        }
                    }
                }
                (Some(_), _) => {}
                (None, new) => fields.push(new),
            }
            if fields.len() >= MAX_FIELDS {
                break;
            }
        }
        let fillable = fields.iter().any(|f| !matches!(f, FormField::Fixed { .. }));
        if !private && fillable {
            forms.push(SearchForm {
                action: action.to_string(),
                method,
                fields,
            });
        }
    }
    forms
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(html: &str) -> PageData {
        let document = scraper::Html::parse_document(html);
        page_data(&document, &Url::parse("https://cars.example/used").unwrap())
    }

    /// A forum's front page as a browser builds it: rows of one shape, no article markup.
    fn forum(rows: usize) -> String {
        let mut html = String::from(
            "<html><body><nav><a href=\"/new\">New stories today</a><a href=\"/ask\">Ask a \
             question here</a><a href=\"/show\">Show your own work</a><a href=\"/jobs\">Jobs \
             going right now</a><a href=\"/top\">Top of the week</a></nav><main>",
        );
        for n in 1..=rows {
            html.push_str(&format!(
                "<div class=\"Story\"><div class=\"Story_data\"><a class=\"Story_title\" \
                 href=\"https://news.example/item/{n}\"><span>Story number {n} about a thing\
                 </span></a><span class=\"Story_meta\">{} points, 3 hours ago</span></div>\
                 <a class=\"Story_comments\" href=\"/item/{n}/comments\">12 comments</a></div>",
                n * 10
            ));
        }
        html.push_str(
            "</main><footer><a href=\"/about\">About this small site</a></footer></body></html>",
        );
        html
    }

    #[test]
    fn a_page_that_is_a_list_of_links_is_read_as_its_entries_in_order() {
        let page = data(&forum(7));
        assert!(page.items.is_empty(), "nothing marks them as items");
        let titles: Vec<&str> = page.listed_links.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles.len(), 7);
        assert_eq!(titles[0], "Story number 1 about a thing");
        assert_eq!(titles[6], "Story number 7 about a thing");
        assert_eq!(
            page.listed_links[0].link.as_deref(),
            Some("https://news.example/item/1")
        );
        assert_eq!(
            page.listed_links[1].snippet.as_deref(),
            Some("20 points, 3 hours ago 12 comments"),
            "the rest of its own row, and nothing of the next"
        );
    }

    #[test]
    fn a_menu_a_footer_and_a_few_links_are_not_a_list() {
        assert!(
            data(&forum(4)).listed_links.is_empty(),
            "four links are not a list"
        );
        let menu = "<html><body><main><ul>\
            <li><a href=\"/a\">Home</a></li><li><a href=\"/b\">About us</a></li>\
            <li><a href=\"/c\">Contact</a></li><li><a href=\"/d\">Blog</a></li>\
            <li><a href=\"/e\">Shop</a></li><li><a href=\"/f\">Help</a></li></ul></main></body></html>";
        assert!(
            data(menu).listed_links.is_empty(),
            "one or two words each is a menu"
        );
    }

    #[test]
    fn a_list_keeps_its_short_titles_and_beats_a_column_of_passing_links() {
        let mut html = String::from("<html><body><table>");
        for (n, title) in [
            "Rust 1.100",
            "A long title about a thing",
            "Another long title here now",
            "Yet another story to read",
            "The fifth story of the day",
            "Story six goes on and on",
        ]
        .iter()
        .enumerate()
        {
            html.push_str(&format!(
                "<tr class=\"row\"><td class=\"title\"><a href=\"https://x.example/{n}\">{title}</a>\
                 </td><td class=\"age\"><a href=\"/item/{n}\">{n} hours ago</a></td></tr>"
            ));
        }
        html.push_str("</table></body></html>");
        let titles: Vec<String> = data(&html)
            .listed_links
            .into_iter()
            .map(|i| i.title)
            .collect();
        assert_eq!(titles.len(), 6);
        assert_eq!(
            titles[0], "Rust 1.100",
            "a two-word title in a list of titles stays"
        );
        assert!(!titles.iter().any(|t| t.ends_with("hours ago")));
    }

    #[test]
    fn a_page_with_items_of_its_own_keeps_them_and_lists_no_links() {
        let mut html = forum(8);
        html = html.replace(
            "<main>",
            "<main><article><h2><a href=\"/post/1\">A proper article here</a></h2><p>Words.</p></article>",
        );
        let page = data(&html);
        assert_eq!(page.items.len(), 1);
        assert!(page.listed_links.is_empty());
    }

    #[test]
    fn a_json_ld_list_of_cars_reads_as_items() {
        let page = data(
            r#"<html><head><script type="application/ld+json">
            {"@context":"https://schema.org","@type":"ItemList","itemListElement":[
              {"@type":"ListItem","position":1,"item":{"@type":"Car","name":"2017 Subaru Forester",
                "url":"/used/123","image":["https://img.example/1.jpg"],
                "mileageFromOdometer":{"@type":"QuantitativeValue","value":98000,"unitCode":"KMT"},
                "vehicleModelDate":"2017",
                "offers":{"@type":"Offer","price":1450000,"priceCurrency":"KES"}}},
              {"@type":"ListItem","position":2,"item":{"@type":"Car","name":"2015 Toyota RAV4",
                "url":"https://cars.example/used/456","offers":{"price":"1,200,000","priceCurrency":"KES"}}}
            ]}</script></head><body></body></html>"#,
        );
        assert_eq!(page.items_from, Some(ItemSource::JsonLd));
        assert_eq!(page.items.len(), 2);
        let first = &page.items[0];
        assert_eq!(first.title, "2017 Subaru Forester");
        assert_eq!(first.price.as_deref(), Some("1450000"));
        assert_eq!(first.currency.as_deref(), Some("KES"));
        assert_eq!(first.mileage.as_deref(), Some("98000 km"));
        assert_eq!(first.year.as_deref(), Some("2017"));
        assert_eq!(first.link.as_deref(), Some("https://cars.example/used/123"));
        assert_eq!(first.image.as_deref(), Some("https://img.example/1.jpg"));
        assert_eq!(page.items[1].price.as_deref(), Some("1,200,000"));
    }

    #[test]
    fn a_json_ld_graph_with_one_product_is_one_item() {
        let page = data(
            r#"<script type="application/ld+json">{"@graph":[{"@type":"WebPage","name":"Shop"},
            {"@type":["Product"],"name":"Kettle","offers":[{"@type":"Offer","price":"2500","priceCurrency":"KES"}]}]}
            </script>"#,
        );
        assert_eq!(page.items.len(), 1, "{:?}", page.items);
        assert_eq!(page.items[0].title, "Kettle");
        assert_eq!(page.items[0].price.as_deref(), Some("2500"));
    }

    #[test]
    fn a_next_app_ships_its_listing_in_its_page_data() {
        let page = data(
            r#"<script id="__NEXT_DATA__" type="application/json">{"props":{"pageProps":{
              "nav":[{"title":"Home","href":"/"}],
              "results":{"listings":[
                {"id":1,"title":"Subaru Outback","price":{"amount":1650000,"currency":"KES"},"mileage":120000,"year":2016,"url":"/used/9"},
                {"id":2,"title":"Mazda CX-5","price":{"amount":1900000,"currency":"KES"},"mileage":80000,"year":2018,"url":"/used/10"},
                {"id":3,"make":"Honda","model":"CR-V","year":2015,"price":1400000,"url":"/used/11"}
              ]}}}}</script>"#,
        );
        assert_eq!(page.items_from, Some(ItemSource::NextData));
        assert_eq!(page.items.len(), 3);
        assert_eq!(page.items[0].price.as_deref(), Some("1650000"));
        assert_eq!(page.items[0].currency.as_deref(), Some("KES"));
        assert_eq!(page.items[0].mileage.as_deref(), Some("120000"));
        assert_eq!(
            page.items[0].link.as_deref(),
            Some("https://cars.example/used/9")
        );
        assert_eq!(page.items[2].title, "2015 Honda CR-V");
    }

    #[test]
    fn a_nuxt_app_ships_its_listing_as_devalue() {
        // [root, {data:2}, ["Reactive",3], {cars:4}, [5,8], {title:6,price:7}, "Forester", 1450000,
        //  {title:9,price:10}, "Outback", 1650000]
        let page = data(
            r#"<script id="__NUXT_DATA__" type="application/json">[{"state":1},{"data":2},["Reactive",3],{"cars":4},[5,8],{"title":6,"price":7},"Forester",1450000,{"title":9,"price":10},"Outback",1650000]</script>"#,
        );
        assert_eq!(page.items_from, Some(ItemSource::NuxtData));
        let titles: Vec<&str> = page.items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, vec!["Forester", "Outback"]);
        assert_eq!(page.items[1].price.as_deref(), Some("1650000"));
    }

    #[test]
    fn content_links_are_kept_and_furniture_left() {
        let page = data(
            r##"<nav><a href="/about">About us</a></nav>
            <main><a href="/used/123">2017 Subaru Forester</a> <a href="#top">Top</a>
            <a href="/used/123">again</a> <a href="mailto:x@y.z">Mail</a> <a href="/used"><img></a></main>
            <footer><a href="/terms">Terms</a></footer>"##,
        );
        assert_eq!(
            page.links,
            vec![Link {
                address: "https://cars.example/used/123".into(),
                text: "2017 Subaru Forester".into()
            }]
        );
    }

    #[test]
    fn forms_are_read_and_private_ones_skipped() {
        let page = data(
            r#"<form action="/search"><input name="q"><select name="make"><option value="">Any</option>
              <option value="subaru">Subaru</option><option>Toyota</option></select>
              <input type="radio" name="fuel" value="petrol"><input type="radio" name="fuel" value="diesel">
              <input type="hidden" name="kind" value="used"><button type="submit">Go</button></form>
            <form method="POST" action="/enquire"><input type="hidden" name="csrf" value="t0k">
              <textarea name="message"></textarea></form>
            <form method="post" action="/login"><input name="user"><input type="password" name="pw"></form>
            <form method="post" action="/news"><input type="email" name="address"></form>
            <form method="post" action="/call"><input type="tel" name="number"></form>
            <form method="post" action="/cv" enctype="multipart/form-data"><input name="name"></form>
            <form method="post" action="/upload"><input type="file" name="doc"></form>
            <form method="dialog"><input name="x"></form>
            <form action="/pay"><input name="card_number"></form>"#,
        );
        assert_eq!(page.forms.len(), 2, "{:?}", page.forms);
        let form = &page.forms[0];
        assert_eq!(form.action, "https://cars.example/search");
        assert_eq!(form.method, FormMethod::Get);
        assert_eq!(
            page.forms[1],
            SearchForm {
                action: "https://cars.example/enquire".into(),
                method: FormMethod::Post,
                fields: vec![
                    FormField::Fixed {
                        name: "csrf".into(),
                        value: "t0k".into()
                    },
                    FormField::Text("message".into()),
                ],
            }
        );
        assert_eq!(
            form.fields,
            vec![
                FormField::Text("q".into()),
                FormField::Choice {
                    name: "make".into(),
                    options: vec!["".into(), "subaru".into(), "Toyota".into()]
                },
                FormField::Choice {
                    name: "fuel".into(),
                    options: vec!["petrol".into(), "diesel".into()]
                },
                FormField::Fixed {
                    name: "kind".into(),
                    value: "used".into()
                },
            ]
        );
    }

    #[test]
    fn a_private_field_is_known_by_type_hint_or_whole_word() {
        assert!(is_private("x", "password", ""));
        assert!(is_private("x", "text", "cc-number"));
        assert!(is_private("contact_phone", "text", ""));
        assert!(!is_private("hotel", "text", ""));
        assert!(!is_private("scorecard_min", "text", ""));
        assert!(!is_private("q", "search", "off"));
    }

    #[test]
    fn json_ld_blog_posts_and_articles_read_as_items() {
        let page = data(
            r#"<html><head><script type="application/ld+json">
            {"@context":"https://schema.org","@type":"BlogPosting",
             "headline":"Announcing Model v3.0",
             "description":"Our new multimodal model is now available.",
             "datePublished":"2026-11-01",
             "url":"https://blog.example/v3"}
            </script></head><body></body></html>"#,
        );
        assert_eq!(page.items_from, Some(ItemSource::JsonLd));
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].title, "Announcing Model v3.0");
        assert_eq!(
            page.items[0].link.as_deref(),
            Some("https://blog.example/v3")
        );
        assert_eq!(page.items[0].year.as_deref(), Some("2026"));
        assert_eq!(
            page.items[0].snippet.as_deref(),
            Some("Our new multimodal model is now available.")
        );
    }

    #[test]
    fn html_articles_read_as_items() {
        let page = data(
            r#"<html><body>
            <main>
              <article class="post">
                <h2><a href="/posts/first">First Post</a></h2>
                <time datetime="2026-10-15">Oct 15, 2026</time>
                <p>Welcome to our new publication feed.</p>
              </article>
              <article class="post">
                <h2><a href="/posts/second">Second Post</a></h2>
                <time datetime="2026-11-02">Nov 02, 2026</time>
                <p>Updates on distributed training techniques.</p>
              </article>
            </main>
            </body></html>"#,
        );
        assert_eq!(page.items_from, Some(ItemSource::HtmlArticles));
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].title, "First Post");
        assert_eq!(
            page.items[0].link.as_deref(),
            Some("https://cars.example/posts/first")
        );
        assert_eq!(page.items[0].year.as_deref(), Some("2026"));
        assert_eq!(
            page.items[0].snippet.as_deref(),
            Some("Welcome to our new publication feed.")
        );
        assert_eq!(page.items[1].title, "Second Post");
    }
}
