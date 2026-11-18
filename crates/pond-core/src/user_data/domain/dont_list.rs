//! What a member told Goose not to offer. Every maker and critic reads the `never` rules; the
//! `avoid` rules are read by relevance.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::interest::topic_key;

const MAX_RULE_CHARS: usize = 200;

/// The key two wordings of one rule share.
pub fn rule_key(rule: &str) -> String {
    topic_key(rule)
}

/// A rule as stored: one line, bounded, without the strength word a model may lead with.
pub fn clean_rule(raw: &str) -> Option<String> {
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let after = |lead: &str| {
        let head = one_line.get(..lead.len())?;
        head.eq_ignore_ascii_case(lead)
            .then(|| one_line.get(lead.len()..))
            .flatten()
    };
    let body = after("never:")
        .or_else(|| after("avoid:"))
        .map_or(one_line.as_str(), str::trim);
    if rule_key(body).is_empty() {
        return None;
    }
    Some(body.chars().take(MAX_RULE_CHARS).collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strength {
    /// A general rule, read by every job.
    Never,
    /// One thing turned down, read where it is relevant.
    Avoid,
}

impl Strength {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Never => "never",
            Self::Avoid => "avoid",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "never" => Some(Self::Never),
            "avoid" => Some(Self::Avoid),
            _ => None,
        }
    }
}

/// What the member did that wrote the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOrigin {
    /// Rejected a deliverable.
    Reject,
    /// Stopped an anchor.
    Stop,
    /// Removed a fact line.
    Fact,
    /// Wrote the rule by hand.
    You,
}

impl RuleOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reject => "reject",
            Self::Stop => "stop",
            Self::Fact => "fact",
            Self::You => "you",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "reject" => Some(Self::Reject),
            "stop" => Some(Self::Stop),
            "fact" => Some(Self::Fact),
            "you" => Some(Self::You),
            _ => None,
        }
    }
}

/// The reason chips, the same on every surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    TooGeneric,
    NotRelevant,
    WrongFacts,
    AlreadyKnow,
    BadTiming,
}

impl Reason {
    pub const ALL: [Reason; 5] = [
        Self::TooGeneric,
        Self::NotRelevant,
        Self::WrongFacts,
        Self::AlreadyKnow,
        Self::BadTiming,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TooGeneric => "too_generic",
            Self::NotRelevant => "not_relevant",
            Self::WrongFacts => "wrong_facts",
            Self::AlreadyKnow => "already_know",
            Self::BadTiming => "bad_timing",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "too_generic" => Some(Self::TooGeneric),
            "not_relevant" => Some(Self::NotRelevant),
            "wrong_facts" => Some(Self::WrongFacts),
            "already_know" => Some(Self::AlreadyKnow),
            "bad_timing" => Some(Self::BadTiming),
            _ => None,
        }
    }

    /// The chip as a maker or critic reads it.
    pub fn as_words(self) -> &'static str {
        match self {
            Self::TooGeneric => "too generic",
            Self::NotRelevant => "not relevant to them",
            Self::WrongFacts => "built on wrong facts",
            Self::AlreadyKnow => "something they already know",
            Self::BadTiming => "badly timed",
        }
    }
}

/// One rule of a member's don't-list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DontRule {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub rule: String,
    pub strength: Strength,
    #[serde(default)]
    pub reasons: Vec<Reason>,
    /// The member's own words, when they gave any.
    #[serde(default)]
    pub said: Option<String>,
    pub origin: RuleOrigin,
    /// The deliverable, anchor or fact line the rule came from.
    #[serde(default)]
    pub origin_ref: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl DontRule {
    /// `None` when `rule` has no words.
    pub fn new(
        profile_id: &str,
        rule: &str,
        strength: Strength,
        origin: RuleOrigin,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            rule: clean_rule(rule)?,
            strength,
            reasons: Vec::new(),
            said: None,
            origin,
            origin_ref: None,
            created_at: now,
        })
    }

    /// "Not this one": the title turned down, with its chips; needs no model.
    pub fn avoid_title(
        profile_id: &str,
        title: &str,
        reasons: &[Reason],
        origin: RuleOrigin,
        origin_ref: &str,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        let mut rule = Self::new(profile_id, title, Strength::Avoid, origin, now)?;
        rule.reasons = deduplicated(reasons);
        rule.origin_ref = Some(origin_ref.to_string());
        Some(rule)
    }

    pub fn key(&self) -> String {
        rule_key(&self.rule)
    }

    /// The rule as a prompt reads it: `Never: ...` or `Avoid: ... (too generic)`.
    pub fn as_line(&self) -> String {
        let lead = match self.strength {
            Strength::Never => "Never",
            Strength::Avoid => "Avoid",
        };
        if self.reasons.is_empty() {
            return format!("{lead}: {}", self.rule);
        }
        let why: Vec<&str> = self.reasons.iter().map(|r| r.as_words()).collect();
        format!("{lead}: {} ({})", self.rule, why.join(", "))
    }

    /// This rule with a second saying of it merged in: the stronger strength wins and the
    /// chips add up, so a `never` is not weakened by a later `avoid`.
    pub fn merged_with(mut self, again: &DontRule) -> Self {
        if again.strength == Strength::Never {
            self.strength = Strength::Never;
            self.rule = again.rule.clone();
        }
        let all: Vec<Reason> = self
            .reasons
            .iter()
            .chain(again.reasons.iter())
            .copied()
            .collect();
        self.reasons = deduplicated(&all);
        if again.said.is_some() {
            self.said = again.said.clone();
        }
        self
    }
}

fn deduplicated(reasons: &[Reason]) -> Vec<Reason> {
    let mut seen = std::collections::HashSet::new();
    reasons
        .iter()
        .copied()
        .filter(|r| seen.insert(*r))
        .collect()
}

/// Orders rules as they are read: `never` first, then newest.
pub fn sort_rules(rules: &mut [DontRule]) {
    rules.sort_by(|a, b| {
        (a.strength == Strength::Avoid)
            .cmp(&(b.strength == Strength::Avoid))
            .then(b.created_at.cmp(&a.created_at))
            .then(a.id.cmp(&b.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_is_stored_without_its_strength_word() {
        assert_eq!(
            clean_rule("Never:  recipes built on a\ncolour preference").as_deref(),
            Some("recipes built on a colour preference")
        );
        assert_eq!(
            clean_rule("avoid: Duck à l'orange").as_deref(),
            Some("Duck à l'orange")
        );
        assert_eq!(clean_rule("Never: ..."), None);
    }

    #[test]
    fn a_rule_reads_as_one_line_with_its_chips() {
        let now = Utc::now();
        let never = DontRule::new(
            "jerry",
            "celebrity gossip",
            Strength::Never,
            RuleOrigin::You,
            now,
        )
        .unwrap();
        assert_eq!(never.as_line(), "Never: celebrity gossip");
        let avoid = DontRule::avoid_title(
            "jerry",
            "A duck recipe in teal",
            &[Reason::TooGeneric, Reason::WrongFacts, Reason::TooGeneric],
            RuleOrigin::Reject,
            "d1",
            now,
        )
        .unwrap();
        assert_eq!(
            avoid.as_line(),
            "Avoid: A duck recipe in teal (too generic, built on wrong facts)"
        );
    }

    #[test]
    fn a_never_is_not_weakened_by_a_later_avoid() {
        let now = Utc::now();
        let never = DontRule::new(
            "jerry",
            "Duck recipes",
            Strength::Never,
            RuleOrigin::You,
            now,
        )
        .unwrap();
        let avoid = DontRule::avoid_title(
            "jerry",
            "duck recipes",
            &[Reason::BadTiming],
            RuleOrigin::Reject,
            "d1",
            now,
        )
        .unwrap();
        let merged = never.clone().merged_with(&avoid);
        assert_eq!(merged.strength, Strength::Never);
        assert_eq!(merged.rule, "Duck recipes");
        assert_eq!(merged.reasons, [Reason::BadTiming]);
        assert_eq!(avoid.merged_with(&never).strength, Strength::Never);
    }

    #[test]
    fn the_chips_are_spelled_as_the_surfaces_send_them() {
        for reason in Reason::ALL {
            assert_eq!(Reason::parse(reason.as_str()), Some(reason));
            assert_eq!(
                serde_json::to_string(&reason).unwrap(),
                format!("\"{}\"", reason.as_str())
            );
        }
    }
}
