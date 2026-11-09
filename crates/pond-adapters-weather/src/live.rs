//! The household's weather, at the place the settings name now.
//!
//! The serve process used to build one [`OpenMeteoWeatherAdapter`] at startup, so a location set
//! afterwards (on the dashboard, in owner setup, or a phone's fix over the home network) reached
//! the weather only after a restart. [`LiveWeather`] reads the settings on each call and keeps the
//! adapter, with its 15-minute cache, for as long as the place is the same.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::location::resolve;

use crate::{ForecastData, OpenMeteoWeatherAdapter, WeatherData, WeatherProvider};

/// The household has weather switched off, or no place it could be fetched for. Not a failure:
/// `GET /weather` answers `{"enabled": false}` for it, and the assistant's tools say it is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeatherOff;

impl std::fmt::Display for WeatherOff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("weather is off: switch it on and set a location")
    }
}

impl std::error::Error for WeatherOff {}

/// Builds the adapter for one place: latitude, longitude, the name it is shown under.
type Build = Box<dyn Fn(f64, f64, String) -> OpenMeteoWeatherAdapter + Send + Sync>;

/// The place an adapter was built for, compared bit for bit.
#[derive(Clone, PartialEq)]
struct Place {
    latitude: u64,
    longitude: u64,
    name: String,
}

pub struct LiveWeather {
    settings: Arc<dyn SettingsRepository>,
    build: Build,
    current: Mutex<Option<(Place, Arc<OpenMeteoWeatherAdapter>)>>,
}

impl LiveWeather {
    /// Weather from Open-Meteo for wherever `settings` says the household is.
    pub fn new(settings: Arc<dyn SettingsRepository>) -> Self {
        Self::with_builder(
            settings,
            Box::new(|lat, lon, name| OpenMeteoWeatherAdapter::new(lat, lon, name)),
        )
    }

    fn with_builder(settings: Arc<dyn SettingsRepository>, build: Build) -> Self {
        Self {
            settings,
            build,
            current: Mutex::new(None),
        }
    }

    /// The adapter for the place the settings name now; [`WeatherOff`] when there is none.
    async fn adapter(&self) -> Result<Arc<OpenMeteoWeatherAdapter>> {
        let settings = self.settings.get().await?;
        let target = resolve(&settings).weather_target();
        let (true, Some((latitude, longitude, name))) = (settings.weather_enabled, target) else {
            return Err(WeatherOff.into());
        };
        let place = Place {
            latitude: latitude.to_bits(),
            longitude: longitude.to_bits(),
            name: name.clone(),
        };
        let mut current = self.current.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((held, adapter)) = current.as_ref() {
            if *held == place {
                return Ok(adapter.clone());
            }
        }
        // The name only: a household's coordinates stay out of the logs.
        tracing::info!(kind = "weather_place", place = %name, "weather now follows this place");
        let adapter = Arc::new((self.build)(latitude, longitude, name));
        *current = Some((place, adapter.clone()));
        Ok(adapter)
    }
}

#[async_trait]
impl WeatherProvider for LiveWeather {
    async fn current(&self) -> Result<WeatherData> {
        self.adapter().await?.current().await
    }

    async fn current_for(&self, location: &str) -> Result<WeatherData> {
        self.adapter().await?.current_for(location).await
    }

    async fn forecast(&self, days: u8) -> Result<ForecastData> {
        self.adapter().await?.forecast(days).await
    }

    async fn forecast_for(&self, location: &str, days: u8) -> Result<ForecastData> {
        self.adapter().await?.forecast_for(location, days).await
    }
}

#[cfg(test)]
mod tests {
    use pond_core::user_data::domain::settings::Settings;
    use tokio::sync::RwLock;

    use super::*;

    /// Settings held in memory, every field kept.
    struct Held(RwLock<Settings>);

    #[async_trait]
    impl SettingsRepository for Held {
        async fn get(&self) -> Result<Settings> {
            Ok(self.0.read().await.clone())
        }
        async fn update(&self, settings: &Settings) -> Result<()> {
            *self.0.write().await = settings.clone();
            Ok(())
        }
        async fn get_key(&self, _key: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _key: &str, _value: String) -> Result<()> {
            Ok(())
        }
    }

    fn settings(enabled: bool, lat: f64, lon: f64, name: &str) -> Settings {
        Settings {
            weather_enabled: enabled,
            weather_latitude: lat,
            weather_longitude: lon,
            weather_location_name: name.into(),
            ..Settings::default()
        }
    }

    /// Records each place an adapter is built for.
    fn live(held: Arc<Held>) -> (LiveWeather, Arc<Mutex<Vec<String>>>) {
        let built = Arc::new(Mutex::new(Vec::new()));
        let log = built.clone();
        let weather = LiveWeather::with_builder(
            held,
            Box::new(move |lat, lon, name| {
                log.lock().unwrap().push(format!("{name} {lat} {lon}"));
                OpenMeteoWeatherAdapter::new(lat, lon, name)
            }),
        );
        (weather, built)
    }

    #[tokio::test]
    async fn a_new_place_is_followed_without_a_restart_and_the_same_place_reuses_its_adapter() {
        let held = Arc::new(Held(RwLock::new(settings(true, -1.286, 36.817, "Nairobi"))));
        let (weather, built) = live(held.clone());
        let first = weather.adapter().await.unwrap();
        let again = weather.adapter().await.unwrap();
        assert!(
            Arc::ptr_eq(&first, &again),
            "the cache lives with the adapter"
        );

        held.update(&settings(true, -1.193, 36.744, "Ndenderu"))
            .await
            .unwrap();
        let moved = weather.adapter().await.unwrap();
        assert!(!Arc::ptr_eq(&first, &moved));
        assert_eq!(
            *built.lock().unwrap(),
            vec![
                "Nairobi -1.286 36.817".to_string(),
                "Ndenderu -1.193 36.744".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn weather_switched_off_or_with_no_place_is_off_not_a_failure() {
        let held = Arc::new(Held(RwLock::new(settings(
            false, -1.286, 36.817, "Nairobi",
        ))));
        let (weather, _) = live(held.clone());
        let off = weather.current().await.unwrap_err();
        assert_eq!(off.downcast_ref::<WeatherOff>(), Some(&WeatherOff));

        let mut nowhere = settings(true, 0.0, 0.0, "");
        nowhere.timezone = "UTC".into();
        held.update(&nowhere).await.unwrap();
        let off = weather.forecast(3).await.unwrap_err();
        assert_eq!(off.downcast_ref::<WeatherOff>(), Some(&WeatherOff));
    }
}
