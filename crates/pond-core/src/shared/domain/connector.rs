//! A connector: a service the Pond can connect to, described by a manifest. The manifest names
//! the connector, says how the phone draws its setup (fields, or a browser sign-in), which events
//! it raises for to-do hooks and which parts of My Goose it can feed, and how the Pond connects
//! it. The phone draws any manifest it is sent, so a new connector needs no app release.
//! `docs/CONNECTORS.md` is the guide for writing one.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// The built-in manifests, from the repository's `connectors/` directory.
const BUILT_IN: &[(&str, &str)] = &[
    (
        "mail.json",
        include_str!("../../../../../connectors/mail.json"),
    ),
    (
        "calendar.json",
        include_str!("../../../../../connectors/calendar.json"),
    ),
    (
        "web_search.json",
        include_str!("../../../../../connectors/web_search.json"),
    ),
    (
        "weather.json",
        include_str!("../../../../../connectors/weather.json"),
    ),
    (
        "home.json",
        include_str!("../../../../../connectors/home.json"),
    ),
    (
        "sensors.json",
        include_str!("../../../../../connectors/sensors.json"),
    ),
    (
        "cameras.json",
        include_str!("../../../../../connectors/cameras.json"),
    ),
    (
        "memory.json",
        include_str!("../../../../../connectors/memory.json"),
    ),
    (
        "reminders.json",
        include_str!("../../../../../connectors/reminders.json"),
    ),
    (
        "directions.json",
        include_str!("../../../../../connectors/directions.json"),
    ),
    (
        "personal_context.json",
        include_str!("../../../../../connectors/personal_context.json"),
    ),
    (
        "agents.json",
        include_str!("../../../../../connectors/agents.json"),
    ),
    (
        "uber.json",
        include_str!("../../../../../connectors/uber.json"),
    ),
    (
        "spotify.json",
        include_str!("../../../../../connectors/spotify.json"),
    ),
    (
        "webhook.json",
        include_str!("../../../../../connectors/webhook.json"),
    ),
];

pub const CATEGORIES: &[&str] = &[
    "mail", "calendar", "home", "web", "travel", "weather", "music", "money", "custom",
];

/// The events each source raises, as `hook_events` names them.
pub const EVENT_VOCABULARY: &[(&str, &[&str])] = &[
    ("context_source:mail", &["new_mail"]),
    (
        "context_source:calendar",
        &["event_added", "event_starting"],
    ),
    ("webhook", &["call"]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    BuiltIn,
    Account,
    Mcp,
    Webhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorStatus {
    Connected,
    Available,
    NeedsSetup,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    Text,
    Secret,
    Url,
    Toggle,
    Select,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
}

/// One thing the phone asks for before connecting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupField {
    pub key: String,
    pub label: String,
    pub kind: FieldKind,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<FieldOption>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Setup {
    #[serde(default)]
    pub fields: Vec<SetupField>,
    /// A sign-in address to open in a browser, when connecting needs one.
    #[serde(default)]
    pub oauth: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorEvent {
    pub id: String,
    pub label: String,
}

/// How the Pond connects what a manifest describes: through the service behind an existing
/// route, never a second copy of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum ConnectVia {
    /// A household switch in the settings, with any others it needs on beside it.
    Setting {
        setting: String,
        #[serde(default)]
        also: Vec<String>,
        /// Whether it needs the Pond's location to be set.
        #[serde(default)]
        needs_location: bool,
    },
    /// A member's mail or calendar account, kept as a context source.
    ContextSource { source_kind: String },
    /// The built-in web search's Brave key.
    WebSearchKey,
    /// The Matter controller, with the switch that offers its tools.
    Matter { setting: String },
    /// A member's own Uber sign-in, made on the Pond itself.
    Uber,
    /// A browser sign-in with one of the Pond's OAuth providers.
    Oauth { provider: String },
    /// A webhook address any service can call.
    Webhook,
    /// An MCP server from the extension marketplace, by its id there.
    Marketplace { extension: String },
}

/// A connector as its manifest describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorManifest {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub icon: String,
    pub category: String,
    pub kind: ConnectorKind,
    pub connect: ConnectVia,
    #[serde(default)]
    pub setup: Setup,
    #[serde(default)]
    pub events: Vec<ConnectorEvent>,
    #[serde(default)]
    pub feeds: Vec<String>,
    /// Tools it brings, when no running extension can be counted.
    #[serde(default)]
    pub tools: usize,
    /// The running extension whose tools it brings, for the count.
    #[serde(default)]
    pub extension: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provides {
    pub tools: usize,
    pub events: Vec<ConnectorEvent>,
    pub feeds: Vec<String>,
}

/// A connector as the phone shows it, with its state on this Pond.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connector {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub icon: String,
    pub category: String,
    pub status: ConnectorStatus,
    pub status_detail: Option<String>,
    pub kind: ConnectorKind,
    pub provides: Provides,
    pub setup: Setup,
}

impl Connector {
    /// The connector a manifest describes, in `status`.
    pub fn of(manifest: &ConnectorManifest, status: ConnectorStatus) -> Self {
        Self {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            summary: manifest.summary.clone(),
            icon: manifest.icon.clone(),
            category: manifest.category.clone(),
            status,
            status_detail: None,
            kind: manifest.kind,
            provides: Provides {
                tools: manifest.tools,
                events: manifest.events.clone(),
                feeds: manifest.feeds.clone(),
            },
            setup: manifest.setup.clone(),
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.status_detail = Some(detail.into());
        self
    }
}

/// Every built-in manifest. They are checked by test, so a broken one never ships.
pub fn built_in_manifests() -> Vec<ConnectorManifest> {
    BUILT_IN
        .iter()
        .filter_map(|(file, text)| match serde_json::from_str(text) {
            Ok(manifest) => Some(manifest),
            Err(e) => {
                tracing::warn!(file, error = %e, "a built-in connector manifest does not parse");
                None
            }
        })
        .collect()
}

/// What is wrong with `manifest`, if anything: each problem in a sentence.
pub fn problems(manifest: &ConnectorManifest) -> Vec<String> {
    let mut out = Vec::new();
    let id_ok = !manifest.id.is_empty()
        && manifest
            .id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if !id_ok {
        out.push(format!(
            "id {:?} must be lower-case letters, digits, - and _",
            manifest.id
        ));
    }
    if manifest.name.trim().is_empty() || manifest.summary.trim().is_empty() {
        out.push("name and summary must not be empty".into());
    }
    if !CATEGORIES.contains(&manifest.category.as_str()) {
        out.push(format!(
            "category {:?} is not one of {CATEGORIES:?}",
            manifest.category
        ));
    }
    let expected_kind = match &manifest.connect {
        ConnectVia::ContextSource { .. } | ConnectVia::Uber | ConnectVia::Oauth { .. } => {
            ConnectorKind::Account
        }
        ConnectVia::Webhook => ConnectorKind::Webhook,
        ConnectVia::Marketplace { .. } => ConnectorKind::Mcp,
        ConnectVia::Setting { .. } | ConnectVia::WebSearchKey | ConnectVia::Matter { .. } => {
            ConnectorKind::BuiltIn
        }
    };
    if manifest.kind != expected_kind {
        out.push(format!(
            "kind {:?} does not match connect via {:?}",
            manifest.kind, manifest.connect
        ));
    }
    let raised = raised_by(&manifest.connect);
    for event in &manifest.events {
        if !raised.contains(&event.id.as_str()) {
            out.push(format!(
                "event {:?} is not one this connector raises ({raised:?})",
                event.id
            ));
        }
    }
    for feed in &manifest.feeds {
        if !["news", "todos", "ideas"].contains(&feed.as_str()) {
            out.push(format!("feed {feed:?} is not news, todos or ideas"));
        }
    }
    let mut keys = HashSet::new();
    for field in &manifest.setup.fields {
        if !keys.insert(field.key.as_str()) {
            out.push(format!("field {:?} appears twice", field.key));
        }
        if (field.kind == FieldKind::Select) == field.options.is_empty() {
            out.push(format!(
                "field {:?}: a select has options, and nothing else does",
                field.key
            ));
        }
    }
    out
}

/// The events a connection of this sort raises in `hook_events`.
pub fn raised_by(via: &ConnectVia) -> Vec<&'static str> {
    let source = match via {
        ConnectVia::ContextSource { source_kind } => format!("context_source:{source_kind}"),
        ConnectVia::Webhook => "webhook".to_string(),
        _ => return Vec::new(),
    };
    EVENT_VOCABULARY
        .iter()
        .find(|(s, _)| *s == source)
        .map(|(_, events)| events.to_vec())
        .unwrap_or_default()
}

/// A marketplace category as a connector's.
pub fn category_of_marketplace(category: &str) -> &'static str {
    match category {
        "entertainment" | "music" => "music",
        "finance" | "money" => "money",
        "automation" | "browser" | "search" => "web",
        "home" | "smart_home" => "home",
        _ => "custom",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_manifest_parses_and_is_sound() {
        assert_eq!(built_in_manifests().len(), BUILT_IN.len());
        let mut ids = HashSet::new();
        for manifest in built_in_manifests() {
            assert!(ids.insert(manifest.id.clone()), "{} twice", manifest.id);
            assert_eq!(problems(&manifest), Vec::<String>::new(), "{}", manifest.id);
        }
    }

    #[test]
    fn every_setting_a_manifest_names_is_a_settings_field() {
        let settings =
            serde_json::to_value(crate::user_data::domain::settings::Settings::default()).unwrap();
        for manifest in built_in_manifests() {
            let names: Vec<String> = match &manifest.connect {
                ConnectVia::Setting { setting, also, .. } => std::iter::once(setting.clone())
                    .chain(also.clone())
                    .collect(),
                ConnectVia::Matter { setting } => vec![setting.clone()],
                _ => continue,
            };
            for name in names {
                assert!(
                    settings.get(&name).is_some_and(|v| v.is_boolean()),
                    "{}: {name} is not a switch in Settings",
                    manifest.id
                );
            }
        }
    }

    #[test]
    fn a_bad_manifest_says_what_is_wrong() {
        let manifest: ConnectorManifest = serde_json::from_value(serde_json::json!({
            "id": "GitHub!", "name": "GitHub", "summary": "s", "icon": "git",
            "category": "code", "kind": "account", "connect": {"via": "webhook"},
            "events": [{"id": "push", "label": "A push"}],
            "setup": {"fields": [{"key": "k", "label": "K", "kind": "select"}]}
        }))
        .unwrap();
        let problems = problems(&manifest);
        assert_eq!(problems.len(), 5, "{problems:#?}");
    }
}
