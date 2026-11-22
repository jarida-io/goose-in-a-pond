//! The heartbeat's intentions: do one thing once a condition holds, until a deadline. Code
//! checks the condition; a met intention is carried out and cleared, a late one is cleared.

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::anchor::IllegalMove;
use super::interest::topic_key;
use crate::shared::domain::todo::Transition;

const MAX_TEXT_CHARS: usize = 300;

/// The key two wordings of one intention share.
pub fn text_key(text: &str) -> String {
    topic_key(text)
}

/// What Goose does when the condition holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentAct {
    /// Tell the member now.
    Push,
    /// Queue the anchor's deliverable again.
    Refresh,
    /// Look something up and tell the member.
    LookInto,
    /// Push the anchor's open questions.
    Ask,
    /// Offer a to-do the member adds or declines.
    ProposeTodo,
}

impl IntentAct {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Refresh => "refresh",
            Self::LookInto => "look_into",
            Self::Ask => "ask",
            Self::ProposeTodo => "propose_todo",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "push" => Some(Self::Push),
            "refresh" => Some(Self::Refresh),
            "look_into" => Some(Self::LookInto),
            "ask" => Some(Self::Ask),
            "propose_todo" => Some(Self::ProposeTodo),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConditionKind {
    Time,
    Place,
    Thing,
    Chat,
}

impl ConditionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Time => "time",
            Self::Place => "place",
            Self::Thing => "thing",
            Self::Chat => "chat",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "time" => Some(Self::Time),
            "place" => Some(Self::Place),
            "thing" => Some(Self::Thing),
            "chat" => Some(Self::Chat),
            _ => None,
        }
    }
}

/// What must hold before an intention is carried out; each is something code can check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// The time has come.
    Time { at: DateTime<Utc> },
    /// The member's phone reports crossing one of their saved places.
    Place {
        place_id: String,
        transition: Transition,
    },
    /// A camera, sensor or device from the hook catalogue raises an event.
    Thing { source_id: String, event: String },
    /// The member's next turn.
    Chat,
}

impl Condition {
    pub fn kind(&self) -> ConditionKind {
        match self {
            Self::Time { .. } => ConditionKind::Time,
            Self::Place { .. } => ConditionKind::Place,
            Self::Thing { .. } => ConditionKind::Thing,
            Self::Chat => ConditionKind::Chat,
        }
    }

    /// The kind's spec as `condition_json` stores it.
    pub fn spec(&self) -> Value {
        match self {
            Self::Time { at } => json!({"at": at.to_rfc3339_opts(SecondsFormat::Secs, true)}),
            Self::Place {
                place_id,
                transition,
            } => json!({"place_id": place_id, "transition": transition.as_str()}),
            Self::Thing { source_id, event } => json!({"source_id": source_id, "event": event}),
            Self::Chat => json!({}),
        }
    }

    /// Reads a stored condition; `None` when the spec lacks what its kind needs.
    pub fn from_stored(kind: ConditionKind, spec: &Value) -> Option<Self> {
        let text = |key: &str| {
            spec.get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        };
        match kind {
            ConditionKind::Time => {
                let at = DateTime::parse_from_rfc3339(text("at")?).ok()?;
                Some(Self::Time {
                    at: at.with_timezone(&Utc),
                })
            }
            ConditionKind::Place => Some(Self::Place {
                place_id: text("place_id")?.to_string(),
                transition: Transition::parse(text("transition")?)?,
            }),
            ConditionKind::Thing => Some(Self::Thing {
                source_id: text("source_id")?.to_string(),
                event: text("event")?.to_string(),
            }),
            ConditionKind::Chat => Some(Self::Chat),
        }
    }

    /// Whether a place crossing the member's phone reported meets it.
    pub fn is_met_by_crossing(&self, place_id: &str, transition: Transition) -> bool {
        matches!(self, Self::Place { place_id: p, transition: t } if p == place_id && *t == transition)
    }

    /// Whether an event a source raised meets it.
    pub fn is_met_by_event(&self, source_id: &str, event: &str) -> bool {
        matches!(self, Self::Thing { source_id: s, event: e } if s == source_id && e == event)
    }
}

impl Serialize for Condition {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut spec = self.spec();
        spec["kind"] = json!(self.kind().as_str());
        spec.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Condition {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spec = Value::deserialize(deserializer)?;
        spec.get("kind")
            .and_then(Value::as_str)
            .and_then(ConditionKind::parse)
            .and_then(|kind| Self::from_stored(kind, &spec))
            .ok_or_else(|| serde::de::Error::custom("not a condition code can check"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentionState {
    Waiting,
    /// Its condition held and it was carried out.
    Done,
    /// Its deadline passed first.
    Expired,
    /// The member, or a later review, withdrew it.
    Dropped,
}

impl IntentionState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Done => "done",
            Self::Expired => "expired",
            Self::Dropped => "dropped",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "waiting" => Some(Self::Waiting),
            "done" => Some(Self::Done),
            "expired" => Some(Self::Expired),
            "dropped" => Some(Self::Dropped),
            _ => None,
        }
    }
}

/// Why an intention was not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentionRefused {
    NoText,
    /// The deadline is not after now.
    DeadlinePassed,
    /// A time condition that falls after the deadline can never hold in time.
    ConditionAfterDeadline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intention {
    pub id: String,
    #[serde(skip)]
    pub profile_id: String,
    pub act: IntentAct,
    /// What to do or say.
    pub text: String,
    #[serde(default)]
    pub why: String,
    pub condition: Condition,
    /// The condition as the member reads it.
    pub condition_label: String,
    #[serde(default)]
    pub anchor_id: Option<String>,
    pub until: DateTime<Utc>,
    pub state: IntentionState,
    #[serde(default)]
    pub outcome: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub settled_at: Option<DateTime<Utc>>,
}

impl Intention {
    /// A waiting intention, refused when it has no words or could never be met in time.
    pub fn new(
        profile_id: &str,
        act: IntentAct,
        text: &str,
        condition: Condition,
        condition_label: &str,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<Self, IntentionRefused> {
        let text: String = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(MAX_TEXT_CHARS)
            .collect();
        if text_key(&text).is_empty() {
            return Err(IntentionRefused::NoText);
        }
        if until <= now {
            return Err(IntentionRefused::DeadlinePassed);
        }
        if matches!(condition, Condition::Time { at } if at > until) {
            return Err(IntentionRefused::ConditionAfterDeadline);
        }
        Ok(Self {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            act,
            text,
            why: String::new(),
            condition,
            condition_label: condition_label.trim().to_string(),
            anchor_id: None,
            until,
            state: IntentionState::Waiting,
            outcome: None,
            created_at: now,
            settled_at: None,
        })
    }

    pub fn key(&self) -> String {
        text_key(&self.text)
    }

    /// Waiting on a time that has come, with its deadline not yet passed.
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        self.state == IntentionState::Waiting
            && !self.is_expired(now)
            && matches!(self.condition, Condition::Time { at } if at <= now)
    }

    /// Still waiting after its deadline.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.state == IntentionState::Waiting && now > self.until
    }

    fn settle(
        &mut self,
        to: IntentionState,
        outcome: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), IllegalMove> {
        if self.state != IntentionState::Waiting {
            return Err(IllegalMove {
                what: "an intention",
                from: self.state.as_str(),
                to: to.as_str(),
            });
        }
        self.state = to;
        self.outcome = outcome.map(|o| o.split_whitespace().collect::<Vec<_>>().join(" "));
        self.settled_at = Some(now);
        Ok(())
    }

    /// Its condition held and it was carried out; `outcome` says what happened.
    pub fn done(&mut self, outcome: &str, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.settle(IntentionState::Done, Some(outcome), now)
    }

    pub fn expire(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.settle(IntentionState::Expired, None, now)
    }

    /// The member deleted it, or a later review withdrew it.
    pub fn withdraw(&mut self, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        self.settle(IntentionState::Dropped, None, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn at_eight() -> Condition {
        Condition::Time {
            at: now() + Duration::hours(23),
        }
    }

    fn intention(condition: Condition) -> Intention {
        Intention::new(
            "jerry",
            IntentAct::Push,
            "Leave by 7:15, the summit doors open at 8",
            condition,
            "tomorrow at 8am",
            now() + Duration::days(2),
            now(),
        )
        .unwrap()
    }

    #[test]
    fn an_intention_needs_words_and_a_deadline_it_can_meet() {
        let make = |text: &str, condition: Condition, until: DateTime<Utc>| {
            Intention::new(
                "jerry",
                IntentAct::Push,
                text,
                condition,
                "then",
                until,
                now(),
            )
        };
        let later = now() + Duration::days(1);
        assert_eq!(
            make(" . ", Condition::Chat, later).unwrap_err(),
            IntentionRefused::NoText
        );
        assert_eq!(
            make("Say hello", Condition::Chat, now()).unwrap_err(),
            IntentionRefused::DeadlinePassed
        );
        assert_eq!(
            make("Say hello", at_eight(), now() + Duration::hours(2)).unwrap_err(),
            IntentionRefused::ConditionAfterDeadline
        );
        assert!(make("Say hello", at_eight(), later).is_ok());
    }

    #[test]
    fn a_time_intention_is_due_once_its_time_has_come_and_not_after_its_deadline() {
        let waiting = intention(at_eight());
        assert!(!waiting.is_due(now() + Duration::hours(22)));
        assert!(waiting.is_due(now() + Duration::hours(23)));
        assert!(waiting.is_due(now() + Duration::days(2)));
        assert!(!waiting.is_due(now() + Duration::days(2) + Duration::seconds(1)));
        assert!(waiting.is_expired(now() + Duration::days(2) + Duration::seconds(1)));
        assert!(!waiting.is_expired(now() + Duration::days(2)));
    }

    #[test]
    fn only_a_time_condition_is_due_by_the_clock() {
        let on_arrival = intention(Condition::Place {
            place_id: "office".into(),
            transition: Transition::Arrive,
        });
        assert!(!on_arrival.is_due(now() + Duration::days(1)));
        assert!(on_arrival
            .condition
            .is_met_by_crossing("office", Transition::Arrive));
        assert!(!on_arrival
            .condition
            .is_met_by_crossing("office", Transition::Leave));
        let on_package = Condition::Thing {
            source_id: "camera:gate".into(),
            event: "package".into(),
        };
        assert!(on_package.is_met_by_event("camera:gate", "package"));
        assert!(!on_package.is_met_by_event("camera:gate", "person"));
        assert!(!intention(Condition::Chat).is_due(now() + Duration::days(1)));
    }

    #[test]
    fn a_settled_intention_is_never_due_expired_or_settled_again() {
        let mut met = intention(at_eight());
        met.done("  Told Jerry\n at 08:00 ", now() + Duration::hours(23))
            .unwrap();
        assert_eq!(met.state, IntentionState::Done);
        assert_eq!(met.outcome.as_deref(), Some("Told Jerry at 08:00"));
        assert!(!met.is_due(now() + Duration::days(1)));
        assert!(!met.is_expired(now() + Duration::days(9)));
        assert!(met.expire(now()).is_err());
        assert!(met.withdraw(now()).is_err());

        let mut late = intention(Condition::Chat);
        late.expire(now() + Duration::days(3)).unwrap();
        assert_eq!(late.state, IntentionState::Expired);
        assert!(late.done("too late", now()).is_err());
    }

    #[test]
    fn a_condition_is_stored_as_its_kind_and_spec_and_read_back() {
        for condition in [
            at_eight(),
            Condition::Place {
                place_id: "office".into(),
                transition: Transition::Leave,
            },
            Condition::Thing {
                source_id: "camera:gate".into(),
                event: "package".into(),
            },
            Condition::Chat,
        ] {
            let spec = condition.spec();
            assert!(spec.get("kind").is_none(), "the kind has its own column");
            assert_eq!(
                Condition::from_stored(condition.kind(), &spec),
                Some(condition.clone())
            );
            let wire = serde_json::to_string(&condition).unwrap();
            assert!(wire.contains(&format!("\"kind\":\"{}\"", condition.kind().as_str())));
            assert_eq!(serde_json::from_str::<Condition>(&wire).unwrap(), condition);
        }
        assert_eq!(
            Condition::from_stored(ConditionKind::Place, &json!({"place_id": "office"})),
            None
        );
        assert_eq!(
            Condition::from_stored(ConditionKind::Time, &json!({"at": "tomorrow"})),
            None
        );
    }
}
