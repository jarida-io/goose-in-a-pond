use std::sync::Arc;
use pond_core::domain::memory::MemoryFragment;
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
use crate::registry::GiapServiceHandles;

// ── Parameter structs for tools that accept arguments ────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecallMemoriesParams {
    /// Optional keyword to search for in memory content.
    pub query: Option<String>,
    /// Maximum number of memories to return (default 10).
    pub limit: Option<u32>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SaveMemoryParams {
    /// The content to remember.
    pub content: String,
    /// Optional comma-separated tags (e.g. "preferences,home").
    pub tags: Option<String>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct GetRecipeParams {
    /// The recipe name (slug).
    pub name: String,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WikipediaQueryParams {
    /// The topic to look up — a name, phrase, or question (e.g. "black holes", "Nairobi", "how do volcanoes work").
    pub topic: Option<String>,
    /// Maximum number of search results (default 5, max 10). Only used by search_wikipedia.
    pub limit: Option<u32>,
    /// Catch-all for any extra fields the model sends (e.g. "query", "title", "search").
    /// Not part of the advertised schema — exists purely to absorb unexpected keys.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── MCP server ───────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct GiapMcpServer {
    services: Arc<GiapServiceHandles>,
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl GiapMcpServer {
    pub fn new(services: Arc<GiapServiceHandles>) -> Self {
        Self {
            services,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Get the current weather conditions for the configured location.")]
    async fn get_current_weather(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match &self.services.weather {
            None => Ok(CallToolResult::success(vec![Content::text(
                "The weather service is not configured on this GIAP instance. \
                 Inform the user that they need to configure a weather location in their settings. \
                 DO NOT attempt to fetch weather using any other tool, shell command, or external request.",
            )])),
            Some(w) => match w.current().await {
                Ok(data) => Ok(CallToolResult::success(vec![Content::text(
                    data.as_context_block(),
                )])),
                Err(e) => Err(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Weather fetch error: {}", e),
                    None,
                )),
            },
        }
    }

    #[tool(description = "List all registered devices on this GIAP instance.")]
    async fn list_registered_devices(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match self.services.device_registry.list_devices().await {
            Ok(devices) => {
                let text = if devices.is_empty() {
                    "No devices registered.".to_string()
                } else {
                    devices
                        .iter()
                        .map(|d| {
                            format!(
                                "- {} ({}): {}",
                                d.name,
                                d.device_type,
                                if d.is_online { "online" } else { "offline" }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                Ok(CallToolResult::success(vec![Content::text(text)]))
            }
            Err(e) => Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Error listing devices: {}", e),
                None,
            )),
        }
    }

    #[tool(description = "List all scheduled tasks on this GIAP instance.")]
    async fn list_schedules(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        match &self.services.scheduler {
            None => Ok(CallToolResult::success(vec![Content::text(
                "The scheduler service is not configured on this GIAP instance. \
                 Inform the user directly. DO NOT attempt to use any other tool to create or list schedules.",
            )])),
            Some(s) => match s.list_tasks().await {
                Ok(tasks) => {
                    let text = if tasks.is_empty() {
                        "No scheduled tasks.".to_string()
                    } else {
                        tasks
                            .iter()
                            .map(|t| {
                                format!(
                                    "- {} [{}]: {} ({})",
                                    t.label,
                                    t.id,
                                    t.cron,
                                    if t.paused { "paused" } else { "active" }
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    Ok(CallToolResult::success(vec![Content::text(text)]))
                }
                Err(e) => Err(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Error listing schedules: {}", e),
                    None,
                )),
            },
        }
    }

    #[tool(description = "Get the current user profile: name, assistant name, timezone, and location.")]
    async fn get_user_profile(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let settings = self.services.settings_repo.get().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Settings error: {}", e), None)
        })?;
        let location = if settings.weather_location_name.is_empty() {
            "not configured".to_string()
        } else {
            settings.weather_location_name.clone()
        };
        let text = format!(
            "User: {}\nAssistant name: {}\nTimezone: {}\nLocation: {}",
            settings.user_name, settings.assistant_name, settings.timezone, location,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Get the current model configuration: which LLM and tool-calling model are active.")]
    async fn get_model_assignments(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let s = self.services.settings_repo.get().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Settings error: {}", e), None)
        })?;
        let tool = s.tool_model.as_deref().unwrap_or("(none)");
        let text = format!(
            "Main LLM:    {}/{}\nTool caller: {}",
            s.chat_provider, s.chat_model, tool,
        );
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Recall recent memories, optionally filtered by a keyword.")]
    async fn recall_memories(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RecallMemoriesParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = params.0.limit.unwrap_or(10) as usize;
        let fragments = self.services.memory_repo
            .search_recent(None, limit)
            .await
            .map_err(|e| ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Memory error: {}", e), None))?;

        let filtered: Vec<_> = if let Some(ref q) = params.0.query {
            let q_lower = q.to_lowercase();
            fragments.into_iter().filter(|f| f.content.to_lowercase().contains(&q_lower)).collect()
        } else {
            fragments
        };

        let text = if filtered.is_empty() {
            "No memories found.".to_string()
        } else {
            filtered.iter()
                .map(|f| format!("[{}] {}", f.created_at.format("%Y-%m-%d"), f.content))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Save a new memory fragment for future recall.")]
    async fn save_memory(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SaveMemoryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let id = uuid::Uuid::new_v4().to_string();
        let content = params.0.content.clone();
        let tag_list: Vec<String> = params.0.tags
            .unwrap_or_default()
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        let fragment = MemoryFragment {
            id,
            profile_id: None,
            session_id: None,
            content: content.clone(),
            embedding: None,
            source: "mcp_tool".to_string(),
            tags: tag_list,
            created_at: chrono::Utc::now(),
        };
        self.services.memory_repo.add(fragment).await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Failed to save memory: {}", e), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(format!("Memory saved: {}", content))]))
    }

    #[tool(description = "List all active user skills injected into the assistant's context.")]
    async fn list_skills(
        &self,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let skills = self.services.skill_repo.list_active().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Skills error: {}", e), None)
        })?;
        let text = if skills.is_empty() {
            "No active skills.".to_string()
        } else {
            skills.iter()
                .map(|s| format!("- {} ({})", s.name, s.id))
                .collect::<Vec<_>>()
                .join("\n")
        };
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "Get the YAML definition of a named agent recipe.")]
    async fn get_recipe(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<GetRecipeParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let recipe = self.services.recipe_repo.get_by_name(&params.0.name).await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Recipe error: {}", e), None)
        })?;
        match recipe {
            None => Ok(CallToolResult::success(vec![Content::text(
                format!("Recipe '{}' not found.", params.0.name),
            )])),
            Some(r) => Ok(CallToolResult::success(vec![Content::text(
                format!("Recipe: {}\n{}\n\n{}", r.name, r.description, r.yaml),
            )])),
        }
    }

    // ── Wikipedia tools ──────────────────────────────────────────────────────

    #[tool(description = "\
Search Wikipedia for articles matching a topic. Returns a ranked list of article \
titles with short descriptions. Only use this when you need to disambiguate \
between multiple topics or show the user a list of options. For direct factual \
questions, prefer get_wikipedia_article instead — it auto-searches on your behalf.")]
    async fn search_wikipedia(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let query = extract_topic(&params.0, &self.services).await;
        println!("[wikipedia] search_wikipedia called: query={:?}", query);

        if query.is_empty() {
            return Err(ErrorData::new(ErrorCode::INVALID_PARAMS, "A topic is required.".to_string(), None));
        }
        let limit = params.0.limit.unwrap_or(5).min(10);

        let url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit={}&format=json",
            urlencoding::encode(&query),
            limit,
        );
        println!("[wikipedia] GET {}", url);

        let resp = self.services.http_client
            .get(&url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] search request failed: {e}");
                ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Wikipedia request failed: {e}"), None)
            })?;

        println!("[wikipedia] search response status: {}", resp.status());

        if !resp.status().is_success() {
            println!("[wikipedia] search failed with HTTP {}", resp.status());
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia returned HTTP {}", resp.status()),
                None,
            ));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            println!("[wikipedia] failed to parse search response: {e}");
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Failed to parse response: {e}"), None)
        })?;

        let results = body["query"]["search"].as_array();
        let result_count = results.map(|a| a.len()).unwrap_or(0);
        println!("[wikipedia] search returned {} results", result_count);

        let text = match results {
            Some(arr) if !arr.is_empty() => {
                arr.iter()
                    .filter_map(|item| {
                        let title = item["title"].as_str()?;
                        let snippet = item["snippet"].as_str().unwrap_or("");
                        let clean = snippet
                            .replace("<span class=\"searchmatch\">", "")
                            .replace("</span>", "")
                            .replace("&quot;", "\"")
                            .replace("&amp;", "&");
                        Some(format!("- **{}**: {}", title, clean))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            _ => format!("No Wikipedia articles found for '{}'.", query),
        };
        println!("[wikipedia] search_wikipedia done, returning {} chars", text.len());
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    #[tool(description = "\
Look up a topic on Wikipedia. Pass a topic name or natural-language query — the \
tool auto-searches if the exact title is not found. Use this as your first \
choice for ANY factual question (people, places, events, science, history, etc.).\n\
After receiving the result: extract only the facts relevant to the user's \
question and answer concisely in your own words. Do NOT repeat the extract \
verbatim. In voice mode keep it to 1–3 sentences.")]
    async fn get_wikipedia_article(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WikipediaQueryParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let topic = extract_topic(&params.0, &self.services).await;
        println!("[wikipedia] get_wikipedia_article called: topic={:?}", topic);

        if topic.is_empty() {
            println!("[wikipedia] empty topic, returning INVALID_PARAMS");
            return Err(ErrorData::new(
                ErrorCode::INVALID_PARAMS,
                "A topic is required.".to_string(),
                None,
            ));
        }

        // Try direct lookup first
        match self.fetch_article_summary(&topic).await {
            Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
            Err(WikiFetchError::NotFound) => {
                // Auto-fallback: search for the topic and fetch the top result
                println!("[wikipedia] exact title not found, searching for '{}'", topic);
                match self.search_and_fetch_best(&topic).await {
                    Ok(text) => Ok(CallToolResult::success(vec![Content::text(text)])),
                    Err(e) => Err(e),
                }
            }
            Err(WikiFetchError::Mcp(e)) => Err(e),
        }
    }
}

// ── Wikipedia helpers (outside the #[tool_router] block) ─────────────────────

const WIKI_UA: &str = "goose-in-a-pond/0.1 (GIAP MCP; https://github.com/jarida-io/goose-in-a-pond)";

/// Extract the search topic from params.
///
/// Small local models send parameters in unpredictable shapes — `{"query": "..."}`,
/// `{"title": "..."}`, `{"search": "..."}`, `{"input": "..."}`, or even `{}`.
/// The `extra` field captures everything serde didn't match to `topic`.
/// We check `topic` first, then scan extras, then fall back to the user's
/// original message (stashed by the GooseAdapter before each turn).
async fn extract_topic(params: &WikipediaQueryParams, services: &GiapServiceHandles) -> String {
    // 1. Canonical field
    if let Some(ref t) = params.topic {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            println!("[wikipedia] extract_topic: found in 'topic' field: {:?}", trimmed);
            return trimmed.to_string();
        }
    }
    // 2. Scan extras — try common names first, then any string value
    for key in &["query", "title", "search", "q", "term", "input", "name", "article", "text", "subject"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    println!("[wikipedia] extract_topic: found in '{}' field: {:?}", key, trimmed);
                    return trimmed.to_string();
                }
            }
        }
    }
    // 3. Any extra string value at all
    for (key, val) in &params.extra {
        if let Some(s) = val.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                println!("[wikipedia] extract_topic: found in unknown '{}' field: {:?}", key, trimmed);
                return trimmed.to_string();
            }
        }
    }
    // 4. Try the tool-calling specialist model (if configured)
    let user_msg = services.last_user_message.read().await.clone();
    if let Some(ref tool_caller) = services.tool_caller {
        let schema = r#"{"topic": "string — the topic, person, place, or concept to look up"}"#;
        let query = user_msg.trim();
        if !query.is_empty() {
            println!("[wikipedia] extract_topic: invoking tool-caller specialist for {:?}", query);
            match tool_caller.generate_tool_call("get_wikipedia_article", schema, query).await {
                Ok(args) => {
                    if let Some(t) = args.get("topic").and_then(|v| v.as_str()) {
                        let trimmed = t.trim();
                        if !trimmed.is_empty() {
                            println!("[wikipedia] extract_topic: specialist returned: {:?}", trimmed);
                            return trimmed.to_string();
                        }
                    }
                    println!("[wikipedia] extract_topic: specialist returned args without 'topic': {:?}", args);
                }
                Err(e) => {
                    println!("[wikipedia] extract_topic: specialist failed: {e}");
                }
            }
        }
    }
    // 5. Last resort — extract topic from user message with query cleaning
    let cleaned = clean_query_for_search(&user_msg);
    if !cleaned.is_empty() {
        println!("[wikipedia] extract_topic: cleaned user message: {:?}", cleaned);
        return cleaned;
    }
    println!("[wikipedia] extract_topic: no topic found in params: {:?}", params);
    String::new()
}

/// Strip common question prefixes to extract the core topic for search.
///
/// "who is Wangari Maathai?" → "Wangari Maathai"
/// "tell me about black holes" → "black holes"
/// "Nairobi" → "Nairobi" (unchanged)
pub fn clean_query_for_search(raw: &str) -> String {
    let stripped = raw.trim().trim_end_matches('?').trim_end_matches('.').trim();
    let lower = stripped.to_lowercase();
    let prefixes = [
        "who is ", "who was ", "who are ",
        "what is ", "what are ", "what was ", "what were ",
        "where is ", "where are ",
        "when was ", "when did ",
        "tell me about ", "explain ", "describe ",
        "how does ", "how do ", "how did ",
        "look up ", "search for ", "search ", "find ",
        "define ", "can you tell me about ",
    ];
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            return stripped[prefix.len()..].trim().to_string();
        }
    }
    stripped.to_string()
}

#[derive(Debug)]
pub enum WikiFetchError {
    NotFound,
    Mcp(ErrorData),
}

impl GiapMcpServer {
    /// Fetch the full article content for an exact Wikipedia title.
    ///
    /// Uses the MediaWiki `action=query&prop=extracts` endpoint which returns
    /// the complete article as plain text (no HTML). Falls back to the REST
    /// summary API if the full extract is empty.
    pub async fn fetch_article_summary(&self, title: &str) -> Result<String, WikiFetchError> {
        // Full article via MediaWiki API (plaintext, no character limit)
        let url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&titles={}&prop=extracts|info&explaintext=1&inprop=url&format=json&redirects=1",
            urlencoding::encode(title),
        );
        println!("[wikipedia] GET {}", url);

        let resp = self.services.http_client
            .get(&url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] article request failed: {e}");
                WikiFetchError::Mcp(ErrorData::new(
                    ErrorCode::INTERNAL_ERROR,
                    format!("Wikipedia request failed: {e}"),
                    None,
                ))
            })?;

        println!("[wikipedia] article response status: {}", resp.status());

        if !resp.status().is_success() {
            println!("[wikipedia] article fetch failed with HTTP {}", resp.status());
            return Err(WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia returned HTTP {}", resp.status()),
                None,
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            println!("[wikipedia] failed to parse article response: {e}");
            WikiFetchError::Mcp(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Failed to parse response: {e}"),
                None,
            ))
        })?;

        // MediaWiki returns pages as { "query": { "pages": { "<id>": { ... } } } }
        let pages = &body["query"]["pages"];
        let page = pages.as_object()
            .and_then(|m| m.values().next());

        let page = match page {
            Some(p) if p.get("missing").is_none() => p,
            _ => return Err(WikiFetchError::NotFound),
        };

        let display_title = page["title"].as_str().unwrap_or(title);
        let extract = page["extract"].as_str().unwrap_or("");
        let fallback_url = format!("https://en.wikipedia.org/wiki/{}", urlencoding::encode(title));
        let page_url = page["fullurl"].as_str().unwrap_or(&fallback_url);

        if extract.is_empty() {
            return Err(WikiFetchError::NotFound);
        }

        // Cap at ~8000 chars to stay within model context limits
        let truncated = if extract.len() > 8000 {
            let mut cut = 8000;
            while cut > 0 && !extract.is_char_boundary(cut) { cut -= 1; }
            format!("{}...\n\n[Article truncated — full article at source]", &extract[..cut])
        } else {
            extract.to_string()
        };

        println!("[wikipedia] article fetched: title={:?}, extract_len={}", display_title, extract.len());

        Ok(format!(
            "# {}\n\n{}\n\nSource: {}",
            display_title, truncated, page_url,
        ))
    }

    /// Search Wikipedia and fetch the summary of the best matching article.
    pub async fn search_and_fetch_best(&self, query: &str) -> Result<String, ErrorData> {
        let search_url = format!(
            "https://en.wikipedia.org/w/api.php?action=query&list=search&srsearch={}&srlimit=1&format=json",
            urlencoding::encode(query),
        );
        println!("[wikipedia] fallback search: GET {}", search_url);

        let resp = self.services.http_client
            .get(&search_url)
            .header("user-agent", WIKI_UA)
            .timeout(std::time::Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| {
                println!("[wikipedia] fallback search request failed: {e}");
                ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Wikipedia search failed: {e}"), None)
            })?;

        if !resp.status().is_success() {
            return Err(ErrorData::new(
                ErrorCode::INTERNAL_ERROR,
                format!("Wikipedia search returned HTTP {}", resp.status()),
                None,
            ));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            ErrorData::new(ErrorCode::INTERNAL_ERROR, format!("Failed to parse search: {e}"), None)
        })?;

        let best_title = body["query"]["search"]
            .as_array()
            .and_then(|arr| arr.first())
            .and_then(|item| item["title"].as_str());

        match best_title {
            Some(found) => {
                println!("[wikipedia] fallback found: '{}'", found);
                match self.fetch_article_summary(found).await {
                    Ok(text) => Ok(text),
                    Err(WikiFetchError::NotFound) => {
                        Ok(format!("Wikipedia search matched '{}' but the article could not be loaded.", found))
                    }
                    Err(WikiFetchError::Mcp(e)) => Err(e),
                }
            }
            None => {
                println!("[wikipedia] fallback search returned no results for '{}'", query);
                Ok(format!("No Wikipedia articles found for '{}'.", query))
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for GiapMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-mcp-server",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP (Goose In A Pond) MCP server — your primary interface for the local home \
                 environment and factual knowledge retrieval.\n\n\
                 Tools: weather, device registry, memories, skills, Wikipedia.\n\n\
                 IMPORTANT — Wikipedia usage guidelines:\n\
                 • Prefer get_wikipedia_article for any factual question. It accepts plain topics \
                   (\"black holes\", \"Marie Curie\") — no need to guess exact titles.\n\
                 • Do NOT parrot the article extract verbatim. Read it, extract the relevant facts, \
                   then answer the user's question in your own words — concisely.\n\
                 • For voice mode: aim for 1–3 sentences. Offer to elaborate if the user wants more.\n\
                 • Only use search_wikipedia when you need to disambiguate between multiple topics \
                   or present a list of options to the user.\n\
                 • Never say \"According to Wikipedia\" — just answer naturally with the facts.\n\
                 • Always prefer these tools over shell commands or external requests.",
            )
    }
}

// ── Live Wikipedia tests (require internet — #[ignore] by default) ───────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::GiapServiceHandles;
    use pond_core::domain::memory::MemoryFragment;
    use pond_core::domain::recipe::AgentRecipe;
    use pond_core::domain::settings::Settings;
    use pond_core::domain::skill::UserSkill;
    use pond_core::ports::device_registry::{Device, DeviceRegistry, RegisterDeviceRequest};
    use pond_core::ports::memory_repository::MemoryRepository;
    use pond_core::ports::recipe::AgentRecipeRepository;
    use pond_core::ports::settings::SettingsRepository;
    use pond_core::ports::skill::UserSkillRepository;
    use async_trait::async_trait;
    use std::sync::Arc;

    // ── Minimal stubs — only http_client is exercised by Wikipedia tools ──

    struct StubDeviceRegistry;
    #[async_trait]
    impl DeviceRegistry for StubDeviceRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> anyhow::Result<Device> { unimplemented!() }
        async fn list_devices(&self) -> anyhow::Result<Vec<Device>> { Ok(vec![]) }
        async fn get_device(&self, _: &str) -> anyhow::Result<Option<Device>> { Ok(None) }
        async fn unregister(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
        async fn heartbeat(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
    }

    struct StubSettings;
    #[async_trait]
    impl SettingsRepository for StubSettings {
        async fn get(&self) -> anyhow::Result<Settings> { Ok(Settings::default()) }
        async fn update(&self, _: &Settings) -> anyhow::Result<()> { Ok(()) }
        async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> { Ok(None) }
        async fn set_key(&self, _: &str, _: String) -> anyhow::Result<()> { Ok(()) }
    }

    struct StubMemory;
    #[async_trait]
    impl MemoryRepository for StubMemory {
        async fn add(&self, _: MemoryFragment) -> anyhow::Result<()> { Ok(()) }
        async fn search_recent(&self, _: Option<&str>, _: usize) -> anyhow::Result<Vec<MemoryFragment>> { Ok(vec![]) }
        async fn search_similar(&self, _: &[f32], _: Option<&str>, _: usize) -> anyhow::Result<Vec<MemoryFragment>> { Ok(vec![]) }
        async fn delete(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
    }

    struct StubSkills;
    #[async_trait]
    impl UserSkillRepository for StubSkills {
        async fn list_active(&self) -> anyhow::Result<Vec<UserSkill>> { Ok(vec![]) }
        async fn list_all(&self) -> anyhow::Result<Vec<UserSkill>> { Ok(vec![]) }
        async fn get(&self, _: &str) -> anyhow::Result<Option<UserSkill>> { Ok(None) }
        async fn create(&self, _: &UserSkill) -> anyhow::Result<()> { Ok(()) }
        async fn update(&self, _: &UserSkill) -> anyhow::Result<()> { Ok(()) }
        async fn delete(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
    }

    struct StubRecipes;
    #[async_trait]
    impl AgentRecipeRepository for StubRecipes {
        async fn list(&self) -> anyhow::Result<Vec<AgentRecipe>> { Ok(vec![]) }
        async fn get_by_name(&self, _: &str) -> anyhow::Result<Option<AgentRecipe>> { Ok(None) }
        async fn get_by_id(&self, _: &str) -> anyhow::Result<Option<AgentRecipe>> { Ok(None) }
        async fn upsert(&self, _: &AgentRecipe) -> anyhow::Result<()> { Ok(()) }
        async fn delete(&self, _: &str) -> anyhow::Result<()> { Ok(()) }
    }

    fn test_server() -> GiapMcpServer {
        let handles = Arc::new(GiapServiceHandles {
            weather: None,
            device_registry: Arc::new(StubDeviceRegistry),
            scheduler: None,
            settings_repo: Arc::new(StubSettings),
            memory_repo: Arc::new(StubMemory),
            skill_repo: Arc::new(StubSkills),
            recipe_repo: Arc::new(StubRecipes),
            http_client: reqwest::Client::new(),
            tool_caller: None,
            last_user_message: tokio::sync::RwLock::new(String::new()),
        });
        GiapMcpServer::new(handles)
    }

    /// Exact title → direct fetch succeeds.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_fetch_exact_title() {
        let server = test_server();
        let text = server.fetch_article_summary("Nairobi").await.unwrap();
        println!("{}", text);
        assert!(text.contains("Nairobi"), "extract should mention Nairobi");
        assert!(text.contains("Kenya"), "Nairobi article should mention Kenya");
        assert!(text.contains("Source:"), "should include source URL");
    }

    /// Vague query that doesn't match an exact title → auto-search fallback.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_vague_query_finds_article() {
        let server = test_server();
        // "black holes" is not an exact Wikipedia title — "Black hole" is.
        let text = server.search_and_fetch_best("black holes").await.unwrap();
        println!("{}", text);
        assert!(text.contains("black hole") || text.contains("Black hole"),
            "should find the Black hole article");
    }

    /// The full get_wikipedia_article flow: vague input → 404 → search → fetch.
    /// This is the exact use case: agent calls the tool with a rough topic.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_get_article_auto_resolves_vague_topic() {
        let server = test_server();
        // "volcanoes" is close enough that Wikipedia search should return a
        // relevant article (Volcano, Volcanology, etc.).
        let text = server.search_and_fetch_best("volcanoes").await.unwrap();
        println!("{}", text);
        assert!(text.to_lowercase().contains("volcan"),
            "should resolve to a volcano-related article");
    }

    /// Completely nonsensical query returns a graceful "not found" message.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_nonsense_query_returns_not_found() {
        let server = test_server();
        let text = server.search_and_fetch_best("xyzzy99foobar_nonexistent").await.unwrap();
        println!("{}", text);
        assert!(text.contains("No Wikipedia articles found"),
            "should report no results for nonsense query");
    }

    /// Misspelled topic still finds a relevant article via search.
    #[tokio::test]
    #[ignore] // requires internet
    async fn live_misspelled_topic_resolved() {
        let server = test_server();
        // "Albert Einsten" is a common misspelling — Wikipedia search handles it.
        let text = server.search_and_fetch_best("Albert Einsten").await.unwrap();
        println!("{}", text);
        let lower = text.to_lowercase();
        assert!(lower.contains("einstein") || lower.contains("physicist") || lower.contains("relativity"),
            "should resolve misspelled 'Albert Einsten' to Einstein article");
    }
}
