//! What a member is into: the rows the interview, chat and the member themselves write, and the
//! portrait the background model keeps of them. The news and ideas read these instead of every
//! memory.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The categories an interest may carry; anything else is filed under `other`.
pub const CATEGORIES: &[&str] = &[
    "sport", "tech", "science", "business", "money", "politics", "culture", "film_tv", "music",
    "games", "books", "food", "travel", "health", "home", "cars", "shopping", "people", "places",
    "learning", "other",
];

/// Facets kept on one interest; past it the row is a list of everything, not a focus.
pub const MAX_FACETS: usize = 8;
const MAX_TOPIC_CHARS: usize = 60;
const MAX_FACET_CHARS: usize = 40;

/// Evidence at which a guess is offered to the member to confirm.
pub const OFFERED_AT: u32 = 2;
/// Evidence at which an unconfirmed guess feeds the news and ideas.
pub const STRONG_AT: u32 = 3;

/// A category as stored: a known one, else `other`.
pub fn category_of(raw: &str) -> String {
    let cleaned = raw.trim().to_lowercase().replace([' ', '-', '/'], "_");
    let cleaned = match cleaned.as_str() {
        "film" | "tv" | "film_and_tv" | "film_&_tv" | "movies" => "film_tv".to_string(),
        "game" | "gaming" => "games".to_string(),
        "book" | "reading" => "books".to_string(),
        "car" | "motoring" => "cars".to_string(),
        "sports" => "sport".to_string(),
        "technology" => "tech".to_string(),
        other => other.to_string(),
    };
    if CATEGORIES.contains(&cleaned.as_str()) {
        cleaned
    } else {
        "other".to_string()
    }
}

/// The key two spellings of one topic share: lower case, words only.
pub fn topic_key(topic: &str) -> String {
    topic
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A topic as stored: trimmed, one line, bounded. `None` when nothing is left.
pub fn clean_topic(raw: &str) -> Option<String> {
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = one_line.trim_matches(|c: char| c == '"' || c == '.' || c.is_whitespace());
    if topic_key(trimmed).is_empty() {
        return None;
    }
    Some(trimmed.chars().take(MAX_TOPIC_CHARS).collect())
}

/// Facets as stored: cleaned, deduplicated by key, bounded.
pub fn clean_facets<'a>(raw: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter_map(|f| {
            let one_line = f.split_whitespace().collect::<Vec<_>>().join(" ");
            let key = topic_key(&one_line);
            (!key.is_empty() && seen.insert(key))
                .then(|| one_line.chars().take(MAX_FACET_CHARS).collect())
        })
        .take(MAX_FACETS)
        .collect()
}

/// `existing` with every new facet of `more` added after it, still bounded.
pub fn merge_facets(existing: &[String], more: &[String]) -> Vec<String> {
    clean_facets(existing.iter().chain(more.iter()).map(String::as_str))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Depth {
    #[default]
    Curious,
    Keen,
    Devoted,
}

impl Depth {
    pub fn as_str(self) -> &'static str {
        match self {
            Depth::Curious => "curious",
            Depth::Keen => "keen",
            Depth::Devoted => "devoted",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "curious" => Some(Depth::Curious),
            "keen" => Some(Depth::Keen),
            "devoted" => Some(Depth::Devoted),
            _ => None,
        }
    }
}

/// Whether and how an interest feeds the news.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum NewsMode {
    Off,
    Tiles,
    Digest,
    #[default]
    Both,
}

impl NewsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            NewsMode::Off => "off",
            NewsMode::Tiles => "tiles",
            NewsMode::Digest => "digest",
            NewsMode::Both => "both",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "off" | "none" | "no" => Some(NewsMode::Off),
            "tiles" => Some(NewsMode::Tiles),
            "digest" => Some(NewsMode::Digest),
            "both" | "yes" => Some(NewsMode::Both),
            _ => None,
        }
    }
}

/// Who wrote the row: the interview, a guess from chat, or the member by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    Interview,
    Chat,
    You,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Interview => "interview",
            Origin::Chat => "chat",
            Origin::You => "you",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "interview" => Origin::Interview,
            "chat" => Origin::Chat,
            _ => Origin::You,
        }
    }
}

/// One thing a member is into, as the phone reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interest {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub topic: String,
    pub category: String,
    pub facets: Vec<String>,
    pub depth: Depth,
    pub news: NewsMode,
    pub avoid: bool,
    pub origin: Origin,
    pub evidence: u32,
    pub confirmed: bool,
    /// The topic's own picture as a Pond path, found in the background; `None` until then.
    #[serde(default)]
    pub picture: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// The path the phone fetches a kept report image from.
pub fn picture_path(key: &str) -> String {
    format!("/api/v1/report-images/{key}")
}

impl Interest {
    /// Whether the news and ideas may build on it: confirmed, or a guess resting on several
    /// conversations, and never one the member asked to keep away from.
    pub fn feeds_research(&self) -> bool {
        !self.avoid && (self.confirmed || self.evidence >= STRONG_AT)
    }
}

/// A new or confirmed interest, as the member or the interview states it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct InterestInput {
    pub topic: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub facets: Option<Vec<String>>,
    #[serde(default)]
    pub depth: Option<Depth>,
    #[serde(default)]
    pub news: Option<NewsMode>,
    #[serde(default)]
    pub avoid: Option<bool>,
}

/// A change to one interest; only the fields present change.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct InterestPatch {
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub facets: Option<Vec<String>>,
    #[serde(default)]
    pub depth: Option<Depth>,
    #[serde(default)]
    pub news: Option<NewsMode>,
    #[serde(default)]
    pub avoid: Option<bool>,
    #[serde(default)]
    pub confirmed: Option<bool>,
}

/// How a conversation showed the member caring about something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// They asked about it.
    Asked,
    /// They said they like it, follow it or want it.
    Said,
    /// They do it, own it or are working on it.
    Does,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Asked => "asked",
            Signal::Said => "said",
            Signal::Does => "does",
        }
    }

    /// A missing or unknown signal reads as `Asked`, the weakest.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "said" | "says" | "likes" => Signal::Said,
            "does" | "do" | "doing" | "working" => Signal::Does,
            _ => Signal::Asked,
        }
    }
}

/// An interest one conversation showed, before it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedInterest {
    pub topic: String,
    pub category: String,
    pub facets: Vec<String>,
    pub signal: Signal,
}

/// What writing one observation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Observed {
    /// A new guess, resting on this conversation.
    Created,
    /// This conversation added one to an existing row's evidence.
    Counted,
    /// This conversation was already counted for the row.
    AlreadyCounted,
    /// The member asked to keep it away; nothing changed.
    Avoided,
}

/// The background model's short account of a member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberPortrait {
    pub summary: String,
    pub into: Vec<String>,
    pub working_on: Vec<String>,
    pub likes: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

impl MemberPortrait {
    /// The portrait as a prompt reads it, a few lines.
    pub fn as_prompt_lines(&self) -> String {
        let mut out = self.summary.trim().to_string();
        for (label, items) in [
            ("Into", &self.into),
            ("Working on", &self.working_on),
            ("Likes", &self.likes),
        ] {
            if !items.is_empty() {
                out.push_str(&format!("\n{label}: {}", items.join("; ")));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_spellings_of_one_topic_share_a_key() {
        assert_eq!(topic_key("The Boys"), topic_key("the boys!"));
        assert_eq!(topic_key("  GTA-V "), "gta v");
        assert!(topic_key("...").is_empty());
    }

    #[test]
    fn an_unknown_category_is_other_and_near_misses_are_forgiven() {
        assert_eq!(category_of("Film/TV"), "film_tv");
        assert_eq!(category_of("gaming"), "games");
        assert_eq!(category_of("sports"), "sport");
        assert_eq!(category_of("astrology"), "other");
    }

    #[test]
    fn facets_are_deduplicated_and_bounded() {
        let facets = clean_facets(["Ferrari", "ferrari", " race-day  strategy ", ""]);
        assert_eq!(facets, vec!["Ferrari", "race-day strategy"]);
        let many: Vec<String> = (0..20).map(|i| format!("facet {i}")).collect();
        assert_eq!(
            clean_facets(many.iter().map(String::as_str)).len(),
            MAX_FACETS
        );
    }

    #[test]
    fn only_a_confirmed_or_strong_guess_that_is_not_avoided_feeds_research() {
        let mut interest = Interest {
            id: "i".into(),
            profile_id: "p".into(),
            topic: "Formula 1".into(),
            category: "sport".into(),
            facets: vec![],
            depth: Depth::Keen,
            news: NewsMode::Both,
            avoid: false,
            origin: Origin::Chat,
            evidence: 1,
            confirmed: false,
            picture: None,
            updated_at: Utc::now(),
        };
        assert!(!interest.feeds_research(), "one conversation is curiosity");
        interest.evidence = STRONG_AT;
        assert!(interest.feeds_research());
        interest.avoid = true;
        assert!(!interest.feeds_research());
    }

    #[test]
    fn an_unknown_signal_is_the_weakest() {
        assert_eq!(Signal::parse(""), Signal::Asked);
        assert_eq!(Signal::parse("Does"), Signal::Does);
    }
}
