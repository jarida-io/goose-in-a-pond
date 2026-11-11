//! Reading one product page for the cart: its price, currency, stock, title and picture. A chain
//! of readers, cheapest first: the page's schema.org JSON-LD Product and Offer, then its price
//! meta tags and microdata, and only when both find nothing, the model reading the page's text.
//! The page is fetched through the public-only client, and its text is never kept.

use std::collections::VecDeque;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use pond_core::models::domain::message::ChatMessage;
use pond_core::models::ports::provider::LlmProvider;
use pond_core::shared::domain::cart::{
    availability_of, currency_code, currency_in, price_in, ProductReading, ReadBy,
};
use pond_core::shared::ports::cart::{ModelUse, ProductReadError, ProductReader};
use pond_core::shared::services::cart_watch::{
    model_prompt, parse_model_answer, site_of, MODEL_PAGE_CHARS,
};
use pond_core::shared::services::report_images;
use reqwest::Url;
use serde_json::{Map, Value};

use crate::page_data::{ld_item, types_of, Item, ITEM_TYPES};
use crate::web_page::{fetch_product_page, FetchFailure, EXTRACT_DEADLINE};
use crate::web_search::{self, bounded_html};

/// The longest the model may take over one page.
const MODEL_DEADLINE: Duration = Duration::from_secs(45);
/// JSON values visited at most in one page's JSON-LD.
const MAX_LD_NODES: usize = 20_000;

/// The live chat model, swapped when the household changes it.
pub type SharedProvider = Arc<tokio::sync::RwLock<Option<Arc<dyn LlmProvider>>>>;

/// What a page's own markup says about the product it sells.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProductFacts {
    pub title: Option<String>,
    pub site: Option<String>,
    pub price: Option<f64>,
    pub currency: Option<String>,
    pub in_stock: Option<bool>,
    /// The product's picture, as an absolute address.
    pub image: Option<String>,
    pub read_by: Option<ReadBy>,
}

fn selector(css: &'static str) -> scraper::Selector {
    scraper::Selector::parse(css).expect("a fixed selector")
}

fn attr_or_text(element: scraper::ElementRef<'_>) -> Option<String> {
    element
        .value()
        .attr("content")
        .or_else(|| element.value().attr("href"))
        .map(str::to_string)
        .or_else(|| Some(element.text().collect::<String>()))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

fn first(document: &scraper::Html, selector: &scraper::Selector) -> Option<String> {
    document.select(selector).find_map(attr_or_text)
}

/// The product `document` sells, read from its markup alone.
pub fn product_facts(document: &scraper::Html, base: &Url) -> ProductFacts {
    static OG_TITLE: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"meta[property="og:title"], meta[name="twitter:title"]"#));
    static OG_SITE: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"meta[property="og:site_name"]"#));
    static OG_IMAGE: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"meta[property="og:image"], meta[property="og:image:url"], meta[name="twitter:image"]"#,
        )
    });
    static TITLE: LazyLock<scraper::Selector> = LazyLock::new(|| selector("title"));

    let mut facts = ProductFacts {
        site: first(document, &OG_SITE),
        ..Default::default()
    };
    let ld = ld_product(document, base);
    if let Some((item, in_stock)) = &ld {
        facts.title = Some(item.title.clone());
        facts.image = item.image.clone();
        facts.in_stock = *in_stock;
        if let Some(price) = item.price.as_deref().and_then(price_in) {
            facts.price = Some(price);
            facts.currency = item
                .currency
                .as_deref()
                .and_then(currency_code)
                .or_else(|| item.price.as_deref().and_then(currency_in));
            facts.read_by = Some(ReadBy::JsonLd);
        }
    }
    if facts.price.is_none() {
        let tags = price_tags(document);
        if tags.price.is_some() {
            facts.price = tags.price;
            facts.currency = tags.currency;
            facts.read_by = Some(ReadBy::PageTags);
        }
        facts.in_stock = facts.in_stock.or(tags.in_stock);
    }
    if facts.title.is_none() {
        facts.title = first(document, &OG_TITLE).or_else(|| first(document, &TITLE));
    }
    if facts.image.is_none() {
        facts.image = first(document, &OG_IMAGE);
    }
    facts.image = facts
        .image
        .and_then(|i| base.join(&i).ok())
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.to_string());
    facts.title = facts.title.map(|t| clip(&t));
    facts
}

fn clip(text: &str) -> String {
    let text = web_search::defang(text);
    let words: Vec<&str> = text.split_whitespace().collect();
    let joined = words.join(" ");
    joined.chars().take(200).collect()
}

/// The product a page's JSON-LD describes, with whether its offer says it is in stock. The
/// shallowest Product wins, so the page's own product beats the related ones it lists below it.
fn ld_product(document: &scraper::Html, base: &Url) -> Option<(Item, Option<bool>)> {
    static LD: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"script[type="application/ld+json"]"#));
    let mut found: Vec<(Item, Option<bool>)> = Vec::new();
    for script in document.select(&LD) {
        let text = script.text().collect::<String>();
        let Ok(value) = serde_json::from_str::<Value>(text.trim()) else {
            continue;
        };
        let mut queue: VecDeque<&Value> = VecDeque::from([&value]);
        let mut budget = MAX_LD_NODES;
        while let Some(value) = queue.pop_front() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            match value {
                Value::Array(values) => queue.extend(values),
                Value::Object(object) => {
                    let types = types_of(object);
                    let is_product = types
                        .iter()
                        .any(|t| ITEM_TYPES.iter().any(|i| i.eq_ignore_ascii_case(t)));
                    if is_product {
                        if let Some(item) = ld_item(object, base) {
                            found.push((item, availability(object)));
                        }
                    }
                    queue.extend(object.values());
                }
                _ => {}
            }
        }
    }
    let page = base.as_str().split(['?', '#']).next().unwrap_or_default();
    let priced =
        |(item, _): &&(Item, Option<bool>)| item.price.as_deref().and_then(price_in).is_some();
    let this_page = |(item, _): &&(Item, Option<bool>)| {
        item.link
            .as_deref()
            .is_some_and(|l| l.split(['?', '#']).next() == Some(page))
    };
    let pick = found
        .iter()
        .find(|f| priced(f) && this_page(f))
        .or_else(|| found.iter().find(priced))
        .or_else(|| found.first())?;
    Some(pick.clone())
}

fn availability(product: &Map<String, Value>) -> Option<bool> {
    let offers = match product.get("offers") {
        Some(Value::Array(offers)) => offers.first(),
        other => other,
    };
    offers
        .and_then(Value::as_object)
        .and_then(|o| o.get("availability"))
        .or_else(|| product.get("availability"))
        .and_then(Value::as_str)
        .and_then(availability_of)
}

#[derive(Debug, Default, PartialEq)]
struct PriceTags {
    price: Option<f64>,
    currency: Option<String>,
    in_stock: Option<bool>,
}

/// The price a page states in its meta tags (`product:price:amount`, `og:price:amount`, and
/// the `og:product:` spelling some shops use) or its `itemprop` microdata.
fn price_tags(document: &scraper::Html) -> PriceTags {
    static PRICE: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"meta[property="product:price:amount"], meta[property="og:price:amount"],
               meta[property="og:product:price:amount"], meta[name="product:price:amount"],
               [itemprop="price"]"#,
        )
    });
    static CURRENCY: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"meta[property="product:price:currency"], meta[property="og:price:currency"],
               meta[property="og:product:price:currency"], meta[name="product:price:currency"],
               [itemprop="priceCurrency"]"#,
        )
    });
    static STOCK: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"meta[property="product:availability"], meta[property="og:availability"],
               meta[property="og:product:availability"], [itemprop="availability"]"#,
        )
    });
    let raw_price = document
        .select(&PRICE)
        .find_map(|e| attr_or_text(e).filter(|t| price_in(t).is_some()));
    PriceTags {
        price: raw_price.as_deref().and_then(price_in),
        currency: first(document, &CURRENCY)
            .and_then(|c| currency_code(&c))
            .or_else(|| raw_price.as_deref().and_then(currency_in)),
        in_stock: document
            .select(&STOCK)
            .filter_map(attr_or_text)
            .find_map(|t| availability_of(&t)),
    }
}

/// Lines of text a shop page shows before its product's title, kept for context.
const LINES_BEFORE_TITLE: usize = 8;

/// A shop page's title and its visible text, from just above the product's heading. Not a
/// reader's text: that drops a price as page furniture, since it is a few characters in a box.
pub fn shop_text(document: &scraper::Html) -> (String, String) {
    static BODY: LazyLock<scraper::Selector> = LazyLock::new(|| selector("body"));
    static H1: LazyLock<scraper::Selector> = LazyLock::new(|| selector("h1"));
    static TITLE: LazyLock<scraper::Selector> = LazyLock::new(|| selector("title"));
    const HIDDEN: &[&str] = &[
        "script", "style", "noscript", "svg", "template", "iframe", "head",
    ];

    let mut lines: Vec<String> = Vec::new();
    if let Some(body) = document.select(&BODY).next() {
        for node in body.descendants() {
            let Some(text) = node.value().as_text() else {
                continue;
            };
            let hidden = node.ancestors().any(|a| {
                a.value()
                    .as_element()
                    .is_some_and(|e| HIDDEN.contains(&e.name()))
            });
            let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
            if !hidden && !line.is_empty() && lines.last() != Some(&line) {
                lines.push(line);
            }
        }
    }
    let heading = first(document, &H1).map(|h| h.split_whitespace().collect::<Vec<_>>().join(" "));
    let start = heading
        .as_deref()
        .and_then(|h| lines.iter().position(|l| l == h))
        .map_or(0, |at| at.saturating_sub(LINES_BEFORE_TITLE));
    let mut text = String::new();
    for line in &lines[start..] {
        if text.len() + line.len() > MODEL_PAGE_CHARS {
            break;
        }
        text.push_str(line);
        text.push('\n');
    }
    let title = first(document, &TITLE).or(heading).unwrap_or_default();
    (clip(&title), web_search::defang(&text))
}

/// Reads product pages for the cart, asking the live chat model only when a page's markup names
/// no price.
pub struct ProductPageReader {
    llm: SharedProvider,
}

impl ProductPageReader {
    pub fn new(llm: SharedProvider) -> Self {
        Self { llm }
    }
}

/// What parsing a fetched page gave: its facts, and its title and text when the model may need
/// them.
struct Parsed {
    facts: ProductFacts,
    text: Option<(String, String)>,
}

#[async_trait]
impl ProductReader for ProductPageReader {
    async fn read(&self, url: &str, model: ModelUse) -> Result<ProductReading, ProductReadError> {
        let (address, html) = fetch_product_page(url).await.map_err(|f| match f {
            FetchFailure::Status(401 | 403) => ProductReadError::Refused,
            FetchFailure::Status(429) => ProductReadError::RateLimited,
            FetchFailure::Status(404 | 410) => ProductReadError::Gone,
            FetchFailure::NotPublic => ProductReadError::NotPublic,
            other => ProductReadError::Other(format!("{other:?}")),
        })?;
        let parse_address = address.clone();
        let parsing = tokio::task::spawn_blocking(move || {
            let base = Url::parse(&parse_address).ok()?;
            let document = bounded_html::parse_within(
                &html,
                web_search::MAX_NODES_PER_PAGE,
                web_search::PARSE_TIME_PER_PAGE,
            );
            let facts = product_facts(&document, &base);
            let text =
                (facts.price.is_none() && model == ModelUse::Allowed).then(|| shop_text(&document));
            Some(Parsed { facts, text })
        });
        let parsed = match tokio::time::timeout(EXTRACT_DEADLINE, parsing).await {
            Ok(Ok(Some(parsed))) => parsed,
            _ => {
                return Err(ProductReadError::Other(
                    "the page was too large to read in time".into(),
                ))
            }
        };
        let mut facts = parsed.facts;
        if let Some((title, text)) = parsed.text {
            if let Some(read) = self.ask_model(&address, &title, &text).await {
                facts.price = read.price;
                facts.currency = read.currency.or(facts.currency);
                facts.in_stock = read.in_stock.or(facts.in_stock);
                facts.title = read.title.or(facts.title);
                facts.read_by = read.price.map(|_| ReadBy::Model);
            }
        }
        let picture_key = match &facts.image {
            Some(image) => picture_for(&address, image).await,
            None => None,
        };
        Ok(ProductReading {
            title: facts.title,
            site: facts.site.or_else(|| Some(site_of(&address))),
            price: facts.price,
            currency: facts.currency,
            in_stock: facts.in_stock,
            picture_key,
            read_by: facts.read_by,
        })
    }
}

impl ProductPageReader {
    async fn ask_model(&self, address: &str, title: &str, text: &str) -> Option<ProductReading> {
        if text.trim().is_empty() {
            return None;
        }
        let provider = self.llm.read().await.clone()?;
        let (system, user) = model_prompt(address, title, text);
        let started = std::time::Instant::now();
        let answer = tokio::time::timeout(
            MODEL_DEADLINE,
            provider.complete(system, vec![ChatMessage::user(user)]),
        )
        .await;
        let reading = match answer {
            Ok(Ok(message)) => parse_model_answer(after_thinking(&message.content)),
            Ok(Err(e)) => {
                tracing::warn!(target: "giap::trace", kind = "cart_model_read_failed", error = %e);
                None
            }
            Err(_) => {
                tracing::warn!(target: "giap::trace", kind = "cart_model_read_timed_out");
                None
            }
        };
        tracing::info!(
            target: "giap::trace",
            kind = "cart_model_read",
            ms = started.elapsed().as_millis() as u64,
            priced = reading.as_ref().is_some_and(|r| r.price.is_some()),
        );
        reading
    }
}

/// The answer after any reasoning a thinking model printed before it.
fn after_thinking(answer: &str) -> &str {
    answer
        .rsplit_once("</think>")
        .map(|(_, after)| after)
        .unwrap_or(answer)
}

/// The kept picture for `page`, fetched once and served by the Pond.
async fn picture_for(page: &str, image: &str) -> Option<String> {
    let images = report_images::installed()?;
    report_images::note_preview(page, image);
    images
        .for_pages(&[page.to_string()], 1)
        .await
        .into_iter()
        .next()
        .map(|i| i.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JUMIA: &str = include_str!("../fixtures/cart/jumia_kettle.html");
    const KILIMALL: &str = include_str!("../fixtures/cart/kilimall_kettle.html");
    const WIKIPEDIA: &str = include_str!("../fixtures/cart/wikipedia_fielder.html");

    fn facts(html: &str, url: &str) -> ProductFacts {
        let document = scraper::Html::parse_document(html);
        product_facts(&document, &Url::parse(url).unwrap())
    }

    fn without_json_ld(html: &str) -> String {
        let mut out = html.to_string();
        while let Some(start) = out.find("<script type=\"application/ld+json\"") {
            let end = out[start..].find("</script>").unwrap() + start + "</script>".len();
            out.replace_range(start..end, "");
        }
        out
    }

    #[test]
    fn jumia_names_its_product_in_json_ld() {
        let f = facts(
            JUMIA,
            "https://www.jumia.co.ke/ramtons-cordless-ergonomic-plastic-electric-water-kettle-heater-jug-177871891.html",
        );
        assert_eq!(f.read_by, Some(ReadBy::JsonLd));
        assert_eq!(f.price, Some(1799.0));
        assert_eq!(f.currency.as_deref(), Some("KES"));
        assert_eq!(f.in_stock, Some(true));
        assert_eq!(
            f.title.as_deref(),
            Some("Ramtons Cordless Ergonomic Plastic Electric Water Kettle Heater Jug")
        );
        assert_eq!(f.site.as_deref(), Some("Jumia Kenya"));
        assert!(f.image.unwrap().starts_with("https://ke.jumia.is/"));
    }

    #[test]
    fn kilimall_names_its_product_as_the_main_entity_of_an_item_page() {
        let f = facts(
            KILIMALL,
            "https://www.kilimall.co.ke/listing/1000418363-SONAR-2L-1500W-Cordless-Electric-Kettle-SRP2",
        );
        assert_eq!(f.read_by, Some(ReadBy::JsonLd));
        assert_eq!(
            f.price,
            Some(449.0),
            "the sale price the page shows, not its stale tags"
        );
        assert_eq!(f.currency.as_deref(), Some("KES"));
        assert_eq!(f.in_stock, Some(true));
        assert!(f
            .title
            .unwrap()
            .starts_with("Sonar 2L 1500W Electric Kettle"));
    }

    #[test]
    fn without_json_ld_the_price_tags_still_give_a_price() {
        let f = facts(
            &without_json_ld(KILIMALL),
            "https://www.kilimall.co.ke/listing/1000418363",
        );
        assert_eq!(f.read_by, Some(ReadBy::PageTags));
        assert_eq!(f.price, Some(499.0));
        assert_eq!(f.currency.as_deref(), Some("KES"));
        assert!(f.image.unwrap().starts_with("https://img.kilimall.com/"));
    }

    #[test]
    fn microdata_and_meta_tags_are_read() {
        let html = r#"<html><head>
            <meta property="product:price:amount" content="KSh 1,199,000">
            <meta property="og:availability" content="out of stock">
            <title>2014 Toyota Fielder | A Kenyan dealer</title></head>
            <body><div itemscope itemtype="https://schema.org/Product">
            <h1 itemprop="name">2014 Toyota Fielder</h1></div></body></html>"#;
        let f = facts(html, "https://dealer.example/fielder");
        assert_eq!(f.price, Some(1_199_000.0));
        assert_eq!(f.currency.as_deref(), Some("KES"));
        assert_eq!(f.in_stock, Some(false));
        assert_eq!(
            f.title.as_deref(),
            Some("2014 Toyota Fielder | A Kenyan dealer")
        );

        let microdata = r#"<html><body><div itemscope itemtype="https://schema.org/Product">
            <span itemprop="name">Mini Cooper S</span>
            <div itemprop="offers" itemscope itemtype="https://schema.org/Offer">
              <span itemprop="priceCurrency" content="USD">$</span>
              <span itemprop="price" content="19999.00">19,999</span>
              <link itemprop="availability" href="https://schema.org/InStock">
            </div></div></body></html>"#;
        let f = facts(microdata, "https://shop.example/mini");
        assert_eq!(f.read_by, Some(ReadBy::PageTags));
        assert_eq!(f.price, Some(19_999.0));
        assert_eq!(f.currency.as_deref(), Some("USD"));
        assert_eq!(f.in_stock, Some(true));
    }

    #[test]
    fn a_page_that_sells_nothing_has_no_price() {
        let f = facts(
            WIKIPEDIA,
            "https://en.wikipedia.org/wiki/Toyota_Corolla_Fielder",
        );
        assert_eq!(f.price, None);
        assert_eq!(f.read_by, None);
        assert_eq!(f.in_stock, None);
        assert!(f.title.is_some());
    }

    #[test]
    fn the_models_text_keeps_the_price_a_reader_drops() {
        let document = scraper::Html::parse_document(&without_json_ld(KILIMALL));
        let (title, text) = shop_text(&document);
        assert!(title.starts_with("Best price for Sonar 2L"), "{title}");
        assert!(text.contains("KSh 449"), "{text}");
        assert!(text.starts_with("Download") || text.len() <= MODEL_PAGE_CHARS + 1);
        let wiki = scraper::Html::parse_document(WIKIPEDIA);
        assert!(shop_text(&wiki).1.len() <= MODEL_PAGE_CHARS + 1);
    }

    #[test]
    fn the_answer_after_thinking_is_read() {
        assert_eq!(
            after_thinking("<think>{maybe}</think>{\"amount\": 1}"),
            "{\"amount\": 1}"
        );
        assert_eq!(after_thinking("{\"amount\": 1}"), "{\"amount\": 1}");
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// The model's reading of real Kenyan shop pages, as their text alone, and of a page that
    /// sells nothing. Prints each answer for a person to judge.
    #[tokio::test]
    #[ignore = "needs the network and Ollama with gemma4:latest on 127.0.0.1:11434"]
    async fn gemma_reads_prices_from_page_text() {
        let pages = [
            "https://www.jumia.co.ke/ramtons-cordless-ergonomic-plastic-electric-water-kettle-heater-jug-177871891.html",
            "https://www.kilimall.co.ke/listing/1000418363-SONAR-2L-1500W-Cordless-Electric-Kettle-SRP2",
            "https://en.wikipedia.org/wiki/Toyota_Corolla_Fielder",
        ];
        let client = reqwest::Client::new();
        for page in pages {
            let (address, html) = fetch_product_page(page).await.expect("the page reads");
            let (title, text) = {
                let document = scraper::Html::parse_document(&html);
                shop_text(&document)
            };
            let (system, user) = model_prompt(&address, &title, &text);
            let body = serde_json::json!({
                "model": "gemma4:latest",
                "stream": false,
                "options": {"temperature": 0.7},
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user},
                ],
            });
            let started = std::time::Instant::now();
            let answer: Value = client
                .post("http://127.0.0.1:11434/api/chat")
                .json(&body)
                .send()
                .await
                .expect("Ollama answers")
                .json()
                .await
                .expect("Ollama's JSON");
            let content = answer["message"]["content"].as_str().unwrap_or_default();
            println!(
                "== {page}\n   text chars: {}, {} ms\n   answer: {content}\n   read: {:?}",
                text.chars().count(),
                started.elapsed().as_millis(),
                parse_model_answer(after_thinking(content))
            );
        }
    }
}
