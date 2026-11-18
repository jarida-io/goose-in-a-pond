//! The specialists a pond ships with: each owns some kinds of anchor and adds its playbook to
//! their briefs. Which ones a member has on is theirs to switch.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::anchor::AnchorKind;

/// Money captures a member sends before the Budget specialist is offered to them.
pub const MONEY_CAPTURES_BEFORE_OFFER: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecialistId {
    Budget,
    Work,
    Trip,
    Hunt,
}

impl SpecialistId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::Work => "work",
            Self::Trip => "trip",
            Self::Hunt => "hunt",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "budget" => Some(Self::Budget),
            "work" => Some(Self::Work),
            "trip" => Some(Self::Trip),
            "hunt" => Some(Self::Hunt),
            _ => None,
        }
    }
}

/// Whether a member has a specialist on, and where an offer of it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSwitch {
    On,
    Off,
    /// Goose offered it and the member has not answered.
    Offered,
    /// The member turned the offer down; it is not offered again.
    Declined,
}

impl AgentSwitch {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
            Self::Offered => "offered",
            Self::Declined => "declined",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "on" => Some(Self::On),
            "off" => Some(Self::Off),
            "offered" => Some(Self::Offered),
            "declined" => Some(Self::Declined),
            _ => None,
        }
    }

    pub fn is_on(self) -> bool {
        self == Self::On
    }
}

/// A shipped specialist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Specialist {
    pub id: SpecialistId,
    pub title: &'static str,
    /// One sentence a member reads beside its switch.
    pub about: &'static str,
    /// The anchor kinds it owns; Budget owns none by kind, only its standing anchor.
    pub owns: &'static [AnchorKind],
    /// What a deliverable in its domain must contain, added to the brief.
    pub playbook: &'static str,
    pub default_on: bool,
}

impl Specialist {
    pub fn default_switch(&self) -> AgentSwitch {
        if self.default_on {
            AgentSwitch::On
        } else {
            AgentSwitch::Off
        }
    }
}

pub const SPECIALISTS: &[Specialist] = &[
    Specialist {
        id: SpecialistId::Trip,
        title: "Trips and events",
        about: "Plans trips and prepares you for events: days, transport, stays and what to bring.",
        owns: &[AnchorKind::Trip, AnchorKind::Event],
        playbook: "\
Give every day its date, and every booking a price in the member's currency and a link.
Name the way there and back with its duration and cost, and say which leg to book first.
Check the weather for the dates and the opening hours on the days they would visit.
For an event, name the sessions and people that fit the member and say why.
End with what to bring and what is still undecided.",
        default_on: true,
    },
    Specialist {
        id: SpecialistId::Work,
        title: "Projects",
        about: "Follows your projects: what is new, who is involved and which deadlines are near.",
        owns: &[AnchorKind::Project],
        playbook: "\
Lead with what changed since the last version, and say so plainly when nothing did.
For each item give its date, its source and why it matters to this project.
Name the people and organisations involved and what each of them did.
List the deadlines ahead, nearest first, with what each one needs from the member.",
        default_on: true,
    },
    Specialist {
        id: SpecialistId::Hunt,
        title: "Purchases",
        about: "Hunts for things you want to buy: real listings compared, with the full cost.",
        owns: &[AnchorKind::Purchase],
        playbook: "\
List real listings only: the seller, the price, the date seen and a link for each.
Compare them in one table on the specifications the member asked about.
Say why each fits or falls short, and which one you would pick.
Add what the price leaves out: delivery, duty, fees and running costs.",
        default_on: true,
    },
    Specialist {
        id: SpecialistId::Budget,
        title: "Budget",
        about: "Keeps a ledger from the receipts and payment messages you capture, month by month.",
        owns: &[],
        playbook: "\
Total the month by category in the member's currency, from the ledger entries only.
Compare it with the month before and name the three largest changes.
List each recurring payment with its amount and the date it next falls.
Never guess an amount: leave out what the records do not show and say what is missing.",
        default_on: false,
    },
];

/// The shipped specialist with this id.
pub fn specialist(id: SpecialistId) -> &'static Specialist {
    SPECIALISTS
        .iter()
        .find(|s| s.id == id)
        .expect("every specialist id has a row in SPECIALISTS")
}

/// The specialist that owns anchors of `kind`, if one does.
pub fn owner_of(kind: AnchorKind) -> Option<SpecialistId> {
    SPECIALISTS
        .iter()
        .find(|s| s.owns.contains(&kind))
        .map(|s| s.id)
}

/// One member's switch for one specialist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberAgent {
    pub agent: SpecialistId,
    pub state: AgentSwitch,
    /// `None` while the member is on the shipped default.
    pub changed_at: Option<DateTime<Utc>>,
}

/// Every specialist's switch for one member: what they stored, else the shipped default.
pub fn switches(stored: &[MemberAgent]) -> Vec<MemberAgent> {
    SPECIALISTS
        .iter()
        .map(|specialist| {
            stored
                .iter()
                .find(|row| row.agent == specialist.id)
                .copied()
                .unwrap_or(MemberAgent {
                    agent: specialist.id,
                    state: specialist.default_switch(),
                    changed_at: None,
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_specialist_is_shipped_once_with_a_playbook() {
        for id in [
            SpecialistId::Budget,
            SpecialistId::Work,
            SpecialistId::Trip,
            SpecialistId::Hunt,
        ] {
            assert_eq!(SPECIALISTS.iter().filter(|s| s.id == id).count(), 1);
            let lines = specialist(id).playbook.lines().count();
            assert!((3..=6).contains(&lines), "{id:?} has {lines} lines");
            assert_eq!(SpecialistId::parse(id.as_str()), Some(id));
        }
    }

    #[test]
    fn each_anchor_kind_has_at_most_one_owner() {
        assert_eq!(owner_of(AnchorKind::Trip), Some(SpecialistId::Trip));
        assert_eq!(owner_of(AnchorKind::Event), Some(SpecialistId::Trip));
        assert_eq!(owner_of(AnchorKind::Project), Some(SpecialistId::Work));
        assert_eq!(owner_of(AnchorKind::Purchase), Some(SpecialistId::Hunt));
        assert_eq!(owner_of(AnchorKind::Interest), None);
        assert_eq!(owner_of(AnchorKind::Standing), None);
    }

    #[test]
    fn a_member_with_nothing_stored_is_on_the_shipped_defaults() {
        let defaults = switches(&[]);
        assert_eq!(defaults.len(), SPECIALISTS.len());
        let state_of =
            |rows: &[MemberAgent], id| rows.iter().find(|r| r.agent == id).unwrap().state;
        assert_eq!(state_of(&defaults, SpecialistId::Trip), AgentSwitch::On);
        assert_eq!(state_of(&defaults, SpecialistId::Budget), AgentSwitch::Off);
        let stored = [MemberAgent {
            agent: SpecialistId::Budget,
            state: AgentSwitch::Offered,
            changed_at: Some(Utc::now()),
        }];
        assert_eq!(
            state_of(&switches(&stored), SpecialistId::Budget),
            AgentSwitch::Offered
        );
    }
}
