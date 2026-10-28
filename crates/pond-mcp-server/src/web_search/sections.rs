//! What Brave's web search adds beside its pages when it judges a question wants them: news,
//! videos, and places with their hours, address, phone and rating. Brave decides; nothing here
//! reads the question.

use pond_core::mcp::domain::web_search::is_brave_thumbnail;
use pond_core::shared::services::passages;
use serde::Deserialize;
use serde_json::Value;

use super::{defang, domain_of, plain_text, sanitize, without_urls, MAX_TITLE_BYTES};

/// News items read from one answer, at most: the copy quotes them beside the pages.
pub(super) const MAX_NEWS: usize = 3;

/// Videos read from one answer, at most: they go to the card alone.
pub(super) const MAX_VIDEOS: usize = 3;

/// Places read from one answer, at most, as LLM Context's places are.
const MAX_LOCATIONS: usize = 5;

/// A publisher, duration, creator, address or phone, at most.
const MAX_FIELD_BYTES: usize = 120;

/// A news description, at most, before the ranker cuts it into windows.
const MAX_DESCRIPTION_BYTES: usize = 600;

/// Everything Brave added beside the web results.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Sections {
    pub news: Vec<News>,
    pub videos: Vec<Video>,
    pub places: Vec<PlaceDetails>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct News {
    pub title: String,
    pub url: String,
    pub description: String,
    pub publisher: String,
    /// `YYYY-MM-DD` when Brave dates the page, else its relative age ("2 days ago").
    pub date: Option<String>,
    /// Only ever a Brave-proxied thumbnail.
    pub thumbnail: Option<String>,
    pub breaking: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct Video {
    pub title: String,
    pub url: String,
    pub duration: Option<String>,
    pub creator: Option<String>,
    pub date: Option<String>,
    /// Only ever a Brave-proxied thumbnail.
    pub thumbnail: Option<String>,
}

/// A place Brave's web search knows, with what a household asks of one.
#[derive(Debug, Clone, Default, PartialEq)]
pub(super) struct PlaceDetails {
    pub name: String,
    pub url: String,
    pub address: Option<String>,
    /// The day Brave reports and its hours: "Monday 07:00 to 22:00".
    pub hours: Option<String>,
    pub phone: Option<String>,
    /// "4.5 of 5 from 120 reviews".
    pub rating: Option<String>,
}

impl PlaceDetails {
    /// Each detail as its own short passage, so the ranker gives a place's line the one that
    /// answers the question.
    pub fn snippets(&self) -> Vec<String> {
        [
            self.hours.as_ref().map(|h| format!("open {h}")),
            self.address.as_ref().map(|a| format!("address: {a}")),
            self.phone.as_ref().map(|p| format!("phone: {p}")),
            self.rating.as_ref().map(|r| format!("rating: {r}")),
        ]
        .into_iter()
        .flatten()
        .collect()
    }
}

/// The three sections of a web search answer, each read leniently: an item Brave shaped
/// differently is skipped, never the answer.
pub(super) fn read_sections(
    news: Option<&Value>,
    videos: Option<&Value>,
    locations: Option<&Value>,
) -> Sections {
    Sections {
        news: items(news).filter_map(read_news).take(MAX_NEWS).collect(),
        videos: items(videos).filter_map(read_video).take(MAX_VIDEOS).collect(),
        places: items(locations)
            .filter_map(read_location)
            .take(MAX_LOCATIONS)
            .collect(),
    }
}

fn items(section: Option<&Value>) -> impl Iterator<Item = &Value> {
    section
        .and_then(|s| s.get("results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

#[derive(Debug, Default, Deserialize)]
struct RawNews {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    age: Option<String>,
    #[serde(default)]
    page_age: Option<String>,
    #[serde(default)]
    profile: Option<RawName>,
    #[serde(default)]
    meta_url: Option<RawMetaUrl>,
    #[serde(default)]
    thumbnail: Option<RawThumbnail>,
    #[serde(default)]
    breaking: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVideo {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    age: Option<String>,
    #[serde(default)]
    page_age: Option<String>,
    #[serde(default)]
    video: Option<RawVideoMeta>,
    #[serde(default)]
    thumbnail: Option<RawThumbnail>,
}

#[derive(Debug, Default, Deserialize)]
struct RawVideoMeta {
    #[serde(default)]
    duration: Option<String>,
    #[serde(default)]
    creator: Option<String>,
    #[serde(default)]
    publisher: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawLocation {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    provider_url: Option<String>,
    #[serde(default)]
    postal_address: Option<RawAddress>,
    #[serde(default)]
    opening_hours: Option<RawOpeningHours>,
    #[serde(default)]
    contact: Option<RawContact>,
    #[serde(default)]
    rating: Option<RawRating>,
}

#[derive(Debug, Default, Deserialize)]
struct RawName {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawMetaUrl {
    #[serde(default)]
    hostname: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawThumbnail {
    #[serde(default)]
    src: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawAddress {
    #[serde(default, rename = "displayAddress")]
    display_address: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawOpeningHours {
    #[serde(default)]
    current_day: Option<Vec<RawDayHours>>,
}

#[derive(Debug, Default, Deserialize)]
struct RawDayHours {
    #[serde(default)]
    full_name: Option<String>,
    #[serde(default)]
    opens: Option<String>,
    #[serde(default)]
    closes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawContact {
    #[serde(default)]
    telephone: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct RawRating {
    #[serde(default, rename = "ratingValue")]
    rating_value: Option<f64>,
    #[serde(default, rename = "bestRating")]
    best_rating: Option<f64>,
    #[serde(default, rename = "reviewCount")]
    review_count: Option<u64>,
}

fn read_news(value: &Value) -> Option<News> {
    let raw: RawNews = serde_json::from_value(value.clone()).ok()?;
    let url = web_url(raw.url.as_deref())?;
    let title = text_field(raw.title.as_deref(), MAX_TITLE_BYTES)?;
    let publisher = [
        raw.profile.and_then(|p| p.name),
        raw.meta_url.and_then(|m| m.hostname),
    ]
    .into_iter()
    .flatten()
    .find_map(|p| text_field(Some(p.as_str()), MAX_FIELD_BYTES))
    .unwrap_or_else(|| domain_of(&url));
    Some(News {
        title,
        description: text_field(raw.description.as_deref(), MAX_DESCRIPTION_BYTES)
            .unwrap_or_default(),
        publisher,
        date: date_of(raw.page_age.as_deref(), raw.age.as_deref()),
        thumbnail: thumbnail(raw.thumbnail),
        breaking: raw.breaking.unwrap_or(false),
        url,
    })
}

fn read_video(value: &Value) -> Option<Video> {
    let raw: RawVideo = serde_json::from_value(value.clone()).ok()?;
    let url = web_url(raw.url.as_deref())?;
    let title = text_field(raw.title.as_deref(), MAX_TITLE_BYTES)?;
    let meta = raw.video.unwrap_or_default();
    Some(Video {
        title,
        duration: text_field(meta.duration.as_deref(), MAX_FIELD_BYTES),
        creator: [meta.creator, meta.publisher]
            .into_iter()
            .flatten()
            .find_map(|c| text_field(Some(c.as_str()), MAX_FIELD_BYTES)),
        date: date_of(raw.page_age.as_deref(), raw.age.as_deref()),
        thumbnail: thumbnail(raw.thumbnail),
        url,
    })
}

fn read_location(value: &Value) -> Option<PlaceDetails> {
    let raw: RawLocation = serde_json::from_value(value.clone()).ok()?;
    let name = text_field(raw.title.as_deref(), MAX_TITLE_BYTES)?;
    let url = [raw.url.as_deref(), raw.provider_url.as_deref()]
        .into_iter()
        .find_map(web_url)
        .unwrap_or_default();
    let place = PlaceDetails {
        name,
        url,
        address: raw
            .postal_address
            .and_then(|a| text_field(a.display_address.as_deref(), MAX_FIELD_BYTES)),
        hours: raw.opening_hours.and_then(|h| hours_of(&h)),
        phone: raw
            .contact
            .and_then(|c| text_field(c.telephone.as_deref(), MAX_FIELD_BYTES)),
        rating: raw.rating.and_then(|r| rating_of(&r)),
    };
    Some(place)
}

/// The day Brave reports and each of its opening spans: "Monday 07:00 to 12:00 and 14:00 to 22:00".
fn hours_of(hours: &RawOpeningHours) -> Option<String> {
    let spans = hours.current_day.as_deref().unwrap_or_default();
    let day = spans
        .iter()
        .find_map(|s| text_field(s.full_name.as_deref(), 20))?;
    let times: Vec<String> = spans
        .iter()
        .filter_map(|s| {
            let opens = text_field(s.opens.as_deref(), 20)?;
            let closes = text_field(s.closes.as_deref(), 20)?;
            Some(format!("{opens} to {closes}"))
        })
        .collect();
    if times.is_empty() {
        return None;
    }
    Some(passages::clip(
        &format!("{day} {}", times.join(" and ")),
        MAX_FIELD_BYTES,
    ))
}

fn rating_of(rating: &RawRating) -> Option<String> {
    let value = rating.rating_value.filter(|v| v.is_finite() && *v > 0.0)?;
    let best = rating
        .best_rating
        .filter(|b| b.is_finite() && *b >= value)
        .unwrap_or(5.0);
    let mut text = format!("{} of {}", number(value), number(best));
    if let Some(count) = rating.review_count.filter(|c| *c > 0) {
        let noun = if count == 1 { "review" } else { "reviews" };
        text.push_str(&format!(" from {count} {noun}"));
    }
    Some(text)
}

/// `4.0` as `4`, `4.25` as `4.3`.
fn number(n: f64) -> String {
    let rounded = (n * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

/// `YYYY-MM-DD` from an ISO timestamp, else the relative age Brave gives.
fn date_of(page_age: Option<&str>, age: Option<&str>) -> Option<String> {
    let dated = page_age.map(str::trim).and_then(|p| {
        let day = p.get(..10)?;
        let b = day.as_bytes();
        let shaped = b.len() == 10
            && b[4] == b'-'
            && b[7] == b'-'
            && b.iter()
                .enumerate()
                .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
        shaped.then(|| day.to_string())
    });
    dated.or_else(|| text_field(age, 40))
}

fn thumbnail(raw: Option<RawThumbnail>) -> Option<String> {
    raw.and_then(|t| t.src)
        .map(|s| s.trim().to_string())
        .filter(|s| is_brave_thumbnail(s))
}

/// A link a card may open: `http(s)` only.
fn web_url(raw: Option<&str>) -> Option<String> {
    let url = raw?.trim();
    let parsed = reqwest::Url::parse(url).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| url.to_string())
}

/// Plain text with no URL and no card marker, within `max_bytes`; `None` when empty.
fn text_field(raw: Option<&str>, max_bytes: usize) -> Option<String> {
    let text = defang(&without_urls(&plain_text(raw?)));
    let text = passages::clip(&text, max_bytes);
    (!text.is_empty()).then_some(text)
}

/// The card's `news` rows.
pub(super) fn news_card(news: &[News]) -> Value {
    news.iter()
        .map(|n| {
            serde_json::json!({
                "title": sanitize(&n.title),
                "url": sanitize(&n.url),
                "domain": sanitize(&domain_of(&n.url)),
                "publisher": sanitize(&n.publisher),
                "date": n.date.as_deref().map(sanitize),
                "thumbnail": n.thumbnail.as_deref().map(sanitize),
                "breaking": n.breaking,
            })
        })
        .collect()
}

/// The card's `videos` rows.
pub(super) fn videos_card(videos: &[Video]) -> Value {
    videos
        .iter()
        .map(|v| {
            serde_json::json!({
                "title": sanitize(&v.title),
                "url": sanitize(&v.url),
                "domain": sanitize(&domain_of(&v.url)),
                "duration": v.duration.as_deref().map(sanitize),
                "creator": v.creator.as_deref().map(sanitize),
                "date": v.date.as_deref().map(sanitize),
                "thumbnail": v.thumbnail.as_deref().map(sanitize),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const THUMB: &str = "https://imgs.search.brave.com/x/rs:fit:200:200:1/g:ce/aHR0cHM6";

    fn sections() -> Sections {
        let news = serde_json::json!({"results": [
            {
                "title": "Expressway <strong>toll</strong> rises",
                "url": "https://nation.africa/kenya/news/expressway-toll",
                "description": "The toll on the Nairobi Expressway rises from Monday. See https://nation.africa/x",
                "age": "2 days ago",
                "page_age": "2026-10-26T08:00:00",
                "profile": {"name": "Nation"},
                "thumbnail": {"src": THUMB, "original": "https://nation.africa/img.jpg"},
                "breaking": true,
            },
            {"title": "No address", "url": "javascript:alert(1)"},
            {"title": "", "url": "https://empty-title.example/"},
            {
                "title": "Second story",
                "url": "https://www.the-star.co.ke/story",
                "meta_url": {"hostname": "www.the-star.co.ke"},
                "thumbnail": {"src": "https://the-star.co.ke/original.jpg"},
                "age": "5 hours ago",
            },
            "not an object",
        ]});
        let videos = serde_json::json!({"results": [{
            "title": "How the expressway was built",
            "url": "https://www.youtube.com/watch?v=abc",
            "age": "March 3, 2024",
            "video": {"duration": "12:31", "publisher": "YouTube", "creator": "NTV Kenya"},
            "thumbnail": {"src": THUMB},
        }]});
        let locations = serde_json::json!({"results": [{
            "title": "Java House Junction",
            "url": "https://www.javahouseafrica.com/junction",
            "postal_address": {"displayAddress": "Junction Mall, Ngong Road, Nairobi"},
            "opening_hours": {"current_day": [
                {"abbr_name": "Mon", "full_name": "Monday", "opens": "07:00", "closes": "12:00"},
                {"abbr_name": "Mon", "full_name": "Monday", "opens": "14:00", "closes": "22:00"},
            ]},
            "contact": {"telephone": "+254 709 000 000"},
            "rating": {"ratingValue": 4.25, "bestRating": 5, "reviewCount": 120},
        }]});
        read_sections(Some(&news), Some(&videos), Some(&locations))
    }

    #[test]
    fn news_is_read_plain_dated_and_attributed() {
        let s = sections();
        assert_eq!(s.news.len(), 2, "{:?}", s.news);
        let first = &s.news[0];
        assert_eq!(first.title, "Expressway toll rises");
        assert_eq!(first.publisher, "Nation");
        assert_eq!(first.date.as_deref(), Some("2026-10-26"));
        assert!(first.breaking);
        assert!(!first.description.contains("http"), "{}", first.description);
        assert_eq!(first.thumbnail.as_deref(), Some(THUMB));
        let second = &s.news[1];
        assert_eq!(second.publisher, "the-star.co.ke", "a bare host, as the page's domain");
        assert_eq!(second.date.as_deref(), Some("5 hours ago"));
        assert_eq!(
            second.thumbnail, None,
            "an original image address is never shown"
        );
    }

    #[test]
    fn a_video_names_its_creator_and_length() {
        let video = &sections().videos[0];
        assert_eq!(video.duration.as_deref(), Some("12:31"));
        assert_eq!(video.creator.as_deref(), Some("NTV Kenya"));
        assert_eq!(video.date.as_deref(), Some("March 3, 2024"));
        assert_eq!(video.thumbnail.as_deref(), Some(THUMB));
    }

    #[test]
    fn a_place_reads_its_hours_address_phone_and_rating() {
        let place = &sections().places[0];
        assert_eq!(
            place.hours.as_deref(),
            Some("Monday 07:00 to 12:00 and 14:00 to 22:00")
        );
        assert_eq!(
            place.address.as_deref(),
            Some("Junction Mall, Ngong Road, Nairobi")
        );
        assert_eq!(place.phone.as_deref(), Some("+254 709 000 000"));
        assert_eq!(place.rating.as_deref(), Some("4.3 of 5 from 120 reviews"));
        assert_eq!(
            place.snippets(),
            vec![
                "open Monday 07:00 to 12:00 and 14:00 to 22:00",
                "address: Junction Mall, Ngong Road, Nairobi",
                "phone: +254 709 000 000",
                "rating: 4.3 of 5 from 120 reviews",
            ]
        );
    }

    #[test]
    fn missing_or_misshapen_sections_read_as_nothing() {
        assert_eq!(read_sections(None, None, None), Sections::default());
        let odd = serde_json::json!({"results": "not a list"});
        assert_eq!(
            read_sections(Some(&odd), Some(&Value::Null), Some(&odd)),
            Sections::default()
        );
        let partial = serde_json::json!({"results": [{"title": "Somewhere", "rating": {"ratingValue": "x"}}]});
        assert!(
            read_sections(None, None, Some(&partial)).places.is_empty(),
            "an item Brave shaped differently is skipped"
        );
        let bare = serde_json::json!({"results": [{"title": "Somewhere"}]});
        let places = read_sections(None, None, Some(&bare)).places;
        assert_eq!(places.len(), 1);
        assert!(places[0].snippets().is_empty());
    }

    #[test]
    fn at_most_the_caps_are_read() {
        let many = |n: usize| {
            serde_json::json!({"results": (0..n).map(|i| serde_json::json!({
                "title": format!("Item {i}"), "url": format!("https://s{i}.example/")
            })).collect::<Vec<_>>()})
        };
        let s = read_sections(Some(&many(9)), Some(&many(9)), Some(&many(9)));
        assert_eq!(s.news.len(), MAX_NEWS);
        assert_eq!(s.videos.len(), MAX_VIDEOS);
        assert_eq!(s.places.len(), MAX_LOCATIONS);
    }

    #[test]
    fn card_rows_cannot_end_a_marker_early() {
        let mut s = sections();
        s.news[0].title = "Breaking ]]] news".to_string();
        s.videos[0].creator = Some("A ]]] creator".to_string());
        let rows = format!("{}{}", news_card(&s.news), videos_card(&s.videos));
        assert!(!rows.contains("]]]"), "{rows}");
        assert!(rows.contains("12:31"), "{rows}");
    }
}
