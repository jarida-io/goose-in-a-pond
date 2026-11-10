//! Travel MCP server: directions, ride-app links, and ride offers sent to the speaker's own phone.
//! Nothing here books a ride: the member gets the fare and confirms on their phone.

use crate::Dep;
use pond_core::mcp::ports::notification::{MemberDelivery, MemberNotifier, Notification};
use pond_core::rides::ports::RideAccounts;
use pond_core::security::ports::draft_authority::DraftAuthority;
use pond_core::user_data::ports::place_lookup::{PlaceFix, PlaceLookup};
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::location::{self, Location};
use pond_core::user_data::services::nearby::{self, NEAR_HOME_KM};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, ErrorData, Implementation, InitializeResult, Meta,
        ProtocolVersion, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub use pond_core::mcp::domain::tool_group::TRAVEL_EXTENSION;

// ── Parameter structs ─────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DirectionsParams {
    pub destination: Option<String>,
    /// Omit for the phone's current location.
    pub origin: Option<String>,
    /// driving (default), walking, transit or cycling.
    pub mode: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RideParams {
    pub destination: Option<String>,
    /// Omit for the phone's current location.
    pub pickup: Option<String>,
    /// uber (default) or bolt.
    pub app: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct BookRideParams {
    pub destination: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ── Link building ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TravelMode {
    Driving,
    Walking,
    Transit,
    Cycling,
}

impl TravelMode {
    /// Unknown or missing words mean driving, the mode a ride-hailing household asks for most.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("walking" | "walk" | "foot") => Self::Walking,
            Some("transit" | "bus" | "matatu" | "train" | "public transport") => Self::Transit,
            Some("cycling" | "bicycling" | "bike" | "bicycle") => Self::Cycling,
            _ => Self::Driving,
        }
    }

    fn google(self) -> &'static str {
        match self {
            Self::Driving => "driving",
            Self::Walking => "walking",
            Self::Transit => "transit",
            Self::Cycling => "bicycling",
        }
    }

    /// Apple Maps has no cycling flag; `None` lets the app choose.
    fn apple(self) -> Option<&'static str> {
        match self {
            Self::Driving => Some("d"),
            Self::Walking => Some("w"),
            Self::Transit => Some("r"),
            Self::Cycling => None,
        }
    }
}

fn enc(s: &str) -> String {
    urlencoding::encode(s).into_owned()
}

/// Google Maps directions URL; without an origin the app starts from the phone's location.
pub fn google_maps_url(destination: &str, origin: Option<&str>, mode: TravelMode) -> String {
    let mut url = format!(
        "https://www.google.com/maps/dir/?api=1&destination={}&travelmode={}",
        enc(destination),
        mode.google()
    );
    if let Some(origin) = origin {
        url.push_str(&format!("&origin={}", enc(origin)));
    }
    url
}

/// Apple Maps directions URL; without `saddr` the app starts from the phone's location.
pub fn apple_maps_url(destination: &str, origin: Option<&str>, mode: TravelMode) -> String {
    let mut url = format!("https://maps.apple.com/?daddr={}", enc(destination));
    if let Some(origin) = origin {
        url.push_str(&format!("&saddr={}", enc(origin)));
    }
    if let Some(flag) = mode.apple() {
        url.push_str(&format!("&dirflg={flag}"));
    }
    url
}

/// A ride endpoint: a resolved place, or the phone's own location.
#[derive(Debug, Clone, PartialEq)]
pub enum RidePoint {
    MyLocation,
    Place {
        /// What the user said, shown in Uber as the address line.
        asked: String,
        fix: PlaceFix,
    },
}

/// Uber's documented universal link (`m.uber.com/ul`): opens the app, or the mobile site, on the
/// confirm screen. Uber requires coordinates for a drop-off, so `None` leaves it for the user.
pub fn uber_url(pickup: &RidePoint, dropoff: Option<&RidePoint>) -> String {
    let mut url = String::from("https://m.uber.com/ul/?action=setPickup");
    push_uber_point(&mut url, "pickup", pickup);
    if let Some(point) = dropoff {
        push_uber_point(&mut url, "dropoff", point);
    }
    url
}

fn push_uber_point(url: &mut String, role: &str, point: &RidePoint) {
    match point {
        RidePoint::MyLocation => url.push_str(&format!("&{role}=my_location")),
        RidePoint::Place { asked, fix } => url.push_str(&format!(
            "&{role}[latitude]={:.6}&{role}[longitude]={:.6}&{role}[nickname]={}\
             &{role}[formatted_address]={}",
            fix.latitude,
            fix.longitude,
            enc(&fix.name),
            enc(asked),
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RideApp {
    Uber,
    Bolt,
}

impl RideApp {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some(s) if s.contains("bolt") || s.contains("taxify") => Self::Bolt,
            _ => Self::Uber,
        }
    }
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct TravelMcpServer {
    places: Option<Arc<dyn PlaceLookup>>,
    /// Where home is, read per call; without it a name takes its best match anywhere.
    settings: Option<Arc<dyn SettingsRepository + Send + Sync>>,
    /// The home's country, keyed by the home it was learned for: learning it costs a lookup.
    home_country: Arc<Mutex<Option<LearnedCountry>>>,
    /// Both set: links are also pushed to the speaker's own phones. Either unset: reply only.
    authority: Dep<dyn DraftAuthority>,
    notifier: Dep<dyn MemberNotifier>,
    /// Which members can book; unset means booking is not set up and `book_ride` says so.
    ride_accounts: Dep<dyn RideAccounts>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl TravelMcpServer {
    /// Every tool this server exposes, without constructing it (`tool_router()` is private).
    pub(crate) fn tool_defs() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    pub fn new(places: Option<Arc<dyn PlaceLookup>>) -> Self {
        Self {
            places,
            settings: None,
            home_country: Arc::new(Mutex::new(None)),
            authority: Dep::Fixed(None),
            notifier: Dep::Fixed(None),
            ride_accounts: Dep::Fixed(None),
            tool_router: Self::tool_router(),
        }
    }

    /// Match place names near the home in these settings rather than anywhere in the world.
    pub fn with_home(
        mut self,
        settings: Option<Arc<dyn SettingsRepository + Send + Sync>>,
    ) -> Self {
        self.settings = settings;
        self
    }

    pub fn with_phone_delivery(
        mut self,
        authority: Option<Arc<dyn DraftAuthority>>,
        notifier: Option<Arc<dyn MemberNotifier>>,
    ) -> Self {
        self.authority = Dep::Fixed(authority);
        self.notifier = Dep::Fixed(notifier);
        self
    }

    pub fn with_ride_accounts(mut self, accounts: Option<Arc<dyn RideAccounts>>) -> Self {
        self.ride_accounts = Dep::Fixed(accounts);
        self
    }

    /// Read the speaker authority, member notifier and ride accounts from the process-wide
    /// slots on every call, so ones installed after goose spawned this server are still used.
    fn reading_installed_deps(mut self) -> Self {
        self.authority = Dep::Installed(crate::speaker_authority);
        self.notifier = Dep::Installed(crate::member_notifier);
        self.ride_accounts = Dep::Installed(installed_ride_accounts);
        self
    }

    #[tool(description = "\
Directions to a place as Google Maps and Apple Maps links the user opens on \
their phone. Omit origin for their current location. Never invent routes or times.")]
    async fn get_directions_link(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<DirectionsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_directions_link");
        Ok(self.directions_result(&ctx.meta, &params.0).await)
    }

    #[tool(description = "\
Book an Uber for the person speaking: sends the trip to their own phone, which gets \
the fare from where they are and books only when they confirm. Never claim a ride is booked.")]
    async fn book_ride(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<BookRideParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("book_ride");
        Ok(self.book_result(&ctx.meta, &params.0).await)
    }

    #[tool(description = "\
Link that opens Uber (default) or Bolt with a ride filled in; the user confirms \
and pays in the app. Omit pickup for their current location. Never claim a ride is booked.")]
    async fn get_ride_link(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<RideParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("get_ride_link");
        Ok(self.ride_result(&ctx.meta, &params.0).await)
    }
}

impl TravelMcpServer {
    /// The body of `get_ride_link`, apart from the rmcp wrapper so tests can call it.
    pub async fn ride_result(&self, meta: &Meta, params: &RideParams) -> CallToolResult {
        let destination = text_param(&params.destination, &params.extra, DESTINATION_KEYS);
        let pickup = text_param(&params.pickup, &params.extra, PICKUP_KEYS);
        let app = RideApp::parse(
            params
                .app
                .as_deref()
                .or_else(|| extra_str(&params.extra, &["provider", "service"])),
        );

        if app == RideApp::Bolt {
            return CallToolResult::error(vec![ContentBlock::text(format!(
                "Bolt publishes no link that fills in a trip, so none was made. Nothing has been \
                 booked.{}",
                destination
                    .map(|d| format!(" Destination asked for: {d}."))
                    .unwrap_or_default()
            ))]);
        }

        let mut notes = Vec::new();

        let pickup_point = match &pickup {
            None => RidePoint::MyLocation,
            Some(asked) => match self.resolve(asked).await {
                Ok(matched) => {
                    notes.extend(matched.note(asked));
                    RidePoint::Place {
                        asked: asked.clone(),
                        fix: matched.fix,
                    }
                }
                Err(why) => {
                    notes.push(format!(
                        "Pickup is the phone's current location: {why} for \"{asked}\"."
                    ));
                    RidePoint::MyLocation
                }
            },
        };

        let dropoff_point = match &destination {
            None => {
                notes.push("No destination was given, so the drop-off is empty.".to_string());
                None
            }
            Some(asked) => match self.resolve(asked).await {
                Ok(matched) => {
                    notes.extend(matched.note(asked));
                    Some(RidePoint::Place {
                        asked: asked.clone(),
                        fix: matched.fix,
                    })
                }
                Err(why) => {
                    notes.push(format!("The drop-off is empty: {why} for \"{asked}\"."));
                    None
                }
            },
        };

        let url = uber_url(&pickup_point, dropoff_point.as_ref());
        let mut lines = vec![format!("Uber link: {url}")];
        lines.push(format!("Pickup: {}", describe(&pickup_point)));
        if let Some(point) = &dropoff_point {
            lines.push(format!("Drop-off: {}", describe(point)));
        }
        lines.extend(notes);
        let title = match &dropoff_point {
            Some(RidePoint::Place { fix, .. }) => format!("Ride to {}", fix.name),
            _ => "Uber ride".to_string(),
        };
        let link = PhoneLink {
            kind: "ride",
            url,
            title,
            body: "Opens Uber with the trip filled in. Nothing is booked until you confirm it \
                   there."
                .to_string(),
            label: "Open Uber",
        };
        lines.extend(self.deliver(meta, link).await);
        lines.push(
            "Nothing has been booked. The ride is requested only when the user confirms it in \
             Uber, which also shows the fare."
                .to_string(),
        );
        CallToolResult::success(vec![ContentBlock::text(lines.join("\n"))])
    }

    /// The place `query` names: the match nearest home within [`NEAR_HOME_KM`], or with no
    /// home set, the best match anywhere. `Err` says why there is none.
    async fn resolve(&self, query: &str) -> Result<Matched, String> {
        let Some(places) = &self.places else {
            return Err("this pond has no place lookup".to_string());
        };
        let home = self.home().await;
        let found = match &home {
            Some(home) => {
                let country = self.home_country(places.as_ref(), home).await;
                nearby::near_home(
                    places.as_ref(),
                    query,
                    (home.latitude, home.longitude),
                    country.as_deref(),
                )
                .await
                .map(|fix| {
                    fix.map(|fix| Matched {
                        fix,
                        anywhere: false,
                    })
                })
            }
            None => places.candidates(query, None, 1).await.map(|found| {
                found.into_iter().next().map(|c| Matched {
                    fix: c.fix,
                    anywhere: true,
                })
            }),
        };
        match found {
            Ok(Some(matched)) => Ok(matched),
            Ok(None) if home.is_some() => Err(format!(
                "no place matched within {NEAR_HOME_KM:.0} km of home"
            )),
            Ok(None) => Err("no place matched".to_string()),
            Err(e) => {
                tracing::debug!(error = %format!("{e:#}"), "travel: place lookup failed");
                Err("the place lookup failed".to_string())
            }
        }
    }

    /// Where the household is, when the pond knows its coordinates.
    async fn home(&self) -> Option<Location> {
        let settings = match self.settings.as_ref()?.get().await {
            Ok(settings) => settings,
            Err(e) => {
                tracing::warn!(error = %e, "travel: could not read where home is");
                return None;
            }
        };
        let home = location::resolve(&settings);
        home.has_coordinates().then_some(home)
    }

    /// The home's country, learned once per home; `None` when it cannot be told.
    async fn home_country(&self, places: &dyn PlaceLookup, home: &Location) -> Option<String> {
        let key = format!("{:.4},{:.4},{}", home.latitude, home.longitude, home.name);
        if let Some(learned) = &*self.lock_home_country() {
            if learned.home == key {
                return learned.code.clone();
            }
        }
        match nearby::home_country(places, home).await {
            Ok(code) => {
                *self.lock_home_country() = Some(LearnedCountry {
                    home: key,
                    code: code.clone(),
                });
                code
            }
            Err(e) => {
                tracing::debug!(error = %format!("{e:#}"), "travel: could not learn home's country");
                None
            }
        }
    }

    fn lock_home_country(&self) -> std::sync::MutexGuard<'_, Option<LearnedCountry>> {
        // A poisoned cache holds a whole value either way: every write is one assignment.
        self.home_country.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A home's country, and the home it was learned for.
struct LearnedCountry {
    home: String,
    code: Option<String>,
}

/// A place a name was matched to.
struct Matched {
    fix: PlaceFix,
    /// No home is set, so this is the best match anywhere in the world.
    anywhere: bool,
}

impl Matched {
    fn note(&self, asked: &str) -> Option<String> {
        self.anywhere.then(|| {
            format!(
                "No home location is set on this pond, so \"{asked}\" was matched to the best \
                 place of that name anywhere."
            )
        })
    }
}

fn describe(point: &RidePoint) -> String {
    match point {
        RidePoint::MyLocation => "the phone's current location".to_string(),
        // The lookup matches towns and landmarks, not addresses, so the match is always shown.
        RidePoint::Place { asked, fix } => format!("{} (matched from \"{asked}\")", fix.name),
    }
}

/// The directions reply and the link to push, or the error result. Pure: the maps apps resolve
/// free text themselves.
fn directions(params: &DirectionsParams) -> Result<(String, PhoneLink), CallToolResult> {
    let Some(destination) = text_param(&params.destination, &params.extra, DESTINATION_KEYS) else {
        return Err(CallToolResult::error(vec![ContentBlock::text(
            "No destination was given, so no directions link was made.",
        )]));
    };
    let origin = text_param(&params.origin, &params.extra, ORIGIN_KEYS);
    let mode = TravelMode::parse(
        params
            .mode
            .as_deref()
            .or_else(|| extra_str(&params.extra, &["travelmode", "travel_mode", "by"])),
    );
    let from = origin
        .as_deref()
        .map(|o| format!("from {o}"))
        .unwrap_or_else(|| "from the phone's current location".to_string());
    let google = google_maps_url(&destination, origin.as_deref(), mode);
    let text = format!(
        "Directions to {destination}, {from}, {}.\nGoogle Maps: {google}\nApple Maps: {}",
        mode.google(),
        apple_maps_url(&destination, origin.as_deref(), mode),
    );
    // Google's link opens on both platforms: Google Maps where installed, else the browser.
    let link = PhoneLink {
        kind: "directions",
        url: google,
        title: format!("Directions to {destination}"),
        body: format!("Opens the map {from}."),
        label: "Open directions",
    };
    Ok((text, link))
}

/// A link to put on the speaker's phone; `kind` and `label` follow GOTG's `open_url` contract.
struct PhoneLink {
    kind: &'static str,
    url: String,
    title: String,
    body: String,
    label: &'static str,
}

/// Send to one member's own devices, never a broadcast.
async fn push(
    notifier: &dyn MemberNotifier,
    profile_id: &str,
    category: &str,
    title: String,
    body: String,
    data: serde_json::Value,
) -> MemberDelivery {
    let notification = Notification {
        id: uuid::Uuid::new_v4().to_string(),
        target: profile_id.to_string(),
        category: category.to_string(),
        title,
        body,
        timestamp: chrono::Utc::now().to_rfc3339(),
        data: Some(data),
    };
    notifier.notify_member(profile_id, notification).await
}

impl TravelMcpServer {
    /// The body of `get_directions_link`, apart from the rmcp wrapper so tests can call it.
    pub async fn directions_result(
        &self,
        meta: &Meta,
        params: &DirectionsParams,
    ) -> CallToolResult {
        match directions(params) {
            Err(result) => result,
            Ok((text, link)) => {
                let mut lines = vec![text];
                lines.extend(self.deliver(meta, link).await);
                CallToolResult::success(vec![ContentBlock::text(lines.join("\n"))])
            }
        }
    }

    /// Push `link` to the speaker's own phones. `None` when delivery isn't wired; otherwise the
    /// line saying where it went. Only one member is ever pushed to (see [`Self::speaker`]): a
    /// broadcast would put one person's trip on every phone.
    async fn deliver(&self, meta: &Meta, link: PhoneLink) -> Option<String> {
        let (Some(_), Some(notifier)) = (self.authority.get(), self.notifier.get()) else {
            return None;
        };
        let profile_id = match self.speaker(meta).await {
            Ok(id) => id,
            Err(why) => return Some(format!("Not sent to a phone: {why}.")),
        };
        let data = serde_json::json!({
            "action": "open_url",
            "kind": link.kind,
            "url": link.url,
            "label": link.label,
        });
        let delivery = push(
            notifier.as_ref(),
            &profile_id,
            "info",
            link.title,
            link.body,
            data,
        )
        .await;
        Some(match delivery {
            MemberDelivery::Reached(devices) if devices.len() == 1 => {
                "Also sent to the speaker's phone.".to_string()
            }
            MemberDelivery::Reached(devices) => {
                format!(
                    "Also sent to the speaker's {} paired devices.",
                    devices.len()
                )
            }
            MemberDelivery::NoPhone => {
                "Not sent to a phone: the speaker has no paired phone of their own.".to_string()
            }
            MemberDelivery::Failed(why) => {
                tracing::warn!(error = %why, "travel: could not send the link to the speaker's phone");
                "Not sent to a phone: sending to the speaker's phone failed on the pond."
                    .to_string()
            }
        })
    }

    async fn speaker(&self, meta: &Meta) -> Result<String, &'static str> {
        crate::session_meta::speaking_member(self.authority.get(), meta).await
    }

    /// The body of `book_ride`, apart from the rmcp wrapper so tests can call it.
    pub async fn book_result(&self, meta: &Meta, params: &BookRideParams) -> CallToolResult {
        let fail = |text: String| CallToolResult::error(vec![ContentBlock::text(text)]);
        let Some(accounts) = self.ride_accounts.get() else {
            return fail(
                "Booking rides is not set up on this pond, so no ride offer was sent.".into(),
            );
        };
        let Some(notifier) = self.notifier.get() else {
            return fail("This pond cannot send to phones, so no ride offer was sent.".into());
        };
        let Some(destination) = text_param(&params.destination, &params.extra, DESTINATION_KEYS)
        else {
            return fail("No destination was given, so no ride offer was sent.".into());
        };
        let profile_id = match self.speaker(meta).await {
            Ok(id) => id,
            Err(why) => return fail(format!("No ride offer was sent: {why}.")),
        };
        match accounts.is_connected(&profile_id).await {
            Ok(true) => {}
            Ok(false) => {
                return fail(
                    "This member has not connected Uber on this pond (Settings, Accounts), so no \
                     ride offer was sent."
                        .into(),
                )
            }
            Err(e) => {
                tracing::warn!(error = %e, "travel: could not read the member's Uber connection");
                return fail(
                    "Could not read this member's Uber connection; no ride offer was sent.".into(),
                );
            }
        }
        let matched = match self.resolve(&destination).await {
            Ok(matched) => matched,
            Err(why) => {
                return fail(format!(
                    "No ride offer was sent: {why} for \"{destination}\"."
                ))
            }
        };
        let note = matched.note(&destination);
        let fix = matched.fix;

        let data = serde_json::json!({
            "action": "ride_offer",
            "provider": "uber",
            "dropoff": {"name": fix.name, "latitude": fix.latitude, "longitude": fix.longitude},
            "label": "Get a fare",
        });
        let delivery = push(
            notifier.as_ref(),
            &profile_id,
            "action_required",
            format!("Ride to {}?", fix.name),
            "Get an Uber fare from where you are. Nothing is booked until you confirm.".into(),
            data,
        )
        .await;
        match delivery {
            MemberDelivery::Reached(_) => {}
            MemberDelivery::NoPhone => {
                return fail(
                    "No ride offer was sent: the speaker has no paired phone of their own.".into(),
                )
            }
            MemberDelivery::Failed(why) => {
                tracing::warn!(error = %why, "travel: could not send the ride offer");
                return fail(
                    "No ride offer was sent: sending to the speaker's phone failed on the pond."
                        .into(),
                );
            }
        }
        let mut text = format!(
            "Sent a ride offer to the speaker's phone: a ride to {} (matched from \"{destination}\"). \
             The phone gets the fare from where it is. Nothing is booked until they confirm on the phone.",
            fix.name
        );
        if let Some(note) = note {
            text.push('\n');
            text.push_str(&note);
        }
        CallToolResult::success(vec![ContentBlock::text(text)])
    }
}

// ── Param resolution ──────────────────────────────────────────────────────────

const DESTINATION_KEYS: &[&str] = &["destination", "to", "dropoff", "place", "where", "address"];
const ORIGIN_KEYS: &[&str] = &["origin", "from", "start"];
const PICKUP_KEYS: &[&str] = &["pickup", "from", "origin", "start"];

/// The named param, else the first non-blank string among `keys` in the extras.
fn text_param(
    direct: &Option<String>,
    extra: &HashMap<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    direct
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| extra_str(extra, keys))
        .map(str::to_string)
}

fn extra_str<'a>(extra: &'a HashMap<String, serde_json::Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|k| extra.get(*k).and_then(|v| v.as_str()))
        .map(str::trim)
        .find(|s| !s.is_empty())
}

#[tool_handler]
impl ServerHandler for TravelMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                TRAVEL_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Travel MCP server — links the user opens on their phone.\n\n\
                 Tools:\n\
                 - get_directions_link: Google Maps and Apple Maps directions to a place.\n\
                 - get_ride_link: an Uber link with pickup and drop-off filled in. Bolt has no \
                 such link.\n\
                 - book_ride: an Uber ride offer on the speaker's own phone, booked only when \
                 they confirm there.\n\n\
                 Nothing here books a ride without the member's confirmation on their phone.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct TravelDeps {
    places: Option<Arc<dyn PlaceLookup>>,
    settings: Option<Arc<dyn SettingsRepository + Send + Sync>>,
}

static TRAVEL_DEPS: OnceLock<TravelDeps> = OnceLock::new();
static RIDE_ACCOUNTS: OnceLock<Arc<dyn RideAccounts>> = OnceLock::new();

/// Turn on `book_ride`. Call once at startup, when a ride provider is configured.
pub fn init_ride_accounts(accounts: Arc<dyn RideAccounts>) {
    let _ = RIDE_ACCOUNTS.set(accounts);
}

fn installed_ride_accounts() -> Option<Arc<dyn RideAccounts>> {
    RIDE_ACCOUNTS.get().cloned()
}

/// Install the place lookup and the settings that say where home is. Call once at startup.
pub fn init_travel_deps(
    places: Option<Arc<dyn PlaceLookup>>,
    settings: Option<Arc<dyn SettingsRepository + Send + Sync>>,
) {
    let _ = TRAVEL_DEPS.set(TravelDeps { places, settings });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_travel_server(reader: DuplexStream, writer: DuplexStream) {
    // No panic: a panic in one spawn fn kills every builtin's startup.
    let Some(deps) = TRAVEL_DEPS.get() else {
        tracing::error!(
            "spawn_travel_server called before init_travel_deps — extension will not start"
        );
        return;
    };
    let server = TravelMcpServer::new(deps.places.clone())
        .with_home(deps.settings.clone())
        .reading_installed_deps();
    crate::serve_builtin(TRAVEL_EXTENSION, server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Test doubles shared by the travel test modules.
#[cfg(test)]
mod fakes {
    use super::*;
    use async_trait::async_trait;
    use pond_core::user_data::domain::settings::Settings;
    use pond_core::user_data::ports::place_lookup::PlaceCandidate;

    fn candidate(name: &str, latitude: f64, longitude: f64, country: &str) -> PlaceCandidate {
        PlaceCandidate {
            fix: PlaceFix {
                name: name.to_string(),
                latitude,
                longitude,
                timezone: None,
            },
            country_code: Some(country.to_string()),
        }
    }

    /// Ranks like the real geocoder: a namesake far away comes before the one near home. It
    /// ignores the country asked for, so only picking by distance can find the near one.
    #[derive(Default)]
    pub struct Places {
        asked: Mutex<Vec<(String, Option<String>)>>,
    }

    impl Places {
        /// Every `(name, country_code)` searched, in order.
        pub fn asked(&self) -> Vec<(String, Option<String>)> {
            self.asked.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl PlaceLookup for Places {
        async fn by_name(&self, query: &str) -> anyhow::Result<PlaceFix> {
            anyhow::bail!("travel matches names through candidates(), not by_name({query})")
        }

        async fn candidates(
            &self,
            query: &str,
            country_code: Option<&str>,
            limit: usize,
        ) -> anyhow::Result<Vec<PlaceCandidate>> {
            self.asked
                .lock()
                .unwrap()
                .push((query.to_string(), country_code.map(str::to_string)));
            let all = match query {
                "JKIA" => vec![
                    candidate("Jkia Hill, Tanzania", -6.8, 39.2, "TZ"),
                    candidate(
                        "Jomo Kenyatta International Airport, Kenya",
                        -1.319167,
                        36.9275,
                        "KE",
                    ),
                ],
                "Westlands" => vec![
                    candidate("Westlands, Jamaica", 18.03, -76.79, "JM"),
                    candidate("Westlands, Kenya", -1.2676, 36.8108, "KE"),
                ],
                "Kisumu" => vec![candidate("Kisumu, Kenya", -0.1022, 34.7617, "KE")],
                "Nairobi" => vec![candidate("Nairobi, Kenya", -1.2833, 36.8167, "KE")],
                _ => Vec::new(),
            };
            Ok(all.into_iter().take(limit).collect())
        }
    }

    /// Settings whose home is the given place, or unset.
    pub struct Home(pub Option<(&'static str, f64, f64)>);

    /// A pond at home in Nairobi.
    pub fn nairobi() -> Arc<Home> {
        Arc::new(Home(Some(("Nairobi, Kenya", -1.286, 36.817))))
    }

    #[async_trait]
    impl SettingsRepository for Home {
        async fn get(&self) -> anyhow::Result<Settings> {
            let mut settings = Settings::default();
            if let Some((name, latitude, longitude)) = self.0 {
                settings.weather_location_name = name.to_string();
                settings.weather_latitude = latitude;
                settings.weather_longitude = longitude;
            }
            Ok(settings)
        }
        async fn update(&self, _: &Settings) -> anyhow::Result<()> {
            Ok(())
        }
        async fn get_key(&self, _: &str) -> anyhow::Result<Option<String>> {
            Ok(None)
        }
        async fn set_key(&self, _: &str, _: String) -> anyhow::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fakes::{nairobi, Home, Places};
    use super::*;

    fn server() -> TravelMcpServer {
        TravelMcpServer::new(Some(Arc::new(Places::default()))).with_home(Some(nairobi()))
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn ride(destination: Option<&str>, pickup: Option<&str>, app: Option<&str>) -> RideParams {
        RideParams {
            destination: destination.map(str::to_string),
            pickup: pickup.map(str::to_string),
            app: app.map(str::to_string),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn google_link_encodes_the_destination_and_omits_a_missing_origin() {
        let url = google_maps_url("Sarit Centre, Nairobi", None, TravelMode::Driving);
        assert_eq!(
            url,
            "https://www.google.com/maps/dir/?api=1&destination=Sarit%20Centre%2C%20Nairobi\
             &travelmode=driving"
        );
    }

    #[test]
    fn google_link_carries_an_origin_and_the_mode() {
        let url = google_maps_url("Kisumu", Some("Nakuru"), TravelMode::Cycling);
        assert!(url.contains("&origin=Nakuru"), "{url}");
        assert!(url.contains("&travelmode=bicycling"), "{url}");
    }

    #[test]
    fn apple_link_uses_daddr_saddr_and_flags() {
        assert_eq!(
            apple_maps_url("Kisumu", Some("Nakuru"), TravelMode::Transit),
            "https://maps.apple.com/?daddr=Kisumu&saddr=Nakuru&dirflg=r"
        );
        assert_eq!(
            apple_maps_url("Kisumu", None, TravelMode::Cycling),
            "https://maps.apple.com/?daddr=Kisumu"
        );
    }

    #[test]
    fn mode_words_map_and_unknown_means_driving() {
        assert_eq!(TravelMode::parse(Some("Walk")), TravelMode::Walking);
        assert_eq!(TravelMode::parse(Some("matatu")), TravelMode::Transit);
        assert_eq!(TravelMode::parse(Some("bike")), TravelMode::Cycling);
        assert_eq!(TravelMode::parse(Some("hovercraft")), TravelMode::Driving);
        assert_eq!(TravelMode::parse(None), TravelMode::Driving);
    }

    #[test]
    fn uber_link_with_my_location_and_a_resolved_dropoff() {
        let dropoff = RidePoint::Place {
            asked: "JKIA".to_string(),
            fix: PlaceFix {
                name: "JKIA, Kenya".to_string(),
                latitude: -1.319167,
                longitude: 36.9275,
                timezone: None,
            },
        };
        assert_eq!(
            uber_url(&RidePoint::MyLocation, Some(&dropoff)),
            "https://m.uber.com/ul/?action=setPickup&pickup=my_location\
             &dropoff[latitude]=-1.319167&dropoff[longitude]=36.927500\
             &dropoff[nickname]=JKIA%2C%20Kenya&dropoff[formatted_address]=JKIA"
        );
    }

    #[test]
    fn uber_link_without_a_dropoff_sets_only_the_pickup() {
        assert_eq!(
            uber_url(&RidePoint::MyLocation, None),
            "https://m.uber.com/ul/?action=setPickup&pickup=my_location"
        );
    }

    #[test]
    fn ride_app_defaults_to_uber() {
        assert_eq!(RideApp::parse(None), RideApp::Uber);
        assert_eq!(RideApp::parse(Some("an Uber please")), RideApp::Uber);
        assert_eq!(RideApp::parse(Some("Bolt")), RideApp::Bolt);
    }

    #[tokio::test]
    async fn a_resolved_ride_names_the_match_and_books_nothing() {
        let out = text(
            &server()
                .ride_result(&Meta::new(), &ride(Some("JKIA"), None, None))
                .await,
        );
        assert!(out.contains("https://m.uber.com/ul/?action=setPickup&pickup=my_location"));
        assert!(out.contains("dropoff[latitude]=-1.319167"), "{out}");
        assert!(
            out.contains("Jomo Kenyatta International Airport, Kenya"),
            "{out}"
        );
        assert!(out.contains("Nothing has been booked"), "{out}");
    }

    #[tokio::test]
    async fn a_named_pickup_is_resolved_too() {
        let out = text(
            &server()
                .ride_result(&Meta::new(), &ride(Some("JKIA"), Some("Westlands"), None))
                .await,
        );
        assert!(out.contains("pickup[latitude]=-1.267600"), "{out}");
        assert!(!out.contains("pickup=my_location"), "{out}");
    }

    #[tokio::test]
    async fn an_unmatched_destination_leaves_the_dropoff_empty_and_says_so() {
        let result = server()
            .ride_result(&Meta::new(), &ride(Some("my aunt's place"), None, None))
            .await;
        let out = text(&result);
        assert_ne!(result.is_error, Some(true));
        assert!(!out.contains("dropoff["), "{out}");
        assert!(out.contains("The drop-off is empty"), "{out}");
    }

    #[tokio::test]
    async fn the_namesake_near_home_beats_the_geocoders_first_match() {
        let out = text(
            &server()
                .ride_result(&Meta::new(), &ride(Some("Westlands"), None, None))
                .await,
        );
        assert!(out.contains("dropoff[latitude]=-1.267600"), "{out}");
        assert!(out.contains("Westlands, Kenya"), "{out}");
        assert!(!out.contains("Jamaica"), "{out}");
        assert!(!out.contains("No home location"), "{out}");
    }

    #[tokio::test]
    async fn a_place_matching_only_far_from_home_is_no_match() {
        let result = server()
            .ride_result(&Meta::new(), &ride(Some("Kisumu"), None, None))
            .await;
        let out = text(&result);
        assert!(!out.contains("dropoff["), "{out}");
        assert!(
            out.contains("no place matched within 50 km of home for \"Kisumu\""),
            "{out}"
        );
    }

    #[tokio::test]
    async fn without_a_home_the_best_match_anywhere_is_used_and_the_result_says_so() {
        let out = text(
            &TravelMcpServer::new(Some(Arc::new(Places::default())))
                .with_home(Some(Arc::new(Home(None))))
                .ride_result(&Meta::new(), &ride(Some("Westlands"), None, None))
                .await,
        );
        assert!(out.contains("Westlands, Jamaica"), "{out}");
        assert!(
            out.contains("No home location is set on this pond"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn the_homes_country_is_learned_once_and_then_searched() {
        let places = Arc::new(Places::default());
        let server = TravelMcpServer::new(Some(places.clone())).with_home(Some(nairobi()));
        server
            .ride_result(&Meta::new(), &ride(Some("Westlands"), None, None))
            .await;
        server
            .ride_result(&Meta::new(), &ride(Some("JKIA"), None, None))
            .await;
        assert_eq!(
            places.asked(),
            vec![
                ("Nairobi".to_string(), None),
                ("Westlands".to_string(), Some("KE".to_string())),
                ("JKIA".to_string(), Some("KE".to_string())),
            ]
        );
    }

    #[tokio::test]
    async fn without_a_place_lookup_the_link_still_opens_uber() {
        let out = text(
            &TravelMcpServer::new(None)
                .ride_result(&Meta::new(), &ride(Some("JKIA"), None, None))
                .await,
        );
        assert!(out.contains("pickup=my_location"), "{out}");
        assert!(out.contains("this pond has no place lookup"), "{out}");
    }

    #[tokio::test]
    async fn bolt_is_an_error_that_makes_no_link() {
        let result = server()
            .ride_result(&Meta::new(), &ride(Some("JKIA"), None, Some("bolt")))
            .await;
        assert_eq!(result.is_error, Some(true));
        let out = text(&result);
        assert!(!out.contains("https://"), "{out}");
        assert!(out.contains("Nothing has been booked"), "{out}");
    }

    #[tokio::test]
    async fn directions_read_the_destination_from_extras() {
        let mut extra = HashMap::new();
        extra.insert("to".to_string(), serde_json::json!("Kisumu"));
        let out = text(
            &server()
                .directions_result(
                    &Meta::new(),
                    &DirectionsParams {
                        extra,
                        ..Default::default()
                    },
                )
                .await,
        );
        assert!(out.contains("destination=Kisumu"), "{out}");
        assert!(out.contains("daddr=Kisumu"), "{out}");
    }

    #[tokio::test]
    async fn directions_without_a_destination_are_an_error() {
        let result = server()
            .directions_result(&Meta::new(), &DirectionsParams::default())
            .await;
        assert_eq!(result.is_error, Some(true));
    }
}

#[cfg(test)]
mod delivery_tests {
    //! Pushing the link to the speaker's own phone: a named member only, never a broadcast.

    use super::*;
    use crate::session_meta::fakes::{meta, FixedSpeaker};
    use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use pond_core::user_data::domain::profile::ProfileScope;

    fn server(speaker: Option<ProfileScope>, notifier: Arc<MockMemberNotifier>) -> TravelMcpServer {
        in_household(speaker, None, notifier)
    }

    fn in_household(
        scope: Option<ProfileScope>,
        sole: Option<&'static str>,
        notifier: Arc<MockMemberNotifier>,
    ) -> TravelMcpServer {
        TravelMcpServer::new(None)
            .with_phone_delivery(Some(Arc::new(FixedSpeaker { scope, sole })), Some(notifier))
    }

    fn directions_to(place: &str) -> DirectionsParams {
        DirectionsParams {
            destination: Some(place.to_string()),
            ..Default::default()
        }
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn a_named_member_gets_the_link_on_their_phone() {
        let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
        let out = text(
            &server(Some(ProfileScope::Owner("liz".into())), notifier.clone())
                .directions_result(&meta(), &directions_to("Kisumu"))
                .await,
        );
        assert!(out.contains("Also sent to the speaker's phone."), "{out}");

        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "liz");
        let data = sent[0].1.data.as_ref().expect("the link rides in data");
        assert_eq!(data["action"], "open_url");
        assert_eq!(data["kind"], "directions");
        assert!(data["url"]
            .as_str()
            .unwrap()
            .starts_with("https://www.google.com/maps/dir/"));
    }

    #[tokio::test]
    async fn a_ride_is_pushed_with_the_uber_link() {
        let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
        server(Some(ProfileScope::Owner("liz".into())), notifier.clone())
            .ride_result(
                &meta(),
                &RideParams {
                    destination: Some("anywhere".into()),
                    ..Default::default()
                },
            )
            .await;
        let data = notifier.sent()[0].1.data.clone().unwrap();
        assert_eq!(data["kind"], "ride");
        assert_eq!(data["label"], "Open Uber");
        assert!(data["url"]
            .as_str()
            .unwrap()
            .starts_with("https://m.uber.com/ul/"));
    }

    #[tokio::test]
    async fn a_one_member_household_is_pushed_to_its_only_member() {
        let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
        let out = text(
            &in_household(Some(ProfileScope::Household), Some("liz"), notifier.clone())
                .directions_result(&meta(), &directions_to("Kisumu"))
                .await,
        );
        assert!(out.contains("Also sent to the speaker's phone."), "{out}");
        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "liz");
    }

    #[tokio::test]
    async fn a_household_of_several_a_guest_or_nobody_is_never_pushed_to() {
        let cases = [
            // Several members: the household is nobody in particular.
            (Some(ProfileScope::Household), None),
            // A guest is nobody's, even in a one-member pond.
            (Some(ProfileScope::Guest), Some("liz")),
            (None, Some("liz")),
        ];
        for (scope, sole) in cases {
            let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
            let out = text(
                &in_household(scope.clone(), sole, notifier.clone())
                    .directions_result(&meta(), &directions_to("Kisumu"))
                    .await,
            );
            assert!(notifier.sent().is_empty(), "{scope:?} was pushed to");
            assert!(out.contains("Not sent to a phone"), "{out}");
        }
    }

    #[tokio::test]
    async fn a_member_without_a_phone_is_told_so() {
        let notifier = Arc::new(MockMemberNotifier::new());
        let out = text(
            &server(Some(ProfileScope::Owner("jerry".into())), notifier)
                .directions_result(&meta(), &directions_to("Kisumu"))
                .await,
        );
        assert!(out.contains("no paired phone of their own"), "{out}");
    }

    #[tokio::test]
    async fn a_delivery_that_failed_is_not_reported_as_no_phone() {
        let notifier = Arc::new(
            MockMemberNotifier::new()
                .with_devices("liz", &["liz-phone"])
                .failing_next(1),
        );
        let out = text(
            &server(Some(ProfileScope::Owner("liz".into())), notifier)
                .directions_result(&meta(), &directions_to("Kisumu"))
                .await,
        );
        assert!(out.contains("failed on the pond"), "{out}");
        assert!(!out.contains("no paired phone"), "{out}");
    }

    #[tokio::test]
    async fn a_call_without_a_session_is_not_pushed() {
        let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
        let out = text(
            &server(Some(ProfileScope::Owner("liz".into())), notifier.clone())
                .directions_result(&Meta::new(), &directions_to("Kisumu"))
                .await,
        );
        assert!(notifier.sent().is_empty());
        assert!(out.contains("carries no session"), "{out}");
    }

    /// A schedule can make goose spawn this server before startup installs these.
    #[tokio::test]
    async fn deps_installed_after_the_server_was_built_are_used() {
        static NOTIFIER: OnceLock<Arc<dyn MemberNotifier>> = OnceLock::new();
        static AUTHORITY: OnceLock<Arc<dyn DraftAuthority>> = OnceLock::new();
        let mut server = TravelMcpServer::new(None);
        server.notifier = Dep::Installed(|| NOTIFIER.get().cloned());
        server.authority = Dep::Installed(|| AUTHORITY.get().cloned());

        let before = text(
            &server
                .directions_result(&meta(), &directions_to("Kisumu"))
                .await,
        );
        assert!(!before.contains("sent to"), "{before}");

        let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
        assert!(NOTIFIER.set(notifier.clone()).is_ok());
        assert!(AUTHORITY
            .set(Arc::new(FixedSpeaker {
                scope: Some(ProfileScope::Owner("liz".into())),
                sole: None,
            }))
            .is_ok());
        let after = text(
            &server
                .directions_result(&meta(), &directions_to("Kisumu"))
                .await,
        );
        assert!(
            after.contains("Also sent to the speaker's phone."),
            "{after}"
        );
        assert_eq!(notifier.sent().len(), 1);
    }

    #[test]
    fn the_spawned_server_reads_its_deps_when_a_tool_runs() {
        let server = TravelMcpServer::new(None).reading_installed_deps();
        assert!(matches!(server.authority, Dep::Installed(_)));
        assert!(matches!(server.notifier, Dep::Installed(_)));
        assert!(matches!(server.ride_accounts, Dep::Installed(_)));
        let spawn = include_str!("travel.rs")
            .split("pub fn spawn_travel_server")
            .nth(1)
            .expect("spawn_travel_server not found");
        let body = &spawn[..spawn.find("serve_builtin").unwrap_or(spawn.len())];
        assert!(
            body.contains(".reading_installed_deps()"),
            "spawn_travel_server copies its deps at spawn; a server spawned before startup \
             installs them keeps None for the life of the process"
        );
    }

    #[tokio::test]
    async fn without_delivery_wired_the_reply_says_nothing_about_phones() {
        let out = text(
            &TravelMcpServer::new(None)
                .directions_result(&meta(), &directions_to("Kisumu"))
                .await,
        );
        assert!(!out.contains("sent to"), "{out}");
    }
}

#[cfg(test)]
mod booking_tests {
    //! `book_ride` sends an offer to the speaker's own phone and books nothing itself.

    use super::fakes::{nairobi, Places};
    use super::*;
    use async_trait::async_trait;
    use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use pond_core::security::ports::policy::{PolicyDecision, PolicyMode};
    use pond_core::user_data::domain::profile::ProfileScope;
    use pond_core::user_data::domain::session::IdentificationSource;

    /// Resolves every session to its scope, in a household of several unless `.1` names the
    /// only member.
    struct Speaker(Option<ProfileScope>, Option<&'static str>);

    #[async_trait]
    impl DraftAuthority for Speaker {
        async fn policy_mode(&self) -> PolicyMode {
            PolicyMode::Audit
        }
        async fn actor_for_engine_session(
            &self,
            _s: &str,
        ) -> Option<(ProfileScope, IdentificationSource)> {
            self.0.clone().map(|s| (s, IdentificationSource::Explicit))
        }
        async fn sole_member(&self) -> Option<String> {
            self.1.map(str::to_string)
        }
        async fn audit(&self, _s: &str, _a: &str, _d: &PolicyDecision) {}
    }

    struct Connected(&'static [&'static str]);

    #[async_trait]
    impl RideAccounts for Connected {
        async fn is_connected(&self, profile_id: &str) -> anyhow::Result<bool> {
            Ok(self.0.contains(&profile_id))
        }
    }

    fn meta() -> Meta {
        let mut m = Meta::new();
        m.0.insert(
            crate::session_meta::SESSION_ID_META_KEY.to_string(),
            serde_json::json!("engine-1"),
        );
        m
    }

    fn server(
        speaker: Option<ProfileScope>,
        connected: &'static [&'static str],
        notifier: Arc<MockMemberNotifier>,
    ) -> TravelMcpServer {
        server_for(Speaker(speaker, None), connected, notifier)
    }

    fn server_for(
        speaker: Speaker,
        connected: &'static [&'static str],
        notifier: Arc<MockMemberNotifier>,
    ) -> TravelMcpServer {
        TravelMcpServer::new(Some(Arc::new(Places::default())))
            .with_home(Some(nairobi()))
            .with_phone_delivery(Some(Arc::new(speaker)), Some(notifier))
            .with_ride_accounts(Some(Arc::new(Connected(connected))))
    }

    fn to(place: &str) -> BookRideParams {
        BookRideParams {
            destination: Some(place.to_string()),
            ..Default::default()
        }
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn liz() -> Option<ProfileScope> {
        Some(ProfileScope::Owner("liz".into()))
    }

    #[tokio::test]
    async fn the_offer_goes_to_the_speakers_phone_with_the_matched_drop_off() {
        let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
        let result = server(liz(), &["liz"], notifier.clone())
            .book_result(&meta(), &to("JKIA"))
            .await;
        assert_ne!(result.is_error, Some(true), "{}", text(&result));
        assert!(text(&result).contains("Nothing is booked"));

        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "liz");
        assert_eq!(sent[0].1.category, "action_required");
        let data = sent[0].1.data.as_ref().unwrap();
        assert_eq!(data["action"], "ride_offer");
        assert_eq!(data["dropoff"]["latitude"], -1.319167);
    }

    #[tokio::test]
    async fn a_one_member_household_offers_the_ride_to_its_only_member() {
        let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
        let result = server_for(
            Speaker(Some(ProfileScope::Household), Some("liz")),
            &["liz"],
            notifier.clone(),
        )
        .book_result(&meta(), &to("JKIA"))
        .await;
        assert_ne!(result.is_error, Some(true), "{}", text(&result));
        let sent = notifier.sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "liz");
    }

    #[tokio::test]
    async fn nothing_is_sent_when_booking_cannot_happen() {
        let cases: [(Option<ProfileScope>, &'static [&'static str], &str, &str); 4] = [
            (
                Some(ProfileScope::Guest),
                &["liz"],
                "JKIA",
                "not identified",
            ),
            (
                Some(ProfileScope::Household),
                &["liz"],
                "JKIA",
                "not identified",
            ),
            (liz(), &[], "JKIA", "has not connected Uber"),
            (liz(), &["liz"], "my aunt's place", "no place matched"),
        ];
        for (speaker, connected, place, says) in cases {
            let notifier = Arc::new(MockMemberNotifier::new().with_devices("liz", &["liz-phone"]));
            let result = server(speaker.clone(), connected, notifier.clone())
                .book_result(&meta(), &to(place))
                .await;
            assert_eq!(result.is_error, Some(true), "{speaker:?} {place}");
            assert!(text(&result).contains(says), "{}", text(&result));
            assert!(
                notifier.sent().is_empty(),
                "{speaker:?} {place} was sent an offer"
            );
        }
    }

    #[tokio::test]
    async fn a_member_without_a_phone_or_a_pond_without_booking_says_so() {
        let no_phone = Arc::new(MockMemberNotifier::new());
        let result = server(liz(), &["liz"], no_phone)
            .book_result(&meta(), &to("JKIA"))
            .await;
        assert!(
            text(&result).contains("no paired phone"),
            "{}",
            text(&result)
        );

        let unset = TravelMcpServer::new(Some(Arc::new(Places::default())))
            .book_result(&meta(), &to("JKIA"))
            .await;
        assert!(text(&unset).contains("not set up"), "{}", text(&unset));
    }

    #[tokio::test]
    async fn an_offer_that_failed_to_send_says_so() {
        let notifier = Arc::new(
            MockMemberNotifier::new()
                .with_devices("liz", &["liz-phone"])
                .failing_next(1),
        );
        let result = server(liz(), &["liz"], notifier)
            .book_result(&meta(), &to("JKIA"))
            .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text(&result).contains("failed on the pond"),
            "{}",
            text(&result)
        );
    }
}

#[cfg(test)]
mod result_wording_tests {
    //! The model acts on what a result names, so results state facts and never instruct it.

    fn tool_bodies() -> &'static str {
        let src = include_str!("travel.rs");
        let end = src.find("#[cfg(test)]").unwrap_or(src.len());
        &src[..end]
    }

    #[test]
    fn no_travel_result_tells_the_model_what_to_tell_the_user() {
        for phrase in ["Tell the user", "Inform the user", "suggest", "DO NOT"] {
            assert!(
                !tool_bodies().contains(phrase),
                "a travel result instructs the model ({phrase:?}); state the fact instead"
            );
        }
    }
}
