//! Weather MCP Server — current weather and forecasts.
//!
//! Provides 2 tools: `get_current_weather`, `get_weather_forecast`.
//! Depends on [`WeatherProvider`] (wrapped in `Option` for unconfigured instances).

use pond_adapters_weather::WeatherProvider;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ─────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct WeatherParams {
    pub location: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ForecastParams {
    pub location: Option<String>,
    /// 1-7, default 3.
    pub days: Option<u8>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WeatherMcpServer {
    weather: Option<Arc<dyn WeatherProvider>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WeatherMcpServer {
    /// Every tool this server exposes, without constructing it or its deps.
    ///
    /// `tool_router()` is generated private to this module, so inventory code
    /// outside it could not reach the real definitions and resorted to scanning
    /// source text for `#[tool(` instead. This is the enumeration that scan was
    /// standing in for.
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(weather: Option<Arc<dyn WeatherProvider>>) -> Self {
        Self {
            weather,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
Current weather. Omit location for the configured home rather than asking \
which city. Never guess weather or shell out for it.")]
    async fn get_current_weather(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<WeatherParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_current_weather");
        let location = resolve_location(&params.0);
        tracing::debug!(
            "get_current_weather called, location={:?}, provider={}",
            location,
            if self.weather.is_some() {
                "configured"
            } else {
                "NONE"
            }
        );

        match &self.weather {
            // An error, not a success. The household prompt's one anti-repeat
            // rule is conditioned on a failure ("an error, an empty result or a
            // 'not found' is NOT an answer ... never the same tool with the same
            // parameters again"), so returning a failure as a success put the
            // rule out of scope for exactly the results that needed it.
            None => Ok(CallToolResult::error(vec![Content::text(
                "Weather is not configured on this pond: no location is set in settings. \
                 No other tool, shell command or external request can supply it.",
            )])),
            Some(w) => {
                let result = match &location {
                    Some(loc) => w.current_for(loc).await,
                    None => w.current().await,
                };
                match result {
                    Ok(data) => {
                        tracing::debug!("weather: fetched current for {}", data.location_name);
                        let ui_data = serde_json::json!({
                            "location": data.location_name,
                            "temperature": data.temperature_c,
                            "feels_like": data.feels_like_c,
                            "condition": data.description,
                            "humidity": data.humidity_pct,
                            "wind_speed": data.wind_speed_kmh,
                            "wind_gusts": data.wind_gusts_kmh,
                            "cloud_cover": data.cloud_cover_pct,
                            "precipitation": data.precipitation_mm,
                            "is_day": data.is_day,
                            "sunrise": data.sunrise,
                            "sunset": data.sunset,
                        });
                        let hint = format!("[[[mcp-ui:weather:{}]]]\n", ui_data);
                        let full_result = format!("{}{}", hint, data.as_context_block());
                        Ok(CallToolResult::success(vec![Content::text(full_result)]))
                    }
                    Err(e) => {
                        tracing::warn!("weather: fetch failed: {e}");
                        Ok(CallToolResult::error(vec![Content::text(format!(
                            "Weather fetch failed: {e}"
                        ))]))
                    }
                }
            }
        }
    }

    #[tool(description = "\
Forecast, days 1-7 (default 3): highs/lows, rain chance, UV, sun times. Omit \
location for the configured home. Never guess data.")]
    async fn get_weather_forecast(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ForecastParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_weather_forecast");
        let location = resolve_forecast_location(&params.0);
        let days = params.0.days.unwrap_or(3).clamp(1, 7);

        tracing::debug!(
            "get_weather_forecast called, location={:?}, days={}, provider={}",
            location,
            days,
            if self.weather.is_some() {
                "configured"
            } else {
                "NONE"
            }
        );

        match &self.weather {
            // A failure, reported as one -- see the note on the current-weather
            // tool above.
            None => Ok(CallToolResult::error(vec![Content::text(
                "Weather is not configured on this pond: no location is set in settings. \
                 No other tool, shell command or external request can supply a forecast.",
            )])),
            Some(w) => {
                let result = match &location {
                    Some(loc) => w.forecast_for(loc, days).await,
                    None => w.forecast(days).await,
                };
                match result {
                    Ok(data) => {
                        tracing::debug!(
                            "weather: fetched {}-day forecast for {}",
                            data.days.len(),
                            data.location_name
                        );
                        // Same `[[[mcp-ui:…]]]` marker the current-weather tool
                        // emits. Without it `extract_ui_hint` returns no
                        // `renderHint`, and the desktop deliberately refuses to
                        // render a card it has no structured data for — which is
                        // why a forecast used to arrive as a wall of text next to
                        // a proper weather card.
                        let ui_data = serde_json::json!({
                            "location": data.location_name,
                            "forecast": data.days.iter().map(|d| serde_json::json!({
                                "date": d.date,
                                "description": d.description,
                                "temp_max_c": d.temp_max_c,
                                "temp_min_c": d.temp_min_c,
                                "precipitation_sum_mm": d.precipitation_sum_mm,
                                "precipitation_probability_pct": d.precipitation_probability_pct,
                                "wind_speed_max_kmh": d.wind_speed_max_kmh,
                                "uv_index_max": d.uv_index_max,
                                "sunrise": d.sunrise,
                                "sunset": d.sunset,
                            })).collect::<Vec<_>>(),
                        });
                        let hint = format!("[[[mcp-ui:weather:{}]]]\n", ui_data);
                        let full_result = format!("{}{}", hint, data.as_context_block());
                        Ok(CallToolResult::success(vec![Content::text(full_result)]))
                    }
                    Err(e) => {
                        tracing::warn!("weather: forecast fetch failed: {e}");
                        Ok(CallToolResult::error(vec![Content::text(format!(
                            "Forecast fetch failed: {e}"
                        ))]))
                    }
                }
            }
        }
    }
}

#[tool_handler]
impl ServerHandler for WeatherMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-weather",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Weather MCP server — current conditions and multi-day forecasts.\n\n\
                 Tools:\n\
                 - get_current_weather: temperature, humidity, wind, cloud cover, sunrise/sunset. \
                 Pass 'location' for any city or omit for the user's default.\n\
                 - get_weather_forecast: multi-day forecast with highs/lows, rain chance, UV index. \
                 Pass 'location' and 'days' (1-7).\n\n\
                 If weather is not configured, instructs the user to set up a location in settings.",
            )
    }
}

// ── Param resolution ──────────────────────────────────────────────────────────

/// Extract location from WeatherParams, scanning extras as fallback.
fn resolve_location(params: &WeatherParams) -> Option<String> {
    // Direct param
    if let Some(ref loc) = params.location {
        let trimmed = loc.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    // Scan extras for common synonyms
    for key in &["location", "city", "place", "loc", "where"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

/// Extract location from ForecastParams, scanning extras as fallback.
fn resolve_forecast_location(params: &ForecastParams) -> Option<String> {
    if let Some(ref loc) = params.location {
        let trimmed = loc.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    for key in &["location", "city", "place", "loc", "where"] {
        if let Some(val) = params.extra.get(*key) {
            if let Some(s) = val.as_str() {
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }
    None
}

// ── MCP App resource ─────────────────────────────────────────────────────

/// Self-contained HTML weather card (MCP App).
/// Embedded at compile time — no filesystem access required at runtime.
const WEATHER_APP_HTML: &str = include_str!("../apps/weather-card.html");

/// Resource URI for the weather MCP App.
pub const WEATHER_APP_URI: &str = "ui://giap-weather/weather-card.html";

/// Returns all `(uri, html_content)` pairs for resources served by this MCP server.
pub fn app_resources() -> Vec<(&'static str, &'static str)> {
    vec![(WEATHER_APP_URI, WEATHER_APP_HTML)]
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct WeatherDeps {
    weather: Option<Arc<dyn WeatherProvider>>,
}

static WEATHER_DEPS: OnceLock<WeatherDeps> = OnceLock::new();

/// Initialize weather server dependencies. Call once at startup.
pub fn init_weather_deps(weather: Option<Arc<dyn WeatherProvider>>) {
    let _ = WEATHER_DEPS.set(WeatherDeps { weather });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_weather_server(reader: DuplexStream, writer: DuplexStream) {
    // Missing deps = this path never initialised this extension (the voice/CLI
    // binary vs `serve` install different families). A skipped extension is a
    // logged, contained failure; a panic here took down every builtin server's
    // startup at once (2026-08-27, giap-context in the voice child).
    let Some(deps) = WEATHER_DEPS.get() else {
        tracing::error!(
            "spawn_weather_server called before init_weather_deps — extension will not start"
        );
        return;
    };
    let server = WeatherMcpServer::new(deps.weather.clone());
    crate::serve_builtin("giap-weather", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_constructs_with_none() {
        let _server = WeatherMcpServer::new(None);
    }

    #[test]
    fn resolve_location_from_direct_param() {
        let params = WeatherParams {
            location: Some("Kisumu".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_location(&params), Some("Kisumu".to_string()));
    }

    #[test]
    fn resolve_location_from_extras() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "city".to_string(),
            serde_json::Value::String("Mombasa".to_string()),
        );
        let params = WeatherParams {
            location: None,
            extra,
        };
        assert_eq!(resolve_location(&params), Some("Mombasa".to_string()));
    }

    #[test]
    fn resolve_location_returns_none_for_empty() {
        let params = WeatherParams {
            location: Some("  ".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_location(&params), None);
    }

    #[test]
    fn resolve_location_returns_none_for_no_params() {
        let params = WeatherParams::default();
        assert_eq!(resolve_location(&params), None);
    }

    #[test]
    fn resolve_forecast_location_works() {
        let params = ForecastParams {
            location: Some("Eldoret".to_string()),
            days: Some(5),
            extra: Default::default(),
        };
        assert_eq!(
            resolve_forecast_location(&params),
            Some("Eldoret".to_string())
        );
    }
}

#[cfg(test)]
mod result_wording_tests {
    //! A tool result is data the model reads, and the household prompt tells it
    //! to act on what a result names. So a result that speaks to the model in
    //! the second person about the user is an instruction in all but name --
    //! which is how "suggest they try again shortly" became a reason to try
    //! again, repeatedly.

    /// The tool bodies only -- everything before the first test module.
    ///
    /// The source is the record here because these strings are built inline in
    /// the tool bodies and reaching them needs a live weather service. The cut
    /// matters: without it this scans its own assertion list and fails on the
    /// phrases it is looking for.
    fn tool_bodies() -> &'static str {
        let src = include_str!("weather.rs");
        let end = src.find("#[cfg(test)]").unwrap_or(src.len());
        &src[..end]
    }

    #[test]
    fn no_weather_result_tells_the_model_what_to_tell_the_user() {
        let src = tool_bodies();
        for phrase in [
            "Tell the user",
            "Inform the user",
            "suggest they try again",
            "DO NOT attempt",
        ] {
            assert!(
                !src.contains(phrase),
                "a weather result still instructs the model ({phrase:?}); state the fact and                  let the prompt decide what to do with it"
            );
        }
    }

    /// A failed fetch must be a failed tool result, not a successful one whose
    /// text happens to describe a failure. goose branches on `is_error`, and
    /// the prompt's anti-repeat rule is conditioned on the failure branch.
    #[test]
    fn every_weather_failure_path_returns_an_error_result() {
        let bodies = tool_bodies();
        for needle in [
            "Weather fetch failed",
            "Forecast fetch failed",
            "Weather is not configured on this pond",
        ] {
            let at = bodies.find(needle).expect("failure path missing");
            // Walk back to the CallToolResult constructor for this path.
            let before = &bodies[..at];
            let ctor = before
                .rfind("CallToolResult::")
                .expect("no constructor before the text");
            assert!(
                before[ctor..].starts_with("CallToolResult::error"),
                "the failure path for {needle:?} still returns CallToolResult::success, so the \
                 prompt's anti-repeat rule never applies to it"
            );
        }
    }

    #[test]
    fn an_unconfigured_weather_service_states_the_fact() {
        assert!(tool_bodies().contains("Weather is not configured on this pond"));
    }
}
