mod geocoding;
mod open_meteo;
mod wmo;

pub use geocoding::{GeoLocation, Geocoder};
pub use open_meteo::OpenMeteoWeatherAdapter;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Egress-gated, egress-traced send; this crate's own client can't use `traced_get`.
pub(crate) async fn traced_send(
    builder: reqwest::RequestBuilder,
    url: &str,
) -> anyhow::Result<reqwest::Response> {
    // Gate first: an `offline` pond must stop asking a third party where the user lives.
    pond_core::shared::services::egress::check_egress(url)?;

    let start = std::time::Instant::now();
    let result = builder.send().await;
    let latency_ms = start.elapsed().as_millis() as u64;
    let status = result.as_ref().ok().map(|r| r.status().as_u16());
    pond_core::shared::services::egress::record_egress(url, "GET", status, latency_ms);
    Ok(result?)
}

// ── Current weather ──────────────────────────────────────────────────────────

/// Current weather snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeatherData {
    pub temperature_c: f64,
    pub feels_like_c: f64,
    pub humidity_pct: u32,
    pub description: String,
    pub wind_speed_kmh: f64,
    pub wind_gusts_kmh: f64,
    pub wind_direction_deg: u32,
    pub precipitation_mm: f64,
    pub cloud_cover_pct: u32,
    pub is_day: bool,
    pub sunrise: String,
    pub sunset: String,
    pub location_name: String,
    pub fetched_at: DateTime<Utc>,
}

impl WeatherData {
    /// Compact context block for the LLM.
    pub fn as_context_block(&self) -> String {
        let day_night = if self.is_day { "Day" } else { "Night" };
        format!(
            "[Current Weather — {}]\n\
             {} | {:.1}\u{00B0}C (feels like {:.1}\u{00B0}C) | Humidity: {}% | \
             Cloud cover: {}% | Wind: {:.0} km/h (gusts {:.0}) | \
             Precip: {:.1} mm | {} | Sunrise: {} | Sunset: {}",
            self.location_name,
            self.description,
            self.temperature_c,
            self.feels_like_c,
            self.humidity_pct,
            self.cloud_cover_pct,
            self.wind_speed_kmh,
            self.wind_gusts_kmh,
            self.precipitation_mm,
            day_night,
            self.sunrise,
            self.sunset,
        )
    }
}

// ── Forecast ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForecastDay {
    pub date: String,
    pub description: String,
    pub temp_max_c: f64,
    pub temp_min_c: f64,
    pub precipitation_sum_mm: f64,
    pub precipitation_probability_pct: u32,
    pub wind_speed_max_kmh: f64,
    pub uv_index_max: f64,
    pub sunrise: String,
    pub sunset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForecastData {
    pub location_name: String,
    pub days: Vec<ForecastDay>,
    pub fetched_at: DateTime<Utc>,
}

impl ForecastData {
    /// Compact context block for the LLM.
    pub fn as_context_block(&self) -> String {
        let mut out = format!(
            "[{}-Day Forecast — {}]\n",
            self.days.len(),
            self.location_name
        );
        for d in &self.days {
            out.push_str(&format!(
                "{}: {} | {:.0}\u{00B0}C/{:.0}\u{00B0}C | Rain: {:.1}mm ({}%) | \
                 Wind: {:.0} km/h | UV: {:.0} | \u{2600} {}-{}\n",
                d.date,
                d.description,
                d.temp_max_c,
                d.temp_min_c,
                d.precipitation_sum_mm,
                d.precipitation_probability_pct,
                d.wind_speed_max_kmh,
                d.uv_index_max,
                d.sunrise,
                d.sunset,
            ));
        }
        out.trim_end().to_string()
    }
}

// ── Port trait ────────────────────────────────────────────────────────────────

/// Port for fetching weather; implementations should cache results for ~15 minutes.
#[async_trait]
pub trait WeatherProvider: Send + Sync {
    /// Current weather for the configured default location.
    async fn current(&self) -> Result<WeatherData>;

    /// Current weather for a named location (geocoded automatically).
    async fn current_for(&self, location: &str) -> Result<WeatherData>;

    /// Multi-day forecast for the configured default location.
    async fn forecast(&self, days: u8) -> Result<ForecastData>;

    /// Multi-day forecast for a named location (geocoded automatically).
    async fn forecast_for(&self, location: &str, days: u8) -> Result<ForecastData>;
}
