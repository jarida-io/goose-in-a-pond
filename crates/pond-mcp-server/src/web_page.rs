//! `read_web_page` for `giap-knowledge`: one page fetched through the public-only client, its
//! content turned into Markdown lines and broken into parts. With a question it quotes the lines
//! that answer it, grouped by part; with a part number, that part whole. Only an address the
//! person wrote, a task's goal names or a search found is fetched (`page_addresses`).

use std::collections::HashSet;
use std::ops::Range;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use pond_core::mcp::domain::tool_group::BRAVE_SEARCH_API_KEY;
use pond_core::mcp::domain::transient_result::{transient_meta, web_page_stub};
use pond_core::mcp::services::page_addresses;
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
use crate::web_search::{self, bounded_html, Sink};

/// The tool this module serves, as the egress record attributes it.
const TOOL_NAME: &str = "read_web_page";

const SEARCH_WEB_TOOL: &str = "giap-knowledge__search_web";

/// Longer than a search gives each page: this read is the whole of the call.
const PAGE_TIMEOUT: Duration = Duration::from_secs(10);

/// Parsing one page off the async workers, at most.
const EXTRACT_DEADLINE: Duration = Duration::from_secs(4);

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

/// Pages kept parsed, so asking for a part after a question costs no second fetch.
const CACHED_PAGES: usize = 8;
const CACHE_LIFETIME: Duration = Duration::from_secs(10 * 60);

const ACCEPT: &str = "text/html,application/xhtml+xml,text/plain;q=0.8";

const URL_ALIASES: &[&str] = &[
    "address", "link", "page", "href", "source", "site", "website", "uri",
];
const QUESTION_ALIASES: &[&str] = &["query", "q", "topic", "what", "about", "find"];
const PART_ALIASES: &[&str] = &["section", "part_number", "page_part", "number"];

// ── Parameters ─────────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ReadWebPageParams {
    /// The link the person gave, or a source's domain from search_web.
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
Read a web page: a link the person gave, or a source search_web found, by its domain. Returns \
the lines that answer the question, or one part whole.")]
    async fn read_web_page(
        &self,
        _ctx: RequestContext<RoleServer>,
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
        let address = match page_addresses::global().resolve(&asked_address) {
            Some(address) => address,
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
            None => match read_page(&address).await {
                Ok(page) => {
                    let page = Arc::new(page);
                    cache_page(&address, page.clone());
                    (page, false)
                }
                Err(why) => return Ok(text_result(&why)),
            },
        };
        let request = Request::from(&params);
        let reply = page.reply(&request);
        tracing::info!(
            target: "giap::trace",
            kind = "web_page",
            host = %web_search::domain_of(&page.address),
            lines = page.lines.len(),
            parts = page.parts.len(),
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

fn unknown_address(searchable: bool) -> String {
    let why = if searchable {
        format!(
            "I read only a page whose address the person gave or a search found. Find it with \
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

/// The page at `address`, read and broken into parts; the model's text for why not.
async fn read_page(address: &str) -> Result<Page, String> {
    let Some(client) = web_search::page_client() else {
        return Err(format_api_error(
            "Reading the page",
            "the page client could not be built",
        ));
    };
    let (final_address, body) = fetch(client, address).await?;
    let fallback_title = web_search::domain_of(&final_address);
    let parsing = tokio::task::spawn_blocking(move || {
        let (title, lines) = match body {
            Body::Html(html) => lines_from_html(&html),
            Body::Text(text) => (String::new(), lines_from_text(&text)),
        };
        Page::new(final_address, title, fallback_title, lines)
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
    if page.lines.is_empty() {
        return Err(format_dead_end(
            "reading that page",
            "The page has no text the pond can read: it may be built in the browser by \
             scripts, or need a sign-in.",
        ));
    }
    // A redirect's end came from the site, not the model, so it may be read again by name.
    page_addresses::global().note_source(&page.address, &page.title);
    Ok(page)
}

async fn fetch(client: &reqwest::Client, address: &str) -> Result<(String, Body), String> {
    let what = "reading that page";
    let parsed = reqwest::Url::parse(address)
        .map_err(|_| format_dead_end(what, "That is not a web address."))?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    if let Err(refused) = pond_net::public_only::check_target(&parsed) {
        tracing::info!(target: "giap::trace", kind = "web_page_refused", host = %host, %refused);
        return Err(format_dead_end(
            what,
            "That address is not on the public web, and the pond reads only public pages.",
        ));
    }
    let response = crate::http::traced_get_with(client, address, |b| {
        b.timeout(PAGE_TIMEOUT)
            .header(reqwest::header::ACCEPT, ACCEPT)
    })
    .await
    .map_err(|e| {
        tracing::info!(target: "giap::trace", kind = "web_page_unread", host = %host, error = %e);
        format_api_error("Reading the page", &web_search::describe_failure(&e))
    })?;
    let status = response.status();
    if !status.is_success() {
        let why = match status.as_u16() {
            404 | 410 => "There is no page at that address.".to_string(),
            401 | 403 => "The site would not let the pond read the page.".to_string(),
            429 => "The site asked the pond to wait before reading more.".to_string(),
            code => format!("The site answered HTTP {code}."),
        };
        return Err(format_dead_end(what, &why));
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
        other => {
            return Err(format_dead_end(
                what,
                &format!("That address is a file ({other}), not a web page."),
            ))
        }
    };
    let body = web_search::read_capped(response, web_search::PAGE_BYTE_CAP)
        .await
        .ok_or_else(|| format_api_error("Reading the page", "the page stopped arriving"))?;
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

/// The page's title and its content as lines, boilerplate dropped as a search's pages are.
fn lines_from_html(html: &str) -> (String, Vec<Line>) {
    let document = bounded_html::parse_within(
        html,
        web_search::MAX_NODES_PER_PAGE,
        web_search::PARSE_TIME_PER_PAGE,
    );
    let strict = web_search::walk::<Lines>(&document, true).lines;
    // Readability's retry, as `extract_blocks` makes it: too little text means the class test
    // dropped the content's own container.
    let lines = if text_bytes(&strict) >= web_search::READABILITY_CHAR_THRESHOLD {
        strict
    } else {
        let lenient = web_search::walk::<Lines>(&document, false).lines;
        if text_bytes(&lenient) > text_bytes(&strict) {
            lenient
        } else {
            strict
        }
    };
    (title_of(&document), lines)
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
        }
    }

    fn reply(&self, request: &Request) -> Reply {
        if let Some(n) = request.part {
            return match self.parts.get(n - 1) {
                Some(range) => Reply {
                    text: self.part_text(n, range),
                    mode: "part",
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
                },
            };
        }
        if self.parts.len() <= 1 {
            return Reply {
                text: self.whole_text(),
                mode: "whole",
            };
        }
        match self.quote(&request.question) {
            Some(text) => Reply {
                text,
                mode: "question",
            },
            None => Reply {
                text: self.part_text(1, &self.parts[0]),
                mode: "first_part",
            },
        }
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
    fn quote(&self, question: &str) -> Option<String> {
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
        Some(out)
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
    use pond_core::mcp::services::page_addresses::PageAddresses;

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
        let page = read_page(&url).await.unwrap_or_else(|why| panic!("{why}"));
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
