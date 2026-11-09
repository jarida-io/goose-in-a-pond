//! `read_web_page` for `giap-knowledge`: one page fetched through the public-only client, its
//! content turned into Markdown lines and broken into parts. With a question it quotes the lines
//! that answer it, grouped by part; with a part number, that part whole. Beside them it lists the
//! items the page's structured data carries, its search forms, and the links the question names.
//! Only an address the person wrote, a task's goal names, a search found, or a link or form on a
//! page read from those, on its own site and within two clicks, is fetched (`page_addresses`).
//! A POST form is never fetched here: it waits for the member's yes on their phone (`web_form`).
//! Each read goes into the session's source ledger, which also holds a conversation turn to
//! three pages.

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use pond_core::mcp::domain::tool_group::BRAVE_SEARCH_API_KEY;
use pond_core::mcp::domain::transient_result::{transient_meta, web_page_stub};
use pond_core::mcp::services::page_addresses::{self, MAX_LINK_DEPTH};
use pond_core::mcp::services::source_ledger::{self, MAX_CHAT_TURN_READS};
use pond_core::shared::services::passages;
use rmcp::{
    handler::server::wrapper::Parameters,
    model::{CallToolResult, ContentBlock, ErrorData, Meta},
    service::RequestContext,
    tool, tool_router, RoleServer,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::format::{format_api_error, format_dead_end};
use crate::knowledge::KnowledgeMcpServer;
use crate::page_data::{self, PageData};
use crate::web_search::{self, bounded_html, Sink};

/// The tool this module serves, as the egress record attributes it.
const TOOL_NAME: &str = "read_web_page";

const SEARCH_WEB_TOOL: &str = "giap-knowledge__search_web";

/// Longer than a search gives each page: this read is the whole of the call.
pub(crate) const PAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// Parsing one page off the async workers, at most.
pub(crate) const EXTRACT_DEADLINE: Duration = Duration::from_secs(4);

/// A part, about 800 tokens: a helper can read two beside its brief.
const PART_BYTES: usize = 3_200;

/// A question's quote at most, as a search's copy: the next turn prefills all of it.
const ANSWER_BYTES: usize = 1_800;

/// One line at most; a longer paragraph is split at sentences.
const LINE_BYTES: usize = 1_000;

/// Lines kept from one page: past it the page is a catalogue, not something to read.
const MAX_LINES: usize = 1_500;

const MAX_HEADING_BYTES: usize = 80;
const MAX_TITLE_BYTES: usize = 120;

/// Parts the outline names; a longer page says how many more there are.
const OUTLINE_PARTS: usize = 12;
const MAX_PART_NAME_BYTES: usize = 40;

/// Links of one page noted as readable, beside its items' own: the address book is shared.
const NOTED_LINKS: usize = 40;

/// Items a reply lists at most, and their bytes: about 400 tokens.
const MAX_SHOWN_ITEMS: usize = 15;
const ITEMS_BYTES: usize = 1_600;

/// Links a reply offers at most, those the question names.
const MAX_SHOWN_LINKS: usize = 6;

/// One form's line at most: a long option list is cut.
const FORM_LINE_BYTES: usize = 600;
const SHOWN_OPTIONS: usize = 12;

/// Pages kept parsed, so asking for a part after a question costs no second fetch.
const CACHED_PAGES: usize = 8;
const CACHE_LIFETIME: Duration = Duration::from_secs(10 * 60);

pub(crate) const ACCEPT: &str = "text/html,application/xhtml+xml,text/plain;q=0.8";

const URL_ALIASES: &[&str] = &[
    "address", "link", "page", "href", "source", "site", "website", "uri",
];
const QUESTION_ALIASES: &[&str] = &["query", "q", "topic", "what", "about", "find"];
const PART_ALIASES: &[&str] = &["section", "part_number", "page_part", "number"];

// ── Parameters ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ReadWebPageParams {
    /// The link the person gave, a source's domain from search_web, or a link or filled-in
    /// search form a page read with this tool listed.
    #[schemars(with = "String")]
    pub url: Option<String>,
    /// What to find on the page, in the person's own words.
    #[schemars(with = "String")]
    pub question: Option<String>,
    /// To read one part whole: its number, as this tool listed it.
    // A `Value`, so "2" and 2 both read; as `Option<u32>` a string fails the whole call in rmcp.
    #[schemars(with = "Option<u32>")]
    pub part: Option<Value>,
    /// Catch-all for the names a model may use instead. Not part of the advertised schema.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, Value>,
}

impl ReadWebPageParams {
    fn text(&self, named: Option<&String>, aliases: &[&str]) -> String {
        named
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                aliases
                    .iter()
                    .filter_map(|k| self.extra.get(*k).and_then(Value::as_str))
                    .map(str::trim)
                    .find(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .unwrap_or_default()
    }

    fn address(&self) -> String {
        self.text(self.url.as_ref(), URL_ALIASES)
    }

    fn asked(&self) -> String {
        self.text(self.question.as_ref(), QUESTION_ALIASES)
    }

    /// The part asked for, from a number or the digits in a string such as "part 2".
    fn part(&self) -> Option<usize> {
        let value = self
            .part
            .as_ref()
            .or_else(|| PART_ALIASES.iter().find_map(|k| self.extra.get(*k)))?;
        match value {
            Value::Number(n) => n.as_u64().map(|n| n as usize),
            Value::String(s) => {
                let digits: String = s.chars().filter(char::is_ascii_digit).collect();
                digits.parse().ok()
            }
            _ => None,
        }
        .filter(|&n| n > 0)
    }
}

// ── The tool ───────────────────────────────────────────────────────────────

#[tool_router(router = web_page_tool_router, vis = "pub")]
impl KnowledgeMcpServer {
    #[tool(description = "\
Read a web page: a link the person gave, a source search_web found (by its domain), or a link \
or search form a page read here listed. Returns the lines that answer the question, or one part \
whole, and what the page lists.")]
    async fn read_web_page(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<ReadWebPageParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool(TOOL_NAME);
        let params = params.0;
        let asked_address = params.address();
        if asked_address.is_empty() {
            return Ok(text_result(
                "I need the page's address. Retry this tool with 'url' set to the link the \
                 person gave, or to a source's domain as search_web showed it.",
            ));
        }
        let session = crate::session_from_meta(&ctx.meta);
        let ledger = source_ledger::global();
        let turn_reads = session.as_deref().and_then(|s| ledger.turn_reads(s));
        if turn_reads.is_some_and(|n| n >= MAX_CHAT_TURN_READS) {
            tracing::info!(target: "giap::trace", kind = "web_page_turn_cap", reads = MAX_CHAT_TURN_READS);
            return Ok(text_result(&turn_cap_reached()));
        }
        let book = page_addresses::global();
        let mut answer_note = None;
        let found = match book.resolve_with_depth(&asked_address) {
            Some(found) => Some(found),
            None => match book.post_request(&asked_address) {
                Some(request) => match crate::web_form::ask_to_send(&ctx.meta, request).await {
                    crate::web_form::FormAnswer::Said(text) => return Ok(text_result(&text)),
                    crate::web_form::FormAnswer::Read {
                        address,
                        depth,
                        note,
                    } => {
                        answer_note = Some(note);
                        Some((address, depth))
                    }
                },
                None => None,
            },
        };
        let (address, depth) = match found {
            Some(found) => found,
            None => {
                // The host only: the rest of an address nobody gave may be the data it carries.
                tracing::info!(
                    target: "giap::trace",
                    kind = "web_page_unknown",
                    host = %web_search::domain_of(&asked_address),
                );
                let searchable = crate::secrets::secret(BRAVE_SEARCH_API_KEY)
                    .await
                    .is_some_and(|k| !k.trim().is_empty());
                return Ok(text_result(&unknown_address(searchable)));
            }
        };
        let started = Instant::now();
        let (page, cached) = match cached_page(&address) {
            Some(page) => (page, true),
            None => match read_page(&address, depth).await {
                Ok(page) => {
                    let page = Arc::new(page);
                    cache_page(&address, page.clone());
                    (page, false)
                }
                Err(why) => return Ok(text_result(&why)),
            },
        };
        let request = Request::from(&params);
        let mut reply = page.reply_as(&request, turn_reads.map(|n| n + 1));
        if let Some(note) = answer_note {
            reply.text = format!("{note}\n{}", reply.text);
        }
        let source = session.as_deref().map(|s| {
            let site = web_search::domain_of(&page.address);
            ledger.record(s, &page.address, &page.title, &site, reply.used.clone())
        });
        tracing::info!(
            target: "giap::trace",
            kind = "web_page",
            host = %web_search::domain_of(&page.address),
            lines = page.lines.len(),
            parts = page.parts.len(),
            items = page.data.items.len(),
            items_from = page.data.items_from.map(|f| f.as_str()).unwrap_or("none"),
            links = page.data.links.len(),
            forms = page.data.forms.len(),
            depth,
            source = source.unwrap_or(0),
            turn_read = turn_reads.map_or(0, |n| n + 1),
            mode = reply.mode,
            bytes = reply.text.len(),
            cached,
            ms = started.elapsed().as_millis() as u64,
            "web page read"
        );
        let mut result = CallToolResult::success(vec![ContentBlock::text(reply.text)]);
        result.meta = Some(Meta(transient_meta(&web_page_stub(&address))));
        Ok(result)
    }
}

fn text_result(text: &str) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text.to_string())])
}

/// What a conversation turn hears after its last read.
fn last_read_note() -> String {
    format!(
        "That was this turn's last page ({MAX_CHAT_TURN_READS} is the most a conversation turn \
         reads). Answer now from what you have. If there is clearly more to find, offer to keep \
         exploring as a long task."
    )
}

/// What a conversation turn hears when it asks for a page past its last.
fn turn_cap_reached() -> String {
    format_dead_end(
        "reading another page",
        &format!(
            "This turn has read {MAX_CHAT_TURN_READS} pages, the most a conversation turn reads. \
             Answer from what you have, and if there is clearly more to find, offer to keep \
             exploring as a long task."
        ),
    )
}

/// An item as one line: its title, then what it states of price, distance, year and link.
fn item_row(item: &page_data::Item) -> String {
    let mut fields = vec![item.title.clone()];
    if let Some(price) = &item.price {
        fields.push(match &item.currency {
            Some(currency) if !price.contains(currency.as_str()) => format!("{currency} {price}"),
            _ => price.clone(),
        });
    }
    if let Some(mileage) = &item.mileage {
        fields.push(mileage.clone());
    }
    if let Some(year) = item
        .year
        .as_ref()
        .filter(|y| !item.title.contains(y.as_str()))
    {
        fields.push(year.clone());
    }
    if let Some(link) = &item.link {
        fields.push(link.clone());
    }
    fields.join(" | ")
}

/// A form field as the reader fills it.
fn field_words(field: &page_addresses::FormField) -> String {
    use page_addresses::FormField;
    match field {
        FormField::Text(name) => format!("{name} (text)"),
        FormField::Choice { name, options } => {
            let shown: Vec<&str> = options
                .iter()
                .map(String::as_str)
                .filter(|o| !o.is_empty())
                .take(SHOWN_OPTIONS)
                .collect();
            let more = options.len().saturating_sub(shown.len());
            let tail = if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            };
            format!("{name} (one of: {}{tail})", shown.join(", "))
        }
        FormField::Fixed { name, value } => format!("{name} (keep as {value})"),
    }
}

fn unknown_address(searchable: bool) -> String {
    let why = if searchable {
        format!(
            "I read only a page whose address the person gave or a search found, or a link or \
             search form on a page read from those, on that page's own site. Find it with \
             {SEARCH_WEB_TOOL}, then read one of its sources by its domain."
        )
    } else {
        "I read only a page whose address the person gave, and this is not one.".to_string()
    };
    format_dead_end("reading that page", &why)
}

// ── Fetching ───────────────────────────────────────────────────────────────

enum Body {
    Html(String),
    Text(String),
}

/// The page at `address`, `depth` clicks from one given outright, read and broken into parts;
/// the model's text for why not.
async fn read_page(address: &str, depth: u8) -> Result<Page, String> {
    let Some(client) = web_search::page_client() else {
        return Err(format_api_error(
            "Reading the page",
            "the page client could not be built",
        ));
    };
    let (final_address, body) = fetch(client, address).await?;
    let fallback_title = web_search::domain_of(&final_address);
    let parsing = tokio::task::spawn_blocking(move || {
        let (title, lines, data) = match body {
            Body::Html(html) => parse_html(&html, &final_address),
            Body::Text(text) => (String::new(), lines_from_text(&text), PageData::default()),
        };
        Page::new(final_address, title, fallback_title, lines)
            .with_data(data)
            .at_depth(depth)
    });
    let page = match tokio::time::timeout(EXTRACT_DEADLINE, parsing).await {
        Ok(Ok(page)) => page,
        _ => {
            return Err(format_dead_end(
                "reading that page",
                "The page was too large to read in time.",
            ))
        }
    };
    if page.lines.is_empty() && page.data.items.is_empty() {
        return Err(format_dead_end(
            "reading that page",
            "The page has no text the pond can read: it may be built in the browser by \
             scripts, or need a sign-in.",
        ));
    }
    // A redirect's end came from the site, not the model, so it may be read again by name.
    let book = page_addresses::global();
    book.note_read(&page.address, &page.title, depth);
    book.note_links_from(&page.address, depth, &page.followable());
    book.note_forms_from(&page.address, depth, page.data.forms.clone());
    if let Some(image) = &page.data.preview_image {
        pond_core::shared::services::report_images::note_preview(&page.address, image);
    }
    Ok(page)
}

/// Keeps a sent form's answer as a page read at `depth`, so the conversation reads it from the
/// cache: a POST's answer may not come back to a plain read of its address. Returns its title.
pub(crate) async fn keep_answer(address: String, html: String, depth: u8) -> String {
    let fallback_title = web_search::domain_of(&address);
    let parsing = tokio::task::spawn_blocking(move || {
        let (title, lines, data) = parse_html(&html, &address);
        Page::new(address, title, fallback_title, lines)
            .with_data(data)
            .at_depth(depth)
    });
    let Ok(Ok(page)) = tokio::time::timeout(EXTRACT_DEADLINE, parsing).await else {
        return String::new();
    };
    let book = page_addresses::global();
    book.note_read(&page.address, &page.title, depth);
    book.note_links_from(&page.address, depth, &page.followable());
    book.note_forms_from(&page.address, depth, page.data.forms.clone());
    let title = page.title.clone();
    let address = page.address.clone();
    cache_page(&address, Arc::new(page));
    title
}

/// The forms of a page's HTML, as a read would list them.
pub(crate) async fn forms_on(address: String, html: String) -> Vec<page_addresses::SearchForm> {
    let parsing = tokio::task::spawn_blocking(move || {
        let Ok(base) = reqwest::Url::parse(&address) else {
            return Vec::new();
        };
        let document = bounded_html::parse_within(
            &html,
            web_search::MAX_NODES_PER_PAGE,
            web_search::PARSE_TIME_PER_PAGE,
        );
        page_data::forms_of(&document, &base)
    });
    match tokio::time::timeout(EXTRACT_DEADLINE, parsing).await {
        Ok(Ok(forms)) => forms,
        _ => Vec::new(),
    }
}

/// Why a page could not be fetched, kept apart so a watch can back off on a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchFailure {
    NotAnAddress,
    NotPublic,
    Unreachable(String),
    Status(u16),
    StoppedArriving,
    NotAPage(String),
}

impl FetchFailure {
    /// The failure as the model reads it.
    fn text(&self) -> String {
        let what = "reading that page";
        match self {
            FetchFailure::NotAnAddress => format_dead_end(what, "That is not a web address."),
            FetchFailure::NotPublic => format_dead_end(
                what,
                "That address is not on the public web, and the pond reads only public pages.",
            ),
            FetchFailure::Unreachable(why) => format_api_error("Reading the page", why),
            FetchFailure::Status(code) => {
                let why = match code {
                    404 | 410 => "There is no page at that address.".to_string(),
                    401 | 403 => "The site would not let the pond read the page.".to_string(),
                    429 => "The site asked the pond to wait before reading more.".to_string(),
                    code => format!("The site answered HTTP {code}."),
                };
                format_dead_end(what, &why)
            }
            FetchFailure::StoppedArriving => {
                format_api_error("Reading the page", "the page stopped arriving")
            }
            FetchFailure::NotAPage(mime) => format_dead_end(
                what,
                &format!("That address is a file ({mime}), not a web page."),
            ),
        }
    }
}

/// A watched page as a check reads it: where it ended, its title and its items.
#[derive(Debug, Clone)]
pub struct WatchedPage {
    pub address: String,
    pub title: String,
    pub items: Vec<page_data::Item>,
}

/// Reads a watched page fresh, past the cache, for its items. The address was resolved when the
/// member set the watch up, so the address book is not asked again.
pub async fn read_watched(address: &str) -> Result<WatchedPage, FetchFailure> {
    let client = web_search::page_client()
        .ok_or_else(|| FetchFailure::Unreachable("the page client could not be built".into()))?;
    let (final_address, body) = fetch_page(client, address).await?;
    let parsing = tokio::task::spawn_blocking(move || {
        let (title, _, data) = match body {
            Body::Html(html) => parse_html(&html, &final_address),
            Body::Text(_) => (String::new(), Vec::new(), PageData::default()),
        };
        WatchedPage {
            address: final_address,
            title,
            items: data.items,
        }
    });
    match tokio::time::timeout(EXTRACT_DEADLINE, parsing).await {
        Ok(Ok(page)) => Ok(page),
        _ => Err(FetchFailure::Unreachable(
            "the page was too large to read in time".into(),
        )),
    }
}

/// A product page fresh, past the cache, for the cart: where it ended and its HTML (or its text,
/// for a plain-text page).
pub(crate) async fn fetch_product_page(address: &str) -> Result<(String, String), FetchFailure> {
    let client = web_search::page_client()
        .ok_or_else(|| FetchFailure::Unreachable("the page client could not be built".into()))?;
    let (final_address, body) = fetch_page(client, address).await?;
    Ok(match body {
        Body::Html(html) | Body::Text(html) => (final_address, html),
    })
}

async fn fetch(client: &reqwest::Client, address: &str) -> Result<(String, Body), String> {
    fetch_page(client, address).await.map_err(|f| f.text())
}

async fn fetch_page(
    client: &reqwest::Client,
    address: &str,
) -> Result<(String, Body), FetchFailure> {
    let parsed = reqwest::Url::parse(address).map_err(|_| FetchFailure::NotAnAddress)?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    if let Err(refused) = pond_net::public_only::check_target(&parsed) {
        tracing::info!(target: "giap::trace", kind = "web_page_refused", host = %host, %refused);
        return Err(FetchFailure::NotPublic);
    }
    let response = crate::http::traced_get_with(client, address, |b| {
        b.timeout(PAGE_TIMEOUT)
            .header(reqwest::header::ACCEPT, ACCEPT)
    })
    .await
    .map_err(|e| {
        tracing::info!(target: "giap::trace", kind = "web_page_unread", host = %host, error = %e);
        FetchFailure::Unreachable(web_search::describe_failure(&e))
    })?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchFailure::Status(status.as_u16()));
    }
    let final_address = response.url().to_string();
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| ct.split(';').next())
        .map(|m| m.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let plain = match mime.as_str() {
        "text/html" | "application/xhtml+xml" | "" => false,
        "text/plain" | "text/markdown" => true,
        other => return Err(FetchFailure::NotAPage(other.to_string())),
    };
    let body = web_search::read_capped(response, web_search::PAGE_BYTE_CAP)
        .await
        .ok_or(FetchFailure::StoppedArriving)?;
    let body = if plain {
        Body::Text(body)
    } else {
        Body::Html(body)
    };
    Ok((final_address, body))
}

// ── The page cache ─────────────────────────────────────────────────────────

type Cached = (String, Instant, Arc<Page>);

static CACHE: LazyLock<Mutex<Vec<Cached>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn cached_page(address: &str) -> Option<Arc<Page>> {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(_, at, _)| at.elapsed() < CACHE_LIFETIME);
    cache
        .iter()
        .find(|(a, _, _)| a == address)
        .map(|(_, _, page)| page.clone())
}

fn cache_page(address: &str, page: Arc<Page>) {
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|(a, _, _)| a != address);
    cache.push((address.to_string(), Instant::now(), page));
    let excess = cache.len().saturating_sub(CACHED_PAGES);
    cache.drain(..excess);
}

// ── Lines ──────────────────────────────────────────────────────────────────

/// One line of the page as Markdown, with the heading it sits under.
#[derive(Debug, Clone, PartialEq)]
struct Line {
    text: String,
    section: Option<String>,
    heading: bool,
}

/// Words a line needs to be quoted for a question. A table row is exempt: its label names its value.
const MIN_QUOTED_WORDS: usize = 3;

impl Line {
    /// Whether the line may answer a question alone. A heading is printed above the lines it
    /// heads, and BM25 ranks a one-word item such as "- Mombasa" above whole sentences.
    fn is_quotable(&self) -> bool {
        let words = self
            .text
            .trim_start_matches(['-', '>', ' '])
            .split_whitespace()
            .count();
        !self.heading && (self.text.contains(" | ") || words >= MIN_QUOTED_WORDS)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Text,
    Heading(usize),
    Item,
    Row,
    Quote,
}

/// The walk's sink for a page read: every content block as a Markdown line, nothing dropped for
/// being short, since a price or an hour is a short line.
#[derive(Default)]
struct Lines {
    lines: Vec<Line>,
    seen: HashSet<String>,
    open: Vec<Open>,
    current: String,
    cells: usize,
    section: Option<String>,
}

impl Sink for Lines {
    fn full(&self) -> bool {
        self.lines.len() >= MAX_LINES
    }

    fn open(&mut self, name: &str) {
        if self.is_cell_of_open_row(name) {
            if self.cells > 0 {
                self.current.push_str(" | ");
            }
            self.cells += 1;
            return;
        }
        if web_search::BLOCKS.contains(&name) {
            self.flush();
            let open = match name {
                "h1" => Open::Heading(1),
                "h2" => Open::Heading(2),
                "h3" => Open::Heading(3),
                "h4" => Open::Heading(4),
                "li" => Open::Item,
                "blockquote" => Open::Quote,
                n if n == web_search::ROW => Open::Row,
                _ => Open::Text,
            };
            if open == Open::Row {
                self.cells = 0;
            }
            self.open.push(open);
        } else if !self.open.is_empty() && !web_search::INLINE.contains(&name) {
            self.current.push(' ');
        }
    }

    fn close(&mut self, name: &str) {
        if self.is_cell_of_open_row(name) {
            return;
        }
        if web_search::BLOCKS.contains(&name) {
            self.flush();
            self.open.pop();
        } else if !self.open.is_empty() && !web_search::INLINE.contains(&name) {
            self.current.push(' ');
        }
    }

    fn text(&mut self, text: &str) {
        if !self.open.is_empty() {
            self.current.push_str(text);
        }
    }
}

impl Lines {
    fn is_cell_of_open_row(&self, name: &str) -> bool {
        web_search::CELLS.contains(&name) && self.open.last() == Some(&Open::Row)
    }

    /// What the innermost block reads as: a paragraph inside a list item is that item.
    fn kind(&self) -> Open {
        match self.open.last().copied().unwrap_or(Open::Text) {
            Open::Text => self
                .open
                .iter()
                .rev()
                .find(|o| matches!(o, Open::Item | Open::Quote))
                .copied()
                .unwrap_or(Open::Text),
            other => other,
        }
    }

    fn flush(&mut self) {
        let text = self
            .current
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        self.current.clear();
        if !text.chars().any(char::is_alphanumeric) {
            return;
        }
        let text = web_search::defang(&text);
        match self.kind() {
            Open::Heading(level) => {
                let name = passages::clip(text.trim_end_matches([':', ' ']), MAX_HEADING_BYTES);
                self.section = Some(name.clone());
                self.push(format!("{} {name}", "#".repeat(level)), true);
            }
            Open::Row => self.push(passages::clip(&text, LINE_BYTES), false),
            kind => {
                let prefix = match kind {
                    Open::Item => "- ",
                    Open::Quote => "> ",
                    _ => "",
                };
                for window in web_search::sentence_windows(&text, LINE_BYTES) {
                    self.push(format!("{prefix}{window}"), false);
                }
            }
        }
    }

    fn push(&mut self, text: String, heading: bool) {
        if self.full() || !self.seen.insert(text.clone()) {
            return;
        }
        self.lines.push(Line {
            text,
            section: self.section.clone(),
            heading,
        });
    }
}

/// The page's title, its content as lines, and what it carries beside them.
fn parse_html(html: &str, address: &str) -> (String, Vec<Line>, PageData) {
    let document = bounded_html::parse_within(
        html,
        web_search::MAX_NODES_PER_PAGE,
        web_search::PARSE_TIME_PER_PAGE,
    );
    let data = reqwest::Url::parse(address)
        .map(|base| page_data::page_data(&document, &base))
        .unwrap_or_default();
    let (title, lines) = lines_of(&document);
    (title, lines, data)
}

/// The page's title and its content as lines, boilerplate dropped as a search's pages are.
#[cfg(test)]
fn lines_from_html(html: &str) -> (String, Vec<Line>) {
    let document = bounded_html::parse_within(
        html,
        web_search::MAX_NODES_PER_PAGE,
        web_search::PARSE_TIME_PER_PAGE,
    );
    lines_of(&document)
}

fn lines_of(document: &scraper::Html) -> (String, Vec<Line>) {
    let strict = web_search::walk::<Lines>(document, true).lines;
    // Readability's retry, as `extract_blocks` makes it: too little text means the class test
    // dropped the content's own container.
    let lines = if text_bytes(&strict) >= web_search::READABILITY_CHAR_THRESHOLD {
        strict
    } else {
        let lenient = web_search::walk::<Lines>(document, false).lines;
        if text_bytes(&lenient) > text_bytes(&strict) {
            lenient
        } else {
            strict
        }
    };
    (title_of(document), lines)
}

fn title_of(document: &scraper::Html) -> String {
    static TITLE: LazyLock<scraper::Selector> =
        LazyLock::new(|| scraper::Selector::parse("title").expect("the title selector"));
    document
        .select(&TITLE)
        .next()
        .map(|t| t.text().collect::<Vec<_>>().join(" "))
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|t| passages::clip(&web_search::defang(&t), MAX_TITLE_BYTES))
        .unwrap_or_default()
}

/// A plain-text page: each paragraph, between blank lines, as lines.
fn lines_from_text(text: &str) -> Vec<Line> {
    let mut sink = Lines::default();
    for paragraph in text.split("\n\n") {
        sink.open("p");
        sink.text(paragraph);
        sink.close("p");
    }
    sink.lines
}

fn text_bytes(lines: &[Line]) -> usize {
    lines.iter().map(|l| l.text.len()).sum()
}

// ── The page and the reply ─────────────────────────────────────────────────

#[derive(Debug)]
struct Page {
    address: String,
    title: String,
    lines: Vec<Line>,
    parts: Vec<Range<usize>>,
    data: PageData,
    /// Clicks from an address given outright; at [`MAX_LINK_DEPTH`] its links lead nowhere.
    depth: u8,
}

/// What the call asked of the page.
#[derive(Debug, Default)]
struct Request {
    question: String,
    part: Option<usize>,
}

impl From<&ReadWebPageParams> for Request {
    fn from(params: &ReadWebPageParams) -> Self {
        Self {
            question: params.asked(),
            part: params.part(),
        }
    }
}

struct Reply {
    text: String,
    mode: &'static str,
    /// The page's lines the reply gave, for the source ledger.
    used: Vec<String>,
}

impl Page {
    fn new(address: String, title: String, fallback_title: String, lines: Vec<Line>) -> Self {
        let title = if !title.is_empty() {
            title
        } else {
            lines
                .iter()
                .find(|l| l.heading)
                .and_then(|l| l.section.clone())
                .unwrap_or(fallback_title)
        };
        let parts = parts_of(&lines);
        Self {
            address,
            title,
            lines,
            parts,
            data: PageData::default(),
            depth: 0,
        }
    }

    fn with_data(mut self, data: PageData) -> Self {
        self.data = data;
        self
    }

    fn at_depth(mut self, depth: u8) -> Self {
        self.depth = depth;
        self
    }

    /// The links a reader may follow from here, with their names: its items', then the first of
    /// its content's, on its own site; none once the next click would pass the depth.
    fn followable(&self) -> Vec<(String, String)> {
        if self.depth >= MAX_LINK_DEPTH {
            return Vec::new();
        }
        let items = self
            .data
            .items
            .iter()
            .filter_map(|i| i.link.clone().map(|l| (l, i.title.clone())));
        let links = self
            .data
            .links
            .iter()
            .map(|l| (l.address.clone(), l.text.clone()));
        let mut seen = HashSet::new();
        items
            .chain(links.take(NOTED_LINKS))
            .filter(|(a, _)| page_addresses::same_site(a, &self.address))
            .filter(|(a, _)| seen.insert(a.clone()))
            .collect()
    }

    #[cfg(test)]
    fn reply(&self, request: &Request) -> Reply {
        self.reply_as(request, None)
    }

    /// The reply, with what the page carries beside its text, and a word on the turn's reads
    /// when `turn_read` is the last one a conversation turn may make.
    fn reply_as(&self, request: &Request, turn_read: Option<usize>) -> Reply {
        let mut reply = self.text_reply(request);
        let first = request.part.is_none_or(|n| n == 1);
        if first {
            reply.text.push_str(&self.items_text(&request.question));
            reply.text.push_str(&self.forms_text());
        }
        if request.part.is_none() {
            reply.text.push_str(&self.links_text(&request.question));
        }
        if turn_read.is_some_and(|n| n >= MAX_CHAT_TURN_READS) {
            reply.text.push_str(&format!("\n{}", last_read_note()));
        }
        reply
    }

    fn text_reply(&self, request: &Request) -> Reply {
        let lines = |range: &Range<usize>| -> Vec<String> {
            self.lines[range.clone()]
                .iter()
                .map(|l| l.text.clone())
                .collect()
        };
        if let Some(n) = request.part {
            return match self.parts.get(n - 1) {
                Some(range) => Reply {
                    text: self.part_text(n, range),
                    mode: "part",
                    used: lines(range),
                },
                None => Reply {
                    text: format!(
                        "{}\nThis page has {} part{}; ask for one from 1 to {}.\n{}",
                        self.header(),
                        self.parts.len(),
                        if self.parts.len() == 1 { "" } else { "s" },
                        self.parts.len(),
                        self.outline()
                    ),
                    mode: "no_such_part",
                    used: Vec::new(),
                },
            };
        }
        if self.parts.len() <= 1 {
            return Reply {
                text: self.whole_text(),
                mode: "whole",
                used: lines(&(0..self.lines.len())),
            };
        }
        match self.quote(&request.question) {
            Some((text, used)) => Reply {
                text,
                mode: "question",
                used,
            },
            None => Reply {
                text: self.part_text(1, &self.parts[0]),
                mode: "first_part",
                used: lines(&self.parts[0]),
            },
        }
    }

    /// The page's listed items, those the question names first, within [`ITEMS_BYTES`].
    fn items_text(&self, question: &str) -> String {
        let items = &self.data.items;
        if items.is_empty() {
            return String::new();
        }
        let rows: Vec<String> = items.iter().map(item_row).collect();
        let mut order: Vec<usize> = Vec::new();
        if !question.trim().is_empty() {
            order.extend(
                passages::rank(question, &rows)
                    .into_iter()
                    .filter(|r| r.score > 0.0)
                    .map(|r| r.index),
            );
        }
        let ranked: HashSet<usize> = order.iter().copied().collect();
        order.extend((0..rows.len()).filter(|i| !ranked.contains(i)));
        let mut shown = Vec::new();
        let mut used = 0;
        for i in order {
            let cost = rows[i].len() + 5;
            if shown.len() == MAX_SHOWN_ITEMS || used + cost > ITEMS_BYTES {
                break;
            }
            used += cost;
            shown.push(i);
        }
        shown.sort_unstable();
        let mut out = format!(
            "\nListed on this page, from its own data ({} of {}):",
            shown.len(),
            items.len()
        );
        for (n, i) in shown.into_iter().enumerate() {
            out.push_str(&format!("\n{}. {}", n + 1, rows[i]));
        }
        if self.depth < MAX_LINK_DEPTH {
            out.push_str("\nTo read one, call read_web_page with its link.");
        }
        out
    }

    /// Links on the page's own site that the question names, which may be read next.
    fn links_text(&self, question: &str) -> String {
        if self.depth >= MAX_LINK_DEPTH || question.trim().is_empty() {
            return String::new();
        }
        let item_links: HashSet<&str> = self
            .data
            .items
            .iter()
            .filter_map(|i| i.link.as_deref())
            .collect();
        let links: Vec<&page_data::Link> = self
            .data
            .links
            .iter()
            .take(NOTED_LINKS)
            .filter(|l| page_addresses::same_site(&l.address, &self.address))
            .filter(|l| !item_links.contains(l.address.as_str()))
            .collect();
        let texts: Vec<&str> = links.iter().map(|l| l.text.as_str()).collect();
        let picked: Vec<&page_data::Link> = passages::rank(question, &texts)
            .into_iter()
            .filter(|r| r.score > 0.0)
            .take(MAX_SHOWN_LINKS)
            .map(|r| links[r.index])
            .collect();
        if picked.is_empty() {
            return String::new();
        }
        let mut out =
            String::from("\nLinks on this page that may say more (read one by its link):");
        for link in picked {
            out.push_str(&format!("\n- {}: {}", link.text, link.address));
        }
        out
    }

    /// The page's forms, as addresses a reader may fill in. A POST form's hidden fields are left
    /// out: the pond fills them in fresh when it sends.
    fn forms_text(&self) -> String {
        if self.depth >= MAX_LINK_DEPTH {
            return String::new();
        }
        let forms: Vec<&page_addresses::SearchForm> = self
            .data
            .forms
            .iter()
            .filter(|f| page_addresses::same_site(&f.action, &self.address))
            .collect();
        if forms.is_empty() {
            return String::new();
        }
        let mut out = String::from(
            "\nForms on this page. To use one, call read_web_page with the form's address and a \
             query of its own fields, such as ?field=value&other=value; set fields only:",
        );
        for form in forms {
            let posts = form.method == page_addresses::FormMethod::Post;
            let fields: Vec<String> = form
                .fields
                .iter()
                .filter(|f| !(posts && matches!(f, page_addresses::FormField::Fixed { .. })))
                .map(field_words)
                .collect();
            let how = if posts {
                " (POST: sent only after the person says yes on their phone)"
            } else {
                ""
            };
            let line = format!("\n- {}{how}: {}", form.action, fields.join("; "));
            out.push_str(&passages::clip(&line, FORM_LINE_BYTES));
        }
        out
    }

    fn header(&self) -> String {
        format!(
            "Page: {}\nAddress: {}\n{}",
            self.title,
            self.address,
            web_search::WEB_TEXT_NOTICE
        )
    }

    fn whole_text(&self) -> String {
        let mut out = self.header();
        for line in &self.lines {
            out.push('\n');
            out.push_str(&line.text);
        }
        out
    }

    fn part_text(&self, n: usize, range: &Range<usize>) -> String {
        let mut out = format!(
            "{}\nPart {n} of {}: {}",
            self.header(),
            self.parts.len(),
            self.part_name(n - 1)
        );
        for line in &self.lines[range.clone()] {
            out.push('\n');
            out.push_str(&line.text);
        }
        if self.parts.len() > 1 {
            out.push('\n');
            out.push_str(&self.outline());
        }
        out
    }

    /// The lines that answer `question` within [`ANSWER_BYTES`], in page order under their parts;
    /// `None` when no line shares a word or an answer's shape with it.
    fn quote(&self, question: &str) -> Option<(String, Vec<String>)> {
        let eligible: Vec<usize> = (0..self.lines.len())
            .filter(|&i| self.lines[i].is_quotable())
            .collect();
        let candidates: Vec<String> = eligible
            .iter()
            .map(|&i| {
                let line = &self.lines[i];
                match &line.section {
                    Some(s) => format!("{s}: {}", line.text),
                    None => line.text.clone(),
                }
            })
            .collect();
        let ranked = passages::rank(question, &candidates);
        let mut chosen: Vec<usize> = Vec::new();
        let mut used = 0usize;
        for r in ranked.iter().filter(|r| r.score > 0.0) {
            let index = eligible[r.index];
            let cost = self.lines[index].text.len() + 1;
            if used + cost > ANSWER_BYTES {
                continue;
            }
            used += cost;
            chosen.push(index);
        }
        if chosen.is_empty() {
            return None;
        }
        chosen.sort_unstable();
        let used = chosen.iter().map(|&i| self.lines[i].text.clone()).collect();
        let mut out = format!("{}\nLines that answer: {}", self.header(), echo(question));
        let mut last_part = None;
        let mut last_section: Option<&str> = None;
        for index in chosen {
            let part = self.part_of(index);
            if last_part != Some(part) {
                out.push_str(&format!("\n[Part {}: {}]", part + 1, self.part_name(part)));
                last_part = Some(part);
                last_section = None;
            }
            let line = &self.lines[index];
            let section = line.section.as_deref();
            if !line.heading && section.is_some() && section != last_section {
                out.push_str(&format!("\n## {}", section.unwrap_or_default()));
            }
            last_section = section;
            out.push('\n');
            out.push_str(&line.text);
        }
        out.push('\n');
        out.push_str(&self.outline());
        Some((out, used))
    }

    fn part_of(&self, index: usize) -> usize {
        self.parts
            .iter()
            .position(|r| r.contains(&index))
            .unwrap_or(0)
    }

    /// A part's name: its first heading, else the section it continues.
    fn part_name(&self, part: usize) -> String {
        let range = &self.parts[part];
        let lines = &self.lines[range.clone()];
        let name = lines
            .iter()
            .find(|l| l.heading)
            .or_else(|| lines.first())
            .and_then(|l| l.section.clone());
        match name {
            Some(name) => passages::clip(&name, MAX_PART_NAME_BYTES),
            None if part == 0 => "the start".to_string(),
            None => format!("part {}", part + 1),
        }
    }

    fn outline(&self) -> String {
        let named: Vec<String> = (0..self.parts.len().min(OUTLINE_PARTS))
            .map(|p| format!("{} {}", p + 1, self.part_name(p)))
            .collect();
        let more = self.parts.len().saturating_sub(OUTLINE_PARTS);
        let tail = if more > 0 {
            format!(", and {more} more")
        } else {
            String::new()
        };
        format!(
            "This page has {} parts: {}{tail}. To read one whole, call read_web_page again with \
             this address and its part number.",
            self.parts.len(),
            named.join(", ")
        )
    }
}

/// The lines in parts of at most [`PART_BYTES`], a new part starting at a heading once the
/// current one is half full, so a part is a section where the page allows.
fn parts_of(lines: &[Line]) -> Vec<Range<usize>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (i, line) in lines.iter().enumerate() {
        let cost = line.text.len() + 1;
        let over = size + cost > PART_BYTES;
        let at_heading = line.heading && size >= PART_BYTES / 2;
        if i > start && (over || at_heading) {
            parts.push(start..i);
            start = i;
            size = 0;
        }
        size += cost;
    }
    if start < lines.len() {
        parts.push(start..lines.len());
    }
    parts
}

fn echo(question: &str) -> String {
    passages::clip(
        &web_search::defang(&question.split_whitespace().collect::<Vec<_>>().join(" ")),
        200,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::mcp::services::page_addresses::{
        FormField, FormMethod, PageAddresses, SearchForm,
    };

    fn listing(depth: u8) -> Page {
        let item = |title: &str, price: &str, link: &str| page_data::Item {
            title: title.into(),
            price: Some(price.into()),
            currency: Some("KES".into()),
            link: Some(link.into()),
            ..Default::default()
        };
        let data = PageData {
            items: vec![
                item("2016 Toyota RAV4", "1200000", "https://cars.example/used/1"),
                item(
                    "2017 Subaru Forester",
                    "1450000",
                    "https://cars.example/used/2",
                ),
            ],
            items_from: Some(page_data::ItemSource::JsonLd),
            links: vec![
                page_data::Link {
                    address: "https://cars.example/finance".into(),
                    text: "Car finance and loans".into(),
                },
                page_data::Link {
                    address: "https://cars.example/subaru-guide".into(),
                    text: "Buying a used Subaru Forester".into(),
                },
                page_data::Link {
                    address: "https://other.example/forester".into(),
                    text: "Subaru Forester reviews".into(),
                },
            ],
            forms: vec![
                SearchForm {
                    action: "https://cars.example/search".into(),
                    method: FormMethod::Get,
                    fields: vec![
                        FormField::Text("q".into()),
                        FormField::Fixed {
                            name: "kind".into(),
                            value: "used".into(),
                        },
                    ],
                },
                SearchForm {
                    action: "https://cars.example/enquire".into(),
                    method: FormMethod::Post,
                    fields: vec![
                        FormField::Fixed {
                            name: "csrf".into(),
                            value: "t0k".into(),
                        },
                        FormField::Text("message".into()),
                    ],
                },
            ],
            preview_image: None,
        };
        Page::new(
            "https://cars.example/used".into(),
            "Used cars".into(),
            "cars.example".into(),
            Vec::new(),
        )
        .with_data(data)
        .at_depth(depth)
    }

    fn ask(question: &str) -> Request {
        Request {
            question: question.into(),
            part: None,
        }
    }

    #[test]
    fn a_page_with_only_structured_data_lists_its_items() {
        let reply = listing(0).reply(&ask("subaru forester price"));
        assert!(
            reply
                .text
                .contains("Listed on this page, from its own data (2 of 2):"),
            "{}",
            reply.text
        );
        assert!(reply
            .text
            .contains("2017 Subaru Forester | KES 1450000 | https://cars.example/used/2"));
        assert!(reply
            .text
            .contains("To read one, call read_web_page with its link."));
        assert!(reply
            .text
            .contains("- https://cars.example/search: q (text); kind (keep as used)"));
        assert!(
            reply.text.contains(
                "- https://cars.example/enquire (POST: sent only after the person says yes on \
                 their phone): message (text)"
            ),
            "{}",
            reply.text
        );
        assert!(
            !reply.text.contains("t0k"),
            "a POST form's hidden values are the pond's"
        );
    }

    #[test]
    fn links_shown_are_on_the_site_and_named_by_the_question() {
        let reply = listing(0).reply(&ask("subaru forester"));
        assert!(reply
            .text
            .contains("- Buying a used Subaru Forester: https://cars.example/subaru-guide"));
        assert!(!reply.text.contains("other.example"), "{}", reply.text);
        assert!(!reply.text.contains("Car finance"), "{}", reply.text);
        let page = listing(0);
        let followable: Vec<String> = page.followable().into_iter().map(|(a, _)| a).collect();
        assert_eq!(
            followable,
            vec![
                "https://cars.example/used/1",
                "https://cars.example/used/2",
                "https://cars.example/finance",
                "https://cars.example/subaru-guide",
            ]
        );
    }

    #[test]
    fn a_page_two_clicks_deep_offers_no_way_on() {
        let page = listing(MAX_LINK_DEPTH);
        let reply = page.reply(&ask("subaru forester"));
        assert!(reply.text.contains("2017 Subaru Forester"));
        assert!(!reply.text.contains("To read one"), "{}", reply.text);
        assert!(!reply.text.contains("Links on this page"), "{}", reply.text);
        assert!(!reply.text.contains("Forms on this page"), "{}", reply.text);
        assert!(page.followable().is_empty());
    }

    #[test]
    fn a_conversation_turns_last_read_says_to_answer() {
        let page = listing(0);
        let third = page.reply_as(&ask("price"), Some(MAX_CHAT_TURN_READS));
        assert!(
            third.text.contains("That was this turn's last page"),
            "{}",
            third.text
        );
        assert!(third
            .text
            .contains("offer to keep exploring as a long task"));
        let second = page.reply_as(&ask("price"), Some(MAX_CHAT_TURN_READS - 1));
        assert!(!second.text.contains("last page"));
        let step = page.reply_as(&ask("price"), None);
        assert!(!step.text.contains("last page"));
        assert!(turn_cap_reached().contains("3 pages"));
    }

    #[test]
    fn a_reply_names_the_lines_it_gave() {
        let reply = page(FARES).reply(&ask("economy fare"));
        assert!(reply.used.iter().any(|l| l.contains("Economy | KSh 1,500")));
        let long = long_page();
        let quoted = long.reply(&ask("What is the economy fare?"));
        assert!(quoted
            .used
            .contains(&"The economy fare is KSh 1,500 for an adult.".to_string()));
        assert!(quoted.used.len() < long.lines.len());
    }

    #[test]
    fn a_page_reads_its_structured_data_beside_its_lines() {
        let html = r#"<html><head><title>Cars</title><script type="application/ld+json">
            {"@type":"Car","name":"2017 Subaru Forester","offers":{"price":1450000,"priceCurrency":"KES"}}
            </script></head><body><main><p>Used cars for sale in Nairobi this week.</p>
            <a href="/used/2">2017 Subaru Forester in Nairobi</a></main></body></html>"#;
        let (title, lines, data) = parse_html(html, "https://cars.example/used");
        assert_eq!(title, "Cars");
        assert!(!lines.is_empty());
        assert_eq!(data.items.len(), 1);
        assert_eq!(data.links[0].address, "https://cars.example/used/2");
    }

    fn page(html: &str) -> Page {
        let (title, lines) = lines_from_html(html);
        Page::new(
            "https://example.com/p".into(),
            title,
            "example.com".into(),
            lines,
        )
    }

    const FARES: &str = r#"<html><head><title>Madaraka Express | Fares</title></head><body>
        <nav><a href="/">Home</a> <a href="/about">About us</a></nav>
        <article>
          <h1>Madaraka Express</h1>
          <p>The Madaraka Express runs daily between Nairobi Terminus and Mombasa Terminus.</p>
          <h2>Fares</h2>
          <table>
            <tr><th>Class</th><th>Adult</th><th>Child</th></tr>
            <tr><td>Economy</td><td>KSh 1,500</td><td>KSh 750</td></tr>
            <tr><td>First class</td><td>KSh 4,500</td><td>KSh 2,250</td></tr>
          </table>
          <h2>Timetable</h2>
          <ul><li><p>Inter-county train leaves Nairobi at 08:00</p></li><li>Express leaves at 15:00</li></ul>
          <blockquote>Book at least a day ahead.</blockquote>
        </article>
        <footer>Copyright Kenya Railways</footer>
        </body></html>"#;

    #[test]
    fn a_page_reads_as_markdown_without_its_furniture() {
        let page = page(FARES);
        let lines: Vec<&str> = page.lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(page.title, "Madaraka Express | Fares");
        assert_eq!(
            lines,
            vec![
                "# Madaraka Express",
                "The Madaraka Express runs daily between Nairobi Terminus and Mombasa Terminus.",
                "## Fares",
                "Class | Adult | Child",
                "Economy | KSh 1,500 | KSh 750",
                "First class | KSh 4,500 | KSh 2,250",
                "## Timetable",
                "- Inter-county train leaves Nairobi at 08:00",
                "- Express leaves at 15:00",
                "> Book at least a day ahead.",
            ]
        );
        assert_eq!(page.lines[4].section.as_deref(), Some("Fares"));
    }

    #[test]
    fn a_short_page_is_given_whole() {
        let page = page(FARES);
        let reply = page.reply(&Request {
            question: "economy fare".into(),
            part: None,
        });
        assert_eq!(reply.mode, "whole");
        assert!(reply.text.starts_with(
            "Page: Madaraka Express | Fares\nAddress: https://example.com/p\nThe lines below"
        ));
        assert!(reply.text.contains("Economy | KSh 1,500"));
    }

    /// A page long enough for several parts, each section a few paragraphs.
    fn long_page() -> Page {
        let mut html = String::from("<html><head><title>Guide</title></head><body><main>");
        for (section, words) in [
            ("History", "railway colonial built"),
            ("Fares", "economy ticket price shillings"),
            ("Stations", "terminus platform station"),
        ] {
            html.push_str(&format!("<h2>{section}</h2>"));
            for i in 0..12 {
                html.push_str(&format!(
                    "<p>Paragraph {i} about the {section}: {words}, with enough words to make \
                     a line of some length that fills the part a little more each time.</p>"
                ));
            }
        }
        html.push_str("<p>The economy fare is KSh 1,500 for an adult.</p></main></body></html>");
        page(&html)
    }

    #[test]
    fn parts_hold_their_budget_and_start_at_headings() {
        let page = long_page();
        assert!(page.parts.len() >= 3, "{} parts", page.parts.len());
        for range in &page.parts {
            let bytes: usize = page.lines[range.clone()]
                .iter()
                .map(|l| l.text.len() + 1)
                .sum();
            assert!(bytes <= PART_BYTES, "a part of {bytes} bytes");
        }
        let starts_at_heading = page
            .parts
            .iter()
            .filter(|r| page.lines[r.start].heading)
            .count();
        assert!(starts_at_heading >= 2, "{:?}", page.parts);
    }

    #[test]
    fn a_question_quotes_its_lines_under_their_part() {
        let page = long_page();
        let reply = page.reply(&Request {
            question: "What is the economy fare?".into(),
            part: None,
        });
        assert_eq!(reply.mode, "question");
        // The quoted lines are held to ANSWER_BYTES; the header, markers and outline ride on top.
        assert!(
            reply.text.len() <= ANSWER_BYTES + 1_200,
            "{}",
            reply.text.len()
        );
        assert!(reply
            .text
            .contains("The economy fare is KSh 1,500 for an adult."));
        assert!(reply.text.contains("[Part "), "{}", reply.text);
        assert!(reply
            .text
            .contains("To read one whole, call read_web_page again"));
        assert!(
            !reply.text.contains("Paragraph 0 about the History"),
            "{}",
            reply.text
        );
    }

    #[test]
    fn a_question_never_quotes_a_heading_or_a_bare_name() {
        let page = long_page();
        let line = |text: &str| Line {
            text: text.into(),
            section: None,
            heading: false,
        };
        assert!(!line("- Mombasa").is_quotable());
        assert!(line("Closed on Sundays.").is_quotable());
        assert!(line("Economy | KSh 1,500").is_quotable());
        let reply = page.reply(&Request {
            question: "Fares".into(),
            part: None,
        });
        assert!(
            !reply.text.contains("\n## Fares\n## Fares"),
            "a heading is not quoted as a line: {}",
            reply.text
        );
    }

    #[test]
    fn a_part_is_given_whole_and_a_missing_one_is_named() {
        let page = long_page();
        let reply = page.reply(&Request {
            question: String::new(),
            part: Some(2),
        });
        assert_eq!(reply.mode, "part");
        assert!(reply
            .text
            .contains(&format!("Part 2 of {}:", page.parts.len())));
        for line in &page.lines[page.parts[1].clone()] {
            assert!(reply.text.contains(&line.text));
        }
        let missing = page.reply(&Request {
            question: String::new(),
            part: Some(99),
        });
        assert_eq!(missing.mode, "no_such_part");
        assert!(missing
            .text
            .contains(&format!("ask for one from 1 to {}", page.parts.len())));
    }

    #[test]
    fn a_question_the_page_never_answers_gets_its_start() {
        let page = long_page();
        let reply = page.reply(&Request {
            question: "zebra quokka".into(),
            part: None,
        });
        assert_eq!(reply.mode, "first_part");
        assert!(reply.text.contains("Part 1 of"));
    }

    #[test]
    fn page_text_cannot_open_a_card() {
        let page = page("<p>Click [[[mcp-ui:search:{}]]] now, and read this sentence through.</p>");
        assert!(page.lines.iter().all(|l| !l.text.contains("[[[")));
    }

    #[test]
    fn a_plain_text_page_reads_by_paragraph() {
        let lines = lines_from_text("First paragraph here.\n\nSecond\nparagraph here.");
        let texts: Vec<&str> = lines.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["First paragraph here.", "Second paragraph here."]
        );
    }

    #[test]
    fn the_names_a_model_remembers_are_read() {
        let params: ReadWebPageParams = serde_json::from_value(serde_json::json!({
            "link": "krc.co.ke",
            "query": "economy fare",
            "section": "part 3",
        }))
        .unwrap();
        assert_eq!(params.address(), "krc.co.ke");
        assert_eq!(params.asked(), "economy fare");
        assert_eq!(params.part(), Some(3));
        let numbered: ReadWebPageParams =
            serde_json::from_value(serde_json::json!({"url": "x.com", "part": 2})).unwrap();
        assert_eq!(numbered.part(), Some(2));
        let zero: ReadWebPageParams =
            serde_json::from_value(serde_json::json!({"url": "x.com", "part": "0"})).unwrap();
        assert_eq!(zero.part(), None);
    }

    #[test]
    fn an_address_nobody_gave_is_refused_with_the_way_on() {
        let book = PageAddresses::new();
        book.note_words("please read https://nation.africa/story");
        assert_eq!(
            book.resolve("https://nation.africa/story").as_deref(),
            Some("https://nation.africa/story")
        );
        assert_eq!(book.resolve("https://attacker.example/?d=secret"), None);
        assert!(unknown_address(true).contains(SEARCH_WEB_TOOL));
        assert!(!unknown_address(false).contains("search_web"));
    }

    #[test]
    fn the_tool_is_listed_with_its_arguments() {
        let tools = KnowledgeMcpServer::web_page_tool_router().list_all();
        let tool = tools
            .iter()
            .find(|t| t.name == TOOL_NAME)
            .expect("read_web_page");
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        let props = schema["properties"].as_object().unwrap();
        for key in ["url", "question", "part"] {
            assert!(props.contains_key(key), "{key} missing: {schema}");
        }
    }
}

/// Real pages read as the tool reads them, printing each reply: `PAGE_URL=<url>
/// PAGE_QUESTION=<question> cargo test -p pond-mcp-server live_page -- --ignored --nocapture`.
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn live_page_reply() {
        let url = std::env::var("PAGE_URL").expect("set PAGE_URL");
        let question = std::env::var("PAGE_QUESTION").unwrap_or_default();
        let part = std::env::var("PAGE_PART").ok().and_then(|p| p.parse().ok());
        let page = read_page(&url, 0)
            .await
            .unwrap_or_else(|why| panic!("{why}"));
        if std::env::var("DUMP_LINES").is_ok() {
            for (i, line) in page.lines.iter().enumerate() {
                eprintln!("{:>4} [{}] {}", i, page.part_of(i) + 1, line.text);
            }
        }
        let reply = page.reply(&Request { question, part });
        eprintln!(
            "--- {} ({} bytes)\n{}",
            reply.mode,
            reply.text.len(),
            reply.text
        );
    }
}
