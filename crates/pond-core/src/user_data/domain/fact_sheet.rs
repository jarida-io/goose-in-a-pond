//! One member's fact sheet: typed lines about them, each with a source and the tools it bears
//! on. Background jobs read all of it; a chat turn is handed the relevant lines.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::interest::topic_key;

/// Active lines one member keeps; a rebuild retires the weakest beyond it.
pub const FACT_SHEET_LIMIT: usize = 100;
const MAX_FACT_CHARS: usize = 200;

/// The key two wordings of one fact share.
pub fn fact_key(fact: &str) -> String {
    topic_key(fact)
}

/// A fact as stored: one line, bounded. `None` when nothing is left.
pub fn clean_fact(raw: &str) -> Option<String> {
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if fact_key(&one_line).is_empty() {
        return None;
    }
    Some(one_line.chars().take(MAX_FACT_CHARS).collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FactCategory {
    Identity,
    Home,
    Work,
    People,
    Plan,
    Hunt,
    Interest,
    Food,
    Health,
    Style,
    Routine,
    Avoid,
    #[default]
    Other,
}

impl FactCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Home => "home",
            Self::Work => "work",
            Self::People => "people",
            Self::Plan => "plan",
            Self::Hunt => "hunt",
            Self::Interest => "interest",
            Self::Food => "food",
            Self::Health => "health",
            Self::Style => "style",
            Self::Routine => "routine",
            Self::Avoid => "avoid",
            Self::Other => "other",
        }
    }

    /// The stored word, or `Other` for anything a model invents.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "identity" => Self::Identity,
            "home" => Self::Home,
            "work" => Self::Work,
            "people" => Self::People,
            "plan" | "plans" => Self::Plan,
            "hunt" => Self::Hunt,
            "interest" | "interests" => Self::Interest,
            "food" => Self::Food,
            "health" => Self::Health,
            "style" => Self::Style,
            "routine" | "routines" => Self::Routine,
            "avoid" => Self::Avoid,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Several sources agree, or the member confirmed it.
    Sure,
    /// Inferred from what the sources show.
    #[default]
    Likely,
    /// Mentioned once and taken at the member's word.
    Said,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sure => "sure",
            Self::Likely => "likely",
            Self::Said => "said",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sure" => Some(Self::Sure),
            "likely" => Some(Self::Likely),
            "said" => Some(Self::Said),
            _ => None,
        }
    }

    fn weight(self) -> f64 {
        match self {
            Self::Sure => 3.0,
            Self::Likely => 2.0,
            Self::Said => 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSourceKind {
    Memory,
    Summary,
    Interest,
    Anchor,
    Answer,
    Interview,
    /// The member wrote the line themselves.
    You,
}

impl FactSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Summary => "summary",
            Self::Interest => "interest",
            Self::Anchor => "anchor",
            Self::Answer => "answer",
            Self::Interview => "interview",
            Self::You => "you",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "memory" => Some(Self::Memory),
            "summary" => Some(Self::Summary),
            "interest" => Some(Self::Interest),
            "anchor" => Some(Self::Anchor),
            "answer" => Some(Self::Answer),
            "interview" => Some(Self::Interview),
            "you" => Some(Self::You),
            _ => None,
        }
    }
}

/// Where a line came from: the kind of source and the row it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactSource {
    pub kind: FactSourceKind,
    #[serde(rename = "ref", default)]
    pub reference: Option<String>,
}

impl FactSource {
    pub fn new(kind: FactSourceKind, reference: Option<&str>) -> Self {
        Self {
            kind,
            reference: reference.map(str::to_string),
        }
    }

    pub fn you() -> Self {
        Self::new(FactSourceKind::You, None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FactState {
    #[default]
    Active,
    /// Lost its place to better lines; a later rebuild may bring it back.
    Retired,
    /// The member removed it; its key is never written again.
    Removed,
}

impl FactState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Removed => "removed",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "active" => Some(Self::Active),
            "retired" => Some(Self::Retired),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

/// One line of a member's fact sheet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FactLine {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub category: FactCategory,
    pub fact: String,
    pub confidence: Confidence,
    /// Since when it has held, in the member's or the model's own words.
    #[serde(default)]
    pub since: Option<String>,
    pub source: FactSource,
    /// Names of the pond's tools the line bears on.
    #[serde(default)]
    pub tools: Vec<String>,
    pub pinned: bool,
    pub score: f64,
    /// Times the line rode a turn or a brief.
    pub used: u32,
    pub state: FactState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl FactLine {
    /// A new active line; `None` when `fact` has no words.
    pub fn new(
        profile_id: &str,
        category: FactCategory,
        fact: &str,
        confidence: Confidence,
        source: FactSource,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            category,
            fact: clean_fact(fact)?,
            confidence,
            since: None,
            source,
            tools: Vec::new(),
            pinned: false,
            score: 0.0,
            used: 0,
            state: FactState::Active,
            created_at: now,
            updated_at: now,
        })
    }

    /// A line the member wrote: theirs, sure, and kept whatever a rebuild says.
    pub fn written_by_member(
        profile_id: &str,
        category: FactCategory,
        fact: &str,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        Self::new(
            profile_id,
            category,
            fact,
            Confidence::Sure,
            FactSource::you(),
            now,
        )
    }

    pub fn key(&self) -> String {
        fact_key(&self.fact)
    }

    /// Pinned or written by the member: a rebuild never retires or rewrites it.
    pub fn is_protected(&self) -> bool {
        self.pinned || self.source.kind == FactSourceKind::You
    }
}

/// A change to one line; only the fields present change.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct FactPatch {
    #[serde(default)]
    pub fact: Option<String>,
    #[serde(default)]
    pub category: Option<FactCategory>,
    #[serde(default)]
    pub pinned: Option<bool>,
}

impl FactPatch {
    /// The line with the patch applied; rewording a line makes it the member's own.
    pub fn apply(&self, line: &mut FactLine, now: DateTime<Utc>) {
        let reworded = self
            .fact
            .as_deref()
            .and_then(clean_fact)
            .filter(|fact| *fact != line.fact);
        if let Some(fact) = reworded {
            line.fact = fact;
            line.source = FactSource::you();
            line.confidence = Confidence::Sure;
        }
        if let Some(category) = self.category {
            line.category = category;
        }
        if let Some(pinned) = self.pinned {
            line.pinned = pinned;
        }
        line.updated_at = now;
    }
}

/// How much a line deserves its place: confidence first, then evidence, recency and use.
pub fn score(
    confidence: Confidence,
    evidence: u32,
    updated_at: DateTime<Utc>,
    used: u32,
    now: DateTime<Utc>,
) -> f64 {
    // One step of confidence outweighs everything below it put together (at most 11).
    const CONFIDENCE_STEP: f64 = 12.0;
    const HALF_LIFE_DAYS: f64 = 90.0;
    let age_days = (now - updated_at).num_hours().max(0) as f64 / 24.0;
    let recency = 3.0 * 0.5_f64.powf(age_days / HALF_LIFE_DAYS);
    confidence.weight() * CONFIDENCE_STEP
        + f64::from(evidence.min(6))
        + recency
        + f64::from(used.min(10)) * 0.2
}

/// Orders lines best first: pinned, then the member's own, then by score, newest breaking ties.
pub fn rank(lines: &mut [FactLine]) {
    lines.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then(b.is_protected().cmp(&a.is_protected()))
            .then(b.score.total_cmp(&a.score))
            .then(b.updated_at.cmp(&a.updated_at))
            .then(a.id.cmp(&b.id))
    });
}

/// `lines` with their states settled: every protected line active, then the best of the rest
/// up to `limit`, the others retired. Removed lines stay removed.
pub fn keep_best(mut lines: Vec<FactLine>, limit: usize) -> Vec<FactLine> {
    rank(&mut lines);
    let mut active = 0;
    for line in lines.iter_mut() {
        if line.state == FactState::Removed {
            continue;
        }
        if line.is_protected() || active < limit {
            line.state = FactState::Active;
            active += 1;
        } else {
            line.state = FactState::Retired;
        }
    }
    lines
}

/// A member's rows after a rebuild writes `incoming` over `existing`: a removed key is never
/// added back, a member's own line is left as written, a line the rebuild left out is retired
/// unless protected, and [`keep_best`] holds what is left to [`FACT_SHEET_LIMIT`].
pub fn rebuilt(
    existing: Vec<FactLine>,
    incoming: Vec<FactLine>,
    now: DateTime<Utc>,
) -> Vec<FactLine> {
    let mut rows = existing;
    let mut at: HashMap<String, usize> =
        rows.iter().enumerate().map(|(i, r)| (r.key(), i)).collect();
    let mut written = HashSet::new();
    for line in incoming {
        let key = line.key();
        if key.is_empty() || !written.insert(key.clone()) {
            continue;
        }
        match at.get(&key) {
            Some(&i) => {
                let row = &mut rows[i];
                if row.state == FactState::Removed || row.source.kind == FactSourceKind::You {
                    continue;
                }
                row.category = line.category;
                row.fact = line.fact;
                row.confidence = line.confidence;
                row.since = line.since;
                row.source = line.source;
                row.tools = line.tools;
                row.score = line.score;
                row.state = FactState::Active;
                row.updated_at = now;
            }
            None => {
                at.insert(key, rows.len());
                rows.push(FactLine {
                    state: FactState::Active,
                    ..line
                });
            }
        }
    }
    let (kept, mut left): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| {
        row.state != FactState::Removed && (row.is_protected() || written.contains(&row.key()))
    });
    for row in left.iter_mut() {
        if row.state == FactState::Active {
            row.state = FactState::Retired;
            row.updated_at = now;
        }
    }
    let mut rows = keep_best(kept, FACT_SHEET_LIMIT);
    rows.extend(left);
    rows
}

/// A member's rows after one line is written by its key, with the limit held; `None` when the
/// member removed that key. A line the member wrote is rewritten only by the member.
pub fn with_line(
    existing: Vec<FactLine>,
    line: FactLine,
    now: DateTime<Utc>,
) -> Option<(Vec<FactLine>, FactLine)> {
    let key = line.key();
    let mut rows = existing;
    let stored = match rows.iter_mut().find(|r| r.key() == key) {
        Some(row) if row.state == FactState::Removed => return None,
        Some(row)
            if row.source.kind == FactSourceKind::You
                && line.source.kind != FactSourceKind::You =>
        {
            row.clone()
        }
        Some(row) => {
            row.category = line.category;
            row.fact = line.fact;
            row.confidence = line.confidence;
            row.since = line.since.or(row.since.take());
            row.source = line.source;
            row.tools = line.tools;
            row.score = line.score;
            row.pinned = row.pinned || line.pinned;
            row.state = FactState::Active;
            row.updated_at = now;
            row.clone()
        }
        None => {
            let row = FactLine {
                state: FactState::Active,
                ..line
            };
            rows.push(row.clone());
            row
        }
    };
    let (sheet, rest): (Vec<_>, Vec<_>) =
        rows.into_iter().partition(|r| r.state == FactState::Active);
    let mut rows = keep_best(sheet, FACT_SHEET_LIMIT);
    rows.extend(rest);
    let stored = rows
        .iter()
        .find(|r| r.id == stored.id)
        .cloned()
        .unwrap_or(stored);
    Some((rows, stored))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn line(fact: &str, score: f64) -> FactLine {
        let mut line = FactLine::new(
            "jerry",
            FactCategory::Work,
            fact,
            Confidence::Likely,
            FactSource::new(FactSourceKind::Memory, Some("m1")),
            now(),
        )
        .unwrap();
        line.score = score;
        line
    }

    fn active(rows: &[FactLine]) -> Vec<&str> {
        rows.iter()
            .filter(|r| r.state == FactState::Active)
            .map(|r| r.fact.as_str())
            .collect()
    }

    #[test]
    fn two_wordings_of_one_fact_share_a_key_and_an_empty_fact_is_no_line() {
        assert_eq!(fact_key("Runs Jarida."), fact_key("runs  jarida"));
        assert!(FactLine::written_by_member("jerry", FactCategory::Work, " ... ", now()).is_none());
    }

    #[test]
    fn confidence_outweighs_evidence_recency_and_use_together() {
        let fresh = now();
        let old = now() - Duration::days(400);
        let weak_but_busy = score(Confidence::Likely, 50, fresh, 50, now());
        let sure_but_bare = score(Confidence::Sure, 0, old, 0, now());
        assert!(sure_but_bare > weak_but_busy);
        assert!(
            score(Confidence::Said, 3, fresh, 0, now())
                > score(Confidence::Said, 1, fresh, 0, now())
        );
        assert!(
            score(Confidence::Said, 1, fresh, 0, now()) > score(Confidence::Said, 1, old, 0, now())
        );
        assert!(
            score(Confidence::Said, 1, fresh, 4, now())
                > score(Confidence::Said, 1, fresh, 0, now())
        );
    }

    #[test]
    fn the_best_lines_stay_and_pinned_and_member_written_lines_always_do() {
        let mut pinned = line("Allergic to peanuts", 0.0);
        pinned.pinned = true;
        let own =
            FactLine::written_by_member("jerry", FactCategory::Home, "Lives in Kilimani", now())
                .unwrap();
        let rows = keep_best(
            vec![
                line("Weak guess", 1.0),
                line("Strong fact", 9.0),
                line("Middling fact", 5.0),
                pinned,
                own,
            ],
            3,
        );
        assert_eq!(
            active(&rows),
            ["Allergic to peanuts", "Lives in Kilimani", "Strong fact"]
        );
        let retired: Vec<&str> = rows
            .iter()
            .filter(|r| r.state == FactState::Retired)
            .map(|r| r.fact.as_str())
            .collect();
        assert_eq!(retired, ["Middling fact", "Weak guess"]);
    }

    #[test]
    fn protected_lines_past_the_limit_are_all_kept_and_nothing_else_is() {
        let mut rows: Vec<FactLine> = (0..4)
            .map(|i| {
                FactLine::written_by_member(
                    "jerry",
                    FactCategory::Other,
                    &format!("own {i}"),
                    now(),
                )
                .unwrap()
            })
            .collect();
        rows.push(line("Guess", 99.0));
        let rows = keep_best(rows, 3);
        assert_eq!(active(&rows).len(), 4);
        assert!(!active(&rows).contains(&"Guess"));
    }

    #[test]
    fn a_rebuild_never_adds_back_a_removed_key_or_rewrites_the_members_own_line() {
        let mut removed = line("Likes duck recipes", 1.0);
        removed.state = FactState::Removed;
        let own =
            FactLine::written_by_member("jerry", FactCategory::Work, "Runs Jarida", now()).unwrap();
        let dropped = line("Asked about rain once", 4.0);
        let later = now() + Duration::days(1);
        let rows = rebuilt(
            vec![removed, own.clone(), dropped],
            vec![
                line("likes duck recipes!", 9.0),
                line("runs jarida", 9.0),
                line("Building a talk for December", 6.0),
            ],
            later,
        );
        assert_eq!(
            active(&rows),
            ["Runs Jarida", "Building a talk for December"]
        );
        let kept = rows.iter().find(|r| r.id == own.id).unwrap();
        assert_eq!(kept.source.kind, FactSourceKind::You);
        assert_eq!(kept.updated_at, own.updated_at);
        let duck = rows
            .iter()
            .find(|r| r.key() == "likes duck recipes")
            .unwrap();
        assert_eq!(duck.state, FactState::Removed);
        let rain = rows
            .iter()
            .find(|r| r.fact == "Asked about rain once")
            .unwrap();
        assert_eq!(rain.state, FactState::Retired, "left out of the rebuild");
    }

    #[test]
    fn a_rebuild_holds_the_sheet_to_its_limit() {
        let incoming: Vec<FactLine> = (0..FACT_SHEET_LIMIT + 20)
            .map(|i| line(&format!("fact number {i}"), i as f64))
            .collect();
        let rows = rebuilt(Vec::new(), incoming, now());
        assert_eq!(active(&rows).len(), FACT_SHEET_LIMIT);
        assert!(active(&rows).contains(&"fact number 119"));
        assert!(!active(&rows).contains(&"fact number 0"));
    }

    #[test]
    fn one_written_line_updates_its_key_and_is_refused_when_removed() {
        let first = line("Drives a Fielder", 2.0);
        let (rows, stored) = with_line(Vec::new(), first.clone(), now()).unwrap();
        assert_eq!(stored.id, first.id);
        let (rows, stored) = with_line(rows, line("drives a fielder", 7.0), now()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(stored.id, first.id, "the row keeps its id");
        assert_eq!(stored.score, 7.0);
        let mut gone = rows;
        gone[0].state = FactState::Removed;
        assert!(with_line(gone, line("Drives a Fielder", 1.0), now()).is_none());
    }

    #[test]
    fn a_line_the_member_wrote_is_rewritten_only_by_the_member() {
        let own =
            FactLine::written_by_member("jerry", FactCategory::Home, "Lives in Kilimani", now())
                .unwrap();
        let (rows, _) = with_line(Vec::new(), own.clone(), now()).unwrap();
        let (rows, stored) = with_line(rows, line("lives in kilimani", 9.0), now()).unwrap();
        assert_eq!(stored, own, "a guess does not overwrite it");
        let moved =
            FactLine::written_by_member("jerry", FactCategory::Home, "Lives in  Kilimani!", now())
                .unwrap();
        let (_, stored) = with_line(rows, moved, now()).unwrap();
        assert_eq!(stored.fact, "Lives in Kilimani!");
        assert_eq!(stored.id, own.id);
    }

    #[test]
    fn rewording_a_line_makes_it_the_members_own() {
        let mut row = line("Works at a bank", 3.0);
        FactPatch {
            fact: Some("Works at Jarida".into()),
            pinned: Some(true),
            ..Default::default()
        }
        .apply(&mut row, now());
        assert_eq!(row.fact, "Works at Jarida");
        assert_eq!(row.source.kind, FactSourceKind::You);
        assert!(row.pinned && row.is_protected());
    }
}
