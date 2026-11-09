//! What a member keeps from the assistant's work: a recipe, a listing, a place or a report, as a
//! typed card with a Markdown body, in collections they name. A card belongs to the member who
//! saved it; sharing it shows it to the household.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Cards one member may keep.
pub const MAX_SAVED_PER_MEMBER: usize = 500;
pub const MAX_TITLE_CHARS: usize = 160;
pub const MAX_BODY_CHARS: usize = 20_000;
pub const MAX_COLLECTION_CHARS: usize = 60;
const MAX_LIST_ENTRIES: usize = 80;
const MAX_FIELD_CHARS: usize = 400;

/// The kind of card, with the fields that kind has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SavedFields {
    Recipe {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        servings: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        time: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        ingredients: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        steps: Vec<String>,
    },
    Listing {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        price: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        currency: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mileage: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        year: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        location: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        photos: Vec<String>,
    },
    Place {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        address: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hours: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        link: Option<String>,
    },
    Report {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    Note,
}

impl SavedFields {
    pub fn kind(&self) -> &'static str {
        match self {
            SavedFields::Recipe { .. } => "recipe",
            SavedFields::Listing { .. } => "listing",
            SavedFields::Place { .. } => "place",
            SavedFields::Report { .. } => "report",
            SavedFields::Note => "note",
        }
    }

    /// The page a listing or place came from, which a watch can keep checking.
    pub fn link(&self) -> Option<&str> {
        match self {
            SavedFields::Listing { link, .. } | SavedFields::Place { link, .. } => link.as_deref(),
            _ => None,
        }
    }

    /// The fields within their bounds, blanks dropped, so a model's or a client's excess is cut
    /// here rather than stored.
    pub fn bounded(self) -> Self {
        let one = |v: Option<String>| {
            v.map(|s| clip(s.trim(), MAX_FIELD_CHARS))
                .filter(|s| !s.is_empty())
        };
        let many = |v: Vec<String>| {
            v.into_iter()
                .map(|s| clip(s.trim(), MAX_FIELD_CHARS))
                .filter(|s| !s.is_empty())
                .take(MAX_LIST_ENTRIES)
                .collect()
        };
        match self {
            SavedFields::Recipe {
                servings,
                time,
                ingredients,
                steps,
            } => SavedFields::Recipe {
                servings: one(servings),
                time: one(time),
                ingredients: many(ingredients),
                steps: many(steps),
            },
            SavedFields::Listing {
                price,
                currency,
                mileage,
                year,
                location,
                link,
                photos,
            } => SavedFields::Listing {
                price: one(price),
                currency: one(currency),
                mileage: one(mileage),
                year: one(year),
                location: one(location),
                link: one(link).filter(|l| is_web(l)),
                photos: many(photos).into_iter().filter(|p| is_web(p)).collect(),
            },
            SavedFields::Place {
                address,
                hours,
                link,
            } => SavedFields::Place {
                address: one(address),
                hours: one(hours),
                link: one(link).filter(|l| is_web(l)),
            },
            SavedFields::Report { task_id } => SavedFields::Report {
                task_id: one(task_id),
            },
            SavedFields::Note => SavedFields::Note,
        }
    }
}

/// A web source a card cites.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSource {
    pub title: String,
    pub url: String,
}

/// One kept card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedItem {
    pub id: String,
    pub owner: String,
    pub title: String,
    #[serde(flatten)]
    pub fields: SavedFields,
    /// The card's text: a recipe's notes, a report whole, why a listing fits.
    #[serde(default)]
    pub body: String,
    /// The collection it sits in, such as "Cars" or "Dinners"; none is the member's loose cards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Shown to the whole household; otherwise the owner's alone.
    #[serde(default)]
    pub shared: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SavedSource>,
    /// The conversation or task it was saved from, which a tap can open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Why a card could not be kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveRefused {
    NoTitle,
    TooMany,
}

impl SaveRefused {
    pub fn words(&self) -> String {
        match self {
            SaveRefused::NoTitle => "A saved card needs a title.".to_string(),
            SaveRefused::TooMany => {
                format!("They already keep {MAX_SAVED_PER_MEMBER} cards, the most one member may.")
            }
        }
    }
}

impl SavedItem {
    /// A new card, its text within bounds; refused without a title.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        owner: String,
        title: &str,
        fields: SavedFields,
        body: &str,
        collection: Option<&str>,
        sources: Vec<SavedSource>,
        session_id: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<Self, SaveRefused> {
        let title = clip(title.trim(), MAX_TITLE_CHARS);
        if title.is_empty() {
            return Err(SaveRefused::NoTitle);
        }
        Ok(Self {
            id,
            owner,
            title,
            fields: fields.bounded(),
            body: clip(body.trim(), MAX_BODY_CHARS),
            collection: collection_name(collection),
            shared: false,
            sources: sources
                .into_iter()
                .filter(|s| is_web(&s.url))
                .take(MAX_LIST_ENTRIES)
                .collect(),
            session_id,
            created_at: now,
            updated_at: now,
        })
    }

    /// Whether `member` may see it: the owner always, anyone once shared.
    pub fn visible_to(&self, member: &str) -> bool {
        self.owner == member || self.shared
    }
}

/// A collection's name as kept: trimmed, bounded, none when blank. Names differing only in case
/// are one collection, so the first spelling is kept by the store.
pub fn collection_name(raw: Option<&str>) -> Option<String> {
    raw.map(|c| {
        clip(
            &c.split_whitespace().collect::<Vec<_>>().join(" "),
            MAX_COLLECTION_CHARS,
        )
    })
    .filter(|c| !c.is_empty())
}

/// A recipe's ingredients and steps, from the lists under its "Ingredients" and "Steps" (or
/// "Method", "Instructions", "Directions") headings.
pub fn recipe_parts(markdown: &str) -> (Vec<String>, Vec<String>) {
    #[derive(PartialEq)]
    enum Under {
        Ingredients,
        Steps,
        Other,
    }
    let mut under = Under::Other;
    let (mut ingredients, mut steps) = (Vec::new(), Vec::new());
    for line in markdown.lines() {
        let trimmed = line.trim();
        let heading = trimmed
            .trim_start_matches('#')
            .trim()
            .trim_matches('*')
            .trim_end_matches(':');
        if trimmed.starts_with('#') || (trimmed.starts_with("**") && trimmed.ends_with("**")) {
            let lower = heading.to_lowercase();
            under = if lower.contains("ingredient") {
                Under::Ingredients
            } else if ["step", "method", "instruction", "direction", "preparation"]
                .iter()
                .any(|w| lower.contains(w))
            {
                Under::Steps
            } else {
                Under::Other
            };
            continue;
        }
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| {
                let digits = trimmed.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
                (digits > 0)
                    .then(|| {
                        trimmed[digits..]
                            .strip_prefix(". ")
                            .or_else(|| trimmed[digits..].strip_prefix(") "))
                    })
                    .flatten()
            });
        match (item.map(str::trim).filter(|i| !i.is_empty()), &under) {
            (Some(item), Under::Ingredients) => ingredients.push(item.to_string()),
            (Some(item), Under::Steps) => steps.push(item.to_string()),
            _ => {}
        }
    }
    (ingredients, steps)
}

fn is_web(address: &str) -> bool {
    address.starts_with("https://") || address.starts_with("http://")
}

fn clip(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => text[..at].trim_end().to_string(),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_is_kept_within_bounds_and_only_web_links_stay() {
        let fields = SavedFields::Listing {
            price: Some("  KES 1,450,000 ".into()),
            currency: None,
            mileage: Some(" ".into()),
            year: None,
            location: None,
            link: Some("javascript:alert(1)".into()),
            photos: vec![
                "https://img.example/1.jpg".into(),
                "file:///etc/passwd".into(),
            ],
        };
        let item = SavedItem::new(
            "c1".into(),
            "amina".into(),
            &"t".repeat(MAX_TITLE_CHARS + 10),
            fields,
            "body",
            Some("  Cars   to buy "),
            vec![SavedSource {
                title: "x".into(),
                url: "ftp://x".into(),
            }],
            None,
            Utc::now(),
        )
        .unwrap();
        assert_eq!(item.title.chars().count(), MAX_TITLE_CHARS);
        assert_eq!(item.collection.as_deref(), Some("Cars to buy"));
        assert!(item.sources.is_empty());
        let SavedFields::Listing {
            price,
            mileage,
            link,
            photos,
            ..
        } = &item.fields
        else {
            panic!("a listing");
        };
        assert_eq!(price.as_deref(), Some("KES 1,450,000"));
        assert!(mileage.is_none() && link.is_none());
        assert_eq!(photos, &vec!["https://img.example/1.jpg".to_string()]);
        assert!(item.visible_to("amina") && !item.visible_to("baraka"));
        let blank = SavedItem::new(
            "c2".into(),
            "a".into(),
            "  ",
            SavedFields::Note,
            "",
            None,
            vec![],
            None,
            Utc::now(),
        );
        assert_eq!(blank, Err(SaveRefused::NoTitle));
    }

    #[test]
    fn a_recipe_reads_its_lists_under_their_headings() {
        let (ingredients, steps) = recipe_parts(
            "# Chapati\n**Ingredients:**\n- 2 cups flour\n* Salt\nSome chat.\n### Directions\n1) Knead.\n2. Rest it.\n## Notes\n- Serve warm",
        );
        assert_eq!(ingredients, vec!["2 cups flour", "Salt"]);
        assert_eq!(steps, vec!["Knead.", "Rest it."]);
    }

    #[test]
    fn a_card_reads_and_writes_its_kind_beside_its_fields() {
        let json = serde_json::json!({"kind": "recipe", "servings": "4", "ingredients": ["rice"]});
        let fields: SavedFields = serde_json::from_value(json).unwrap();
        assert_eq!(fields.kind(), "recipe");
        let note: SavedFields =
            serde_json::from_value(serde_json::json!({"kind": "note"})).unwrap();
        assert_eq!(note, SavedFields::Note);
    }
}
