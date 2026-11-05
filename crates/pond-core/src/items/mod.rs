//! The context space's item header (context-space.md section 4.1): one row per piece of
//! knowledge in `items`, whatever detail table holds the fields only its kind has.

pub mod from_detail;
pub mod lifecycle;
pub mod timestamp;

use std::fmt;

use uuid::Uuid;

/// What an item is. Each kind keeps its own fields in [`Kind::detail_table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Message,
    Conversation,
    Fact,
    Entity,
    Event,
    Media,
    Notification,
    Mail,
    HomeEvent,
    Visit,
    Report,
    Suggestion,
    Trait,
}

impl Kind {
    /// Every kind; `items.kind`'s CHECK lists exactly these (a pond-infra test compares them).
    pub const ALL: [Kind; 13] = [
        Kind::Message,
        Kind::Conversation,
        Kind::Fact,
        Kind::Entity,
        Kind::Event,
        Kind::Media,
        Kind::Notification,
        Kind::Mail,
        Kind::HomeEvent,
        Kind::Visit,
        Kind::Report,
        Kind::Suggestion,
        Kind::Trait,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Message => "message",
            Kind::Conversation => "conversation",
            Kind::Fact => "fact",
            Kind::Entity => "entity",
            Kind::Event => "event",
            Kind::Media => "media",
            Kind::Notification => "notification",
            Kind::Mail => "mail",
            Kind::HomeEvent => "home_event",
            Kind::Visit => "visit",
            Kind::Report => "report",
            Kind::Suggestion => "suggestion",
            Kind::Trait => "trait",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == raw)
    }

    /// The table holding this kind's own fields, keyed by its `item_id` column.
    pub fn detail_table(self) -> &'static str {
        match self {
            Kind::Message => "session_messages",
            Kind::Conversation => "sessions",
            Kind::Fact | Kind::Entity | Kind::Trait => "memory_fragments",
            // Cameras and sensors write here today; P6 gives home events device-state details.
            Kind::Mail | Kind::Notification | Kind::Event | Kind::HomeEvent => "context_items",
            Kind::Media => "media_details",
            Kind::Visit => "visit_details",
            Kind::Suggestion => "suggestion_queue",
            Kind::Report => "long_task_artifacts",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Who may read an item besides its owner. Named members are rows in `item_shares`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Audience {
    Private,
    Household,
}

impl Audience {
    pub const ALL: [Audience; 2] = [Audience::Private, Audience::Household];

    pub fn as_str(self) -> &'static str {
        match self {
            Audience::Private => "private",
            Audience::Household => "household",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.as_str() == raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
}

impl Modality {
    pub const ALL: [Modality; 4] = [
        Modality::Text,
        Modality::Image,
        Modality::Audio,
        Modality::Video,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Modality::Text => "text",
            Modality::Image => "image",
            Modality::Audio => "audio",
            Modality::Video => "video",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == raw)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    Active,
    /// Kept and listed, but out of recall: what memory decay does to an unused fact.
    Archived,
    Superseded,
    Dismissed,
    Accepted,
}

impl State {
    pub const ALL: [State; 5] = [
        State::Active,
        State::Archived,
        State::Superseded,
        State::Dismissed,
        State::Accepted,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Active => "active",
            State::Archived => "archived",
            State::Superseded => "superseded",
            State::Dismissed => "dismissed",
            State::Accepted => "accepted",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.as_str() == raw)
    }
}

/// How two items relate; a link reads `from <relation> to`, e.g. a conversation is
/// `evidence_for` the fact it taught, and a suggestion is `derived_from` its memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Relation {
    DerivedFrom,
    EvidenceFor,
    Mentions,
    SameAs,
    Supersedes,
}

impl Relation {
    pub const ALL: [Relation; 5] = [
        Relation::DerivedFrom,
        Relation::EvidenceFor,
        Relation::Mentions,
        Relation::SameAs,
        Relation::Supersedes,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Relation::DerivedFrom => "derived_from",
            Relation::EvidenceFor => "evidence_for",
            Relation::Mentions => "mentions",
            Relation::SameAs => "same_as",
            Relation::Supersedes => "supersedes",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.as_str() == raw)
    }
}

/// What wrote an item, so a bad agent version's output can be found and withdrawn.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProducedBy {
    Ingest,
    /// One of the pond's own writers that is not a recipe, e.g. `extraction` or `chat`.
    Pond(String),
    /// A recipe, by name and the hash of its definition.
    Recipe {
        name: String,
        hash: String,
    },
}

impl ProducedBy {
    pub fn pond(writer: &str) -> Self {
        ProducedBy::Pond(writer.trim().to_string())
    }

    pub fn to_stored(&self) -> String {
        match self {
            ProducedBy::Ingest => "ingest".to_string(),
            ProducedBy::Pond(writer) => format!("pond:{writer}"),
            ProducedBy::Recipe { name, hash } => format!("recipe:{name}@{hash}"),
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        if raw == "ingest" {
            return Some(ProducedBy::Ingest);
        }
        if let Some(writer) = raw.strip_prefix("pond:") {
            return (!writer.is_empty()).then(|| ProducedBy::Pond(writer.to_string()));
        }
        let (name, hash) = raw.strip_prefix("recipe:")?.rsplit_once('@')?;
        (!name.is_empty() && !hash.is_empty()).then(|| ProducedBy::Recipe {
            name: name.to_string(),
            hash: hash.to_string(),
        })
    }
}

/// An item's id: a UUIDv7, so ids sort by when the header was written and an agent's cursor
/// is one high-water mark. Generated when the header is written, never from the item's own
/// date, so a header written late for an old row still sorts after every existing cursor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(String);

impl ItemId {
    pub fn generate() -> Self {
        ItemId(Uuid::now_v7().hyphenated().to_string())
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let uuid = Uuid::try_parse(raw).ok()?;
        (uuid.get_version_num() == 7).then(|| ItemId(uuid.hyphenated().to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// `source_id` of items whose details live in one of the pond's own tables, paired with that
/// row's key as `external_id`. The `pond:` prefix cannot collide with a context source's id.
pub mod source {
    pub const MEMORY: &str = "pond:memory";
    pub const SESSION: &str = "pond:session";
    pub const SUGGESTION: &str = "pond:suggestion";
}

/// Who owns an item and who may read it. The `items` CHECK holds the same rule, so a private
/// item with no owner cannot be built here or stored there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    owner: Option<String>,
    audience: Audience,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    #[error("a private item needs an owner")]
    PrivateWithoutOwner,
}

impl Scope {
    pub fn private(owner: &str) -> Result<Self, ScopeError> {
        let owner = owner.trim();
        if owner.is_empty() {
            return Err(ScopeError::PrivateWithoutOwner);
        }
        Ok(Scope {
            owner: Some(owner.to_string()),
            audience: Audience::Private,
        })
    }

    pub fn household(owner: Option<&str>) -> Self {
        Scope {
            owner: owner
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(str::to_string),
            audience: Audience::Household,
        }
    }

    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    pub fn audience(&self) -> Audience {
        self.audience
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_enum_round_trips_through_its_stored_form() {
        for k in Kind::ALL {
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
        for a in Audience::ALL {
            assert_eq!(Audience::parse(a.as_str()), Some(a));
        }
        for m in Modality::ALL {
            assert_eq!(Modality::parse(m.as_str()), Some(m));
        }
        for s in State::ALL {
            assert_eq!(State::parse(s.as_str()), Some(s));
        }
        for r in Relation::ALL {
            assert_eq!(Relation::parse(r.as_str()), Some(r));
        }
        assert_eq!(Kind::parse("memory"), None);
    }

    #[test]
    fn kind_all_names_every_variant() {
        // Adding a variant without listing it in ALL fails to compile here.
        fn position(k: Kind) -> usize {
            match k {
                Kind::Message => 0,
                Kind::Conversation => 1,
                Kind::Fact => 2,
                Kind::Entity => 3,
                Kind::Event => 4,
                Kind::Media => 5,
                Kind::Notification => 6,
                Kind::Mail => 7,
                Kind::HomeEvent => 8,
                Kind::Visit => 9,
                Kind::Report => 10,
                Kind::Suggestion => 11,
                Kind::Trait => 12,
            }
        }
        for (i, k) in Kind::ALL.into_iter().enumerate() {
            assert_eq!(position(k), i, "{k} is out of place in Kind::ALL");
        }
    }

    #[test]
    fn produced_by_round_trips_and_refuses_what_it_cannot_read() {
        for p in [
            ProducedBy::Ingest,
            ProducedBy::pond("extraction"),
            ProducedBy::Recipe {
                name: "conversation".into(),
                hash: "ab12".into(),
            },
        ] {
            assert_eq!(ProducedBy::parse(&p.to_stored()), Some(p));
        }
        for bad in ["", "pond:", "recipe:x", "recipe:@h", "recipe:n@", "agent"] {
            assert_eq!(ProducedBy::parse(bad), None, "{bad:?} parsed");
        }
    }

    #[test]
    fn item_ids_are_v7_and_sort_in_the_order_they_were_made() {
        let ids: Vec<ItemId> = (0..64).map(|_| ItemId::generate()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        assert!(ItemId::parse(ids[0].as_str()).is_some());
        assert!(
            ItemId::parse(&Uuid::new_v4().to_string()).is_none(),
            "a v4 id is not an item id"
        );
        assert!(ItemId::parse("pond:memory").is_none());
    }

    #[test]
    fn a_private_scope_needs_an_owner_and_a_household_one_does_not() {
        assert_eq!(Scope::private("  "), Err(ScopeError::PrivateWithoutOwner));
        let mine = Scope::private("amina").unwrap();
        assert_eq!(
            (mine.owner(), mine.audience()),
            (Some("amina"), Audience::Private)
        );
        let shared = Scope::household(Some(" "));
        assert_eq!(
            (shared.owner(), shared.audience()),
            (None, Audience::Household)
        );
    }
}
