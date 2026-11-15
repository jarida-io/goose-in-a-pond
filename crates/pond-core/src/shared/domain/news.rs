//! A member's news: stories written from pages the pond read itself, and the daily digest made
//! from them. Nothing here holds a search result's own words: Brave allows them only transient
//! storage, so a story keeps the model's writing, the page's address and name, and its picture.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};

/// When the digest is ready unless the member chose another time.
pub const DEFAULT_DIGEST_TIME: &str = "07:00";

/// A member's news choices. A member who never chose has [`NewsSettings::default`]; speech in the
/// house is opt-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewsSettings {
    pub tiles: bool,
    pub digest: bool,
    /// Local `"HH:MM"` in [`NewsSettings::timezone`], else the household's zone.
    pub digest_time: String,
    pub spoken_on_speaker: bool,
    /// The member's own zone; `None` follows the household's.
    pub timezone: Option<String>,
}

impl Default for NewsSettings {
    fn default() -> Self {
        Self {
            tiles: true,
            digest: true,
            digest_time: DEFAULT_DIGEST_TIME.to_string(),
            spoken_on_speaker: false,
            timezone: None,
        }
    }
}

impl NewsSettings {
    /// The digest time, or the default when the stored text is not a time.
    pub fn digest_time_of_day(&self) -> NaiveTime {
        parse_time_of_day(&self.digest_time).unwrap_or_else(|| {
            parse_time_of_day(DEFAULT_DIGEST_TIME).expect("the default digest time parses")
        })
    }

    /// The zone the member's day runs in: their own, else the household's, else UTC.
    pub fn zone(&self, household: &str) -> chrono_tz::Tz {
        self.timezone
            .as_deref()
            .and_then(|z| z.trim().parse().ok())
            .or_else(|| household.trim().parse().ok())
            .unwrap_or(chrono_tz::UTC)
    }
}

/// A local `"HH:MM"`, as the settings store it.
pub fn parse_time_of_day(text: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(text.trim(), "%H:%M").ok()
}

/// Whether `zone` names an IANA time zone.
pub fn is_zone(zone: &str) -> bool {
    zone.trim().parse::<chrono_tz::Tz>().is_ok()
}

/// How an interest feeds the news, as the `interests.news` column spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsUse {
    Off,
    Tiles,
    Digest,
    Both,
}

impl NewsUse {
    /// An unknown spelling feeds nothing rather than guessing.
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "tiles" => Self::Tiles,
            "digest" => Self::Digest,
            "both" => Self::Both,
            _ => Self::Off,
        }
    }

    pub fn tiles(self) -> bool {
        matches!(self, Self::Tiles | Self::Both)
    }

    pub fn digest(self) -> bool {
        matches!(self, Self::Digest | Self::Both)
    }
}

/// One `interests` row as the news reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterestRow {
    pub id: String,
    pub topic: String,
    pub category: String,
    pub facets: Vec<String>,
    /// `curious`, `keen` or `devoted`.
    pub depth: String,
    pub news: NewsUse,
    pub avoid: bool,
    pub evidence: u32,
    pub confirmed: bool,
}

/// The background model's account of a member (`member_portraits`), for tone and angle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Portrait {
    pub summary: String,
    pub into: Vec<String>,
    pub working_on: Vec<String>,
    pub likes: Vec<String>,
}

/// The interest a story or a digest section is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterestRef {
    pub id: String,
    pub topic: String,
}

/// What a story is: the news of the day, or a deeper evergreen piece from several pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoryKind {
    News,
    LongRead,
}

impl StoryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::News => "news",
            Self::LongRead => "long_read",
        }
    }

    pub fn parse(text: &str) -> Self {
        if text == "long_read" {
            Self::LongRead
        } else {
            Self::News
        }
    }
}

/// One story: pages the pond read and summarised in its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Story {
    pub id: String,
    pub profile_id: String,
    pub kind: StoryKind,
    pub interest: InterestRef,
    pub headline: String,
    pub dek: String,
    /// Markdown, in Goose's own words.
    pub summary: String,
    pub why_you: String,
    pub url: String,
    /// The publisher as the page names itself, else its host.
    pub site: String,
    /// The page's own title, as the page gave it.
    pub source_title: String,
    /// The other pages a long read was written from; a news story has none.
    pub further_sources: Vec<Reference>,
    pub picture_key: Option<String>,
    /// The topic-map query that found it, so the member's votes can move that query.
    pub found_by: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub written_at: DateTime<Utc>,
    pub read_at: Option<DateTime<Utc>>,
    pub vote: Option<Vote>,
}

/// What a member said of a story. `Wrong` hides it; up and down move its interest's and its
/// query's topics at the next map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vote {
    Up,
    Down,
    Wrong,
}

impl Vote {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Wrong => "wrong",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "wrong" => Some(Self::Wrong),
            _ => None,
        }
    }
}

/// One entry of a member's topic map (`member_topics`): something to search the web for, scored
/// for how much they care. Newsworthy ones feed the news; the rest feed ideas and long reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Topic {
    pub id: String,
    pub profile_id: String,
    pub area: String,
    pub subject: String,
    pub query: String,
    pub newsworthy: bool,
    /// 0 to 100.
    pub score: f64,
    pub why: String,
    pub built_at: DateTime<Utc>,
    /// When the news or ideas last searched it.
    pub used_at: Option<DateTime<Utc>>,
}

/// A page a digest section cites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub title: String,
    pub url: String,
    pub site: String,
}

/// A picture a digest section shows: a kept report image and the page it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestPicture {
    pub key: String,
    pub caption: String,
    pub source_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestSection {
    pub interest: InterestRef,
    pub heading: String,
    /// Markdown citing [`DigestSection::references`] as `[n](url)`.
    pub body: String,
    pub pictures: Vec<DigestPicture>,
    pub references: Vec<Reference>,
}

/// One day's digest for one member; every one is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Digest {
    pub id: String,
    pub profile_id: String,
    /// The member's local day it was written for.
    pub day: NaiveDate,
    pub title: String,
    pub intro: String,
    pub sections: Vec<DigestSection>,
    pub written_at: DateTime<Utc>,
}

/// A digest as its list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestEntry {
    pub id: String,
    pub title: String,
    pub written_at: DateTime<Utc>,
}

/// What the news keeps per member between passes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewsBook {
    /// A refresh the member asked for that no pass has started yet.
    pub refresh_requested_at: Option<DateTime<Utc>>,
    /// Set while a pass writes for the member; a stale one means the process stopped mid-pass.
    pub writing_since: Option<DateTime<Utc>>,
    /// When the last story pass for the member finished.
    pub stories_written_at: Option<DateTime<Utc>>,
    /// The local day the digest was last read on the house speaker.
    pub spoken_on: Option<NaiveDate>,
    /// When the member's topic map was last built, and the newest change to their interests.
    pub map_built_at: Option<DateTime<Utc>>,
    pub interests_changed_at: Option<DateTime<Utc>>,
}

/// Why a member's news has nothing to show, as `GET /me/news` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsBlocked {
    /// No interest the news may cover.
    NoInterests,
    /// No web search to find stories with: no key, a refused key, or the allowance spent.
    WebSearch,
}

impl NewsBlocked {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoInterests => "no_interests",
            Self::WebSearch => "web_search",
        }
    }
}

impl Story {
    /// The story as a card sent to chat: what the member read, and what Goose works from.
    pub fn as_card(&self) -> crate::shared::domain::context_card::ContextCard {
        let content = format!("{}\n{}", self.dek, self.summary);
        let mut notes = String::new();
        if !self.why_you.is_empty() {
            notes.push_str(&format!("Why you showed it: {}\n", self.why_you));
        }
        notes.push_str(&format!(
            "It is about their interest in {}.\n",
            self.interest.topic
        ));
        notes.push_str(&format!(
            "Source: {} ({}), {}\n",
            self.source_title, self.site, self.url
        ));
        for further in &self.further_sources {
            notes.push_str(&format!("Also from: {} ({})\n", further.title, further.url));
        }
        if let Some(published) = self.published_at {
            notes.push_str(&format!("Published {}.\n", published.format("%-d %B %Y")));
        }
        notes.push_str(&format!(
            "You wrote it on {} from the page; what happened since is not in it.\n",
            self.written_at.format("%-d %B %Y")
        ));
        crate::shared::domain::context_card::ContextCard::new(
            "News".to_string(),
            self.headline.clone(),
            content,
            notes,
        )
    }
}

impl Digest {
    /// The digest as a card sent to chat.
    pub fn as_card(&self) -> crate::shared::domain::context_card::ContextCard {
        let mut content = self.intro.clone();
        let mut notes = format!("Your digest for {}.\n", self.day.format("%-d %B %Y"));
        for section in &self.sections {
            content.push_str(&format!("\n\n{}\n{}", section.heading, section.body));
            for reference in &section.references {
                notes.push_str(&format!(
                    "Source for \"{}\": {} ({})\n",
                    section.heading, reference.title, reference.url
                ));
            }
        }
        crate::shared::domain::context_card::ContextCard::new(
            "News digest".to_string(),
            self.title.clone(),
            content,
            notes,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_write_both_and_keep_the_house_quiet() {
        let s = NewsSettings::default();
        assert!(s.tiles && s.digest && !s.spoken_on_speaker);
        assert_eq!(s.digest_time, "07:00");
        assert_eq!(s.timezone, None);
    }

    #[test]
    fn the_member_zone_wins_and_a_bad_one_falls_back() {
        let mut s = NewsSettings::default();
        assert_eq!(s.zone("Africa/Nairobi"), chrono_tz::Africa::Nairobi);
        s.timezone = Some("Europe/London".into());
        assert_eq!(s.zone("Africa/Nairobi"), chrono_tz::Europe::London);
        s.timezone = Some("Mars/Olympus".into());
        assert_eq!(s.zone("nowhere"), chrono_tz::UTC);
    }

    #[test]
    fn a_broken_digest_time_reads_as_the_default() {
        let s = NewsSettings {
            digest_time: "7am".into(),
            ..Default::default()
        };
        assert_eq!(
            s.digest_time_of_day(),
            NaiveTime::from_hms_opt(7, 0, 0).unwrap()
        );
    }

    #[test]
    fn news_use_reads_the_column_and_nothing_else() {
        assert!(NewsUse::parse("both").tiles() && NewsUse::parse("both").digest());
        assert!(NewsUse::parse("tiles").tiles() && !NewsUse::parse("tiles").digest());
        assert!(!NewsUse::parse("digest").tiles() && NewsUse::parse("digest").digest());
        assert_eq!(NewsUse::parse("sometimes"), NewsUse::Off);
    }
}
