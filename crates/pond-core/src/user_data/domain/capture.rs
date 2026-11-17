//! A screenshot or photo a member's phone sent: what it shows, what the member seems to want
//! with it, and the picture itself for a while. A private one is never kept at all.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// How long a picture is kept after it is read; its description stays.
pub const PICTURE_KEPT_FOR: Duration = Duration::days(30);
/// Reads a capture gets before its picture is deleted unread.
pub const MAX_ATTEMPTS: u32 = 2;
/// Largest picture taken, as base64 characters (about 1.5 MB of picture).
pub const MAX_IMAGE_CHARS: usize = 2_000_000;
/// Picture types the model reads.
pub const MIME_TYPES: [&str; 3] = ["image/jpeg", "image/png", "image/webp"];

/// Why a picture a phone sent was not taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotTaken {
    TooLarge,
    /// Not a picture type the model reads.
    WrongType,
}

impl NotTaken {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooLarge => "the picture is too large; send it smaller",
            Self::WrongType => "the picture must be a JPEG, a PNG or a WebP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    Screenshot,
    Photo,
}

impl CaptureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screenshot => "screenshot",
            Self::Photo => "photo",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "screenshot" => Some(Self::Screenshot),
            "photo" => Some(Self::Photo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureState {
    /// Waiting for a job to read it.
    Pending,
    Read,
}

impl CaptureState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Read => "read",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pending" => Some(Self::Pending),
            "read" => Some(Self::Read),
            _ => None,
        }
    }
}

/// What a kept capture shows of the member's money; a private capture has no row to say so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    #[default]
    None,
    /// A receipt, a payment message or a balance: read by the Budget specialist, then deleted.
    Money,
}

impl Sensitivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Money => "money",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "money" => Some(Self::Money),
            _ => None,
        }
    }
}

/// What a capture names, as the reader listed it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureDetails {
    #[serde(default)]
    pub names: Vec<String>,
    #[serde(default)]
    pub dates: Vec<String>,
    #[serde(default)]
    pub places: Vec<String>,
    #[serde(default)]
    pub prices: Vec<String>,
    #[serde(default)]
    pub links: Vec<String>,
}

impl CaptureDetails {
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
            && self.dates.is_empty()
            && self.places.is_empty()
            && self.prices.is_empty()
            && self.links.is_empty()
    }

    /// The details as one line a search or a brief reads.
    pub fn as_line(&self) -> String {
        [
            ("Names", &self.names),
            ("Dates", &self.dates),
            ("Places", &self.places),
            ("Prices", &self.prices),
            ("Links", &self.links),
        ]
        .iter()
        .filter(|(_, items)| !items.is_empty())
        .map(|(label, items)| format!("{label}: {}", items.join(", ")))
        .collect::<Vec<_>>()
        .join(". ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub kind: CaptureKind,
    /// The phone album a photo came from.
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub taken_at: Option<DateTime<Utc>>,
    pub state: CaptureState,
    pub sensitive: Sensitivity,
    /// A money capture waiting for the Budget specialist; deleted whole once it is read.
    pub hot: bool,
    #[serde(default)]
    pub description: Option<String>,
    /// What the member seems to want with it.
    #[serde(default)]
    pub intent: Option<String>,
    #[serde(default)]
    pub details: CaptureDetails,
    /// Where the picture is kept on the pond; `None` once it is deleted.
    #[serde(skip)]
    pub picture_file: Option<String>,
    #[serde(default)]
    pub picture_until: Option<DateTime<Utc>>,
    /// A hash of the picture's bytes, so the same picture is not kept twice.
    #[serde(skip)]
    pub content_hash: Option<String>,
    /// Jobs that have tried to read it.
    pub attempts: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Capture {
    /// A capture waiting to be read.
    pub fn new(profile_id: &str, kind: CaptureKind, now: DateTime<Utc>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            kind,
            album: None,
            taken_at: None,
            state: CaptureState::Pending,
            sensitive: Sensitivity::None,
            hot: false,
            description: None,
            intent: None,
            details: CaptureDetails::default(),
            picture_file: None,
            picture_until: None,
            content_hash: None,
            attempts: 0,
            created_at: now,
            updated_at: now,
        }
    }

    /// A job read it and found nothing sensitive: what it shows is kept, and the picture for
    /// [`PICTURE_KEPT_FOR`].
    pub fn read(
        &mut self,
        description: &str,
        intent: Option<&str>,
        details: CaptureDetails,
        now: DateTime<Utc>,
    ) {
        let one_line = |raw: &str| raw.split_whitespace().collect::<Vec<_>>().join(" ");
        self.state = CaptureState::Read;
        self.sensitive = Sensitivity::None;
        self.hot = false;
        self.description = Some(one_line(description)).filter(|d| !d.is_empty());
        self.intent = intent.map(one_line).filter(|i| !i.is_empty());
        self.details = details;
        self.picture_until = self.picture_file.is_some().then(|| now + PICTURE_KEPT_FOR);
        self.updated_at = now;
    }

    /// A job found money in it: held, undescribed, for the Budget specialist to read.
    pub fn hold_for_budget(&mut self, now: DateTime<Utc>) {
        self.state = CaptureState::Read;
        self.sensitive = Sensitivity::Money;
        self.hot = true;
        self.updated_at = now;
    }

    /// Counts a job's try at reading it.
    pub fn count_attempt(&mut self, now: DateTime<Utc>) {
        self.attempts += 1;
        self.updated_at = now;
    }

    /// Still unread after every try it gets.
    pub fn is_unreadable(&self) -> bool {
        self.state == CaptureState::Pending && self.attempts >= MAX_ATTEMPTS
    }

    /// The picture's time is up.
    pub fn picture_is_due(&self, now: DateTime<Utc>) -> bool {
        self.picture_file.is_some() && self.picture_until.is_some_and(|until| until <= now)
    }

    /// The picture went; what it showed stays.
    pub fn forget_picture(&mut self, now: DateTime<Utc>) {
        self.picture_file = None;
        self.picture_until = None;
        self.updated_at = now;
    }

    /// Whether what it shows may be searched: read, described and not about money.
    pub fn is_searchable(&self) -> bool {
        self.state == CaptureState::Read
            && self.sensitive == Sensitivity::None
            && self.description.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(now: DateTime<Utc>) -> Capture {
        let mut capture = Capture::new("jerry", CaptureKind::Screenshot, now);
        capture.picture_file = Some("captures/jerry/c1.jpg".into());
        capture
    }

    #[test]
    fn a_read_capture_keeps_its_description_and_its_picture_for_thirty_days() {
        let now = Utc::now();
        let mut shot = capture(now);
        assert!(!shot.is_searchable());
        shot.read(
            " A flight to\nKisumu on 3 December ",
            Some("planning a trip"),
            CaptureDetails {
                places: vec!["Kisumu".into()],
                prices: vec!["KSh 8,400".into()],
                ..Default::default()
            },
            now,
        );
        assert_eq!(
            shot.description.as_deref(),
            Some("A flight to Kisumu on 3 December")
        );
        assert_eq!(shot.picture_until, Some(now + PICTURE_KEPT_FOR));
        assert_eq!(shot.details.as_line(), "Places: Kisumu. Prices: KSh 8,400");
        assert!(shot.is_searchable());
        assert!(!shot.picture_is_due(now + Duration::days(29)));
        assert!(shot.picture_is_due(now + Duration::days(30)));
        shot.forget_picture(now);
        assert!(!shot.picture_is_due(now + Duration::days(31)));
        assert!(shot.is_searchable(), "what it showed stays");
    }

    #[test]
    fn a_money_capture_is_held_hot_and_never_searched() {
        let now = Utc::now();
        let mut receipt = capture(now);
        receipt.hold_for_budget(now);
        assert!(receipt.hot);
        assert_eq!(receipt.sensitive, Sensitivity::Money);
        assert!(!receipt.is_searchable());
        assert_eq!(receipt.picture_until, None);
    }

    #[test]
    fn a_capture_is_given_up_on_after_its_second_failed_read() {
        let now = Utc::now();
        let mut blurry = capture(now);
        blurry.count_attempt(now);
        assert!(!blurry.is_unreadable());
        blurry.count_attempt(now);
        assert!(blurry.is_unreadable());
    }

    #[test]
    fn the_picture_path_and_hash_never_leave_the_pond() {
        let mut shot = capture(Utc::now());
        shot.content_hash = Some("abc".into());
        let wire = serde_json::to_value(&shot).unwrap();
        assert!(wire.get("picture_file").is_none());
        assert!(wire.get("content_hash").is_none());
        assert!(wire.get("profile_id").is_none());
        assert_eq!(wire["sensitive"], "none");
    }
}
