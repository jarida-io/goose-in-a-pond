# Worked Example: the Weather Tool

How "what's the weather?" reaches Open-Meteo and comes back, file by file. Use it as a map when
adding a tool backed by an outside service. The general port and adapter procedure is in
[Creating Ports and Adapters](creating-ports-and-adapters.md); the rules for MCP tools are in the
[GIAP MCP Server Design Standard](mcp-server-standard.md).

## The path of a request

```
user: "what's the weather?"
  GooseAdapter turn -> model calls giap-weather__get_current_weather {}
    WeatherMcpServer::get_current_weather        crates/pond-mcp-server/src/weather.rs
      WeatherProvider::current()                 crates/pond-adapters-weather/src/lib.rs
        OpenMeteoWeatherAdapter (15 min cache)   crates/pond-adapters-weather/src/open_meteo.rs
          traced_send -> egress gate -> api.open-meteo.com
    result: [[[mcp-ui:weather:{...}]]] line + a text block for the model
  desktop renders the weather card; the model answers from the text
```

## 1. The port and the adapter

`WeatherProvider` is defined in `pond-adapters-weather`, not in `pond-core`: only the MCP server and
the server wiring use it, and no `pond-core` service does. A port that a core service consumes
belongs in `pond-core/src/<context>/ports/` instead.

```rust
#[async_trait]
pub trait WeatherProvider: Send + Sync {
    async fn current(&self) -> Result<WeatherData>;
    async fn current_for(&self, location: &str) -> Result<WeatherData>;
    async fn forecast(&self, days: u8) -> Result<ForecastData>;
    async fn forecast_for(&self, location: &str, days: u8) -> Result<ForecastData>;
}
```

`OpenMeteoWeatherAdapter::new(latitude, longitude, location_name)` implements it:

- a named place is geocoded with `Geocoder` (`geocoding-api.open-meteo.com`); the configured home is
  geocoded from its name when only a name was saved;
- current conditions and forecasts are cached per place for 15 minutes;
- every request goes through `traced_send`, which refuses it on an `offline` pond and records it as
  egress;
- `WeatherData::as_context_block()` and `ForecastData::as_context_block()` render the text the model
  reads.

Tests sit beside the code and run against `wiremock` (`with_base_url` points the adapter at the mock
server): parsing, the context block, the cache serving a second call, an API error.

```bash
cargo test -p pond-adapters-weather
```

## 2. The MCP server

`WeatherMcpServer` (`crates/pond-mcp-server/src/weather.rs`) holds an
`Option<Arc<dyn WeatherProvider>>` and exposes two tools:

| Tool | Parameters |
|---|---|
| `get_current_weather` | `location` (optional; omitted means home) |
| `get_weather_forecast` | `location`, `days` (1 to 7, default 3) |

- Parameters are all optional, with a flattened `extra` map; `resolve_location` also accepts
  `city`, `place`, `loc` and `where`.
- With no provider (weather not set up), the tool returns `CallToolResult::error` saying so, so the
  model does not try another way.
- A success starts with a `[[[mcp-ui:weather:<json>]]]` line, which the desktop turns into a card,
  then the context block. The card is also served as the MCP App resource
  `ui://giap-weather/weather-card.html` (`crates/pond-mcp-server/apps/weather-card.html`), which
  `GET /api/v1/agent/tools` advertises on `get_current_weather`.
- Both tools ask for the same `weather` card with different payloads: `get_current_weather` sends one
  reading (`temperature`, `condition`, `humidity`, `wind_speed`, ...), `get_weather_forecast` sends
  `location` plus a `forecast` array of days (`date` as `YYYY-MM-DD`, `description`, `temp_max_c`,
  `temp_min_c`, `precipitation_probability_pct`, ...) and no reading. The dashboard tells them apart
  by shape in `pond-desktop/src/mcp-ui/cards/weatherPayload.ts`, as the phone does in
  `models/weather-card.ts`: a `forecast` array is a forecast card (a row per day, at most seven, the
  rest counted), a temperature or condition is the current card, and anything else is a plain
  "nothing to show" note (the spinner is only for the placeholder a card holds before its result
  arrives). Forecast dates are calendar days at the place, so they
  are parsed as local dates, not with `new Date("YYYY-MM-DD")` (midnight UTC, the previous day west
  of Greenwich). Renaming any of these fields breaks both clients, so change them together.
- Dependencies arrive through `init_weather_deps`; `spawn_weather_server` serves the server on the
  duplex pair Goose hands it.

## 3. Wiring

In `crates/pond-server/src/main.rs`, the provider is built from settings: `weather_enabled` plus a
place (`weather_latitude` / `weather_longitude`, or `weather_location_name`), resolved by
`pond_core::user_data::services::location::resolve`. Weather is not in `AppState`.

```rust
let weather: Option<Arc<dyn WeatherProvider>> = {
    let place = pond_core::user_data::services::location::resolve(&settings);
    if let (true, Some((lat, lon, loc))) = (settings.weather_enabled, place.weather_target()) {
        Some(Arc::new(OpenMeteoWeatherAdapter::new(lat, lon, loc)))
    } else {
        None
    }
};
```

It is passed to `register_giap_extensions`
(`crates/pond-adapters-goose/src/giap_registration.rs`), which, when `ext_weather_enabled` is on,
calls `init_weather_deps(weather)` and registers `giap-weather` as a Goose builtin. Its entry in
`TOOL_GROUPS` (`crates/pond-core/src/mcp/domain/tool_group.rs`) is what tool selection scores when
`tool_selection_mode` is `relevant`.

Two switches, then: `ext_weather_enabled` decides whether the tools exist; `weather_enabled` and a
place decide whether they have a provider behind them.

## Swapping the service

Implement `WeatherProvider` for another service, gate its requests through the egress gate as
`traced_send` does, and build it in place of `OpenMeteoWeatherAdapter` in `main.rs`. Nothing in the
MCP server or `pond-core` changes.
