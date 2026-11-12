//! A member's profile: what each memory says about them, from a closed list, and the one policy
//! every route that writes a memory goes through.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::user_data::domain::memory::{
    fact_defect, is_captured_request, names_subject, normalise_fact_content, FactDefect,
    MemoryFragment, MemorySegment, MemoryTier,
};

/// How long a passing fact ([`About::Now`]) is shown.
pub const PASSING_FACT_DAYS: i64 = 14;

/// What a memory says about its member. A closed list, so a small model names it in one word
/// and code, not the model, decides what replaces what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum About {
    Name,
    Birthday,
    Work,
    Home,
    Language,
    Partner,
    Child,
    Pet,
    Family,
    Friend,
    Diet,
    Allergy,
    Health,
    Routine,
    Likes,
    Dislikes,
    /// How Goose should talk to them.
    Style,
    Goal,
    Place,
    /// A birthday or anniversary that comes round each year.
    Date,
    /// A standing instruction to Goose.
    Instruction,
    /// True for days or weeks, not months.
    Now,
}

impl About {
    pub const ALL: [About; 22] = [
        Self::Name,
        Self::Birthday,
        Self::Work,
        Self::Home,
        Self::Language,
        Self::Partner,
        Self::Child,
        Self::Pet,
        Self::Family,
        Self::Friend,
        Self::Diet,
        Self::Allergy,
        Self::Health,
        Self::Routine,
        Self::Likes,
        Self::Dislikes,
        Self::Style,
        Self::Goal,
        Self::Place,
        Self::Date,
        Self::Instruction,
        Self::Now,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Birthday => "birthday",
            Self::Work => "work",
            Self::Home => "home",
            Self::Language => "language",
            Self::Partner => "partner",
            Self::Child => "child",
            Self::Pet => "pet",
            Self::Family => "family",
            Self::Friend => "friend",
            Self::Diet => "diet",
            Self::Allergy => "allergy",
            Self::Health => "health",
            Self::Routine => "routine",
            Self::Likes => "likes",
            Self::Dislikes => "dislikes",
            Self::Style => "style",
            Self::Goal => "goal",
            Self::Place => "place",
            Self::Date => "date",
            Self::Instruction => "instruction",
            Self::Now => "now",
        }
    }

    /// The list as a tool description or a prompt names it.
    pub fn listed() -> String {
        Self::ALL
            .iter()
            .map(|a| a.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// One of the list's words, or a near word a model writes for it.
    pub fn parse(raw: &str) -> Option<Self> {
        let word = raw.trim().to_ascii_lowercase().replace([' ', '-'], "_");
        if let Some(about) = Self::ALL.iter().find(|a| a.as_str() == word) {
            return Some(*about);
        }
        Some(match word.as_str() {
            "names" | "nickname" | "preferred_name" => Self::Name,
            "birthdate" | "date_of_birth" | "dob" | "age" => Self::Birthday,
            "job" | "occupation" | "role" | "career" | "school" | "studies" => Self::Work,
            "address" | "house" | "lives" | "city" | "hometown" => Self::Home,
            "languages" => Self::Language,
            "spouse" | "wife" | "husband" | "girlfriend" | "boyfriend" | "relationship"
            | "dating" => Self::Partner,
            "children" | "kid" | "kids" | "son" | "daughter" => Self::Child,
            "pets" => Self::Pet,
            "relative" | "relatives" | "parent" | "parents" | "sibling" | "siblings"
            | "household" => Self::Family,
            "friends" | "colleague" | "colleagues" | "people" | "person" => Self::Friend,
            "food" | "eating" => Self::Diet,
            "allergies" | "intolerance" => Self::Allergy,
            "medical" | "condition" | "accessibility" | "fitness" => Self::Health,
            "routines" | "habit" | "habits" | "schedule" => Self::Routine,
            "like" | "preference" | "preferences" | "favourite" | "favorite" | "interest"
            | "interests" | "hobby" | "hobbies" => Self::Likes,
            "dislike" | "avoid" | "avoids" => Self::Dislikes,
            "tone" | "voice" | "format" => Self::Style,
            "goals" | "project" | "projects" | "plan" | "plans" => Self::Goal,
            "places" | "location" => Self::Place,
            "dates" | "anniversary" => Self::Date,
            "instructions" | "rule" | "rules" | "always" | "never" | "correction" => {
                Self::Instruction
            }
            "current" | "context" | "currently" | "temporary" | "situation" => Self::Now,
            _ => return None,
        })
    }

    /// The profile heading this fact is filed under.
    pub fn segment(self) -> MemorySegment {
        match self {
            Self::Name | Self::Birthday | Self::Work | Self::Home | Self::Language => {
                MemorySegment::Identity
            }
            Self::Partner | Self::Child | Self::Pet | Self::Family | Self::Friend => {
                MemorySegment::Relationship
            }
            Self::Diet | Self::Allergy | Self::Health => MemorySegment::FoodHealth,
            Self::Routine => MemorySegment::Routine,
            Self::Likes | Self::Dislikes | Self::Style => MemorySegment::Preference,
            Self::Goal => MemorySegment::Project,
            Self::Place => MemorySegment::Place,
            Self::Date => MemorySegment::Date,
            Self::Instruction => MemorySegment::Instruction,
            Self::Now => MemorySegment::Context,
        }
    }

    /// A member has one of these at a time: a new one closes the old.
    pub fn single_valued(self) -> bool {
        matches!(
            self,
            Self::Name
                | Self::Birthday
                | Self::Work
                | Self::Home
                | Self::Language
                | Self::Partner
                | Self::Diet
                | Self::Style
        )
    }

    /// Never written into a notification nor said on the house speaker.
    pub fn sensitive(self) -> bool {
        matches!(self, Self::Allergy | Self::Health | Self::Partner)
    }

    /// Shown at the start of every conversation of the member's, whatever the message is about.
    pub fn always_shown(self) -> bool {
        matches!(
            self,
            Self::Name | Self::Diet | Self::Allergy | Self::Style | Self::Instruction
        )
    }

    /// The two facts that may carry a date: one that comes round each year.
    pub fn keeps_a_date(self) -> bool {
        matches!(self, Self::Date | Self::Birthday)
    }

    pub fn tier(self) -> MemoryTier {
        match self {
            Self::Now => MemoryTier::Short,
            Self::Name | Self::Birthday | Self::Allergy | Self::Instruction | Self::Date => {
                MemoryTier::Permanent
            }
            _ => MemoryTier::Long,
        }
    }

    pub fn importance(self) -> f32 {
        match self {
            Self::Instruction | Self::Allergy => 0.9,
            Self::Name | Self::Diet | Self::Health | Self::Style => 0.85,
            Self::Now => 0.4,
            other => other.segment().default_importance(),
        }
    }

    /// When a fact filed now stops being shown; `None` for one that lasts.
    pub fn valid_until(self, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
        (self == Self::Now).then(|| now + Duration::days(PASSING_FACT_DAYS))
    }
}

/// Why something offered as a memory was not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotKept {
    Defect(FactDefect),
    /// It does not say what it is about, or uses a word outside the list.
    NoAbout,
    /// It reports what they asked or looked up once, which says nothing of who they are.
    Asked,
    /// It is a task they gave the assistant, not a fact about them.
    Request,
    /// It does not name the member it is to be filed under.
    NotAboutThem,
}

impl NotKept {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Defect(defect) => defect.as_str(),
            Self::NoAbout => "says nothing of what it is about",
            Self::Asked => "a question asked once",
            Self::Request => "a task, not a fact",
            Self::NotAboutThem => "does not name the member",
        }
    }

    /// What to tell the model, so its next call or its words to the person are the right ones.
    pub fn advice(self, name: &str) -> String {
        let who = if name.trim().is_empty() {
            "the person"
        } else {
            name.trim()
        };
        match self {
            Self::NoAbout => format!(
                "Send `about` as one of: {}. If none fits, it is not something to remember.",
                About::listed()
            ),
            Self::Defect(FactDefect::FirstPerson) | Self::NotAboutThem => format!(
                "Write `content` as one sentence in the third person that names {who}, then \
                 send it again."
            ),
            Self::Defect(FactDefect::CalendarDate) => "A thing on one day is a reminder, not a \
                 memory: use add_reminder with `when`. Only a yearly date is kept, as `about` \
                 \"date\" or \"birthday\"."
                .to_string(),
            Self::Defect(_) => format!(
                "Write `content` as one whole sentence that names {who} and everyone and \
                 everything else it mentions, then send it again."
            ),
            Self::Asked => "A question they asked once is not who they are. Save only what \
                 they said about themselves."
                .to_string(),
            Self::Request => "That is something they asked you to do, not a fact about them. \
                 Do it; there is nothing to remember."
                .to_string(),
        }
    }
}

/// Whether a note only reports what its subject asked or looked up: "inquires about", "asked
/// for". One question is curiosity, which the interests already count.
pub fn describes_a_question(note: &str) -> bool {
    const ASKING: &[&str] = &[
        " asked about ",
        " asks about ",
        " asked for ",
        " asks for ",
        " asked what ",
        " asks what ",
        " asked how ",
        " asks how ",
        " asked whether ",
        " asks whether ",
        " asked if ",
        " asks if ",
        " inquires",
        " inquired",
        " enquires",
        " enquired",
        " wants to know",
        " wanted to know",
        " is curious about",
        " was curious about",
        " requested information",
        " requests information",
        " looked up ",
        " searched for ",
    ];
    let padded = format!(" {} ", note.to_lowercase());
    ASKING.iter().any(|phrase| padded.contains(phrase))
}

/// The sentence as it will be kept, or why it is not. `aliases` are the names of the member it
/// would be filed under; empty for the household, whose facts name nobody in particular.
pub fn admit(content: &str, about: Option<About>, aliases: &[String]) -> Result<String, NotKept> {
    let content = normalise_fact_content(content);
    let about = about.ok_or(NotKept::NoAbout)?;
    match fact_defect(&content) {
        Some(FactDefect::CalendarDate) if about.keeps_a_date() => {}
        Some(defect) => return Err(NotKept::Defect(defect)),
        None => {}
    }
    if describes_a_question(&content) {
        return Err(NotKept::Asked);
    }
    if is_captured_request(&content) {
        return Err(NotKept::Request);
    }
    if !aliases.is_empty() && !names_subject(&content, aliases) {
        return Err(NotKept::NotAboutThem);
    }
    Ok(content)
}

/// File `fragment` under `about`: its heading, how long it lasts and how much it weighs.
pub fn file_under(fragment: &mut MemoryFragment, about: About) {
    let tier = about.tier();
    fragment.about = Some(about);
    fragment.segment = Some(about.segment());
    fragment.importance = Some(about.importance());
    fragment.decay_rate = Some(tier.default_decay_rate());
    fragment.tier = Some(tier);
    fragment.valid_until = about.valid_until(fragment.created_at);
}

/// Whether two sentences say the same thing once case, spacing and the final stop are set aside.
pub fn same_sentence(a: &str, b: &str) -> bool {
    let plain = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_end_matches(['.', '!'])
            .to_lowercase()
    };
    plain(a) == plain(b)
}

/// The rows a new fact about `about` closes: every live one for a single-valued fact, none
/// otherwise. `existing` are the member's live rows with that `about`.
pub fn closed_by<'a>(
    about: About,
    new_content: &str,
    existing: &'a [MemoryFragment],
) -> Vec<&'a MemoryFragment> {
    if !about.single_valued() {
        return Vec::new();
    }
    existing
        .iter()
        .filter(|row| row.about == Some(about) && !same_sentence(&row.content, new_content))
        .collect()
}

/// A profile as the model reads it: each heading on its own line, its facts under it. `rows`
/// are taken in the order given; [`profile_order`] them first for a whole profile.
pub fn render_profile(rows: &[MemoryFragment]) -> String {
    let mut out = String::new();
    for segment in MemorySegment::ALL.iter().map(Some).chain([None]) {
        let facts: Vec<&MemoryFragment> = rows
            .iter()
            .filter(|row| row.segment.as_ref() == segment)
            .collect();
        if facts.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(segment.map_or("Other", MemorySegment::heading));
        for fact in facts {
            out.push_str("\n- ");
            out.push_str(fact.content.trim());
            if let Some(corrects) = fact.corrects.as_deref().filter(|c| !c.trim().is_empty()) {
                out.push_str(&format!(" (replaces: {})", corrects.trim()));
            }
        }
    }
    out
}

/// A member's live rows in the order a profile shows them: standing instructions first, then
/// who they are, then the rest; within a heading the weightiest first.
pub fn profile_order(rows: &mut [MemoryFragment]) {
    rows.sort_by(|a, b| {
        let rank = |r: &MemoryFragment| {
            r.segment
                .as_ref()
                .map_or(usize::MAX, MemorySegment::profile_rank)
        };
        rank(a).cmp(&rank(b)).then(
            b.importance
                .unwrap_or(0.0)
                .total_cmp(&a.importance.unwrap_or(0.0)),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jerry() -> Vec<String> {
        vec!["Jerry".to_string()]
    }

    fn row(about: About, content: &str) -> MemoryFragment {
        let mut fragment =
            MemoryFragment::from_chat("id".into(), Some("m1".into()), None, content.to_string());
        file_under(&mut fragment, about);
        fragment
    }

    #[test]
    fn every_word_of_the_list_reads_back_and_files_under_a_heading() {
        for about in About::ALL {
            assert_eq!(About::parse(about.as_str()), Some(about));
            assert!(!about.segment().heading().is_empty());
        }
        assert_eq!(About::parse("Job"), Some(About::Work));
        assert_eq!(About::parse("date of birth"), Some(About::Birthday));
        assert_eq!(About::parse("knowledge"), None);
        assert_eq!(About::parse(""), None);
    }

    #[test]
    fn a_plain_fact_naming_the_member_is_kept() {
        assert_eq!(
            admit(
                "  Jerry is allergic   to peanuts. ",
                Some(About::Allergy),
                &jerry()
            ),
            Ok("Jerry is allergic to peanuts.".to_string())
        );
    }

    #[test]
    fn what_is_not_a_fact_about_the_member_is_refused_with_its_reason() {
        let kept = |content: &str, about: Option<About>| admit(content, about, &jerry());
        assert_eq!(
            kept("Jerry likes his tea strong.", None),
            Err(NotKept::NoAbout)
        );
        assert_eq!(
            kept("I like my tea strong.", Some(About::Likes)),
            Err(NotKept::Defect(FactDefect::FirstPerson))
        );
        assert_eq!(
            kept(
                "Jerry asked about the weather in Kisumu.",
                Some(About::Likes)
            ),
            Err(NotKept::Asked)
        );
        assert_eq!(
            kept("The fan in the bedroom is noisy.", Some(About::Home)),
            Err(NotKept::NotAboutThem)
        );
        assert_eq!(
            kept("Jerry sees the dentist on Tuesday.", Some(About::Health)),
            Err(NotKept::Defect(FactDefect::CalendarDate))
        );
    }

    #[test]
    fn a_yearly_date_is_kept_where_a_one_off_date_is_not() {
        assert!(admit(
            "Jerry's wedding anniversary is on 14 March.",
            Some(About::Date),
            &jerry()
        )
        .is_ok());
        assert!(admit(
            "Jerry's birthday is on 20 April.",
            Some(About::Birthday),
            &jerry()
        )
        .is_ok());
    }

    #[test]
    fn a_household_fact_need_not_name_anyone() {
        assert!(admit(
            "The household keeps shoes off indoors.",
            Some(About::Routine),
            &[]
        )
        .is_ok());
    }

    #[test]
    fn filing_sets_the_heading_the_weight_and_how_long_it_lasts() {
        let allergy = row(About::Allergy, "Jerry is allergic to peanuts.");
        assert_eq!(allergy.segment, Some(MemorySegment::FoodHealth));
        assert_eq!(allergy.tier, Some(MemoryTier::Permanent));
        assert_eq!(allergy.valid_until, None);

        let passing = row(About::Now, "Jerry is staying in Kisumu this month.");
        assert_eq!(passing.tier, Some(MemoryTier::Short));
        assert_eq!(
            passing.valid_until,
            Some(passing.created_at + Duration::days(PASSING_FACT_DAYS))
        );
    }

    #[test]
    fn a_new_single_valued_fact_closes_the_old_and_a_list_fact_closes_nothing() {
        let rows = vec![
            row(About::Diet, "Jerry is vegetarian."),
            row(About::Likes, "Jerry likes hiking."),
        ];
        let closed = closed_by(About::Diet, "Jerry eats fish but no other meat.", &rows);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].content, "Jerry is vegetarian.");
        assert!(closed_by(About::Diet, "jerry is vegetarian", &rows).is_empty());
        assert!(closed_by(About::Likes, "Jerry likes bowling.", &rows).is_empty());
    }

    #[test]
    fn a_profile_reads_as_headings_with_their_facts() {
        let mut rows = vec![
            row(About::Likes, "Jerry likes hiking."),
            row(About::Allergy, "Jerry is allergic to peanuts."),
            row(About::Diet, "Jerry eats no pork."),
            MemoryFragment::from_chat("old".into(), None, None, "An unfiled note.".into()),
        ];
        profile_order(&mut rows);
        assert_eq!(
            render_profile(&rows),
            "Food and health\n- Jerry is allergic to peanuts.\n- Jerry eats no pork.\n\
             Preferences\n- Jerry likes hiking.\n\
             Other\n- An unfiled note."
        );
        assert_eq!(render_profile(&[]), "");
    }

    #[test]
    fn a_profile_leads_with_instructions_then_who_they_are() {
        let mut rows = vec![
            row(About::Likes, "Jerry likes hiking."),
            row(About::Name, "Jerry goes by Jerr."),
            row(About::Instruction, "Jerry wants prices in shillings."),
            row(About::Allergy, "Jerry is allergic to peanuts."),
        ];
        profile_order(&mut rows);
        let order: Vec<About> = rows.iter().filter_map(|r| r.about).collect();
        assert_eq!(
            order,
            vec![
                About::Instruction,
                About::Name,
                About::Allergy,
                About::Likes
            ]
        );
    }
}
