//! Weather from the free, keyless Open-Meteo API, cached for `cache_ttl` (15 min).

use crate::geocoding::Geocoder;
use crate::wmo;
use crate::{ForecastData, ForecastDay, WeatherData, WeatherProvider};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── API response shapes ──────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct CurrentApiResponse {
    current: CurrentFields,
    daily: Option<DailySunFields>,
}

#[derive(serde::Deserialize)]
struct CurrentFields {
    temperature_2m: f64,
    relative_humidity_2m: u32,
    apparent_temperature: f64,
    precipitation: f64,
    weather_code: u32,
    wind_speed_10m: f64,
    wind_direction_10m: u32,
    wind_gusts_10m: f64,
    cloud_cover: u32,
    is_day: u32,
}

/// Sunrise/sunset come from the `daily` block (even for a single day).
#[derive(serde::Deserialize)]
struct DailySunFields {
    sunrise: Vec<String>,
    sunset: Vec<String>,
}

#[derive(serde::Deserialize)]
struct ForecastApiResponse {
    daily: ForecastDailyFields,
}

#[derive(serde::Deserialize)]
struct ForecastDailyFields {
    time: Vec<String>,
    weather_code: Vec<u32>,
    temperature_2m_max: Vec<f64>,
    temperature_2m_min: Vec<f64>,
    sunrise: Vec<String>,
    sunset: Vec<String>,
    uv_index_max: Vec<f64>,
    precipitation_sum: Vec<f64>,
    precipitation_probability_max: Vec<u32>,
    wind_speed_10m_max: Vec<f64>,
}

// ── Caching ──────────────────────────────────────────────────────────────────

struct CacheEntry<T> {
    data: T,
    fetched: Instant,
}

/// Cache key: lowercase location name (or "__default__" for configured location).
type CacheMap<T> = HashMap<String, CacheEntry<T>>;

// ── Adapter ──────────────────────────────────────────────────────────────────

pub struct OpenMeteoWeatherAdapter {
    client: reqwest::Client,
    latitude: f64,
    longitude: f64,
    location_name: String,
    base_url: String,
    geocoder: Geocoder,
    cache_ttl: Duration,
    current_cache: Mutex<CacheMap<WeatherData>>,
    forecast_cache: Mutex<CacheMap<ForecastData>>,
}

impl OpenMeteoWeatherAdapter {
    /// Create with real Open-Meteo endpoints.
    pub fn new(latitude: f64, longitude: f64, location_name: impl Into<String>) -> Self {
        let client = reqwest::Client::new();
        Self {
            geocoder: Geocoder::new(client.clone()),
            client,
            latitude,
            longitude,
            location_name: location_name.into(),
            base_url: "https://api.open-meteo.com".to_string(),
            cache_ttl: Duration::from_secs(15 * 60),
            current_cache: Mutex::new(HashMap::new()),
            forecast_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Create pointing at custom base URLs (tests with wiremock).
    pub fn with_base_url(
        latitude: f64,
        longitude: f64,
        location_name: impl Into<String>,
        weather_base_url: impl Into<String>,
        geocoding_base_url: impl Into<String>,
    ) -> Self {
        let client = reqwest::Client::new();
        let geo_url = geocoding_base_url.into();
        Self {
            geocoder: Geocoder::with_base_url(client.clone(), &geo_url),
            client,
            latitude,
            longitude,
            location_name: location_name.into(),
            base_url: weather_base_url.into(),
            cache_ttl: Duration::from_secs(15 * 60),
            current_cache: Mutex::new(HashMap::new()),
            forecast_cache: Mutex::new(HashMap::new()),
        }
    }

    // ── Internal fetch helpers ───────────────────────────────────────────

    /// Configured coords, geocoding `location_name` if both are 0 (onboarding saves a name only).
    async fn default_location(&self) -> Result<(f64, f64, String)> {
        if self.latitude != 0.0 || self.longitude != 0.0 {
            return Ok((self.latitude, self.longitude, self.location_name.clone()));
        }
        if self.location_name.trim().is_empty() {
            anyhow::bail!(
                "no default weather location configured (set coordinates or a location name)"
            );
        }
        let geo = self.geocoder.geocode(&self.location_name).await?;
        Ok((geo.latitude, geo.longitude, geo.name))
    }

    async fn fetch_current(&self, lat: f64, lon: f64, name: &str) -> Result<WeatherData> {
        let url = format!(
            "{}/v1/forecast\
             ?latitude={}&longitude={}\
             &current=temperature_2m,relative_humidity_2m,apparent_temperature,\
             precipitation,weather_code,wind_speed_10m,wind_direction_10m,\
             wind_gusts_10m,cloud_cover,is_day\
             &daily=sunrise,sunset\
             &forecast_days=1&timezone=auto\
             &temperature_unit=celsius&wind_speed_unit=kmh&precipitation_unit=mm",
            self.base_url, lat, lon,
        );

        tracing::debug!("weather: fetching current for {name} ({lat}, {lon})");

        let resp = crate::traced_send(self.client.get(&url).timeout(Duration::from_secs(10)), &url)
            .await
            .context("weather API request failed")?
            .error_for_status()
            .context("weather API returned error status")?;

        let api: CurrentApiResponse = resp
            .json()
            .await
            .context("failed to parse weather response")?;
        let c = api.current;

        let (sunrise, sunset) = api
            .daily
            .and_then(|d| {
                let sr = d.sunrise.into_iter().next()?;
                let ss = d.sunset.into_iter().next()?;
                // Extract just the time portion from "2026-05-13T06:30" format
                let sr_time = sr.split('T').nth(1).unwrap_or(&sr).to_string();
                let ss_time = ss.split('T').nth(1).unwrap_or(&ss).to_string();
                Some((sr_time, ss_time))
            })
            .unwrap_or_else(|| ("--:--".to_string(), "--:--".to_string()));

        Ok(WeatherData {
            temperature_c: c.temperature_2m,
            feels_like_c: c.apparent_temperature,
            humidity_pct: c.relative_humidity_2m,
            description: wmo::describe(c.weather_code).to_string(),
            wind_speed_kmh: c.wind_speed_10m,
            wind_gusts_kmh: c.wind_gusts_10m,
            wind_direction_deg: c.wind_direction_10m,
            precipitation_mm: c.precipitation,
            cloud_cover_pct: c.cloud_cover,
            is_day: c.is_day != 0,
            sunrise,
            sunset,
            location_name: name.to_string(),
            fetched_at: Utc::now(),
        })
    }

    async fn fetch_forecast(
        &self,
        lat: f64,
        lon: f64,
        name: &str,
        days: u8,
    ) -> Result<ForecastData> {
        let days = days.clamp(1, 16);
        let url = format!(
            "{}/v1/forecast\
             ?latitude={}&longitude={}\
             &daily=weather_code,temperature_2m_max,temperature_2m_min,\
             sunrise,sunset,uv_index_max,precipitation_sum,\
             precipitation_probability_max,wind_speed_10m_max\
             &forecast_days={}&timezone=auto\
             &temperature_unit=celsius&wind_speed_unit=kmh&precipitation_unit=mm",
            self.base_url, lat, lon, days,
        );

        tracing::debug!("weather: fetching {days}-day forecast for {name} ({lat}, {lon})");

        let resp = crate::traced_send(self.client.get(&url).timeout(Duration::from_secs(10)), &url)
            .await
            .context("forecast API request failed")?
            .error_for_status()
            .context("forecast API returned error status")?;

        let api: ForecastApiResponse = resp
            .json()
            .await
            .context("failed to parse forecast response")?;
        let d = api.daily;

        let mut forecast_days = Vec::with_capacity(d.time.len());
        for i in 0..d.time.len() {
            let sr = d.sunrise.get(i).cloned().unwrap_or_default();
            let ss = d.sunset.get(i).cloned().unwrap_or_default();
            let sr_time = sr.split('T').nth(1).unwrap_or(&sr).to_string();
            let ss_time = ss.split('T').nth(1).unwrap_or(&ss).to_string();

            forecast_days.push(ForecastDay {
                date: d.time[i].clone(),
                description: wmo::describe(d.weather_code[i]).to_string(),
                temp_max_c: d.temperature_2m_max[i],
                temp_min_c: d.temperature_2m_min[i],
                precipitation_sum_mm: d.precipitation_sum[i],
                precipitation_probability_pct: d.precipitation_probability_max[i],
                wind_speed_max_kmh: d.wind_speed_10m_max[i],
                uv_index_max: d.uv_index_max[i],
                sunrise: sr_time,
                sunset: ss_time,
            });
        }

        Ok(ForecastData {
            location_name: name.to_string(),
            days: forecast_days,
            fetched_at: Utc::now(),
        })
    }

    // ── Cache helpers ────────────────────────────────────────────────────

    fn cache_key(location: Option<&str>) -> String {
        location
            .map(|l| l.to_lowercase())
            .unwrap_or_else(|| "__default__".to_string())
    }

    fn get_cached_current(&self, key: &str) -> Option<WeatherData> {
        let guard = self.current_cache.lock().unwrap();
        guard.get(key).and_then(|e| {
            if e.fetched.elapsed() < self.cache_ttl {
                Some(e.data.clone())
            } else {
                None
            }
        })
    }

    fn set_cached_current(&self, key: String, data: &WeatherData) {
        let mut guard = self.current_cache.lock().unwrap();
        guard.insert(
            key,
            CacheEntry {
                data: data.clone(),
                fetched: Instant::now(),
            },
        );
    }

    fn get_cached_forecast(&self, key: &str) -> Option<ForecastData> {
        let guard = self.forecast_cache.lock().unwrap();
        guard.get(key).and_then(|e| {
            if e.fetched.elapsed() < self.cache_ttl {
                Some(e.data.clone())
            } else {
                None
            }
        })
    }

    fn set_cached_forecast(&self, key: String, data: &ForecastData) {
        let mut guard = self.forecast_cache.lock().unwrap();
        guard.insert(
            key,
            CacheEntry {
                data: data.clone(),
                fetched: Instant::now(),
            },
        );
    }
}

#[async_trait]
impl WeatherProvider for OpenMeteoWeatherAdapter {
    async fn current(&self) -> Result<WeatherData> {
        let key = Self::cache_key(None);
        if let Some(data) = self.get_cached_current(&key) {
            tracing::debug!(
                "weather: serving current from cache ({})",
                self.location_name
            );
            return Ok(data);
        }

        let (lat, lon, name) = self.default_location().await?;
        let data = self.fetch_current(lat, lon, &name).await?;
        self.set_cached_current(key, &data);
        Ok(data)
    }

    async fn current_for(&self, location: &str) -> Result<WeatherData> {
        let key = Self::cache_key(Some(location));
        if let Some(data) = self.get_cached_current(&key) {
            tracing::debug!("weather: serving current from cache ({location})");
            return Ok(data);
        }

        let geo = self.geocoder.geocode(location).await?;
        let data = self
            .fetch_current(geo.latitude, geo.longitude, &geo.name)
            .await?;
        self.set_cached_current(key, &data);
        Ok(data)
    }

    async fn forecast(&self, days: u8) -> Result<ForecastData> {
        let key = format!("{}__{}d", Self::cache_key(None), days);
        if let Some(data) = self.get_cached_forecast(&key) {
            tracing::debug!(
                "weather: serving forecast from cache ({})",
                self.location_name
            );
            return Ok(data);
        }

        let (lat, lon, name) = self.default_location().await?;
        let data = self.fetch_forecast(lat, lon, &name, days).await?;
        self.set_cached_forecast(key, &data);
        Ok(data)
    }

    async fn forecast_for(&self, location: &str, days: u8) -> Result<ForecastData> {
        let key = format!("{}__{}d", Self::cache_key(Some(location)), days);
        if let Some(data) = self.get_cached_forecast(&key) {
            tracing::debug!("weather: serving forecast from cache ({location})");
            return Ok(data);
        }

        let geo = self.geocoder.geocode(location).await?;
        let data = self
            .fetch_forecast(geo.latitude, geo.longitude, &geo.name, days)
            .await?;
        self.set_cached_forecast(key, &data);
        Ok(data)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn mock_current_response() -> serde_json::Value {
        serde_json::json!({
            "current": {
                "temperature_2m": 24.3,
                "relative_humidity_2m": 68,
                "apparent_temperature": 25.1,
                "precipitation": 0.2,
                "weather_code": 2,
                "wind_speed_10m": 13.5,
                "wind_direction_10m": 180,
                "wind_gusts_10m": 22.0,
                "cloud_cover": 45,
                "is_day": 1
            },
            "daily": {
                "sunrise": ["2026-05-13T06:32"],
                "sunset": ["2026-05-13T18:28"]
            }
        })
    }

    fn mock_forecast_response() -> serde_json::Value {
        serde_json::json!({
            "daily": {
                "time": ["2026-05-13", "2026-05-14", "2026-05-15"],
                "weather_code": [2, 61, 0],
                "temperature_2m_max": [26.0, 22.5, 28.1],
                "temperature_2m_min": [18.0, 16.3, 19.2],
                "sunrise": ["2026-05-13T06:32", "2026-05-14T06:32", "2026-05-15T06:33"],
                "sunset": ["2026-05-13T18:28", "2026-05-14T18:28", "2026-05-15T18:28"],
                "uv_index_max": [8.5, 4.2, 9.1],
                "precipitation_sum": [0.0, 12.3, 0.0],
                "precipitation_probability_max": [10, 85, 5],
                "wind_speed_10m_max": [15.0, 25.0, 12.0]
            }
        })
    }

    fn mock_geocoding_response() -> serde_json::Value {
        serde_json::json!({
            "results": [{
                "name": "Kisumu",
                "latitude": -0.1022,
                "longitude": 34.7617,
                "country": "Kenya",
                "admin1": "Kisumu County"
            }]
        })
    }

    async fn make_adapter(weather_server: &MockServer) -> OpenMeteoWeatherAdapter {
        OpenMeteoWeatherAdapter::with_base_url(
            -1.286,
            36.817,
            "Nairobi, KE",
            weather_server.uri(),
            // geocoding uses the same mock server in tests
            weather_server.uri(),
        )
    }

    // ── Current weather tests ────────────────────────────────────────────

    #[tokio::test]
    async fn fetches_and_parses_current_weather() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_current_response()))
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        let data = adapter.current().await.unwrap();

        assert_eq!(data.temperature_c, 24.3);
        assert_eq!(data.humidity_pct, 68);
        assert_eq!(data.description, "Partly cloudy");
        assert_eq!(data.wind_speed_kmh, 13.5);
        assert_eq!(data.wind_gusts_kmh, 22.0);
        assert_eq!(data.cloud_cover_pct, 45);
        assert!(data.is_day);
        assert_eq!(data.sunrise, "06:32");
        assert_eq!(data.sunset, "18:28");
        assert_eq!(data.location_name, "Nairobi, KE");
    }

    #[tokio::test]
    async fn context_block_contains_enriched_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_current_response()))
            .mount(&server)
            .await;

        let data = make_adapter(&server).await.current().await.unwrap();
        let block = data.as_context_block();

        assert!(block.contains("Nairobi, KE"));
        assert!(block.contains("24.3\u{00B0}C"));
        assert!(block.contains("Partly cloudy"));
        assert!(block.contains("Humidity: 68%"));
        assert!(block.contains("Cloud cover: 45%"));
        assert!(block.contains("gusts 22"));
        assert!(block.contains("Day"));
        assert!(block.contains("Sunrise: 06:32"));
        assert!(block.contains("Sunset: 18:28"));
    }

    #[tokio::test]
    async fn cache_serves_second_call_without_second_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_current_response()))
            .expect(1) // only ONE real request
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        adapter.current().await.unwrap();
        adapter.current().await.unwrap(); // served from cache

        server.verify().await;
    }

    #[tokio::test]
    async fn api_error_returns_err() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        assert!(adapter.current().await.is_err());
    }

    // ── Forecast tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn fetches_and_parses_forecast() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_forecast_response()))
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        let forecast = adapter.forecast(3).await.unwrap();

        assert_eq!(forecast.location_name, "Nairobi, KE");
        assert_eq!(forecast.days.len(), 3);

        let day1 = &forecast.days[0];
        assert_eq!(day1.date, "2026-05-13");
        assert_eq!(day1.description, "Partly cloudy");
        assert_eq!(day1.temp_max_c, 26.0);
        assert_eq!(day1.temp_min_c, 18.0);
        assert_eq!(day1.precipitation_probability_pct, 10);
        assert_eq!(day1.uv_index_max, 8.5);

        let day2 = &forecast.days[1];
        assert_eq!(day2.description, "Slight rain");
        assert_eq!(day2.precipitation_probability_pct, 85);
        assert_eq!(day2.precipitation_sum_mm, 12.3);
    }

    #[tokio::test]
    async fn forecast_context_block_format() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_forecast_response()))
            .mount(&server)
            .await;

        let forecast = make_adapter(&server).await.forecast(3).await.unwrap();
        let block = forecast.as_context_block();

        assert!(block.contains("3-Day Forecast"));
        assert!(block.contains("Nairobi, KE"));
        assert!(block.contains("2026-05-13"));
        assert!(block.contains("26\u{00B0}C/18\u{00B0}C"));
        assert!(block.contains("Rain: 12.3mm (85%)"));
    }

    // ── Location-aware tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn current_geocodes_the_default_name_when_coordinates_are_unset() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_geocoding_response()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_current_response()))
            .mount(&server)
            .await;

        // lat/lon = 0, name only — exactly what onboarding persists.
        let adapter =
            OpenMeteoWeatherAdapter::with_base_url(0.0, 0.0, "Kisumu", server.uri(), server.uri());
        let data = adapter.current().await.unwrap();
        assert_eq!(data.location_name, "Kisumu, Kenya");
        assert_eq!(data.temperature_c, 24.3);
    }

    #[tokio::test]
    async fn current_errors_when_neither_coordinates_nor_name_are_set() {
        let server = MockServer::start().await;
        let adapter =
            OpenMeteoWeatherAdapter::with_base_url(0.0, 0.0, "", server.uri(), server.uri());
        let err = adapter.current().await.expect_err("no location");
        assert!(err.to_string().contains("no default weather location"));
    }

    #[tokio::test]
    async fn current_for_geocodes_and_fetches() {
        let server = MockServer::start().await;

        // Geocoding endpoint
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_geocoding_response()))
            .mount(&server)
            .await;

        // Weather endpoint
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_current_response()))
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        let data = adapter.current_for("Kisumu").await.unwrap();

        assert_eq!(data.location_name, "Kisumu, Kenya");
        assert_eq!(data.temperature_c, 24.3);
    }

    #[tokio::test]
    async fn forecast_for_geocodes_and_fetches() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_geocoding_response()))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_forecast_response()))
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        let forecast = adapter.forecast_for("Kisumu", 3).await.unwrap();

        assert_eq!(forecast.location_name, "Kisumu, Kenya");
        assert_eq!(forecast.days.len(), 3);
    }

    #[tokio::test]
    async fn location_cache_is_separate_from_default() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_geocoding_response()))
            .mount(&server)
            .await;

        // Allow 2 weather calls: one for default, one for Kisumu
        Mock::given(method("GET"))
            .and(path("/v1/forecast"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_current_response()))
            .expect(2)
            .mount(&server)
            .await;

        let adapter = make_adapter(&server).await;
        let default = adapter.current().await.unwrap();
        let kisumu = adapter.current_for("Kisumu").await.unwrap();

        assert_eq!(default.location_name, "Nairobi, KE");
        assert_eq!(kisumu.location_name, "Kisumu, Kenya");

        server.verify().await;
    }
}

#[tokio::test]
#[ignore]
async fn live_weather_fetch() {
    let adapter = OpenMeteoWeatherAdapter::new(-1.286, 36.817, "Nairobi, KE");

    let data = adapter
        .current()
        .await
        .expect("failed to fetch live weather");

    println!("Current: {:#?}", data);
    println!("Block: {}", data.as_context_block());

    assert!(data.temperature_c > -50.0 && data.temperature_c < 60.0);
    assert!(data.humidity_pct <= 100);
    assert!(!data.description.is_empty());
    assert!(!data.sunrise.is_empty());
    assert!(!data.sunset.is_empty());
}

#[tokio::test]
#[ignore]
async fn live_forecast_fetch() {
    let adapter = OpenMeteoWeatherAdapter::new(-1.286, 36.817, "Nairobi, KE");

    let forecast = adapter
        .forecast(5)
        .await
        .expect("failed to fetch live forecast");

    println!("Forecast: {:#?}", forecast);
    println!("Block:\n{}", forecast.as_context_block());

    assert_eq!(forecast.days.len(), 5);
    assert_eq!(forecast.location_name, "Nairobi, KE");
}

#[tokio::test]
#[ignore]
async fn live_location_weather() {
    let adapter = OpenMeteoWeatherAdapter::new(-1.286, 36.817, "Nairobi, KE");

    let data = adapter
        .current_for("Kisumu")
        .await
        .expect("failed to fetch Kisumu weather");

    println!("Kisumu weather: {:#?}", data);
    println!("Block: {}", data.as_context_block());

    assert!(data.location_name.contains("Kisumu"));
    assert!(data.temperature_c > -50.0 && data.temperature_c < 60.0);
}
