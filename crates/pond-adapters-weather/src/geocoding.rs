//! Place name → coordinates via the free, keyless Open-Meteo Geocoding API.

use anyhow::{Context, Result};
use std::time::Duration;

// ── Response shape ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct GeoResponse {
    results: Option<Vec<GeoResult>>,
}

#[derive(serde::Deserialize)]
struct GeoResult {
    name: String,
    latitude: f64,
    longitude: f64,
    country: Option<String>,
    admin1: Option<String>,
    /// Open-Meteo's zone for this point: a second opinion on a stale system timezone.
    timezone: Option<String>,
}

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GeoLocation {
    pub name: String,
    pub latitude: f64,
    pub longitude: f64,
    pub country: String,
    /// The zone the geocoder believes this point is in, when it says.
    pub timezone: Option<String>,
}

// ── Geocoder ─────────────────────────────────────────────────────────────────

pub struct Geocoder {
    client: reqwest::Client,
    base_url: String,
}

impl Geocoder {
    /// Create with the real Open-Meteo geocoding endpoint.
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            base_url: "https://geocoding-api.open-meteo.com".to_string(),
        }
    }

    /// Create pointing at a custom base URL (tests with wiremock).
    pub fn with_base_url(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into(),
        }
    }

    /// Resolve a place name (e.g. "Kisumu") to coordinates.
    pub async fn geocode(&self, query: &str) -> Result<GeoLocation> {
        let url = format!(
            "{}/v1/search?name={}&count=1&language=en",
            self.base_url,
            urlencoding::encode(query),
        );

        tracing::debug!("geocoding: {url}");

        let resp = crate::traced_send(self.client.get(&url).timeout(Duration::from_secs(5)), &url)
            .await
            .context("geocoding request failed")?
            .error_for_status()
            .context("geocoding API returned error status")?;

        let geo: GeoResponse = resp
            .json()
            .await
            .context("failed to parse geocoding response")?;

        let result = geo
            .results
            .and_then(|mut r| {
                if r.is_empty() {
                    None
                } else {
                    Some(r.remove(0))
                }
            })
            .context(format!("no location found for '{query}'"))?;

        let display_name = match (&result.admin1, &result.country) {
            (Some(admin), Some(country)) if admin != &result.name => {
                format!("{}, {}", result.name, country)
            }
            (_, Some(country)) => format!("{}, {}", result.name, country),
            _ => result.name.clone(),
        };

        Ok(GeoLocation {
            name: display_name,
            latitude: result.latitude,
            longitude: result.longitude,
            country: result.country.unwrap_or_default(),
            timezone: result.timezone,
        })
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    #[tokio::test]
    async fn geocodes_city_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(mock_geocoding_response()))
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        let loc = geocoder.geocode("Kisumu").await.unwrap();

        assert_eq!(loc.name, "Kisumu, Kenya");
        assert!((loc.latitude - (-0.1022)).abs() < 0.001);
        assert!((loc.longitude - 34.7617).abs() < 0.001);
        assert_eq!(loc.country, "Kenya");
    }

    #[tokio::test]
    async fn returns_error_for_empty_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "results": [] })),
            )
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        assert!(geocoder.geocode("xyznonexistent").await.is_err());
    }

    #[tokio::test]
    async fn returns_error_for_null_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let geocoder = Geocoder::with_base_url(reqwest::Client::new(), server.uri());
        assert!(geocoder.geocode("xyznonexistent").await.is_err());
    }
}

// ── The port ─────────────────────────────────────────────────────────────────

/// This geocoder as the core's [`PlaceLookup`].
#[async_trait::async_trait]
impl pond_core::user_data::ports::place_lookup::PlaceLookup for Geocoder {
    async fn by_name(
        &self,
        query: &str,
    ) -> Result<pond_core::user_data::ports::place_lookup::PlaceFix> {
        let g = self.geocode(query).await?;
        Ok(pond_core::user_data::ports::place_lookup::PlaceFix {
            name: g.name,
            latitude: g.latitude,
            longitude: g.longitude,
            timezone: g.timezone,
        })
    }
}
