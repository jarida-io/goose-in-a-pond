//! The member's connectors: every manifest, every marketplace extension and every installed one,
//! each with its state read from what the Pond already keeps. A connection that raises events
//! (a mailbox, a calendar, a webhook) is listed on its own, under the id its `hook_events` carry,
//! so a to-do hook can name it.

use std::collections::HashSet;

use chrono::{DateTime, Utc};

use crate::mcp::domain::marketplace::{BuiltInCapability, MarketplaceExtension};
use crate::mcp::domain::web_search::BuiltInSearchState;
use crate::mcp::ports::extension_manager::ExtensionInfo;
use crate::security::domain::secret::SecretKind;
use crate::shared::domain::connector::{
    category_of_marketplace, ConnectVia, Connector, ConnectorKind, ConnectorManifest,
    ConnectorStatus, FieldKind, FieldOption, Provides, Setup, SetupField,
};
use crate::user_data::domain::settings::Settings;
use crate::user_data::ports::matter_runtime::{MatterState, MatterStatus};

/// A member's mail or calendar account, as its context source records it.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountSource {
    pub id: String,
    /// `mail` or `calendar`.
    pub kind: String,
    pub provider: String,
    /// `connected`, `needs_reauth`, `error` or `paused`.
    pub status: String,
    pub last_sync: Option<DateTime<Utc>>,
}

/// One of the member's webhook connectors.
#[derive(Debug, Clone, PartialEq)]
pub struct WebhookView {
    pub id: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_event_at: Option<DateTime<Utc>>,
}

/// Whether the member has an Uber sign-in on this Pond.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UberState {
    Connected,
    NotConnected,
    /// The credentials service that starts a sign-in is not set up.
    SignInOff,
    Unreadable,
}

/// Everything the states are read from, gathered once per request.
#[derive(Debug, Clone)]
pub struct ConnectorWorld {
    pub settings: Settings,
    pub secret_store: bool,
    pub web_search: BuiltInSearchState,
    /// The calling member's accounts only.
    pub accounts: Vec<AccountSource>,
    /// `None` when the Pond runs no extension manager.
    pub extensions: Option<Vec<ExtensionInfo>>,
    pub marketplace: Vec<MarketplaceExtension>,
    /// OAuth providers with a stored token.
    pub oauth_signed_in: HashSet<String>,
    pub uber: UberState,
    /// `None` on a Pond with no Matter runtime.
    pub matter: Option<MatterStatus>,
    pub webhooks: Vec<WebhookView>,
    pub now: DateTime<Utc>,
}

impl ConnectorWorld {
    fn switch(&self, name: &str) -> bool {
        serde_json::to_value(&self.settings)
            .ok()
            .and_then(|s| s.get(name).and_then(|v| v.as_bool()))
            .unwrap_or(false)
    }

    fn has_location(&self) -> bool {
        let s = &self.settings;
        !s.weather_location_name.trim().is_empty()
            || s.weather_latitude.abs() > f64::EPSILON
            || s.weather_longitude.abs() > f64::EPSILON
    }

    fn extension(&self, name: &str) -> Option<&ExtensionInfo> {
        self.extensions.as_ref()?.iter().find(|e| e.name == name)
    }
}

/// The member's connectors, in manifest order, then the marketplace's, then any other extension.
pub fn connectors(manifests: &[ConnectorManifest], world: &ConnectorWorld) -> Vec<Connector> {
    let mut out = Vec::new();
    let mut covered: HashSet<String> = HashSet::new();
    for manifest in manifests {
        if let ConnectVia::Marketplace { extension } = &manifest.connect {
            covered.insert(extension.clone());
        }
        out.extend(of_manifest(manifest, world));
    }
    for ext in &world.marketplace {
        // The built-in search is the web search connector.
        if covered.contains(&ext.id) || ext.built_in == Some(BuiltInCapability::WebSearch) {
            continue;
        }
        covered.insert(ext.id.clone());
        out.push(installed_or_not(
            &marketplace_manifest(ext),
            world.extension(&ext.id),
        ));
    }
    for ext in world.extensions.iter().flatten() {
        if ext.kind == "builtin" || ext.name.starts_with("giap-") || covered.contains(&ext.name) {
            continue;
        }
        out.push(installed_or_not(&extension_manifest(ext), Some(ext)));
    }
    out
}

/// The connector named `id`, as [`connectors`] lists it.
pub fn find(
    manifests: &[ConnectorManifest],
    world: &ConnectorWorld,
    id: &str,
) -> Option<Connector> {
    connectors(manifests, world)
        .into_iter()
        .find(|c| c.id == id)
}

fn of_manifest(manifest: &ConnectorManifest, world: &ConnectorWorld) -> Vec<Connector> {
    use ConnectorStatus::*;
    let mut connector = Connector::of(manifest, Available);
    if let Some(ext) = manifest
        .extension
        .as_deref()
        .and_then(|e| world.extension(e))
    {
        connector.provides.tools = ext.tools.len();
    }
    match &manifest.connect {
        ConnectVia::Setting {
            setting,
            also,
            needs_location,
        } => {
            let on = world.switch(setting) && also.iter().all(|s| world.switch(s));
            connector.status = if on { Connected } else { Available };
            if on && *needs_location && !world.has_location() {
                connector = connector.detail(
                    "Goose needs to know where your home is. Set it in Settings, under Location.",
                );
                connector.status = NeedsSetup;
            }
            vec![connector]
        }
        ConnectVia::ContextSource { source_kind } => {
            let mut out = vec![if world.secret_store {
                connector
            } else {
                let mut c = connector.detail(
                    "This Pond has no locked store for passwords, so it cannot keep an account's.",
                );
                c.status = Error;
                c
            }];
            for account in world.accounts.iter().filter(|a| &a.kind == source_kind) {
                out.push(account_connector(manifest, account, world.now));
            }
            out
        }
        ConnectVia::WebSearchKey => {
            match world.web_search {
                BuiltInSearchState::On => connector.status = Connected,
                BuiltInSearchState::NeedsKey => {}
                BuiltInSearchState::KnowledgeOff => {
                    connector = connector
                        .detail("Looking things up is switched off on this Pond, in Settings.");
                }
            }
            vec![connector]
        }
        ConnectVia::Matter { setting } => {
            let (status, detail) = if !world.switch(setting) {
                (Available, None)
            } else {
                match world.matter.as_ref().map(|m| &m.state) {
                    None | Some(MatterState::Disabled) => (
                        Error,
                        Some("This Pond was built without home device support.".to_string()),
                    ),
                    Some(MatterState::Connecting) => (
                        Connected,
                        Some("The home controller is starting.".to_string()),
                    ),
                    Some(MatterState::Connected) => (Connected, None),
                    Some(MatterState::Unreachable { error }) => (
                        Error,
                        Some(format!("Goose cannot reach the home controller: {error}")),
                    ),
                }
            };
            connector.status = status;
            connector.status_detail = detail;
            vec![connector]
        }
        ConnectVia::Uber => {
            let (status, detail) = match world.uber {
                UberState::Connected if !world.settings.ext_travel_enabled => (
                    Connected,
                    Some("Rides are switched off. Turn on Directions and rides.".to_string()),
                ),
                UberState::Connected => (Connected, None),
                UberState::NotConnected => (
                    NeedsSetup,
                    Some(
                        "Connect Uber on the Pond itself, in Settings, under Accounts.".to_string(),
                    ),
                ),
                UberState::SignInOff => (
                    Error,
                    Some("Uber sign-in is not set up on this Pond.".to_string()),
                ),
                UberState::Unreadable => (
                    Error,
                    Some("Goose could not read your Uber sign-in.".to_string()),
                ),
            };
            connector.status = status;
            connector.status_detail = detail;
            vec![connector]
        }
        ConnectVia::Oauth { provider } => {
            if world.oauth_signed_in.contains(provider) {
                connector.status = Connected;
            }
            vec![connector]
        }
        ConnectVia::Webhook => {
            let mut out = vec![connector];
            for hook in &world.webhooks {
                out.push(webhook_connector(manifest, hook, world.now));
            }
            out
        }
        ConnectVia::Marketplace { extension } => {
            if !world.marketplace.iter().any(|e| &e.id == extension) {
                return Vec::new();
            }
            vec![installed_or_not(manifest, world.extension(extension))]
        }
    }
}

fn account_connector(
    manifest: &ConnectorManifest,
    account: &AccountSource,
    now: DateTime<Utc>,
) -> Connector {
    let provider = manifest
        .setup
        .fields
        .iter()
        .flat_map(|f| f.options.iter())
        .find(|o| o.value == account.provider)
        .map(|o| o.label.clone())
        .unwrap_or_else(|| account.provider.clone());
    let mut connector = Connector::of(manifest, ConnectorStatus::Connected);
    connector.id = format!("{}:{}", account.kind, account.id);
    connector.name = format!("{} ({provider})", manifest.name);
    connector.setup = Setup::default();
    let (status, detail) = match account.status.as_str() {
        "connected" => (
            ConnectorStatus::Connected,
            Some(match account.last_sync {
                Some(at) => format!("Checked {}.", ago(now, at)),
                None => "Not checked yet.".to_string(),
            }),
        ),
        "paused" => (
            ConnectorStatus::Connected,
            Some("Paused while the Pond is offline.".to_string()),
        ),
        "needs_reauth" => (
            ConnectorStatus::Error,
            Some("The account refused its app password. Connect it again with a new one.".into()),
        ),
        _ => (
            ConnectorStatus::Error,
            Some("The last check of this account failed.".to_string()),
        ),
    };
    connector.status = status;
    connector.status_detail = detail;
    connector
}

fn webhook_connector(
    manifest: &ConnectorManifest,
    hook: &WebhookView,
    now: DateTime<Utc>,
) -> Connector {
    let mut connector = Connector::of(manifest, ConnectorStatus::Connected);
    connector.id = format!("webhook:{}", hook.id);
    connector.name = hook.name.clone();
    connector.setup = Setup::default();
    connector.detail(match hook.last_event_at {
        Some(at) => format!("Last called {}.", ago(now, at)),
        None => "Not called yet.".to_string(),
    })
}

/// A connector for a marketplace extension with no manifest of its own.
fn marketplace_manifest(ext: &MarketplaceExtension) -> ConnectorManifest {
    ConnectorManifest {
        id: format!("mcp:{}", ext.id),
        name: ext.name.clone(),
        summary: ext.description.clone(),
        icon: "puzzle".to_string(),
        category: category_of_marketplace(&ext.category).to_string(),
        kind: ConnectorKind::Mcp,
        connect: ConnectVia::Marketplace {
            extension: ext.id.clone(),
        },
        setup: Setup {
            fields: ext
                .required_secrets
                .iter()
                .filter(|s| s.kind != SecretKind::OAuthFlow && !s.advanced)
                .map(|s| SetupField {
                    key: s.key.clone(),
                    label: s.display_name.clone(),
                    kind: match s.kind {
                        SecretKind::Choice => FieldKind::Select,
                        SecretKind::Generic => FieldKind::Text,
                        _ => FieldKind::Secret,
                    },
                    options: s
                        .options
                        .iter()
                        .map(|o| FieldOption {
                            value: o.value.clone(),
                            label: o.label.clone(),
                        })
                        .collect(),
                    required: s.blocks_install(),
                    help: (!s.description.is_empty()).then(|| s.description.clone()),
                })
                .collect(),
            oauth: None,
        },
        events: Vec::new(),
        feeds: Vec::new(),
        tools: ext.tools.len(),
        extension: Some(ext.id.clone()),
    }
}

/// A connector for an MCP server added by hand, which no catalogue describes.
fn extension_manifest(ext: &ExtensionInfo) -> ConnectorManifest {
    ConnectorManifest {
        id: format!("mcp:{}", ext.name),
        name: ext.name.clone(),
        summary: if ext.description.trim().is_empty() {
            "An MCP server added to this Pond.".to_string()
        } else {
            ext.description.clone()
        },
        icon: "puzzle".to_string(),
        category: "custom".to_string(),
        kind: ConnectorKind::Mcp,
        connect: ConnectVia::Marketplace {
            extension: ext.name.clone(),
        },
        setup: Setup::default(),
        events: Vec::new(),
        feeds: Vec::new(),
        tools: ext.tools.len(),
        extension: Some(ext.name.clone()),
    }
}

fn installed_or_not(manifest: &ConnectorManifest, installed: Option<&ExtensionInfo>) -> Connector {
    let mut connector = Connector::of(manifest, ConnectorStatus::Available);
    let Some(ext) = installed else {
        return connector;
    };
    if !ext.tools.is_empty() {
        connector.provides = Provides {
            tools: ext.tools.len(),
            ..connector.provides
        };
    }
    // Installed, so nothing more to fill in; a switched-off one comes back on with a tap.
    connector.setup.fields.clear();
    let (status, detail) = match ext.status.as_str() {
        "connected" => (ConnectorStatus::Connected, None),
        "no_tools" => (ConnectorStatus::Connected, ext.status_reason.clone()),
        "disabled" => (
            ConnectorStatus::Available,
            Some("Switched off.".to_string()),
        ),
        _ => (
            ConnectorStatus::Error,
            Some(
                ext.last_error
                    .clone()
                    .unwrap_or_else(|| "It failed to start.".to_string()),
            ),
        ),
    };
    connector.status = status;
    connector.status_detail = detail;
    connector
}

/// How long ago `at` was, in words: "just now", "5 minutes ago", "3 days ago".
pub fn ago(now: DateTime<Utc>, at: DateTime<Utc>) -> String {
    let secs = (now - at).num_seconds().max(0);
    let (n, unit) = match secs {
        0..=59 => return "just now".to_string(),
        60..=3_599 => (secs / 60, "minute"),
        3_600..=86_399 => (secs / 3_600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::connector::built_in_manifests;
    use chrono::Duration;

    fn world() -> ConnectorWorld {
        ConnectorWorld {
            settings: Settings::default(),
            secret_store: true,
            web_search: BuiltInSearchState::NeedsKey,
            accounts: Vec::new(),
            extensions: Some(Vec::new()),
            marketplace: Vec::new(),
            oauth_signed_in: HashSet::new(),
            uber: UberState::NotConnected,
            matter: None,
            webhooks: Vec::new(),
            now: Utc::now(),
        }
    }

    fn status_of(list: &[Connector], id: &str) -> (ConnectorStatus, Option<String>) {
        let c = list
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no connector {id}"));
        (c.status, c.status_detail.clone())
    }

    fn extension(name: &str, status: &str, tools: usize) -> ExtensionInfo {
        ExtensionInfo {
            name: name.into(),
            kind: if name.starts_with("giap-") {
                "builtin"
            } else {
                "stdio"
            }
            .into(),
            description: String::new(),
            tools: (0..tools).map(|i| format!("t{i}")).collect(),
            enabled: status != "disabled",
            status: status.into(),
            last_error: (status == "error").then(|| "npx is not installed".to_string()),
            status_reason: None,
        }
    }

    fn marketplace() -> Vec<MarketplaceExtension> {
        serde_json::from_value(serde_json::json!([
            {"id": "github", "name": "GitHub", "description": "Repositories and issues.",
             "kind": "stdio", "category": "development", "author": "x", "tools": ["a", "b"],
             "required_secrets": [{"key": "GITHUB_PERSONAL_ACCESS_TOKEN", "display_name":
             "GitHub token", "description": "A personal access token", "required": true,
             "kind": "api_key"}]},
            {"id": "brave-search", "name": "Brave Search", "description": "Search.",
             "kind": "stdio", "category": "productivity", "author": "x",
             "built_in": "web_search"},
            {"id": "slack", "name": "Slack", "description": "Chat.", "kind": "stdio",
             "category": "communication", "author": "x", "tools": ["c"]}
        ]))
        .unwrap()
    }

    #[test]
    fn a_fresh_pond_lists_every_manifest_in_its_default_state() {
        let list = connectors(&built_in_manifests(), &world());
        assert_eq!(status_of(&list, "mail").0, ConnectorStatus::Available);
        assert_eq!(status_of(&list, "memory").0, ConnectorStatus::Connected);
        assert_eq!(status_of(&list, "directions").0, ConnectorStatus::Available);
        assert_eq!(status_of(&list, "web_search").0, ConnectorStatus::Available);
        assert_eq!(status_of(&list, "uber").0, ConnectorStatus::NeedsSetup);
        assert_eq!(status_of(&list, "spotify").0, ConnectorStatus::Available);
        assert_eq!(status_of(&list, "webhook").0, ConnectorStatus::Available);
        assert_eq!(
            status_of(&list, "home").0,
            ConnectorStatus::Error,
            "a switch on, and no Matter runtime"
        );
    }

    #[test]
    fn accounts_and_webhooks_are_listed_under_their_event_ids() {
        let mut w = world();
        w.accounts = vec![
            AccountSource {
                id: "mail:gmail:jerry".into(),
                kind: "mail".into(),
                provider: "gmail".into(),
                status: "connected".into(),
                last_sync: Some(w.now - Duration::minutes(5)),
            },
            AccountSource {
                id: "calendar:icloud:jerry".into(),
                kind: "calendar".into(),
                provider: "icloud".into(),
                status: "needs_reauth".into(),
                last_sync: None,
            },
        ];
        w.webhooks = vec![WebhookView {
            id: "w1".into(),
            name: "GitHub".into(),
            created_at: w.now,
            last_event_at: None,
        }];
        let list = connectors(&built_in_manifests(), &w);
        let mail = list
            .iter()
            .find(|c| c.id == "mail:mail:gmail:jerry")
            .unwrap();
        assert_eq!(mail.name, "Email (Gmail)");
        assert_eq!(
            mail.status_detail.as_deref(),
            Some("Checked 5 minutes ago.")
        );
        assert_eq!(mail.provides.events[0].id, "new_mail");
        assert!(mail.setup.fields.is_empty());
        assert_eq!(
            status_of(&list, "calendar:calendar:icloud:jerry").0,
            ConnectorStatus::Error
        );
        let hook = list.iter().find(|c| c.id == "webhook:w1").unwrap();
        assert_eq!(hook.name, "GitHub");
        assert_eq!(hook.status_detail.as_deref(), Some("Not called yet."));
    }

    #[test]
    fn switches_keys_sign_ins_and_the_home_controller_are_read() {
        let mut w = world();
        w.settings.ext_weather_enabled = true;
        w.settings.weather_enabled = true;
        w.settings.weather_location_name = String::new();
        w.settings.weather_latitude = 0.0;
        w.settings.weather_longitude = 0.0;
        w.web_search = BuiltInSearchState::On;
        w.oauth_signed_in.insert("spotify".into());
        w.uber = UberState::Connected;
        w.matter = Some(MatterStatus {
            enabled: true,
            url: "ws://127.0.0.1:5580/ws".into(),
            state: MatterState::Unreachable {
                error: "connection refused".into(),
            },
        });
        w.extensions = Some(vec![extension("giap-weather", "connected", 2)]);
        let list = connectors(&built_in_manifests(), &w);
        assert_eq!(status_of(&list, "weather").0, ConnectorStatus::NeedsSetup);
        assert_eq!(
            list.iter()
                .find(|c| c.id == "weather")
                .unwrap()
                .provides
                .tools,
            2
        );
        assert_eq!(status_of(&list, "web_search").0, ConnectorStatus::Connected);
        assert_eq!(status_of(&list, "spotify").0, ConnectorStatus::Connected);
        let uber = status_of(&list, "uber");
        assert_eq!(uber.0, ConnectorStatus::Connected);
        assert!(uber.1.unwrap().contains("Rides are switched off"));
        let home = status_of(&list, "home");
        assert_eq!(home.0, ConnectorStatus::Error);
        assert!(home.1.unwrap().contains("connection refused"));
    }

    #[test]
    fn marketplace_and_installed_extensions_are_connectors() {
        let mut w = world();
        w.marketplace = marketplace();
        w.extensions = Some(vec![
            extension("slack", "error", 0),
            extension("giap-memory", "connected", 3),
            extension("my-notes", "disabled", 0),
        ]);
        let list = connectors(&built_in_manifests(), &w);
        assert!(!list.iter().any(|c| c.id == "mcp:brave-search"));
        let github = list.iter().find(|c| c.id == "mcp:github").unwrap();
        assert_eq!(github.status, ConnectorStatus::Available);
        assert_eq!(github.kind, ConnectorKind::Mcp);
        assert_eq!(github.provides.tools, 2);
        assert_eq!(github.setup.fields[0].key, "GITHUB_PERSONAL_ACCESS_TOKEN");
        assert_eq!(github.setup.fields[0].kind, FieldKind::Secret);
        assert!(github.setup.fields[0].required);
        let slack = status_of(&list, "mcp:slack");
        assert_eq!(slack.0, ConnectorStatus::Error);
        assert_eq!(slack.1.as_deref(), Some("npx is not installed"));
        assert_eq!(
            status_of(&list, "mcp:my-notes").0,
            ConnectorStatus::Available
        );
        assert!(!list.iter().any(|c| c.id == "mcp:giap-memory"));
    }

    #[test]
    fn time_reads_in_words() {
        let now = Utc::now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now, now - Duration::minutes(1)), "1 minute ago");
        assert_eq!(ago(now, now - Duration::hours(3)), "3 hours ago");
        assert_eq!(ago(now, now - Duration::days(2)), "2 days ago");
    }
}
