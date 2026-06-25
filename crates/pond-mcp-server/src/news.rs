//! News MCP Server — news discovery, headlines, and search.
//!
//! Provides 3 tools: `get_top_stories` (Hacker News), `search_news` (The Guardian),
//! `get_headlines` (GNews).
//! Depends on a `reqwest::Client` for HTTP fetches and `SettingsRepository` for API keys.

use pond_core::user_data::ports::settings::SettingsRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct TopStoriesParams {
    /// Category of stories: top, best, new, ask, show, job (default "top").
    pub category: Option<String>,
    /// Number of stories to return (default 5, max 15).
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SearchNewsParams {
    /// Search keywords (e.g. "climate change", "Kenya elections", "AI regulation").
    pub query: Option<String>,
    /// Optional section filter (world, politics, technology, sport, etc.).
    pub section: Option<String>,
    /// Maximum results to return (default 5, max 10).
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct HeadlinesParams {
    /// Topic: general, world, nation, business, technology, entertainment, sports, science, health.
    pub topic: Option<String>,
    /// 2-letter country code (e.g. "us", "ke", "gb"). Omit for global headlines.
    pub country: Option<String>,
    /// Language code (default "en").
    pub language: Option<String>,
    /// Maximum headlines to return (default 5, max 10).
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── Constants ──────────────────────────────────────────────────────────────

const HN_BASE_URL: &str = "https://hacker-news.firebaseio.com/v0";
const GUARDIAN_BASE_URL: &str = "https://content.guardianapis.com";
const GNEWS_BASE_URL: &str = "https://gnews.io/api/v4";
const WIKIMEDIA_FEED_URL: &str = "https://api.wikimedia.org/feed/v1/wikipedia/en/featured";

const TOP_STORIES_BUDGET: usize = 1500;
const SEARCH_NEWS_BUDGET: usize = 2000;
const HEADLINES_BUDGET: usize = 1500;

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct NewsMcpServer {
    http_client: reqwest::Client,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl NewsMcpServer {
    pub fn new(
        http_client: reqwest::Client,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    ) -> Self {
        Self {
            http_client,
            settings_repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Get trending tech stories from Hacker News. Use for 'what's new in tech', \
'trending on HN', or tech industry news.")]
    async fn get_top_stories(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<TopStoriesParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let category = resolve_category(params.0.category.as_deref());
        let limit = params.0.limit.unwrap_or(5).clamp(1, 15) as usize;
        println!(
            "[news] get_top_stories: category={}, limit={}",
            category, limit
        );

        // 1. Fetch story IDs
        let ids_url = format!("{}/{}.json", HN_BASE_URL, category);
        let ids_resp =
            match crate::http::traced_get(&self.http_client, &ids_url, "get_top_stories").await {
                Ok(r) => r,
                Err(e) => {
                    println!("[news] HN story IDs fetch failed: {e}");
                    return Ok(CallToolResult::success(vec![Content::text(
                        crate::format::format_api_error("Hacker News", &e.to_string()),
                    )]));
                }
            };

        if !ids_resp.status().is_success() {
            let status = ids_resp.status();
            println!("[news] HN story IDs returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("Hacker News", &format!("HTTP {status}")),
            )]));
        }

        let ids: Vec<u64> = match ids_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                println!("[news] failed to parse HN story IDs: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Hacker News", &e.to_string()),
                )]));
            }
        };

        let ids_to_fetch = &ids[..ids.len().min(limit)];
        println!("[news] fetching {} story details", ids_to_fetch.len());

        // 2. Fetch story details sequentially (bounded by limit)
        let mut items: Vec<String> = Vec::with_capacity(ids_to_fetch.len());
        let mut ui_items: Vec<serde_json::Value> = Vec::with_capacity(ids_to_fetch.len());
        for &id in ids_to_fetch {
            let item_url = format!("{}/item/{}.json", HN_BASE_URL, id);
            match self
                .http_client
                .get(&item_url)
                .timeout(std::time::Duration::from_secs(8))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(item) = resp.json::<serde_json::Value>().await {
                        let title = item["title"].as_str().unwrap_or("(untitled)");
                        let score = item["score"].as_u64().unwrap_or(0);
                        let by = item["by"].as_str().unwrap_or("unknown");
                        let descendants = item["descendants"].as_u64().unwrap_or(0);
                        let url = item["url"].as_str().unwrap_or("");

                        let domain = extract_domain(url);
                        let hn_link = format!("https://news.ycombinator.com/item?id={}", id);

                        ui_items.push(serde_json::json!({
                            "headline": title,
                            "tag": "Tech",
                            "timeAgo": format!("{} pts", score),
                            "source": if domain.is_empty() { "Hacker News".to_string() } else { domain.clone() },
                        }));

                        let formatted = if domain.is_empty() {
                            // Self-post (Ask HN, etc.)
                            format!(
                                "**{}** ({} pts) — by {} | {} comments | {}",
                                title, score, by, descendants, hn_link,
                            )
                        } else {
                            format!(
                                "**{}** ({} pts) — {} | by {} | {} comments | {}",
                                title, score, domain, by, descendants, hn_link,
                            )
                        };
                        items.push(formatted);
                    }
                }
                _ => {
                    // Skip failed individual item fetches
                    println!("[news] failed to fetch HN item {id}, skipping");
                }
            }
        }

        let category_label = category.trim_end_matches("stories");
        let header = format!("Hacker News — {} stories", category_label);
        let text = crate::format::format_list_result(&items, &header, TOP_STORIES_BUDGET);
        println!(
            "[news] get_top_stories done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "\
Search for news or get today's world events. Use for 'what's happening with X', \
current events, or news about a topic.")]
    async fn search_news(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SearchNewsParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        // 1. Read Guardian API key from settings
        let settings = match self.settings_repo.get().await {
            Ok(s) => s,
            Err(e) => {
                println!("[news] failed to load settings: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("News search", "settings unavailable"),
                )]));
            }
        };

        let has_guardian_key = settings
            .api_key_guardian
            .as_ref()
            .is_some_and(|k| !k.trim().is_empty());

        if has_guardian_key {
            let api_key = settings
                .api_key_guardian
                .as_ref()
                .unwrap()
                .trim()
                .to_string();
            return self.search_news_guardian(&api_key, &params.0).await;
        }

        // Fallback: Wikimedia Featured Content Feed (no API key required)
        println!("[news] No Guardian key — using Wikimedia feed fallback");
        self.search_news_wikimedia().await
    }

    #[tool(description = "\
Get today's top news headlines and trending topics. Use for 'what's in the news', \
'today's headlines', or 'breaking news'.")]
    async fn get_headlines(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<HeadlinesParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        // 1. Read GNews API key from settings
        let settings = match self.settings_repo.get().await {
            Ok(s) => s,
            Err(e) => {
                println!("[news] failed to load settings: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("Headlines", "settings unavailable"),
                )]));
            }
        };

        let has_gnews_key = settings
            .api_key_gnews
            .as_ref()
            .is_some_and(|k| !k.trim().is_empty());

        if has_gnews_key {
            let api_key = settings.api_key_gnews.as_ref().unwrap().trim().to_string();
            return self.get_headlines_gnews(&api_key, &params.0).await;
        }

        // Fallback: Wikimedia Featured Content Feed (no API key required)
        println!("[news] No GNews key — using Wikimedia feed fallback for headlines");
        self.get_headlines_wikimedia().await
    }
}

#[tool_handler]
impl ServerHandler for NewsMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-news",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP News server — current events and headlines.\n\n\
                 Tools: get_top_stories (tech news from HN), search_news (world events/search), \
                 get_headlines (today's top stories).\n\n\
                 For tech news: use get_top_stories. For world events or keyword search: use search_news. \
                 For general 'what's in the news': use get_headlines.\n\
                 All tools work without API keys. Guardian/GNews keys add keyword search and filtering.\n\
                 Summarize the most relevant items — don't list everything verbatim.",
            )
    }
}

// ── Extracted tool implementations ────────────────────────────────────────

impl NewsMcpServer {
    /// Guardian API path for search_news (preferred when key is configured).
    async fn search_news_guardian(
        &self,
        api_key: &str,
        params: &SearchNewsParams,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let query = resolve_query(params).await;
        println!("[news] search_news (Guardian): query={:?}", query);

        if query.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a search query. Retry with a 'query' parameter containing keywords to search for.",
            )]));
        }

        let limit = params.limit.unwrap_or(5).clamp(1, 10);

        let mut url = format!(
            "{}/search?q={}&api-key={}&show-fields=trailText&page-size={}",
            GUARDIAN_BASE_URL,
            urlencoding::encode(&query),
            urlencoding::encode(api_key),
            limit,
        );
        if let Some(ref section) = params.section {
            let trimmed = section.trim();
            if !trimmed.is_empty() {
                url.push_str(&format!("&section={}", urlencoding::encode(trimmed)));
            }
        }
        println!("[news] GET {}", url.replace(api_key, "***"));

        let resp = match self
            .http_client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("[news] Guardian request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("The Guardian", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            println!("[news] Guardian returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("The Guardian", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                println!("[news] failed to parse Guardian response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("The Guardian", &e.to_string()),
                )]));
            }
        };

        let results = body["response"]["results"].as_array();
        let mut ui_items: Vec<serde_json::Value> = Vec::new();
        let items: Vec<String> = results
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let title = item["webTitle"].as_str()?;
                        let section = item["sectionName"].as_str().unwrap_or("");
                        let date = item["webPublicationDate"]
                            .as_str()
                            .unwrap_or("")
                            .get(..10)
                            .unwrap_or("");
                        let url = item["webUrl"].as_str().unwrap_or("");
                        let trail = item["fields"]["trailText"]
                            .as_str()
                            .unwrap_or("")
                            .replace("<p>", "")
                            .replace("</p>", "");

                        ui_items.push(serde_json::json!({
                            "headline": title,
                            "tag": section,
                            "timeAgo": date,
                            "source": "The Guardian",
                        }));

                        let summary = if trail.len() > 120 {
                            format!(
                                "{}...",
                                &trail[..trail
                                    .char_indices()
                                    .nth(120)
                                    .map(|(i, _)| i)
                                    .unwrap_or(trail.len())]
                            )
                        } else {
                            trail
                        };

                        Some(format!(
                            "**{}** ({}, {}) — {} | {}",
                            title, section, date, summary, url,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let header = format!("Guardian search: \"{}\"", query);
        let text = crate::format::format_list_result(&items, &header, SEARCH_NEWS_BUDGET);
        println!(
            "[news] search_news (Guardian) done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Wikimedia Featured Content fallback for search_news (no API key needed).
    async fn search_news_wikimedia(&self) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let body = match self.fetch_wikimedia_feed().await {
            Ok(b) => b,
            Err(text) => return Ok(CallToolResult::success(vec![Content::text(text)])),
        };

        let news = body["news"].as_array();
        let items: Vec<String> = news
            .map(|arr| arr.iter().filter_map(format_wikimedia_story).collect())
            .unwrap_or_default();

        if items.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No current events found from Wikipedia today. Try again later, \
                 or add a Guardian API key in Settings for keyword search.",
            )]));
        }

        // Build UI hints from the wiki news stories
        let ui_items: Vec<serde_json::Value> = items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "headline": item.trim_start_matches("**Story**: "),
                    "tag": "World",
                    "timeAgo": "Today",
                    "source": "Wikipedia",
                })
            })
            .collect();

        let header = "Today's World News (via Wikipedia)";
        let mut text = crate::format::format_list_result(&items, header, SEARCH_NEWS_BUDGET);
        text.push_str(
            "\n\n(Source: Wikipedia current events — for keyword search, \
             add a Guardian API key in Settings.)",
        );
        println!(
            "[news] search_news (Wikimedia) done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// GNews API path for get_headlines (preferred when key is configured).
    async fn get_headlines_gnews(
        &self,
        api_key: &str,
        params: &HeadlinesParams,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let topic = params
            .topic
            .as_deref()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .unwrap_or("general");
        let language = params
            .language
            .as_deref()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .unwrap_or("en");
        let limit = params.limit.unwrap_or(5).clamp(1, 10);

        let mut url = format!(
            "{}/top-headlines?token={}&topic={}&lang={}&max={}",
            GNEWS_BASE_URL,
            urlencoding::encode(api_key),
            urlencoding::encode(topic),
            urlencoding::encode(language),
            limit,
        );
        if let Some(ref country) = params.country {
            let trimmed = country.trim();
            if !trimmed.is_empty() {
                url.push_str(&format!("&country={}", urlencoding::encode(trimmed)));
            }
        }
        println!("[news] GET {}", url.replace(api_key, "***"));

        let resp = match self
            .http_client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("[news] GNews request failed: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("GNews", &e.to_string()),
                )]));
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            println!("[news] GNews returned HTTP {status}");
            return Ok(CallToolResult::success(vec![Content::text(
                crate::format::format_api_error("GNews", &format!("HTTP {status}")),
            )]));
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                println!("[news] failed to parse GNews response: {e}");
                return Ok(CallToolResult::success(vec![Content::text(
                    crate::format::format_api_error("GNews", &e.to_string()),
                )]));
            }
        };

        let articles = body["articles"].as_array();
        let mut ui_items: Vec<serde_json::Value> = Vec::new();
        let items: Vec<String> = articles
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let title = item["title"].as_str()?;
                        let source = item["source"]["name"].as_str().unwrap_or("Unknown");
                        let date = item["publishedAt"]
                            .as_str()
                            .unwrap_or("")
                            .get(..10)
                            .unwrap_or("");
                        let description = item["description"].as_str().unwrap_or("");
                        let url = item["url"].as_str().unwrap_or("");

                        ui_items.push(serde_json::json!({
                            "headline": title,
                            "tag": topic,
                            "timeAgo": date,
                            "source": source,
                        }));

                        let summary = if description.len() > 120 {
                            format!(
                                "{}...",
                                &description[..description
                                    .char_indices()
                                    .nth(120)
                                    .map(|(i, _)| i)
                                    .unwrap_or(description.len())]
                            )
                        } else {
                            description.to_string()
                        };

                        Some(format!(
                            "**{}** ({}, {}) — {} | {}",
                            title, source, date, summary, url,
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let header = format!("Headlines — {} ({})", topic, language);
        let text = crate::format::format_list_result(&items, &header, HEADLINES_BUDGET);
        println!(
            "[news] get_headlines (GNews) done, {} items, {} chars",
            items.len(),
            text.len()
        );

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, text);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    /// Wikimedia Featured Content fallback for get_headlines (no API key needed).
    async fn get_headlines_wikimedia(&self) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let body = match self.fetch_wikimedia_feed().await {
            Ok(b) => b,
            Err(text) => return Ok(CallToolResult::success(vec![Content::text(text)])),
        };

        // News stories
        let news_items: Vec<String> = body["news"]
            .as_array()
            .map(|arr| arr.iter().filter_map(format_wikimedia_story).collect())
            .unwrap_or_default();

        // Trending / most-read articles
        let mostread_items: Vec<String> = body["mostread"]["articles"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .take(5)
                    .filter_map(|article| {
                        let title = article["titles"]["normalized"].as_str()?;
                        let extract = article["extract"].as_str().unwrap_or("");
                        let short = if extract.len() > 120 {
                            format!(
                                "{}...",
                                &extract[..extract
                                    .char_indices()
                                    .nth(120)
                                    .map(|(i, _)| i)
                                    .unwrap_or(extract.len())]
                            )
                        } else {
                            extract.to_string()
                        };
                        Some(format!("**{}** — {}", title, short))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Build combined output
        let mut text = String::with_capacity(HEADLINES_BUDGET);
        text.push_str("**Today's News** (via Wikipedia)\n\n");

        if news_items.is_empty() {
            text.push_str("No current events available today.\n");
        } else {
            for item in &news_items {
                let line = format!("- {}\n", item);
                if text.len() + line.len() > HEADLINES_BUDGET - 200 {
                    text.push_str("...\n");
                    break;
                }
                text.push_str(&line);
            }
        }

        if !mostread_items.is_empty() {
            text.push_str("\n**Trending Articles**\n\n");
            for item in &mostread_items {
                let line = format!("- {}\n", item);
                if text.len() + line.len() > HEADLINES_BUDGET - 50 {
                    text.push_str("...\n");
                    break;
                }
                text.push_str(&line);
            }
        }

        let trimmed = text.trim_end().to_string();
        println!(
            "[news] get_headlines (Wikimedia) done, {} news + {} trending, {} chars",
            news_items.len(),
            mostread_items.len(),
            trimmed.len()
        );

        // Build UI hint from combined news + trending items
        let mut ui_items: Vec<serde_json::Value> = news_items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "headline": item.trim_start_matches("**Story**: "),
                    "tag": "World",
                    "timeAgo": "Today",
                    "source": "Wikipedia",
                })
            })
            .collect();
        for item in &mostread_items {
            ui_items.push(serde_json::json!({
                "headline": item.trim_start_matches("**").split("**").next().unwrap_or(item),
                "tag": "Trending",
                "timeAgo": "Today",
                "source": "Wikipedia",
            }));
        }

        if !ui_items.is_empty() {
            let ui_data = serde_json::json!({ "items": ui_items });
            let hint = format!("[[[mcp-ui:news:{}]]]\n", ui_data);
            let full_result = format!("{}{}", hint, trimmed);
            return Ok(CallToolResult::success(vec![Content::text(full_result)]));
        }

        Ok(CallToolResult::success(vec![Content::text(trimmed)]))
    }

    /// Fetch today's Wikimedia Featured Content Feed.
    /// Returns the parsed JSON body or a formatted error string.
    async fn fetch_wikimedia_feed(&self) -> Result<serde_json::Value, String> {
        let now = chrono::Utc::now();
        let url = format!(
            "{}/{}/{:02}/{:02}",
            WIKIMEDIA_FEED_URL,
            now.format("%Y"),
            now.format("%m"),
            now.format("%d"),
        );
        println!("[news] GET {} (Wikimedia feed)", url);

        let resp = self
            .http_client
            .get(&url)
            .header(
                "user-agent",
                "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)",
            )
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                println!("[news] Wikimedia feed request failed: {e}");
                crate::format::format_api_error("Wikipedia (news feed)", &e.to_string())
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            println!("[news] Wikimedia feed returned HTTP {status}");
            return Err(crate::format::format_api_error(
                "Wikipedia (news feed)",
                &format!("HTTP {status}"),
            ));
        }

        resp.json::<serde_json::Value>().await.map_err(|e| {
            println!("[news] failed to parse Wikimedia feed response: {e}");
            crate::format::format_api_error("Wikipedia (news feed)", &e.to_string())
        })
    }
}

/// Format a single Wikimedia news story into a display string.
/// Each story has `story` (HTML) and `links` (related articles).
fn format_wikimedia_story(story: &serde_json::Value) -> Option<String> {
    let html = story["story"].as_str()?;
    let clean = strip_html_tags(html);
    if clean.trim().is_empty() {
        return None;
    }

    let related: Vec<&str> = story["links"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .take(3)
                .filter_map(|link| link["titles"]["normalized"].as_str())
                .collect()
        })
        .unwrap_or_default();

    if related.is_empty() {
        Some(format!("**Story**: {}", clean.trim()))
    } else {
        Some(format!(
            "**Story**: {} (Related: {})",
            clean.trim(),
            related.join(", "),
        ))
    }
}

/// Strip HTML tags from a string. Simple char-by-char filter — no external dependency.
fn strip_html_tags(html: &str) -> String {
    let mut result = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(c),
            _ => {}
        }
    }
    result
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Map a user-friendly category name to the Hacker News Firebase endpoint path.
fn resolve_category(input: Option<&str>) -> &'static str {
    match input.map(|s| s.trim().to_lowercase()).as_deref() {
        Some("top") | None | Some("") => "topstories",
        Some("best") => "beststories",
        Some("new") | Some("newest") | Some("latest") => "newstories",
        Some("ask") | Some("askhn") | Some("ask hn") => "askstories",
        Some("show") | Some("showhn") | Some("show hn") => "showstories",
        Some("job") | Some("jobs") | Some("hiring") => "jobstories",
        // Unknown category — fall back to top
        Some(_) => "topstories",
    }
}

/// Extract domain from a URL for compact display.
/// "https://www.example.com/path" -> "example.com"
fn extract_domain(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("")
        .trim_start_matches("www.")
        .to_string()
}

/// Resolve the search query using the standard param fallback chain.
const NEWS_QUERY_SCHEMA: &str = r#"{"type":"object","properties":{"query":{"type":"string","description":"Keywords to search for in news articles"}},"required":["query"]}"#;

async fn resolve_query(params: &SearchNewsParams) -> String {
    // 1. ToolCaller specialist — PRIMARY when configured
    if let Some(args) = crate::generate_params("search_news", NEWS_QUERY_SCHEMA).await {
        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
            let trimmed = q.trim();
            if !trimmed.is_empty() {
                println!("[news] resolve_query: ToolCaller produced: {:?}", trimmed);
                return trimmed.to_string();
            }
        }
    }

    // 2. Model params — direct query field
    if let Some(ref q) = params.query {
        let trimmed = q.trim();
        if !trimmed.is_empty() {
            println!("[news] resolve_query: model param 'query': {:?}", trimmed);
            return trimmed.to_string();
        }
    }

    // 3. Scan extras for common synonyms
    for key in &[
        "query", "q", "search", "topic", "keywords", "term", "text", "subject",
    ] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    println!("[news] resolve_query: extras '{}': {:?}", key, trimmed);
                    return trimmed.to_string();
                }
            }
        }
    }

    // 4. Last resort: clean user message
    let msg = crate::last_user_message();
    if !msg.is_empty() {
        let cleaned = crate::clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            println!(
                "[news] resolve_query: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    println!("[news] resolve_query: no query found");
    String::new()
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use rmcp::ServiceExt;
use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct NewsDeps {
    http_client: reqwest::Client,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
}

static NEWS_DEPS: OnceLock<NewsDeps> = OnceLock::new();

/// Initialize news server dependencies. Call once at startup.
pub fn init_news_deps(
    http_client: reqwest::Client,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
) {
    let _ = NEWS_DEPS.set(NewsDeps {
        http_client,
        settings_repo,
    });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_news_server(reader: DuplexStream, writer: DuplexStream) {
    let deps = NEWS_DEPS.get().expect("init_news_deps() not called");
    let server = NewsMcpServer::new(deps.http_client.clone(), deps.settings_repo.clone());
    tokio::spawn(async move {
        match server.serve((reader, writer)).await {
            Ok(running) => {
                let _ = running.waiting().await;
            }
            Err(e) => tracing::error!("giap-news MCP server failed: {e}"),
        }
    });
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_maps_to_endpoint() {
        assert_eq!(resolve_category(Some("top")), "topstories");
        assert_eq!(resolve_category(Some("best")), "beststories");
        assert_eq!(resolve_category(Some("new")), "newstories");
        assert_eq!(resolve_category(Some("newest")), "newstories");
        assert_eq!(resolve_category(Some("latest")), "newstories");
        assert_eq!(resolve_category(Some("ask")), "askstories");
        assert_eq!(resolve_category(Some("askhn")), "askstories");
        assert_eq!(resolve_category(Some("show")), "showstories");
        assert_eq!(resolve_category(Some("showhn")), "showstories");
        assert_eq!(resolve_category(Some("job")), "jobstories");
        assert_eq!(resolve_category(Some("jobs")), "jobstories");
        assert_eq!(resolve_category(Some("hiring")), "jobstories");
    }

    #[test]
    fn default_category_is_top() {
        assert_eq!(resolve_category(None), "topstories");
        assert_eq!(resolve_category(Some("")), "topstories");
    }

    #[test]
    fn unknown_category_falls_back_to_top() {
        assert_eq!(resolve_category(Some("random")), "topstories");
        assert_eq!(resolve_category(Some("foobar")), "topstories");
    }

    #[test]
    fn category_is_case_insensitive() {
        assert_eq!(resolve_category(Some("TOP")), "topstories");
        assert_eq!(resolve_category(Some("Best")), "beststories");
        assert_eq!(resolve_category(Some("NEW")), "newstories");
        assert_eq!(resolve_category(Some("ASK")), "askstories");
    }

    #[test]
    fn extract_domain_works() {
        assert_eq!(
            extract_domain("https://www.example.com/path"),
            "example.com"
        );
        assert_eq!(extract_domain("https://example.com/foo"), "example.com");
        assert_eq!(
            extract_domain("http://blog.rust-lang.org/2025/post"),
            "blog.rust-lang.org"
        );
        assert_eq!(extract_domain(""), "");
    }

    #[test]
    fn extract_domain_strips_www() {
        assert_eq!(
            extract_domain("https://www.nytimes.com/article"),
            "nytimes.com"
        );
    }

    // resolve_query tests — no ToolCaller set, so falls through to model params
    #[tokio::test]
    async fn resolve_query_from_direct_field() {
        let params = SearchNewsParams {
            query: Some("climate change".to_string()),
            section: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_query(&params).await, "climate change");
    }

    #[tokio::test]
    async fn resolve_query_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "q".to_string(),
            serde_json::Value::String("AI regulation".to_string()),
        );
        let params = SearchNewsParams {
            query: None,
            section: None,
            limit: None,
            extra,
        };
        assert_eq!(resolve_query(&params).await, "AI regulation");
    }

    #[tokio::test]
    async fn resolve_query_empty_when_nothing_provided() {
        let params = SearchNewsParams {
            query: None,
            section: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_query(&params).await, "");
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_hacker_news_top_stories() {
        let client = reqwest::Client::new();
        let ids_url = format!("{}/topstories.json", HN_BASE_URL);
        let resp = client
            .get(&ids_url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("HN request failed");
        assert!(resp.status().is_success());
        let ids: Vec<u64> = resp.json().await.expect("failed to parse HN IDs");
        assert!(!ids.is_empty(), "HN should return at least one story ID");
        println!("HN returned {} story IDs, first: {}", ids.len(), ids[0]);
    }

    #[tokio::test]
    #[ignore] // requires internet + GIAP_GUARDIAN_KEY env var
    async fn live_guardian_search() {
        let key = match std::env::var("GIAP_GUARDIAN_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                println!("skipping: GIAP_GUARDIAN_KEY not set");
                return;
            }
        };
        let client = reqwest::Client::new();
        let url = format!(
            "{}/search?q=technology&api-key={}&show-fields=trailText&page-size=3",
            GUARDIAN_BASE_URL, key,
        );
        let resp = client
            .get(&url)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .expect("Guardian request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp
            .json()
            .await
            .expect("failed to parse Guardian response");
        let results = body["response"]["results"].as_array();
        assert!(results.is_some(), "Guardian should return results array");
        println!("Guardian returned {} results", results.unwrap().len());
    }

    // ── strip_html_tags tests ───────────────────────────────────────────

    #[test]
    fn strip_html_tags_removes_basic_tags() {
        assert_eq!(strip_html_tags("<b>bold</b> text"), "bold text");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<a href=\"url\">link</a>"), "link");
    }

    #[test]
    fn strip_html_tags_handles_empty_and_nested() {
        assert_eq!(strip_html_tags(""), "");
        assert_eq!(strip_html_tags("<div><p>nested</p></div>"), "nested");
        assert_eq!(
            strip_html_tags("<span class=\"x\">styled</span> and plain"),
            "styled and plain"
        );
    }

    #[test]
    fn strip_html_tags_preserves_entities() {
        // HTML entities are not tags — they pass through
        assert_eq!(strip_html_tags("A &amp; B"), "A &amp; B");
    }

    // ── format_wikimedia_story tests ────────────────────────────────────

    #[test]
    fn format_wikimedia_story_with_links() {
        let story = serde_json::json!({
            "story": "<b>Breaking</b>: Something <a href=\"x\">happened</a> today.",
            "links": [
                { "titles": { "normalized": "Event A" } },
                { "titles": { "normalized": "Event B" } },
            ]
        });
        let result = format_wikimedia_story(&story).unwrap();
        assert!(result.contains("Breaking: Something happened today."));
        assert!(result.contains("Related: Event A, Event B"));
    }

    #[test]
    fn format_wikimedia_story_without_links() {
        let story = serde_json::json!({
            "story": "Plain story text.",
            "links": []
        });
        let result = format_wikimedia_story(&story).unwrap();
        assert_eq!(result, "**Story**: Plain story text.");
    }

    #[test]
    fn format_wikimedia_story_returns_none_for_empty() {
        let story = serde_json::json!({ "story": "<br/>" });
        // After stripping HTML, only whitespace remains
        assert!(format_wikimedia_story(&story).is_none());
    }

    // ── Live Wikimedia feed test ────────────────────────────────────────

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_wikimedia_feed_returns_news() {
        let client = reqwest::Client::builder()
            .user_agent("goose-in-a-pond/0.1 (GIAP MCP)")
            .build()
            .unwrap();
        let now = chrono::Utc::now();
        let url = format!(
            "{}/{}/{:02}/{:02}",
            WIKIMEDIA_FEED_URL,
            now.format("%Y"),
            now.format("%m"),
            now.format("%d"),
        );
        let resp = client
            .get(&url)
            .send()
            .await
            .expect("Wikimedia request failed");
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.expect("parse failed");
        let news = body["news"].as_array().expect("news array missing");
        assert!(!news.is_empty(), "should have at least one news item");
        println!("Found {} news stories", news.len());
    }
}
