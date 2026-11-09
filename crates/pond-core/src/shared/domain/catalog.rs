//! "More your Goose can do": what Goose can do on this Pond, each in plain words, with whether it
//! is on, off, or waiting on something the member can set up. Read from the settings and the
//! connectors; nothing here calls a model.

use serde::{Deserialize, Serialize};

use crate::shared::domain::connector::{Connector, ConnectorStatus};
use crate::user_data::domain::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogState {
    On,
    Off,
    NeedsSetup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    pub title: String,
    pub body: String,
    pub icon: String,
    pub state: CatalogState,
    #[serde(rename = "try")]
    pub try_prompt: Option<String>,
    pub opens: Option<String>,
}

/// Which parts of My Goose this Pond serves, as `/me/hub` reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Served {
    pub interests: bool,
    pub news: bool,
    pub todos: bool,
    pub cart: bool,
    pub ideas: bool,
}

/// What the states are read from.
pub struct CatalogInputs<'a> {
    pub settings: &'a Settings,
    pub connectors: &'a [Connector],
    pub served: Served,
    /// The built-in web search has its key and is on.
    pub web_search: bool,
    /// Voice notes and audio files can be heard: a speech model is ready.
    pub voice_notes: bool,
}

impl CatalogInputs<'_> {
    fn connector(&self, id: &str) -> Option<ConnectorStatus> {
        self.connectors
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.status)
    }

    /// Whether any account made from `template` is connected.
    fn account_connected(&self, template: &str) -> bool {
        let prefix = format!("{template}:");
        self.connectors
            .iter()
            .any(|c| c.id.starts_with(&prefix) && c.status == ConnectorStatus::Connected)
    }
}

use CatalogState::{NeedsSetup, Off, On};

fn on_if(on: bool) -> CatalogState {
    if on {
        On
    } else {
        Off
    }
}

/// On when it is switched on and its web search has a key; waiting on the key otherwise.
fn needs_search(on: bool, web_search: bool) -> CatalogState {
    match (on, web_search) {
        (false, _) => Off,
        (true, true) => On,
        (true, false) => NeedsSetup,
    }
}

fn item(
    id: &str,
    title: &str,
    body: &str,
    icon: &str,
    state: CatalogState,
    try_prompt: Option<&str>,
    opens: Option<&str>,
) -> CatalogItem {
    CatalogItem {
        id: id.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        icon: icon.to_string(),
        state,
        try_prompt: try_prompt.map(str::to_string),
        opens: opens.map(str::to_string),
    }
}

/// Everything Goose can do on this Pond, in the order the phone lists it.
pub fn catalog(inputs: &CatalogInputs<'_>) -> Vec<CatalogItem> {
    let s = inputs.settings;
    let search = inputs.web_search;
    let tasks = s.ext_orchestrator_enabled && s.long_tasks_enabled;
    let connector_state = |id: &str| match inputs.connector(id) {
        Some(ConnectorStatus::Connected) => On,
        Some(ConnectorStatus::Available) | None => Off,
        Some(ConnectorStatus::NeedsSetup | ConnectorStatus::Error) => NeedsSetup,
    };
    let accounts = if inputs.account_connected("mail") || inputs.account_connected("calendar") {
        On
    } else {
        NeedsSetup
    };
    let rides = match (s.ext_travel_enabled, inputs.connector("uber")) {
        (false, _) => Off,
        (true, Some(ConnectorStatus::Connected)) => On,
        (true, _) => NeedsSetup,
    };
    let music = match inputs.connector("spotify") {
        Some(ConnectorStatus::Connected) => On,
        _ => NeedsSetup,
    };
    let model_chosen = !s.chat_model.trim().is_empty() && !s.chat_provider.trim().is_empty();
    vec![
        item(
            "news",
            "News about what you follow",
            "Goose picks stories on what you care about and writes you a short digest each \
             morning, read aloud if you like.",
            "newspaper",
            needs_search(inputs.served.news, search),
            Some("What's new on the things I follow?"),
            Some("news"),
        ),
        item(
            "todos",
            "To-dos and reminders",
            "Tell Goose what you need to do, and it reminds you at a time, when you get \
             somewhere, or when something happens.",
            "list-checks",
            on_if(inputs.served.todos),
            Some("Remind me to call Emmanuel when I get home."),
            Some("todos"),
        ),
        item(
            "cart",
            "Price watching",
            "Paste a link to something you want, and Goose tells you when it gets cheaper or \
             comes back in stock.",
            "tag",
            on_if(inputs.served.cart),
            Some("Tell me when this gets cheaper: "),
            Some("cart"),
        ),
        item(
            "web_watches",
            "Watching a page of listings",
            "Goose checks a page of listings, such as cars for sale, and tells you when \
             something you want appears.",
            "eye",
            on_if(s.ext_knowledge_enabled),
            Some("Watch this page for a Toyota Fielder under 1.2 million: "),
            Some("web_watches"),
        ),
        item(
            "ideas",
            "Ideas for you",
            "Each day Goose looks into something you care about and suggests something you \
             could do about it.",
            "lightbulb",
            needs_search(inputs.served.ideas, search),
            None,
            Some("ideas"),
        ),
        item(
            "interests",
            "Telling Goose what you're into",
            "A short chat, a tap at a time, so Goose knows what to look out for and what to \
             leave alone.",
            "heart",
            on_if(inputs.served.interests),
            None,
            Some("interests"),
        ),
        item(
            "research",
            "Deep research",
            "Ask a big question, and Goose reads around it while the house is quiet, then \
             writes up what it found.",
            "microscope",
            needs_search(tasks, search),
            Some("Look into which used cars under 1.2 million hold their value best in Kenya."),
            Some("agents"),
        ),
        item(
            "agents",
            "Agents that work on their own",
            "Agents such as a morning brief run on their own schedule and report to you.",
            "bot",
            on_if(tasks && s.agent_activations_enabled),
            Some("Give me a morning brief every weekday at 7."),
            Some("agents"),
        ),
        item(
            "cameras",
            "Cameras that watch for you",
            "Goose watches your cameras for people, pets and parcels, and can act on what it \
             sees.",
            "camera",
            connector_state("cameras"),
            Some("Tell me when someone is at the front door."),
            Some("cameras"),
        ),
        item(
            "voice",
            "Talking to Goose in the room",
            "Say \"hey Goose\" and talk, with no phone in your hand.",
            "mic",
            on_if(s.voice_room_enabled && s.mic_enabled),
            None,
            Some("voice"),
        ),
        item(
            "home",
            "Home devices",
            "Goose switches your lights, plugs and other smart devices when you ask.",
            "house",
            connector_state("home"),
            Some("Turn off the living room lights."),
            Some("connectors"),
        ),
        item(
            "mail_calendar",
            "Mail and calendar",
            "Connect your inbox and calendar, and Goose can tell you what came in and what is \
             coming up.",
            "mail",
            accounts,
            Some("What's on my calendar tomorrow?"),
            Some("connectors"),
        ),
        item(
            "rides",
            "Directions and rides",
            "Goose finds the way to a place and books you a ride there.",
            "car",
            rides,
            Some("Get me a ride to the airport."),
            Some("connectors"),
        ),
        item(
            "music",
            "Music",
            "Goose plays, pauses and picks music on your Spotify.",
            "music",
            music,
            Some("Play some Amapiano."),
            Some("connectors"),
        ),
        item(
            "memory",
            "Remembering you",
            "Goose remembers what you tell it about yourself and brings it up when it helps.",
            "brain",
            on_if(s.ext_memory_enabled),
            Some("Remember that I'm lactose intolerant."),
            Some("settings"),
        ),
        item(
            "attachments",
            "Voice notes, audio and PDFs",
            "Send a voice note, an audio file or a PDF in chat, and Goose listens to it or reads \
             it.",
            "paperclip",
            if inputs.voice_notes { On } else { NeedsSetup },
            Some("Summarise this PDF for me."),
            None,
        ),
        item(
            "models",
            "Choosing Goose's models",
            "Pick the model Goose thinks with, and the voices it hears and speaks with.",
            "cpu",
            if model_chosen { On } else { NeedsSetup },
            None,
            Some("models"),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::connector::{built_in_manifests, Connector};

    fn connector(id: &str, status: ConnectorStatus) -> Connector {
        let manifest = built_in_manifests()
            .into_iter()
            .find(|m| id.starts_with(&m.id))
            .unwrap();
        let mut c = Connector::of(&manifest, status);
        c.id = id.to_string();
        c
    }

    fn state_of(items: &[CatalogItem], id: &str) -> CatalogState {
        items.iter().find(|i| i.id == id).unwrap().state
    }

    #[test]
    fn states_follow_the_settings_and_connectors() {
        let settings = Settings {
            ext_travel_enabled: true,
            ext_orchestrator_enabled: true,
            long_tasks_enabled: true,
            chat_provider: "ollama".into(),
            chat_model: "gemma4:latest".into(),
            ..Default::default()
        };
        let connectors = vec![
            connector("mail:mail:gmail:jerry", ConnectorStatus::Connected),
            connector("uber", ConnectorStatus::NeedsSetup),
            connector("spotify", ConnectorStatus::Connected),
            connector("cameras", ConnectorStatus::Available),
            connector("home", ConnectorStatus::Error),
        ];
        let items = catalog(&CatalogInputs {
            settings: &settings,
            connectors: &connectors,
            served: Served {
                cart: true,
                news: true,
                ..Default::default()
            },
            web_search: false,
            voice_notes: true,
        });
        assert_eq!(state_of(&items, "cart"), CatalogState::On);
        assert_eq!(state_of(&items, "todos"), CatalogState::Off);
        assert_eq!(
            state_of(&items, "news"),
            CatalogState::NeedsSetup,
            "no search key"
        );
        assert_eq!(state_of(&items, "research"), CatalogState::NeedsSetup);
        assert_eq!(state_of(&items, "mail_calendar"), CatalogState::On);
        assert_eq!(state_of(&items, "rides"), CatalogState::NeedsSetup);
        assert_eq!(state_of(&items, "music"), CatalogState::On);
        assert_eq!(state_of(&items, "cameras"), CatalogState::Off);
        assert_eq!(state_of(&items, "home"), CatalogState::NeedsSetup);
        assert_eq!(state_of(&items, "models"), CatalogState::On);
        assert_eq!(state_of(&items, "attachments"), CatalogState::On);
    }

    #[test]
    fn every_item_faces_the_member() {
        let settings = Settings::default();
        let items = catalog(&CatalogInputs {
            settings: &settings,
            connectors: &[],
            served: Served::default(),
            web_search: true,
            voice_notes: false,
        });
        assert_eq!(items.len(), 17);
        let opens = [
            "news",
            "todos",
            "cart",
            "connectors",
            "ideas",
            "interests",
            "agents",
            "web_watches",
            "cameras",
            "voice",
            "models",
            "settings",
        ];
        for item in &items {
            assert!(
                item.opens.as_deref().is_none_or(|o| opens.contains(&o)),
                "{}",
                item.id
            );
            assert!(item.body.ends_with('.'), "{}", item.id);
            for system_word in ["MCP", "Brave", "Ollama", "Matter", "LLM", "API", "ext_"] {
                assert!(
                    !item.title.contains(system_word) && !item.body.contains(system_word),
                    "{} names {system_word}",
                    item.id
                );
            }
        }
        let json = serde_json::to_value(&items[0]).unwrap();
        assert!(json.get("try").is_some());
        assert_eq!(json["state"], "off");
    }
}
