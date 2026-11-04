//! Weather MCP server: current weather and forecasts.

use chrono::{DateTime, Days, NaiveDate, Utc};
use pond_adapters_weather::{ForecastData, WeatherProvider};
use pond_core::user_data::ports::settings::SettingsRepository;
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

/// Which days `get_weather_forecast` returns. One day is `day`, else `date`; with neither,
/// `days` from today. `day` is first because it needs no calendar arithmetic from the model:
/// the tool counts from the household's today (see `docs/developer/example_workflow_weather.md`).
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ForecastParams {
    pub location: Option<String>,
    /// 1-7 (omitted: 3).
    pub days: Option<u8>,
    /// Days from today.
    pub day: Option<u8>,
    /// YYYY-MM-DD.
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
    /// Holds the household's timezone, which decides what "today" is when counting `day`.
    settings: Arc<dyn SettingsRepository>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl WeatherMcpServer {
    /// Every tool this server exposes, without constructing it (`tool_router()` is private).
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    /// A weather server over `weather` (None: unconfigured), counting days on `settings`'
    /// household timezone.
    pub fn new(
        weather: Option<Arc<dyn WeatherProvider>>,
        settings: Arc<dyn SettingsRepository>,
    ) -> Self {
        Self {
            weather,
            settings,
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
Forecast. One day: day (0 today/leo, 1 tomorrow/kesho, 2 the day after/kesho kutwa) or date. Else days 1-7 from \
today, each labelled with its weekday (a week or a weekday name: 7). Highs/lows, rain chance, \
UV, sun times. One call per place. Omit location for the configured home. Never guess data.")]
    async fn get_weather_forecast(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ForecastParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_weather_forecast");
        Ok(self.forecast_result(&params.0, Utc::now()).await)
    }
}

impl WeatherMcpServer {
    /// The forecast tool's result for `params`, `now` being the present moment.
    async fn forecast_result(&self, params: &ForecastParams, now: DateTime<Utc>) -> CallToolResult {
        let location = resolve_forecast_location(params);
        let named = params
            .date
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        let named = match named {
            None => None,
            Some(d) => match NaiveDate::parse_from_str(d, "%Y-%m-%d") {
                Ok(day) => Some(day),
                Err(_) => {
                    tracing::warn!(
                        kind = "forecast_date_invalid",
                        date = d,
                        "forecast date is not YYYY-MM-DD"
                    );
                    return CallToolResult::error(vec![ContentBlock::text(format!(
                        "The date {d} is not YYYY-MM-DD. Pass the day asked for, e.g. 2026-11-05."
                    ))]);
                }
            },
        };
        let today = self.household_today(now).await;
        // `day` wins: it is a count the tool turns into a date, so it cannot carry the model's
        // arithmetic slip that a disagreeing `date` would.
        let asked = match params.day {
            Some(offset) => {
                let day = days_after(today, offset);
                if named.is_some_and(|n| n != day) {
                    tracing::warn!(
                        kind = "forecast_day_date_disagree",
                        day = offset,
                        resolved = %day,
                        date = ?named,
                        "forecast day and date disagree; using the day"
                    );
                }
                Some(day)
            }
            None => named,
        };
        // One day is matched against the forecast's own dates, so all seven are fetched.
        let days = if asked.is_some() {
            7
        } else {
            params.days.unwrap_or(3).clamp(1, 7)
        };

        tracing::debug!(
            kind = "forecast_call",
            location = ?location,
            days,
            day = ?params.day,
            asked = ?asked,
            today = %today,
            configured = self.weather.is_some(),
            "get_weather_forecast called"
        );

        let Some(w) = &self.weather else {
            // A failure, reported as one (see the current-weather tool).
            return CallToolResult::error(vec![ContentBlock::text(
                "Weather is not configured on this pond: no location is set in settings. \
                 No other tool, shell command or external request can supply a forecast.",
            )]);
        };
        let result = match &location {
            Some(loc) => w.forecast_for(loc, days).await,
            None => w.forecast(days).await,
        };
        let mut data = match result {
            Ok(data) => data,
            Err(e) => {
                tracing::warn!(kind = "forecast_fetch_failed", error = %e, "weather: forecast fetch failed");
                return CallToolResult::error(vec![ContentBlock::text(format!(
                    "Forecast fetch failed: {e}"
                ))]);
            }
        };
        let header = match asked {
            None => String::new(),
            Some(day) => {
                let wanted = day.format("%Y-%m-%d").to_string();
                if let Err(missing) = narrow_to_date(&mut data, &wanted) {
                    tracing::warn!(
                        kind = "forecast_date_out_of_range",
                        date = %wanted,
                        "{missing}"
                    );
                    return CallToolResult::error(vec![ContentBlock::text(missing)]);
                }
                format!(
                    "Forecast for {} (today is {}).\n",
                    day_label(day),
                    day_label(today)
                )
            }
        };
        tracing::debug!(
            kind = "forecast_fetched",
            days = data.days.len(),
            location = %data.location_name,
            "weather: forecast fetched"
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
        CallToolResult::success(vec![ContentBlock::text(format!(
            "{hint}{header}{}",
            data.as_context_block_from(today)
        ))])
    }

    /// The household's calendar date at `now`, on the timezone in settings.
    async fn household_today(&self, now: DateTime<Utc>) -> NaiveDate {
        match self.settings.get().await {
            Ok(settings) => household_date(now, &settings.timezone),
            Err(e) => {
                tracing::warn!(
                    kind = "forecast_timezone_unread",
                    error = %e,
                    "settings unreadable; counting forecast days on UTC"
                );
                now.date_naive()
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
                 - get_weather_forecast: highs/lows, rain chance, UV index. One day by 'day' \
                 (days from today) or 'date', else 'days' (1-7) from today.\n\n\
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

/// `now`'s calendar date on IANA `timezone`, or on UTC when it does not parse (the household
/// clock's rule, as in `agent_activations::local_date_label`).
fn household_date(now: DateTime<Utc>, timezone: &str) -> NaiveDate {
    match timezone.trim().parse::<chrono_tz::Tz>() {
        Ok(tz) => now.with_timezone(&tz).date_naive(),
        Err(_) => {
            tracing::warn!(
                kind = "forecast_timezone_invalid",
                timezone,
                "household timezone does not parse; counting forecast days on UTC"
            );
            now.date_naive()
        }
    }
}

/// The date `offset` days after `today`: 0 is today, 1 tomorrow, 2 the day after tomorrow.
fn days_after(today: NaiveDate, offset: u8) -> NaiveDate {
    today + Days::new(u64::from(offset))
}

/// A date as said aloud and repeated by the model, such as "Thursday 5 November 2026".
fn day_label(day: NaiveDate) -> String {
    day.format("%A %-d %B %Y").to_string()
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
    settings: Arc<dyn SettingsRepository>,
}

static WEATHER_DEPS: OnceLock<WeatherDeps> = OnceLock::new();

/// Initialize weather server dependencies. Call once at startup.
pub fn init_weather_deps(
    weather: Option<Arc<dyn WeatherProvider>>,
    settings: Arc<dyn SettingsRepository>,
) {
    let _ = WEATHER_DEPS.set(WeatherDeps { weather, settings });
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
    let server = WeatherMcpServer::new(deps.weather.clone(), deps.settings.clone());
    crate::serve_builtin("giap-weather", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pond_adapters_weather::{ForecastDay, WeatherData};
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

    #[test]
    fn server_constructs_with_none() {
        let _server = WeatherMcpServer::new(None, Arc::new(MockSettingsRepository::new()));
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

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn ymd(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn day_counts_cross_month_and_year_ends() {
        assert_eq!(days_after(ymd("2026-11-03"), 0), ymd("2026-11-03"));
        assert_eq!(days_after(ymd("2026-11-03"), 2), ymd("2026-11-05"));
        assert_eq!(days_after(ymd("2026-11-30"), 1), ymd("2026-12-01"));
        assert_eq!(days_after(ymd("2026-12-31"), 1), ymd("2027-01-01"));
        assert_eq!(days_after(ymd("2026-12-30"), 3), ymd("2027-01-02"));
        assert_eq!(days_after(ymd("2028-02-28"), 1), ymd("2028-02-29"));
    }

    /// Today is the household's date, not UTC's: at 22:30 UTC Nairobi is already tomorrow,
    /// and at 05:00 UTC Los Angeles is still yesterday.
    #[test]
    fn today_is_the_households_date_across_the_utc_boundary() {
        let late = at("2026-11-03T22:30:00Z");
        assert_eq!(household_date(late, "Africa/Nairobi"), ymd("2026-11-04"));
        assert_eq!(household_date(late, "UTC"), ymd("2026-11-03"));
        let early = at("2026-11-04T05:00:00Z");
        assert_eq!(
            household_date(early, "America/Los_Angeles"),
            ymd("2026-11-03")
        );
        // New Year's Eve in UTC is already 1 January in Nairobi.
        assert_eq!(
            household_date(at("2026-12-31T21:00:00Z"), "Africa/Nairobi"),
            ymd("2027-01-01")
        );
    }

    #[test]
    fn an_unparseable_timezone_counts_on_utc() {
        let late = at("2026-11-03T22:30:00Z");
        assert_eq!(household_date(late, "Mars/Olympus"), ymd("2026-11-03"));
        assert_eq!(household_date(late, ""), ymd("2026-11-03"));
    }

    #[test]
    fn a_day_is_labelled_as_said_aloud() {
        assert_eq!(day_label(ymd("2026-11-05")), "Thursday 5 November 2026");
    }

    /// Serves `days` consecutive dates from `first`, as Open-Meteo does from the place's today.
    struct WeekFrom {
        first: NaiveDate,
    }

    #[async_trait::async_trait]
    impl WeatherProvider for WeekFrom {
        async fn current(&self) -> anyhow::Result<WeatherData> {
            anyhow::bail!("not used")
        }
        async fn current_for(&self, _location: &str) -> anyhow::Result<WeatherData> {
            anyhow::bail!("not used")
        }
        async fn forecast(&self, days: u8) -> anyhow::Result<ForecastData> {
            let dates: Vec<String> = (0..days)
                .map(|i| days_after(self.first, i).format("%Y-%m-%d").to_string())
                .collect();
            let refs: Vec<&str> = dates.iter().map(String::as_str).collect();
            Ok(forecast(&refs))
        }
        async fn forecast_for(&self, _location: &str, days: u8) -> anyhow::Result<ForecastData> {
            self.forecast(days).await
        }
    }

    async fn server_on(timezone: &str, first: &str) -> WeatherMcpServer {
        let settings = MockSettingsRepository::new();
        settings
            .set_key("timezone", timezone.to_string())
            .await
            .unwrap();
        WeatherMcpServer::new(
            Some(Arc::new(WeekFrom { first: ymd(first) })),
            Arc::new(settings),
        )
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect()
    }

    fn day_params(day: u8) -> ForecastParams {
        ForecastParams {
            location: Some("Moscow".to_string()),
            day: Some(day),
            ..Default::default()
        }
    }

    /// The 2026-11-03 failure: "the day after tomorrow" in Moscow came back as 4 November.
    #[tokio::test]
    async fn the_day_after_tomorrow_is_resolved_by_the_tool() {
        let server = server_on("Europe/Moscow", "2026-11-03").await;
        let result = server
            .forecast_result(&day_params(2), at("2026-11-03T09:00:00Z"))
            .await;
        let text = text_of(&result);
        assert_ne!(result.is_error, Some(true), "{text}");
        assert!(
            text.contains(
                "Forecast for Thursday 5 November 2026 (today is Tuesday 3 November 2026)"
            ),
            "{text}"
        );
        assert!(
            text.contains("Thursday (the day after tomorrow/kesho kutwa) 2026-11-05:"),
            "{text}"
        );
        assert!(!text.contains("2026-11-04"), "only the day asked: {text}");
    }

    #[tokio::test]
    async fn day_counts_from_the_households_today_not_utcs() {
        // 22:30 UTC on the 3rd is the 4th in Nairobi, so tomorrow is the 5th.
        let server = server_on("Africa/Nairobi", "2026-11-04").await;
        let result = server
            .forecast_result(&day_params(1), at("2026-11-03T22:30:00Z"))
            .await;
        let text = text_of(&result);
        assert!(
            text.contains("Forecast for Thursday 5 November 2026"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn day_wins_over_a_disagreeing_date() {
        let server = server_on("Europe/Moscow", "2026-11-03").await;
        let params = ForecastParams {
            date: Some("2026-11-04".to_string()),
            ..day_params(2)
        };
        let text = text_of(
            &server
                .forecast_result(&params, at("2026-11-03T09:00:00Z"))
                .await,
        );
        assert!(
            text.contains("Thursday (the day after tomorrow/kesho kutwa) 2026-11-05:"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_day_beyond_the_forecast_says_what_is_covered() {
        let server = server_on("Europe/Moscow", "2026-11-03").await;
        let result = server
            .forecast_result(&day_params(9), at("2026-11-03T09:00:00Z"))
            .await;
        let text = text_of(&result);
        assert_eq!(result.is_error, Some(true), "{text}");
        assert!(
            text.contains(
                "No forecast for 2026-11-12: the forecast covers 2026-11-03 to 2026-11-09"
            ),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_named_date_is_still_honoured() {
        let server = server_on("Europe/Moscow", "2026-11-03").await;
        let params = ForecastParams {
            date: Some("2026-11-07".to_string()),
            ..Default::default()
        };
        let text = text_of(
            &server
                .forecast_result(&params, at("2026-11-03T09:00:00Z"))
                .await,
        );
        assert!(
            text.contains("Forecast for Saturday 7 November 2026"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_span_has_no_single_day_header_and_labels_every_day() {
        let server = server_on("Europe/Moscow", "2026-11-03").await;
        let params = ForecastParams {
            days: Some(7),
            ..Default::default()
        };
        let text = text_of(
            &server
                .forecast_result(&params, at("2026-11-03T09:00:00Z"))
                .await,
        );
        assert!(!text.contains("Forecast for "), "{text}");
        assert!(text.contains("Saturday 2026-11-07:") && text.contains("Monday 2026-11-09:"));
    }

    /// Asked "kesho kutwa", Gemma 4 E4B fetched the week rather than `day: 2`; the matching line
    /// must still say which one it is, so the answer reads a tag instead of counting dates.
    #[tokio::test]
    async fn a_span_tags_today_tomorrow_and_the_day_after() {
        let server = server_on("Europe/Moscow", "2026-11-03").await;
        let params = ForecastParams {
            days: Some(7),
            ..Default::default()
        };
        let text = text_of(
            &server
                .forecast_result(&params, at("2026-11-03T09:00:00Z"))
                .await,
        );
        assert!(text.contains("Tuesday (today/leo) 2026-11-03:"), "{text}");
        assert!(
            text.contains("Wednesday (tomorrow/kesho) 2026-11-04:"),
            "{text}"
        );
        assert!(
            text.contains("Thursday (the day after tomorrow/kesho kutwa) 2026-11-05:"),
            "{text}"
        );
        assert!(
            text.contains("Friday 2026-11-06:"),
            "only the first three are tagged: {text}"
        );
    }

    #[test]
    fn the_forecast_schema_offers_day_date_and_days() {
        let tool = WeatherMcpServer::tool_defs()
            .into_iter()
            .find(|t| t.name == "get_weather_forecast")
            .expect("forecast tool");
        let props = tool.input_schema["properties"]
            .as_object()
            .expect("properties");
        for key in ["location", "days", "day", "date"] {
            assert!(props.contains_key(key), "{key} missing: {props:?}");
        }
        // Optional, so schemars renders `["integer", "null"]`.
        assert_eq!(
            props["day"]["type"],
            serde_json::json!(["integer", "null"]),
            "{props:?}"
        );
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.contains("0 today/leo, 1 tomorrow/kesho, 2 the day after/kesho kutwa"),
            "{description}"
        );
    }

    #[test]
    fn resolve_forecast_location_works() {
        let params = ForecastParams {
            location: Some("Eldoret".to_string()),
            days: Some(5),
            day: None,
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
