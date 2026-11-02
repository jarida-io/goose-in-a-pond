//! Weather MCP server: current weather and forecasts.

use pond_adapters_weather::{ForecastData, WeatherProvider};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorData, Implementation, InitializeResult, ProtocolVersion,
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
    /// Days asked for, 1-7 (omitted: 3).
    pub days: Option<u8>,
    /// One day, YYYY-MM-DD; overrides `days`.
    pub date: Option<String>,
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
    /// Every tool this server exposes, without constructing it (`tool_router()` is private).
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
            // An error, not a success: the prompt's anti-repeat rule only applies to failures.
            None => Ok(CallToolResult::error(vec![ContentBlock::text(
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
                        Ok(CallToolResult::success(vec![ContentBlock::text(
                            full_result,
                        )]))
                    }
                    Err(e) => {
                        tracing::warn!("weather: fetch failed: {e}");
                        Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                            "Weather fetch failed: {e}"
                        ))]))
                    }
                }
            }
        }
    }

    #[tool(description = "\
Forecast: date (YYYY-MM-DD) for one day, or days 1-7 (a week is 7): highs/lows, rain \
chance, UV, sun times. One call per place. Omit location for the configured home. Never \
guess data.")]
    async fn get_weather_forecast(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ForecastParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_weather_forecast");
        let location = resolve_forecast_location(&params.0);
        let date = params
            .0
            .date
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        if let Some(d) = date {
            if chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_err() {
                tracing::warn!(
                    kind = "forecast_date_invalid",
                    date = d,
                    "forecast date is not YYYY-MM-DD"
                );
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "The date {d} is not YYYY-MM-DD. Pass the day asked for, e.g. 2026-11-05."
                ))]));
            }
        }
        // A date is matched against the forecast's own dates, so all seven are fetched and the
        // model never has to count days from today -- a small model gets that sum wrong.
        let days = if date.is_some() {
            7
        } else {
            params.0.days.unwrap_or(3).clamp(1, 7)
        };

        tracing::debug!(
            "get_weather_forecast called, location={:?}, days={}, date={:?}, provider={}",
            location,
            days,
            date,
            if self.weather.is_some() {
                "configured"
            } else {
                "NONE"
            }
        );

        match &self.weather {
            // A failure, reported as one (see the current-weather tool).
            None => Ok(CallToolResult::error(vec![ContentBlock::text(
                "Weather is not configured on this pond: no location is set in settings. \
                 No other tool, shell command or external request can supply a forecast.",
            )])),
            Some(w) => {
                let result = match &location {
                    Some(loc) => w.forecast_for(loc, days).await,
                    None => w.forecast(days).await,
                };
                let result = match (result, date) {
                    (Ok(mut data), Some(d)) => match narrow_to_date(&mut data, d) {
                        Ok(()) => Ok(data),
                        Err(missing) => {
                            tracing::warn!(
                                kind = "forecast_date_out_of_range",
                                date = d,
                                "{missing}"
                            );
                            return Ok(CallToolResult::error(vec![ContentBlock::text(missing)]));
                        }
                    },
                    (other, _) => other,
                };
                match result {
                    Ok(data) => {
                        tracing::debug!(
                            "weather: fetched {}-day forecast for {}",
                            data.days.len(),
                            data.location_name
                        );
                        // `[[[mcp-ui:…]]]` marker, or the desktop has no data to render a card.
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
                        Ok(CallToolResult::success(vec![ContentBlock::text(
                            full_result,
                        )]))
                    }
                    Err(e) => {
                        tracing::warn!("weather: forecast fetch failed: {e}");
                        Ok(CallToolResult::error(vec![ContentBlock::text(format!(
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

/// Keep only `date`'s day of a forecast, or say which dates the forecast does cover.
fn narrow_to_date(data: &mut ForecastData, date: &str) -> Result<(), String> {
    let covered = match (data.days.first(), data.days.last()) {
        (Some(first), Some(last)) => format!("{} to {}", first.date, last.date),
        _ => "no dates".to_string(),
    };
    data.days.retain(|d| d.date == date);
    if data.days.is_empty() {
        return Err(format!(
            "No forecast for {date}: the forecast covers {covered}. Say so; do not use another day."
        ));
    }
    Ok(())
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
    // No panic: a binary may not init this family, and a panic kills every builtin's startup.
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
    use pond_adapters_weather::ForecastDay;

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

    fn forecast(dates: &[&str]) -> ForecastData {
        ForecastData {
            location_name: "Kisumu, Kenya".to_string(),
            days: dates
                .iter()
                .map(|d| ForecastDay {
                    date: (*d).to_string(),
                    description: "Light drizzle".to_string(),
                    temp_max_c: 30.0,
                    temp_min_c: 18.0,
                    precipitation_sum_mm: 1.0,
                    precipitation_probability_pct: 70,
                    wind_speed_max_kmh: 20.0,
                    uv_index_max: 9.0,
                    sunrise: "06:20".to_string(),
                    sunset: "18:30".to_string(),
                })
                .collect(),
            fetched_at: chrono::Utc::now(),
        }
    }

    /// On 2026-11-02 "the 4th of November" came back as the 3rd's figures, labelled the 4th:
    /// the model counted two days from today instead of three.
    #[test]
    fn a_date_keeps_only_that_day() {
        let mut data = forecast(&["2026-11-02", "2026-11-03", "2026-11-04", "2026-11-05"]);
        narrow_to_date(&mut data, "2026-11-04").unwrap();
        assert_eq!(data.days.len(), 1);
        assert_eq!(data.days[0].date, "2026-11-04");
    }

    #[test]
    fn a_date_outside_the_forecast_says_what_is_covered() {
        let mut data = forecast(&["2026-11-02", "2026-11-08"]);
        let err = narrow_to_date(&mut data, "2026-11-12").unwrap_err();
        assert!(err.contains("2026-11-02 to 2026-11-08"), "{err}");
        assert!(data.days.is_empty());
    }

    #[test]
    fn resolve_forecast_location_works() {
        let params = ForecastParams {
            location: Some("Eldoret".to_string()),
            days: Some(5),
            date: None,
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
    //! The model acts on what a result names, so results state facts and never instruct it.

    /// Source before the first test module; the cut keeps the assertion lists out of the scan.
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
