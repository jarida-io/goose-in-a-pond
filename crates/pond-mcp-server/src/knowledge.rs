//! Knowledge MCP server: Wikipedia lookup, plus the Wolfram tools from [`crate::wolfram`].

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorCode, ErrorData, Implementation, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

// ── Parameter structs ──────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WikipediaQueryParams {
    /// Name, phrase, or question to look up.
    pub topic: Option<String>,
    /// Max results, default 5, max 10 (search_wikipedia only).
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends (e.g. "query", "title", "search").
    /// Not part of the advertised schema — exists purely to absorb unexpected keys.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DefineWordParams {
    pub word: Option<String>,
    /// Catch-all for any extra fields the model sends (e.g. "term", "query").
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BookSearchParams {
    /// Book title, topic, or keyword.
    pub query: Option<String>,
    /// Filter by author name.
    pub author: Option<String>,
    /// Max results, default 5, max 10.
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── Constants ──────────────────────────────────────────────────────────────

const WIKI_UA: &str =
    "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct KnowledgeMcpServer {
    // `pub(crate)`: the Wolfram router in `wolfram.rs` shares this client pool.
    pub(crate) http_client: reqwest::Client,
    // Read via `router = self.tool_router` (see the `ServerHandler` impl).
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl KnowledgeMcpServer {
    /// Every tool from BOTH routers, without constructing the server or its deps.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        let mut tools = Self::tool_router().list_all();
        tools.extend(Self::wolfram_tool_router().list_all());
        tools
    }

    pub fn new(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            // One extension, two routers: the Wolfram tools live in `wolfram.rs`.
            tool_router: Self::tool_router() + Self::wolfram_tool_router(),
        }
    }

    #[tool(description = "\
Look up a topic on Wikipedia (auto-searches inexact titles). First choice for \
factual questions. Answer in your own words; never repeat the extract \
verbatim.")]
    async fn get_wikipedia_article(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_wikipedia_article");
        eprintln!("[wikipedia] ╔═══ MCP SERVER RECEIVED ═══");
        eprintln!("[wikipedia] ║ params.topic: {:?}", params.0.topic);
        eprintln!("[wikipedia] ║ params.extra: {:?}", params.0.extra);
        eprintln!("[wikipedia] ╚═══════════════════════════");

        let topic = resolve_topic(&params.0, "get_wikipedia_article").await;
        eprintln!(
            "[wikipedia] get_wikipedia_article called: topic={:?}",
            topic
        );

        if topic.is_empty() {
            // Nudge: return guidance as content so the model can retry
            eprintln!("[wikipedia] empty topic, nudging model to retry");
            return Ok(CallToolResult::success(vec![Content::text(
                "I need a topic to look up. Retry this tool with a 'topic' parameter \
                 containing the person, place, event, or concept to search for.",
            )]));
        }

        match self.fetch_article_summary(&topic).await {
            Ok(text) => {
                let full_result = prepend_knowledge_hint(&topic, &text);
                Ok(CallToolResult::success(vec![Content::text(full_result)]))
            }
            Err(WikiFetchError::NotFound) => {
                eprintln!(
                    "[wikipedia] exact title not found, searching for '{}'",
                    topic
                );
                match self.search_and_fetch_best(&topic).await {
                    Ok(text) => {
                        let full_result = prepend_knowledge_hint(&topic, &text);
                        Ok(CallToolResult::success(vec![Content::text(full_result)]))
                    }
                    Err(e) => Err(e),
                }
            }
            Err(WikiFetchError::Mcp(e)) => Err(e),
        }
    }
}

// `router = self.tool_router` is load-bearing: the default is this impl block's router
// alone, which would drop the composed Wolfram tools from `list_tools`.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for KnowledgeMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-knowledge",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Knowledge server — reference lookups, definitions, and computation.\n\n\
                 Tools: get_wikipedia_article (deep articles), \
                 search_wikipedia (find articles), define_word (dictionary), search_books (book search), \
                 compute_answer (Wolfram|Alpha), explore_computation (open a Wolfram suggestion).\n\n\
                 Reading vs computing is the split. If the answer has to be worked out — \
                 arithmetic, a unit or currency conversion, a date difference, a statistic — \
                 use compute_answer. If it has to be read — who someone was, what happened, \
                 what a place is like — use get_wikipedia_article; it auto-searches when the \
                 title is not exact. Use search_wikipedia only to disambiguate between several \
                 possible articles. For word definitions, use define_word. For book queries, \
                 use search_books.\n\
                 A compute_answer result may end with suggestions, each with an id like 'w3'. \
                 When one of them is what the user actually meant, call explore_computation \
                 with that id rather than guessing or re-asking.\n\
                 After receiving results: synthesize in your own words. Do not parrot verbatim.\n\
                 In voice mode: 1-3 sentences. Offer to elaborate if the user wants more.",
            )
    }
}

// ── MCP-UI hint helpers ───────────────────────────────────────────────────

/// Prepend a `[[[mcp-ui:knowledge:{...}]]]` card hint to a Wikipedia article result.
fn prepend_knowledge_hint(topic: &str, text: &str) -> String {
    // Article format: "# Title\n\nExtract...\n\nSource: URL"
    let title = text
        .strip_prefix("# ")
        .and_then(|s| s.split('\n').next())
        .unwrap_or(topic);

    let source_url = text
        .rsplit_once("Source: ")
        .map(|(_, url)| url.trim())
        .unwrap_or("");

    let body_start = text.find("\n\n").map(|i| i + 2).unwrap_or(0);
    let body_end = text.rfind("\n\nSource:").unwrap_or(text.len());
    let summary: String = text[body_start..body_end].chars().take(300).collect();

    let ui_data = serde_json::json!({
        "title": title,
        "summary": summary,
        "source_url": source_url,
        "topic": topic,
    });
    format!("[[[mcp-ui:knowledge:{}]]]\n{}", ui_data, text)
}

// ── Wikipedia helpers (outside the #[tool_router] block) ───────────────────

const WIKI_TOPIC_SCHEMA: &str = r#"{"type":"object","properties":{"topic":{"type":"string","description":"The person, place, event, or concept to look up on Wikipedia"}},"required":["topic"]}"#;

/// Resolve the Wikipedia topic; a configured ToolCaller overrides the model's params.
async fn resolve_topic(params: &WikipediaQueryParams, tool_name: &str) -> String {
    // 1. ToolCaller specialist — PRIMARY when configured
    if let Some(args) = crate::generate_params(tool_name, WIKI_TOPIC_SCHEMA).await {
        if let Some(t) = args.get("topic").and_then(|v| v.as_str()) {
            let trimmed = t.trim();
            if !trimmed.is_empty() {
                eprintln!(
                    "[wikipedia] resolve_topic: ToolCaller produced: {:?}",
                    trimmed
                );
                return trimmed.to_string();
            }
        }
    }

    // 2. No ToolCaller — use model's params directly (capable model path)
    if let Some(ref t) = params.topic {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            eprintln!(
                "[wikipedia] resolve_topic: model param 'topic': {:?}",
                trimmed
            );
            return trimmed.to_string();
        }
    }

    // 3. Scan extras (models send params in unpredictable shapes)
    for key in &[
        "query", "title", "search", "q", "term", "input", "name", "article", "text", "subject",
    ] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    eprintln!("[wikipedia] resolve_topic: extras '{}': {:?}", key, trimmed);
                    return trimmed.to_string();
                }
            }
        }
    }

    // 4. Last resort: clean user message
    let msg = crate::last_user_message();
    if !msg.is_empty() {
        let cleaned = clean_query_for_search(&msg);
        if !cleaned.is_empty() {
            eprintln!(
                "[wikipedia] resolve_topic: user message: {:?} -> {:?}",
                msg, cleaned
            );
            return cleaned;
        }
    }

    eprintln!("[wikipedia] resolve_topic: no topic found");
    String::new()
}

/// Strip question prefixes to get the topic: "who is Wangari Maathai?" -> "Wangari Maathai".
pub fn clean_query_for_search(raw: &str) -> String {
    let stripped = raw
        .trim()
        .trim_end_matches('?')
        .trim_end_matches('.')
        .trim();
    let lower = stripped.to_lowercase();
    // Ordered longest-first so more specific prefixes match before short ones.
    let prefixes = [
        "can you tell me about ",
        "could you tell me about ",
        "tell me about ",
        "tell me more about ",
        "i want to know about ",
        "i'd like to know about ",
        "what do you know about ",
        "what can you tell me about ",
        "would you recommend ",
        "do you recommend ",
        "should i ",
        "how about ",
        "who is ",
        "who was ",
        "who are ",
        "what is ",
        "what are ",
        "what was ",
        "what were ",
        "what is the ",
        "what are the ",
        "where is ",
        "where are ",
        "when was ",
        "when did ",
        "when is ",
        "how does ",
        "how do ",
        "how did ",
        "how is ",
        "why does ",
        "why do ",
        "why is ",
        "why did ",
        "explain ",
        "describe ",
        "look up ",
        "search for ",
        "search ",
        "find ",
        "define ",
    ];
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            return stripped[prefix.len()..].trim().to_string();
        }
    }
    stripped.to_string()
}

// ── Wikipedia fetch internals ──────────────────────────────────────────────

#[derive(Debug)]
pub enum WikiFetchError {
    NotFound,
    Mcp(ErrorData),
}

impl KnowledgeMcpServer {
    /// Full plain-text article for an exact title (MediaWiki `prop=extracts`); empty is `NotFound`.
    pub async fn fetch_article_summary(&self, title: &str) -> Result<String, WikiFetchError> {
        let url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&titles={}&prop=extracts|info&explaintext=1&inprop=url&format=json&redirects=1",
            urlencoding::encode(title),
        );
        eprintln!("[wikipedia] GET {}", url);

        let resp = crate::http::traced_get_with(&self.http_client, &url, |b| {
            b.header("user-agent", WIKI_UA)
                .timeout(std::time::Duration::from_secs(15))
        })
        .await
        .map_err(|e| {
            eprintln!("[wikipedia] article request failed: {e}");
            WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia request failed: {e}"),
                None,
            ))
        })?;

        eprintln!("[wikipedia] article response status: {}", resp.status());

        if !resp.status().is_success() {
            eprintln!(
                "[wikipedia] article fetch failed with HTTP {}",
                resp.status()
            );
            return Err(WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia returned HTTP {}", resp.status()),
                None,
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            eprintln!("[wikipedia] failed to parse article response: {e}");
            WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse response: {e}"),
                None,
            ))
        })?;

        // MediaWiki returns pages as { "query": { "pages": { "<id>": { ... } } } }
        let pages = &body["query"]["pages"];
        let page = pages.as_object().and_then(|m| m.values().next());

        let page = match page {
            Some(p) if p.get("missing").is_none() => p,
            _ => return Err(WikiFetchError::NotFound),
        };

        let display_title = page["title"].as_str().unwrap_or(title);
        let extract = page["extract"].as_str().unwrap_or("");
        let fallback_url = format!(
            "https://en.wikipedia.org/wiki/{}",
            urlencoding::encode(title)
        );
        let page_url = page["fullurl"].as_str().unwrap_or(&fallback_url);

        if extract.is_empty() {
            return Err(WikiFetchError::NotFound);
        }

        // ~1000 tokens: room for the schemas, system prompt and reply on a 16K context.
        let extract = crate::format::truncate_to_budget(extract, 4000);

        eprintln!(
            "[wikipedia] article fetched: title={:?}, extract_len={}",
            display_title,
            extract.len(),
        );

        Ok(format!(
            "# {}\n\n{}\n\nSource: {}",
            display_title, extract, page_url,
        ))
    }

    /// Search Wikipedia and fetch the summary of the best matching article.
    pub async fn search_and_fetch_best(&self, query: &str) -> Result<String, ErrorData> {
        let search_url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=1&format=json",
            urlencoding::encode(query),
        );
        eprintln!("[wikipedia] fallback search: GET {}", search_url);

        let resp = crate::http::traced_get_with(&self.http_client, &search_url, |b| {
            b.header("user-agent", WIKI_UA)
                .timeout(std::time::Duration::from_secs(10))
        })
        .await
        .map_err(|e| {
            eprintln!("[wikipedia] fallback search request failed: {e}");
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia search failed: {e}"),
                None,
            )
        })?;

        if !resp.status().is_success() {
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia search returned HTTP {}", resp.status()),
                None,
            ));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse search: {e}"),
                None,
            )
        })?;

        let best_title = body["query"]["search"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item["title"].as_str());

        match best_title {
            Some(found) => {
                eprintln!("[wikipedia] fallback found: '{}'", found);
                match self.fetch_article_summary(found).await {
                    Ok(text) => Ok(text),
                    Err(WikiFetchError::NotFound) => Ok(format!(
                        "Wikipedia search matched '{}' but the article could not be loaded.",
                        found
                    )),
                    Err(WikiFetchError::Mcp(e)) => Err(e),
                }
            }
            None => {
                eprintln!(
                    "[wikipedia] fallback search returned no results for '{}'",
                    query
                );
                Ok(crate::format::format_no_results(
                    &format!("Wikipedia articles for '{}'", query),
                    &["giap-knowledge__compute_answer"],
                ))
            }
        }
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct KnowledgeDeps {
    http_client: reqwest::Client,
}

static KNOWLEDGE_DEPS: OnceLock<KnowledgeDeps> = OnceLock::new();

/// Initialize knowledge server dependencies. Call once at startup.
pub fn init_knowledge_deps(http_client: reqwest::Client) {
    let _ = KNOWLEDGE_DEPS.set(KnowledgeDeps { http_client });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_knowledge_server(reader: DuplexStream, writer: DuplexStream) {
    // No deps = this binary didn't install this family: skip, as a panic kills every builtin.
    let Some(deps) = KNOWLEDGE_DEPS.get() else {
        tracing::error!(
            "spawn_knowledge_server called before init_knowledge_deps — extension will not start"
        );
        return;
    };
    let server = KnowledgeMcpServer::new(deps.http_client.clone());
    crate::serve_builtin("giap-knowledge", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_server() -> KnowledgeMcpServer {
        KnowledgeMcpServer::new(reqwest::Client::new())
    }

    #[test]
    fn clean_query_strips_prefixes() {
        assert_eq!(
            clean_query_for_search("who is Wangari Maathai?"),
            "Wangari Maathai"
        );
        assert_eq!(
            clean_query_for_search("tell me about black holes"),
            "black holes"
        );
        assert_eq!(clean_query_for_search("Nairobi"), "Nairobi");
        assert_eq!(
            clean_query_for_search("what is the speed of light?"),
            "the speed of light"
        );
    }

    // resolve_topic tests — no ToolCaller set, so it falls through to model params
    #[tokio::test]
    async fn resolve_topic_from_canonical_field() {
        let params = WikipediaQueryParams {
            topic: Some("Nairobi".to_string()),
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_topic(&params, "test").await, "Nairobi");
    }

    #[tokio::test]
    async fn resolve_topic_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "query".to_string(),
            serde_json::Value::String("black holes".to_string()),
        );
        let params = WikipediaQueryParams {
            topic: None,
            limit: None,
            extra,
        };
        assert_eq!(resolve_topic(&params, "test").await, "black holes");
    }

    #[tokio::test]
    async fn resolve_topic_empty_when_nothing_provided() {
        let params = WikipediaQueryParams {
            topic: None,
            limit: None,
            extra: Default::default(),
        };
        assert_eq!(resolve_topic(&params, "test").await, "");
    }

    #[test]
    fn server_constructs() {
        let _server = test_server();
    }

    #[test]
    fn prepend_knowledge_hint_extracts_source_url() {
        let text = "# Nairobi\n\nNairobi is the capital of Kenya.\n\nSource: https://en.wikipedia.org/wiki/Nairobi";
        let result = prepend_knowledge_hint("Nairobi", text);
        assert!(result.contains("\"source_url\":\"https://en.wikipedia.org/wiki/Nairobi\""));
    }

    #[test]
    fn prepend_knowledge_hint_missing_source_uses_empty_string() {
        let text = "# Test\n\nSome article with no source line.";
        let result = prepend_knowledge_hint("Test", text);
        assert!(result.contains("\"source_url\":\"\""));
    }

    #[test]
    fn fetch_article_truncation_is_applied() {
        let long_extract = "x".repeat(5000);
        let truncated = crate::format::truncate_to_budget(&long_extract, 4000);
        assert!(truncated.len() < 5000);
        assert!(truncated.contains("[Truncated"));
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_fetch_exact_title() {
        let server = test_server();
        let text = server.fetch_article_summary("Nairobi").await.unwrap();
        eprintln!("{}", text);
        assert!(text.contains("Nairobi"), "extract should mention Nairobi");
        assert!(
            text.contains("Kenya"),
            "Nairobi article should mention Kenya"
        );
        assert!(text.contains("Source:"), "should include source URL");
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_vague_query_finds_article() {
        let server = test_server();
        let text = server.search_and_fetch_best("black holes").await.unwrap();
        eprintln!("{}", text);
        assert!(
            text.contains("black hole") || text.contains("Black hole"),
            "should find the Black hole article"
        );
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_get_article_auto_resolves_vague_topic() {
        let server = test_server();
        let text = server.search_and_fetch_best("volcanoes").await.unwrap();
        eprintln!("{}", text);
        assert!(
            text.to_lowercase().contains("volcan"),
            "should resolve to a volcano-related article"
        );
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_nonsense_query_returns_not_found() {
        let server = test_server();
        let text = server
            .search_and_fetch_best("xyzzy99foobar_nonexistent")
            .await
            .unwrap();
        eprintln!("{}", text);
        assert!(
            text.contains("No Wikipedia articles found"),
            "should report no results for nonsense query"
        );
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_misspelled_topic_resolved() {
        let server = test_server();
        let text = server
            .search_and_fetch_best("Albert Einsten")
            .await
            .unwrap();
        eprintln!("{}", text);
        let lower = text.to_lowercase();
        assert!(
            lower.contains("einstein")
                || lower.contains("physicist")
                || lower.contains("relativity"),
            "should resolve misspelled 'Albert Einsten' to Einstein article"
        );
    }

    // ── query-cleaning tests ──────────────────────────────────────────────

    #[test]
    fn clean_query_strips_define_prefix() {
        assert_eq!(clean_query_for_search("define serendipity"), "serendipity");
        // "what does" is not a stripped prefix, so only the "?" goes.
        assert_eq!(
            clean_query_for_search("what does ephemeral mean?"),
            "what does ephemeral mean"
        );
        assert_eq!(clean_query_for_search("what is ephemeral?"), "ephemeral");
    }

    #[tokio::test]
    #[ignore] // requires internet
    async fn live_search_books_returns_results() {
        let server = test_server();
        let url = "https://openlibrary.org/search.json?q=Dune+Frank+Herbert&limit=3";
        let resp = server.http_client.get(url).send().await.unwrap();
        assert!(resp.status().is_success());
        let body: serde_json::Value = resp.json().await.unwrap();
        let docs = body["docs"].as_array().expect("should have docs array");
        eprintln!("search_books found {} results", docs.len());
        assert!(!docs.is_empty(), "should find Dune books");
        let first_title = docs[0]["title"].as_str().unwrap_or("");
        eprintln!("first result: {}", first_title);
        assert!(
            first_title.to_lowercase().contains("dune"),
            "first result should be a Dune book"
        );
    }
}
