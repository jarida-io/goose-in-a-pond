//! Anchors: real things in a member's life (an event, a trip, a purchase, a project, an
//! interest), the facts they hold and the deliverables that hang off them until they end.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::interest::topic_key;
use super::specialist::{owner_of, SpecialistId};

/// A candidate the member has not confirmed in this long fades.
pub const CANDIDATE_FADES_AFTER: Duration = Duration::days(7);
/// An anchor ends this long after its end has passed.
pub const ENDS_AFTER: Duration = Duration::days(1);
const MAX_TITLE_CHARS: usize = 120;

/// The key two spellings of one title share.
pub fn title_key(title: &str) -> String {
    topic_key(title)
}

/// A title as stored: one line, bounded. `None` when nothing is left.
pub fn clean_title(raw: &str) -> Option<String> {
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if title_key(&one_line).is_empty() {
        return None;
    }
    Some(one_line.chars().take(MAX_TITLE_CHARS).collect())
}

/// A state move the object's current state does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalMove {
    pub what: &'static str,
    pub from: &'static str,
    pub to: &'static str,
}

impl std::fmt::Display for IllegalMove {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} cannot go from {} to {}",
            self.what, self.from, self.to
        )
    }
}

impl std::error::Error for IllegalMove {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorKind {
    Event,
    Trip,
    Purchase,
    Project,
    Interest,
    /// Has no end: the everyday anchor, or a specialist's own.
    Standing,
}

impl AnchorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Trip => "trip",
            Self::Purchase => "purchase",
            Self::Project => "project",
            Self::Interest => "interest",
            Self::Standing => "standing",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "event" => Some(Self::Event),
            "trip" => Some(Self::Trip),
            "purchase" => Some(Self::Purchase),
            "project" => Some(Self::Project),
            "interest" => Some(Self::Interest),
            "standing" => Some(Self::Standing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorState {
    /// Offered to the member; nothing is made for it yet.
    Candidate,
    Active,
    /// Its end passed.
    Ended,
    /// The member stopped it.
    Stopped,
    /// The member said no, or never answered.
    Faded,
}

impl AnchorState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active => "active",
            Self::Ended => "ended",
            Self::Stopped => "stopped",
            Self::Faded => "faded",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "candidate" => Some(Self::Candidate),
            "active" => Some(Self::Active),
            "ended" => Some(Self::Ended),
            "stopped" => Some(Self::Stopped),
            "faded" => Some(Self::Faded),
            _ => None,
        }
    }

    /// The states a member's anchor list shows.
    pub const LIVE: [AnchorState; 2] = [Self::Candidate, Self::Active];
}

/// Where an anchor came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnchorOrigin {
    /// A plan the member stated in a conversation.
    Chat,
    /// A plan read from a screenshot or photo.
    Capture,
    /// An interest the member confirmed.
    Interest,
    /// The member added it.
    You,
    /// The member asked Goose to plan or track it.
    Goose,
}

impl AnchorOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Capture => "capture",
            Self::Interest => "interest",
            Self::You => "you",
            Self::Goose => "goose",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "chat" => Some(Self::Chat),
            "capture" => Some(Self::Capture),
            "interest" => Some(Self::Interest),
            "you" => Some(Self::You),
            "goose" => Some(Self::Goose),
            _ => None,
        }
    }

    /// Something Goose noticed waits for the member's yes; something they asked for does not.
    pub fn starts_as(self) -> AnchorState {
        match self {
            Self::Chat | Self::Capture => AnchorState::Candidate,
            Self::Interest | Self::You | Self::Goose => AnchorState::Active,
        }
    }
}

/// One fact an anchor or a deliverable holds, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorFact {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub source: String,
}

impl AnchorFact {
    pub fn new(label: &str, value: &str, source: &str) -> Self {
        Self {
            label: label.trim().to_string(),
            value: value.trim().to_string(),
            source: source.trim().to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub id: String,
    /// The member who owns it.
    pub profile_id: String,
    pub kind: AnchorKind,
    pub title: String,
    pub state: AnchorState,
    #[serde(default)]
    pub starts_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub place: Option<String>,
    #[serde(default)]
    pub facts: Vec<AnchorFact>,
    /// The specialist whose playbook its briefs carry.
    #[serde(default)]
    pub agent: Option<SpecialistId>,
    pub origin: AnchorOrigin,
    /// The conversation, capture or interest it came from.
    #[serde(default)]
    pub origin_ref: Option<String>,
    /// One sentence on why Goose offers it.
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub offered_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub confirmed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The other members who may read it.
    #[serde(default)]
    pub shared_with: Vec<String>,
}

impl Anchor {
    /// A new anchor in the state its origin starts in; `None` when `title` has no words.
    pub fn new(
        profile_id: &str,
        kind: AnchorKind,
        title: &str,
        origin: AnchorOrigin,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        let state = origin.starts_as();
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            kind,
            title: clean_title(title)?,
            state,
            starts_at: None,
            ends_at: None,
            place: None,
            facts: Vec::new(),
            agent: owner_of(kind),
            origin,
            origin_ref: None,
            why: String::new(),
            offered_at: None,
            confirmed_at: (state == AnchorState::Active).then_some(now),
            ended_at: None,
            created_at: now,
            updated_at: now,
            shared_with: Vec::new(),
        })
    }

    pub fn key(&self) -> String {
        title_key(&self.title)
    }

    pub fn is_readable_by(&self, profile_id: &str) -> bool {
        self.profile_id == profile_id || self.shared_with.iter().any(|m| m == profile_id)
    }

    /// Sets a fact by its label, replacing the one already held under it.
    pub fn set_fact(&mut self, fact: AnchorFact, now: DateTime<Utc>) {
        let key = topic_key(&fact.label);
        match self.facts.iter_mut().find(|f| topic_key(&f.label) == key) {
            Some(held) => *held = fact,
            None => self.facts.push(fact),
        }
        self.updated_at = now;
    }

    fn go(
        &mut self,
        from: &[AnchorState],
        to: AnchorState,
        now: DateTime<Utc>,
    ) -> Result<(), IllegalMove> {
        if !from.contains(&self.state) {
            return Err(IllegalMove {
                what: "an anchor",
                from: self.state.as_str(),
                to: to.as_str(),
            });
        }
        self.state = to;
        self.updated_at = now;
        Ok(())
    }

    /// The member's yes to a candidate.
    pub fn confirm(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.go(&[AnchorState::Candidate], AnchorState::Active, now)?;
        self.confirmed_at = Some(now);
        Ok(())
    }

    /// The member's no to a candidate.
    pub fn dismiss(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.fade(now)
    }

    /// A candidate nobody answered.
    pub fn fade(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.go(&[AnchorState::Candidate], AnchorState::Faded, now)?;
        self.ended_at = Some(now);
        Ok(())
    }

    /// The member stops an active anchor.
    pub fn stop(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.go(&[AnchorState::Active], AnchorState::Stopped, now)?;
        self.ended_at = Some(now);
        Ok(())
    }

    /// An active anchor whose end has passed.
    pub fn end(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.go(&[AnchorState::Active], AnchorState::Ended, now)?;
        self.ended_at = Some(now);
        Ok(())
    }

    /// A candidate old enough to fade.
    pub fn should_fade(&self, now: DateTime<Utc>) -> bool {
        self.state == AnchorState::Candidate && now - self.created_at >= CANDIDATE_FADES_AFTER
    }

    /// When it is over: its end, or its start when it has no end. `None` for one with no date.
    pub fn over_at(&self) -> Option<DateTime<Utc>> {
        self.ends_at.or(self.starts_at)
    }

    /// An active anchor whose end passed [`ENDS_AFTER`] ago.
    pub fn should_end(&self, now: DateTime<Utc>) -> bool {
        self.state == AnchorState::Active
            && self.over_at().is_some_and(|over| now - over >= ENDS_AFTER)
    }
}

/// Orders anchors as a member's list shows them: dated ones nearest first, then the undated,
/// newest first.
pub fn sort_anchors(anchors: &mut [Anchor]) {
    anchors.sort_by(|a, b| {
        a.starts_at
            .is_none()
            .cmp(&b.starts_at.is_none())
            .then(a.starts_at.cmp(&b.starts_at))
            .then(b.created_at.cmp(&a.created_at))
            .then(a.id.cmp(&b.id))
    });
}

/// How long until an anchor's deliverable is next refreshed, by how near the anchor is.
pub fn refresh_due_after(anchor_start: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Duration {
    let Some(start) = anchor_start else {
        return Duration::days(7);
    };
    let away = start - now;
    if away > Duration::days(14) {
        Duration::days(7)
    } else if away > Duration::days(3) {
        Duration::days(3)
    } else {
        Duration::days(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverableKind {
    Dossier,
    Plan,
    Shortlist,
    Digest,
    Checklist,
}

impl DeliverableKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dossier => "dossier",
            Self::Plan => "plan",
            Self::Shortlist => "shortlist",
            Self::Digest => "digest",
            Self::Checklist => "checklist",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "dossier" => Some(Self::Dossier),
            "plan" => Some(Self::Plan),
            "shortlist" => Some(Self::Shortlist),
            "digest" => Some(Self::Digest),
            "checklist" => Some(Self::Checklist),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverableState {
    Queued,
    Making,
    Ready,
    /// Shown with the critic's objections: the time or the rounds ran out.
    Unsure,
    Rejected,
    Archived,
}

impl DeliverableState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Making => "making",
            Self::Ready => "ready",
            Self::Unsure => "unsure",
            Self::Rejected => "rejected",
            Self::Archived => "archived",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "queued" => Some(Self::Queued),
            "making" => Some(Self::Making),
            "ready" => Some(Self::Ready),
            "unsure" => Some(Self::Unsure),
            "rejected" => Some(Self::Rejected),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    /// Whether the member can read a body in this state.
    pub fn is_shown(self) -> bool {
        matches!(self, Self::Ready | Self::Unsure)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectionKind {
    Grounding,
    Relevance,
    Duplicate,
    DontList,
    Vague,
    Stale,
}

impl ObjectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grounding => "grounding",
            Self::Relevance => "relevance",
            Self::Duplicate => "duplicate",
            Self::DontList => "dont_list",
            Self::Vague => "vague",
            Self::Stale => "stale",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "grounding" => Some(Self::Grounding),
            "relevance" => Some(Self::Relevance),
            "duplicate" => Some(Self::Duplicate),
            "dont_list" => Some(Self::DontList),
            "vague" => Some(Self::Vague),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }
}

/// One thing a critic holds against a draft, and what would make it pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Objection {
    pub kind: ObjectionKind,
    #[serde(default)]
    pub about: String,
    #[serde(default)]
    pub fix: String,
}

impl Objection {
    pub fn new(kind: ObjectionKind, about: &str, fix: &str) -> Self {
        Self {
            kind,
            about: about.trim().to_string(),
            fix: fix.trim().to_string(),
        }
    }
}

/// A page a deliverable was built on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLink {
    #[serde(default)]
    pub title: String,
    pub url: String,
}

/// What a job hands a deliverable when it settles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeliverableDraft {
    /// One sentence that says why it is worth opening.
    pub hook: String,
    pub body: String,
    pub facts: Vec<AnchorFact>,
    pub sources: Vec<SourceLink>,
}

/// Finished work for one recipe slot of an anchor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Deliverable {
    pub id: String,
    pub anchor_id: String,
    pub kind: DeliverableKind,
    pub slot: String,
    pub title: String,
    pub state: DeliverableState,
    #[serde(default)]
    pub hook: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub facts: Vec<AnchorFact>,
    #[serde(default)]
    pub sources: Vec<SourceLink>,
    #[serde(default)]
    pub objections: Vec<Objection>,
    /// What the last refresh changed; empty when it changed nothing.
    #[serde(default)]
    pub changed: String,
    pub version: u32,
    /// The long task that made, or is making, this version.
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub refresh_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub seen_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Deliverable {
    /// A queued deliverable for `slot`; `None` when `title` has no words.
    pub fn new(
        anchor_id: &str,
        kind: DeliverableKind,
        slot: &str,
        title: &str,
        now: DateTime<Utc>,
    ) -> Option<Self> {
        Some(Self {
            id: uuid::Uuid::new_v4().to_string(),
            anchor_id: anchor_id.to_string(),
            kind,
            slot: slot.trim().to_string(),
            title: clean_title(title)?,
            state: DeliverableState::Queued,
            hook: String::new(),
            body: String::new(),
            facts: Vec::new(),
            sources: Vec::new(),
            objections: Vec::new(),
            changed: String::new(),
            version: 0,
            task_id: None,
            refresh_at: None,
            seen_at: None,
            created_at: now,
            updated_at: now,
        })
    }

    fn go(
        &mut self,
        from: &[DeliverableState],
        to: DeliverableState,
        now: DateTime<Utc>,
    ) -> Result<(), IllegalMove> {
        if !from.contains(&self.state) {
            return Err(IllegalMove {
                what: "a deliverable",
                from: self.state.as_str(),
                to: to.as_str(),
            });
        }
        self.state = to;
        self.updated_at = now;
        Ok(())
    }

    fn take(&mut self, draft: DeliverableDraft) {
        self.hook = draft.hook.trim().to_string();
        self.body = draft.body;
        self.facts = draft.facts;
        self.sources = draft.sources;
        self.version += 1;
    }

    /// A job begins a first version or a refresh; one already being made is refused.
    pub fn start_making(&mut self, task_id: &str, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.go(
            &[
                DeliverableState::Queued,
                DeliverableState::Ready,
                DeliverableState::Unsure,
            ],
            DeliverableState::Making,
            now,
        )?;
        self.task_id = Some(task_id.to_string());
        Ok(())
    }

    /// The critic passed the draft. `true` when the member should be told: a first version, or
    /// a refresh that changed something.
    pub fn settle_ready(
        &mut self,
        draft: DeliverableDraft,
        changed: &str,
        refresh_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<bool, IllegalMove> {
        self.go(&[DeliverableState::Making], DeliverableState::Ready, now)?;
        let first = self.version == 0;
        self.take(draft);
        self.objections.clear();
        self.changed = changed.trim().to_string();
        self.refresh_at = refresh_at;
        let news = first || !self.changed.is_empty();
        if news {
            self.seen_at = None;
        }
        Ok(news)
    }

    /// The time or the rounds ran out: the draft is shown with what the critic still held
    /// against it.
    pub fn settle_unsure(
        &mut self,
        draft: DeliverableDraft,
        objections: Vec<Objection>,
        refresh_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Result<(), IllegalMove> {
        self.go(&[DeliverableState::Making], DeliverableState::Unsure, now)?;
        self.take(draft);
        self.objections = objections;
        self.changed.clear();
        self.refresh_at = refresh_at;
        self.seen_at = None;
        Ok(())
    }

    /// The job making it failed or was cancelled: back to what the member last had.
    pub fn release(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        let back = if self.version == 0 {
            DeliverableState::Queued
        } else if self.objections.is_empty() {
            DeliverableState::Ready
        } else {
            DeliverableState::Unsure
        };
        self.go(&[DeliverableState::Making], back, now)
    }

    /// The member turned down what they were shown.
    pub fn reject(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.go(
            &[DeliverableState::Ready, DeliverableState::Unsure],
            DeliverableState::Rejected,
            now,
        )
    }

    /// Its anchor ended; the body is kept.
    pub fn archive(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.go(
            &[
                DeliverableState::Queued,
                DeliverableState::Making,
                DeliverableState::Ready,
                DeliverableState::Unsure,
                DeliverableState::Rejected,
            ],
            DeliverableState::Archived,
            now,
        )?;
        self.refresh_at = None;
        Ok(())
    }

    pub fn mark_seen(&mut self, now: DateTime<Utc>) {
        if self.seen_at.is_none() {
            self.seen_at = Some(now);
        }
    }

    /// Shown to the member and not yet opened.
    pub fn is_unseen(&self) -> bool {
        self.state.is_shown() && self.seen_at.is_none()
    }

    /// Shown to the member, with a refresh time that has come.
    pub fn refresh_is_due(&self, now: DateTime<Utc>) -> bool {
        self.state.is_shown() && self.refresh_at.is_some_and(|at| at <= now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn anchor(origin: AnchorOrigin) -> Anchor {
        Anchor::new(
            "jerry",
            AnchorKind::Event,
            "AI Summit Nairobi",
            origin,
            now(),
        )
        .unwrap()
    }

    fn deliverable() -> Deliverable {
        Deliverable::new(
            "a1",
            DeliverableKind::Dossier,
            "dossier",
            "Summit dossier",
            now(),
        )
        .unwrap()
    }

    fn draft(body: &str) -> DeliverableDraft {
        DeliverableDraft {
            hook: "Three sessions fit you".into(),
            body: body.into(),
            facts: vec![AnchorFact::new("Venue", "KICC", "kicc.co.ke")],
            sources: vec![SourceLink {
                title: "Programme".into(),
                url: "https://example.org/programme".into(),
            }],
        }
    }

    #[test]
    fn what_goose_noticed_is_a_candidate_and_what_the_member_asked_for_is_active() {
        assert_eq!(anchor(AnchorOrigin::Chat).state, AnchorState::Candidate);
        assert_eq!(anchor(AnchorOrigin::Capture).state, AnchorState::Candidate);
        let asked = anchor(AnchorOrigin::Goose);
        assert_eq!(asked.state, AnchorState::Active);
        assert_eq!(asked.confirmed_at, Some(now()));
        assert_eq!(asked.agent, Some(SpecialistId::Trip));
        assert!(Anchor::new("jerry", AnchorKind::Trip, " - ", AnchorOrigin::You, now()).is_none());
    }

    #[test]
    fn a_candidate_is_confirmed_or_fades_and_an_active_anchor_is_stopped_or_ends() {
        let mut offered = anchor(AnchorOrigin::Chat);
        assert!(offered.stop(now()).is_err());
        assert!(offered.end(now()).is_err());
        offered.confirm(now()).unwrap();
        assert_eq!(offered.state, AnchorState::Active);
        assert!(offered.confirm(now()).is_err(), "already confirmed");
        assert!(offered.dismiss(now()).is_err());
        assert!(offered.fade(now()).is_err());
        offered.stop(now()).unwrap();
        assert_eq!(offered.state, AnchorState::Stopped);
        assert_eq!(
            offered.end(now()).unwrap_err().to_string(),
            "an anchor cannot go from stopped to ended"
        );

        let mut declined = anchor(AnchorOrigin::Capture);
        declined.dismiss(now()).unwrap();
        assert_eq!(declined.state, AnchorState::Faded);
        assert!(declined.confirm(now()).is_err());

        let mut over = anchor(AnchorOrigin::You);
        over.end(now()).unwrap();
        assert_eq!(over.state, AnchorState::Ended);
        assert!(over.stop(now()).is_err());
    }

    #[test]
    fn a_candidate_fades_after_a_week_and_an_anchor_ends_a_day_after_its_end() {
        let offered = anchor(AnchorOrigin::Chat);
        assert!(!offered.should_fade(now() + Duration::days(6)));
        assert!(offered.should_fade(now() + CANDIDATE_FADES_AFTER));

        let mut event = anchor(AnchorOrigin::You);
        assert!(
            !event.should_end(now() + Duration::days(400)),
            "no date, no end"
        );
        event.starts_at = Some(now() + Duration::days(2));
        assert!(!event.should_end(now() + Duration::days(2) + Duration::hours(23)));
        assert!(event.should_end(now() + Duration::days(3)));
        event.ends_at = Some(now() + Duration::days(5));
        assert!(
            !event.should_end(now() + Duration::days(3)),
            "its end, not its start"
        );
        assert!(event.should_end(now() + Duration::days(6)));
    }

    #[test]
    fn a_refresh_comes_sooner_the_nearer_the_anchor_is() {
        let at = |days: i64| Some(now() + Duration::days(days));
        assert_eq!(refresh_due_after(None, now()), Duration::days(7));
        assert_eq!(refresh_due_after(at(30), now()), Duration::days(7));
        assert_eq!(refresh_due_after(at(14), now()), Duration::days(3));
        assert_eq!(refresh_due_after(at(4), now()), Duration::days(3));
        assert_eq!(refresh_due_after(at(3), now()), Duration::days(1));
        assert_eq!(refresh_due_after(at(-1), now()), Duration::days(1));
    }

    #[test]
    fn a_deliverable_is_made_then_settled_and_refuses_any_other_order() {
        let mut d = deliverable();
        assert!(d.settle_ready(draft("x"), "", None, now()).is_err());
        assert!(d.reject(now()).is_err(), "nothing shown yet");
        d.start_making("engine-1", now()).unwrap();
        assert!(
            d.start_making("engine-2", now()).is_err(),
            "one job at a time"
        );
        assert_eq!(d.task_id.as_deref(), Some("engine-1"));
        let told = d
            .settle_ready(draft("v1"), "", Some(now() + Duration::days(7)), now())
            .unwrap();
        assert!(told, "a first version is news");
        assert_eq!((d.state, d.version), (DeliverableState::Ready, 1));
        assert!(d.is_unseen());
        d.mark_seen(now());

        d.start_making("engine-3", now()).unwrap();
        assert!(!d.settle_ready(draft("v1"), " ", None, now()).unwrap());
        assert!(!d.is_unseen(), "an unchanged refresh tells nobody");
        d.start_making("engine-4", now()).unwrap();
        assert!(d
            .settle_ready(draft("v3"), "Two new sessions", None, now())
            .unwrap());
        assert!(d.is_unseen());
        assert_eq!(d.version, 3);

        d.reject(now()).unwrap();
        assert!(
            d.start_making("engine-5", now()).is_err(),
            "rejected stays rejected"
        );
        d.archive(now()).unwrap();
        assert!(d.archive(now()).is_err());
    }

    #[test]
    fn an_unsure_deliverable_shows_its_objections_and_a_pass_clears_them() {
        let mut d = deliverable();
        d.start_making("engine-1", now()).unwrap();
        let held = vec![Objection::new(
            ObjectionKind::Grounding,
            "the keynote time",
            "cite the programme",
        )];
        d.settle_unsure(draft("v1"), held.clone(), None, now())
            .unwrap();
        assert_eq!(d.state, DeliverableState::Unsure);
        assert_eq!(d.objections, held);
        assert!(d.is_unseen());
        d.start_making("engine-2", now()).unwrap();
        d.settle_ready(draft("v2"), "Keynote time cited", None, now())
            .unwrap();
        assert!(d.objections.is_empty());
    }

    #[test]
    fn a_job_that_dies_gives_the_deliverable_back_as_the_member_last_had_it() {
        let mut fresh = deliverable();
        fresh.start_making("engine-1", now()).unwrap();
        fresh.release(now()).unwrap();
        assert_eq!(fresh.state, DeliverableState::Queued);
        assert!(fresh.release(now()).is_err());

        let mut ready = deliverable();
        ready.start_making("engine-1", now()).unwrap();
        ready.settle_ready(draft("v1"), "", None, now()).unwrap();
        ready.start_making("engine-2", now()).unwrap();
        ready.release(now()).unwrap();
        assert_eq!(ready.state, DeliverableState::Ready);
        assert_eq!(ready.body, "v1");
    }

    #[test]
    fn an_anchor_is_read_by_its_owner_and_the_members_it_is_shared_with() {
        let mut trip = anchor(AnchorOrigin::You);
        assert!(trip.is_readable_by("jerry"));
        assert!(!trip.is_readable_by("liz"));
        trip.shared_with.push("liz".into());
        assert!(trip.is_readable_by("liz"));
    }

    #[test]
    fn a_fact_set_twice_under_one_label_is_held_once() {
        let mut trip = anchor(AnchorOrigin::You);
        trip.set_fact(AnchorFact::new("Budget", "KSh 80,000", "answer"), now());
        trip.set_fact(AnchorFact::new("budget", "KSh 120,000", "answer"), now());
        assert_eq!(trip.facts.len(), 1);
        assert_eq!(trip.facts[0].value, "KSh 120,000");
    }

    #[test]
    fn a_critics_objection_reads_from_its_json() {
        let read: Objection = serde_json::from_str(
            r#"{"kind": "dont_list", "about": "the duck recipe", "fix": "drop it"}"#,
        )
        .unwrap();
        assert_eq!(read.kind, ObjectionKind::DontList);
        assert_eq!(
            ObjectionKind::parse("dont_list"),
            Some(ObjectionKind::DontList)
        );
    }
}
