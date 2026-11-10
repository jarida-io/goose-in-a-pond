//! The web a member's news is found and read on: Brave's news search with the household's key,
//! its web search's freshness filter where the key's plan has no news search, and each page read
//! through the pond's public-only client. Every request is filed under the news, never a turn.
//! Search results are held only for the pass that asked (Brave's terms 3(b)(i)).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::LazyLock;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use pond_core::mcp::domain::web_search::{
    WebSearchStatus, BRAVE_API_BASE, BRAVE_TIMEOUT, BRAVE_TOKEN_HEADER, BRAVE_WEB_SEARCH_PATH,
};
use pond_core::mcp::services::web_search_status::{self, WebSearchStatusBoard};
use pond_core::shared::ports::news::{
    FoundPage, Freshness, NewsWeb, ReadPage, SearchBlocked, SearchFailure,
};
use pond_core::shared::services::egress::EgressPurpose;
use pond_core::shared::services::report_images;
use regex::Regex;
use serde::Deserialize;

use crate::web_search::{self, domain_of};

/// Brave's news search, on the same Search plan as its web search.
pub const BRAVE_NEWS_SEARCH_PATH: &str = "/res/v1/news/search";
/// Results asked of one search; a pass reads only the first few new ones.
const RESULT_COUNT: usize = 10;
const MAX_QUERY_CHARS: usize = 400;
const MAX_SITE_CHARS: usize = 60;

pub struct BraveNewsWeb {
    brave_base: String,
    board: &'static WebSearchStatusBoard,
    /// Set once the key's plan answered the news search with anything but news or a verdict on
    /// the key, so later searches go straight to the web search.
    news_search_unavailable: AtomicBool,
}

impl Default for BraveNewsWeb {
    fn default() -> Self {
        Self::at(BRAVE_API_BASE, web_search_status::global())
    }
}

#[derive(Debug, Default, Deserialize)]
struct NewsAnswer {
    #[serde(default)]
    results: Vec<Hit>,
}

#[derive(Debug, Default, Deserialize)]
struct WebAnswer {
    news: Option<NewsAnswer>,
    web: Option<NewsAnswer>,
}

#[derive(Debug, Default, Deserialize)]
struct Hit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
}

/// What one request came to.
enum Answer {
    Found(Vec<FoundPage>),
    /// The news search is not on this key's plan: use the web search.
    NotOnPlan,
}

impl BraveNewsWeb {
    /// A web whose searches go to `brave_base`, for tests against a stub.
    pub fn at(brave_base: impl Into<String>, board: &'static WebSearchStatusBoard) -> Self {
        Self {
            brave_base: brave_base.into(),
            board,
            news_search_unavailable: AtomicBool::new(false),
        }
    }

    async fn find_with(
        &self,
        key: &str,
        query: &str,
        freshness: Freshness,
    ) -> Result<Vec<FoundPage>, SearchFailure> {
        if !self.news_search_unavailable.load(Ordering::Relaxed) && freshness != Freshness::Any {
            match self
                .request(key, BRAVE_NEWS_SEARCH_PATH, query, freshness, true)
                .await?
            {
                Answer::Found(pages) => return Ok(pages),
                Answer::NotOnPlan => {
                    tracing::info!(target: "giap::trace", kind = "news_search_not_on_plan");
                    self.news_search_unavailable.store(true, Ordering::Relaxed);
                }
            }
        }
        match self
            .request(key, BRAVE_WEB_SEARCH_PATH, query, freshness, false)
            .await?
        {
            Answer::Found(pages) => Ok(pages),
            Answer::NotOnPlan => Ok(Vec::new()),
        }
    }

    async fn request(
        &self,
        key: &str,
        path: &str,
        query: &str,
        freshness: Freshness,
        news_endpoint: bool,
    ) -> Result<Answer, SearchFailure> {
        let Some(client) = web_search::brave_client() else {
            return Err(SearchFailure::Failed(
                "the search client could not be built".into(),
            ));
        };
        let q: String = query.chars().take(MAX_QUERY_CHARS).collect();
        let count = RESULT_COUNT.to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("q", q.as_str()),
            ("count", count.as_str()),
            ("safesearch", "moderate"),
        ];
        match freshness {
            Freshness::Day => params.push(("freshness", "pd")),
            Freshness::Week => params.push(("freshness", "pw")),
            Freshness::Any => {}
        }
        if !news_endpoint {
            params.push(("text_decorations", "false"));
            params.push(("result_filter", "news,web"));
        }
        let url = reqwest::Url::parse_with_params(
            &format!("{}{path}", self.brave_base.trim_end_matches('/')),
            params,
        )
        .map_err(|_| SearchFailure::Failed("the search address could not be built".into()))?;
        let response =
            crate::http::traced_get_for(client, url.as_str(), EgressPurpose::News, "", |b| {
                b.header(BRAVE_TOKEN_HEADER, key)
                    .header(reqwest::header::ACCEPT, "application/json")
                    .timeout(BRAVE_TIMEOUT)
            })
            .await
            .map_err(|e| SearchFailure::Failed(web_search::describe_failure(&e)))?;
        let status = response.status().as_u16();
        match status {
            200..=299 => self.board.record(key, WebSearchStatus::Ok),
            401 => {
                self.board.record(key, WebSearchStatus::Rejected);
                return Err(SearchFailure::Blocked(SearchBlocked::Refused));
            }
            402 | 429 => {
                self.board.record(key, WebSearchStatus::Quota);
                return Err(SearchFailure::Blocked(SearchBlocked::Quota));
            }
            403 if !news_endpoint => {
                self.board.record(key, WebSearchStatus::Rejected);
                return Err(SearchFailure::Blocked(SearchBlocked::Refused));
            }
            _ if news_endpoint => return Ok(Answer::NotOnPlan),
            _ => return Err(SearchFailure::Failed(format!("it answered HTTP {status}"))),
        }
        let hits: Vec<Hit> = if news_endpoint {
            response
                .json::<NewsAnswer>()
                .await
                .map_err(|_| SearchFailure::Failed("its answer could not be read".into()))?
                .results
        } else {
            let answer = response
                .json::<WebAnswer>()
                .await
                .map_err(|_| SearchFailure::Failed("its answer could not be read".into()))?;
            answer
                .news
                .map(|n| n.results)
                .unwrap_or_default()
                .into_iter()
                .chain(answer.web.map(|w| w.results).unwrap_or_default())
                .collect()
        };
        Ok(Answer::Found(found_pages(hits)))
    }
}

/// The usable pages of an answer, in its order, once each.
fn found_pages(hits: Vec<Hit>) -> Vec<FoundPage> {
    let mut pages: Vec<FoundPage> = Vec::new();
    for hit in hits {
        let url = hit.url.trim().to_string();
        let usable = url.starts_with("https://") || url.starts_with("http://");
        if usable && !pages.iter().any(|p| p.url == url) {
            pages.push(FoundPage {
                url,
                title: web_search::plain_text(&hit.title),
            });
        }
    }
    pages
}

#[async_trait]
impl NewsWeb for BraveNewsWeb {
    async fn blocked(&self) -> Option<SearchBlocked> {
        let Some(key) = web_search::brave_key().await else {
            return Some(SearchBlocked::NotConfigured);
        };
        match self.board.status_for(Some(&key)) {
            WebSearchStatus::Rejected => Some(SearchBlocked::Refused),
            WebSearchStatus::NotConfigured => Some(SearchBlocked::NotConfigured),
            // A spent allowance is tried again each pass: it comes back with the month.
            _ => None,
        }
    }

    async fn find(
        &self,
        query: &str,
        freshness: Freshness,
    ) -> Result<Vec<FoundPage>, SearchFailure> {
        let Some(key) = web_search::brave_key().await else {
            return Err(SearchFailure::Blocked(SearchBlocked::NotConfigured));
        };
        self.find_with(&key, query, freshness).await
    }

    async fn read(&self, url: &str) -> Option<ReadPage> {
        let page = match crate::web_page::read_article(url).await {
            Ok(page) => page,
            Err(failure) => {
                tracing::info!(target: "giap::trace", kind = "news_page_unread", host = %domain_of(url), failure = ?failure);
                return None;
            }
        };
        if let Some(image) = &page.preview_image {
            report_images::note_preview(&page.address, image);
            report_images::note_preview(url, image);
        }
        let site = page
            .site_name
            .as_deref()
            .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|s| !s.is_empty() && s.chars().count() <= MAX_SITE_CHARS)
            .unwrap_or_else(|| domain_of(&page.address));
        Some(ReadPage {
            url: page.address,
            title: page.title,
            site,
            lines: page.lines,
            published_at: page.published.as_deref().and_then(parse_date),
        })
    }

    async fn picture(&self, url: &str) -> Option<String> {
        let images = report_images::installed()?;
        images
            .for_pages(&[url.to_string()], 1)
            .await
            .into_iter()
            .next()
            .map(|image| image.key)
    }
}

/// A page's own date: RFC 3339, a date and time with an offset, or a bare date.
pub fn parse_date(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Some(t.with_timezone(&Utc));
    }
    for format in [
        "%Y-%m-%dT%H:%M:%S%z",
        "%Y-%m-%dT%H:%M:%S%.f%z",
        "%Y-%m-%d %H:%M:%S%z",
    ] {
        if let Ok(t) = DateTime::parse_from_str(text, format) {
            return Some(t.with_timezone(&Utc));
        }
    }
    if let Ok(t) = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S") {
        return Some(t.and_utc());
    }
    let day = text.get(..10)?;
    NaiveDate::parse_from_str(day, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(12, 0, 0))
        .map(|t| t.and_utc())
}

fn selector(css: &str) -> scraper::Selector {
    scraper::Selector::parse(css).expect("a fixed selector")
}

/// The publisher's name and the publication date a page's head gives for itself.
pub(crate) fn head_of(document: &scraper::Html) -> (Option<String>, Option<String>) {
    static SITE: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(r#"meta[property="og:site_name"], meta[name="application-name"]"#)
    });
    static PUBLISHED: LazyLock<scraper::Selector> = LazyLock::new(|| {
        selector(
            r#"meta[property="article:published_time"], meta[property="og:article:published_time"], meta[name="article:published_time"], meta[itemprop="datePublished"], meta[name="pubdate"], meta[name="publish-date"], meta[name="date"], time[itemprop="datePublished"]"#,
        )
    });
    static LD: LazyLock<scraper::Selector> =
        LazyLock::new(|| selector(r#"script[type="application/ld+json"]"#));
    static LD_DATE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#""datePublished"\s*:\s*"([^"]{8,40})""#).expect("a fixed pattern")
    });
    let site = document
        .select(&SITE)
        .filter_map(|m| m.value().attr("content"))
        .map(|c| web_search::defang(c.trim()))
        .find(|c| !c.is_empty());
    let published = document
        .select(&PUBLISHED)
        .filter_map(|m| {
            m.value()
                .attr("content")
                .or_else(|| m.value().attr("datetime"))
        })
        .map(|c| c.trim().to_string())
        .find(|c| !c.is_empty())
        .or_else(|| {
            document.select(&LD).find_map(|script| {
                let text: String = script.text().collect();
                LD_DATE
                    .captures(&text)
                    .and_then(|c| c.get(1))
                    .map(|m| m.as_str().to_string())
            })
        });
    (site, published)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_head_gives_its_publisher_and_date() {
        let html = r#"<html><head>
            <meta property="og:site_name" content="Nation">
            <script type="application/ld+json">{"@type":"NewsArticle","datePublished": "2026-11-08T09:30:00+03:00"}</script>
            </head><body><p>x</p></body></html>"#;
        let document = scraper::Html::parse_document(html);
        let (site, published) = head_of(&document);
        assert_eq!(site.as_deref(), Some("Nation"));
        assert_eq!(
            published.as_deref().and_then(parse_date),
            Some("2026-11-08T06:30:00Z".parse().unwrap())
        );
        let meta = scraper::Html::parse_document(
            r#"<meta property="article:published_time" content="2026-11-07">"#,
        );
        assert_eq!(
            head_of(&meta).1.as_deref().and_then(parse_date),
            Some("2026-11-07T12:00:00Z".parse().unwrap())
        );
    }

    /// Answers each path once per request with its route's status and body, and passes on each
    /// request line.
    fn stub(
        routes: Vec<(&'static str, u16, String)>,
        requests: usize,
    ) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = vec![0u8; 16 * 1024];
                let n = stream.read(&mut buf).unwrap_or(0);
                let line = String::from_utf8_lossy(&buf[..n])
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .split('?')
                    .next()
                    .unwrap_or("/")
                    .to_string();
                let _ = tx.send(line);
                let (status, body) = routes
                    .iter()
                    .find(|(p, ..)| *p == path)
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, "{}".to_string()));
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        (port, rx)
    }

    fn board() -> &'static WebSearchStatusBoard {
        Box::leak(Box::new(WebSearchStatusBoard::new()))
    }

    #[tokio::test]
    async fn the_news_search_is_asked_for_the_last_day() {
        let news = serde_json::json!({"type": "news", "results": [
            {"title": "Sifuna rally", "url": "https://nation.example/sifuna", "description": "never kept"}
        ]})
        .to_string();
        let (port, requests) = stub(vec![(BRAVE_NEWS_SEARCH_PATH, 200, news)], 1);
        let web = BraveNewsWeb::at(format!("http://127.0.0.1:{port}"), board());
        let pages = web
            .find_with("key", "Linda Mwananchi Movement", Freshness::Day)
            .await
            .unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].url, "https://nation.example/sifuna");
        let line = requests.recv().unwrap();
        assert!(line.contains("freshness=pd"), "{line}");
        assert!(
            line.starts_with(&format!("GET {BRAVE_NEWS_SEARCH_PATH}?")),
            "{line}"
        );
    }

    #[tokio::test]
    async fn a_plan_without_news_search_falls_back_to_the_fresh_web_search_and_stays_there() {
        let web_answer = serde_json::json!({
            "news": {"results": [{"title": "Gen V finale", "url": "https://tv.example/genv"}]},
            "web": {"results": [{"title": "The Boys", "url": "https://tv.example/boys"}]},
        })
        .to_string();
        let (port, requests) = stub(
            vec![
                (BRAVE_NEWS_SEARCH_PATH, 422, "{}".to_string()),
                (BRAVE_WEB_SEARCH_PATH, 200, web_answer),
            ],
            3,
        );
        let web = BraveNewsWeb::at(format!("http://127.0.0.1:{port}"), board());
        let pages = web
            .find_with("key", "Gen V", Freshness::Week)
            .await
            .unwrap();
        let urls: Vec<&str> = pages.iter().map(|p| p.url.as_str()).collect();
        assert_eq!(
            urls,
            vec!["https://tv.example/genv", "https://tv.example/boys"]
        );
        assert!(requests.recv().unwrap().contains(BRAVE_NEWS_SEARCH_PATH));
        let web_line = requests.recv().unwrap();
        assert!(web_line.contains(BRAVE_WEB_SEARCH_PATH) && web_line.contains("freshness=pw"));
        web.find_with("key", "Gen V", Freshness::Day).await.unwrap();
        assert!(requests.recv().unwrap().contains(BRAVE_WEB_SEARCH_PATH));
    }

    #[tokio::test]
    async fn a_refused_key_or_a_spent_allowance_stops_the_pass() {
        let (port, _requests) = stub(vec![(BRAVE_NEWS_SEARCH_PATH, 429, "{}".to_string())], 1);
        let board = board();
        let web = BraveNewsWeb::at(format!("http://127.0.0.1:{port}"), board);
        assert_eq!(
            web.find_with("key", "MCP", Freshness::Day).await,
            Err(SearchFailure::Blocked(SearchBlocked::Quota))
        );
        assert_eq!(board.status_for(Some("key")), WebSearchStatus::Quota);
    }

    #[test]
    fn an_answer_keeps_each_web_page_once_in_order() {
        let pages = found_pages(vec![
            Hit {
                title: "<strong>Fielder</strong> prices &amp; more".into(),
                url: "https://a.example/1".into(),
            },
            Hit {
                title: "again".into(),
                url: "https://a.example/1".into(),
            },
            Hit {
                title: "not the web".into(),
                url: "ftp://b.example".into(),
            },
        ]);
        assert_eq!(
            pages,
            vec![FoundPage {
                url: "https://a.example/1".into(),
                title: "Fielder prices & more".into()
            }]
        );
    }
}
