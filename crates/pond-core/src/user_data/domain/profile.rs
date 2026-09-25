//! Profile domain types — represents a household member.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub display_name: String,
    /// Single emoji representing this profile (default: duck emoji)
    pub avatar_emoji: String,
    /// Flat string map; pond-api converts to/from JSON objects at the HTTP boundary.
    pub preferences: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Whose data a read or write is scoped to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileScope {
    /// A specific household member, plus unattributed shared rows.
    Owner(String),
    /// The whole household, unfiltered.
    Household,
    /// An unidentified speaker. Sees no personal data.
    Guest,
}

/// Arbitrary owner id [`ProfileScope::every_shape`] puts in its `Owner`.
pub const EXEMPLAR_OWNER_ID: &str = "exemplar-owner";

/// A second owner id, distinct from [`EXEMPLAR_OWNER_ID`], for incomparable-pair guards.
/// Shared so `the_second_owner_really_is_a_different_member` covers every fixture using it.
pub const SECOND_EXEMPLAR_OWNER_ID: &str = "a-different-member";

impl ProfileScope {
    /// One value of every variant, for guards quantifying over scopes. Coverage, not enforcement:
    /// `orchestration::ChildScope` clamps what the parent does not contain to `Guest`.
    pub fn every_shape() -> Vec<ProfileScope> {
        vec![
            ProfileScope::Owner(EXEMPLAR_OWNER_ID.to_string()),
            ProfileScope::Household,
            ProfileScope::Guest,
        ]
    }

    /// Only for `serde(default)` on payloads predating the scope field; in code, state the scope.
    pub fn household() -> Self {
        ProfileScope::Household
    }

    /// The id to filter on; `None` for both `Household` (all) and `Guest` (nothing), so never
    /// check it alone — pair it with [`excludes_everything`](Self::excludes_everything).
    pub fn owner_id(&self) -> Option<&str> {
        match self {
            ProfileScope::Owner(id) => Some(id.as_str()),
            ProfileScope::Household | ProfileScope::Guest => None,
        }
    }

    /// True when the scope can never match a row, so a query need not run.
    pub fn excludes_everything(&self) -> bool {
        matches!(self, ProfileScope::Guest)
    }

    /// True when personal data may be surfaced to this scope at all.
    pub fn allows_personal_data(&self) -> bool {
        !self.excludes_everything()
    }

    /// True when `self` reaches no row that `wider` cannot. Partial order: distinct owners are
    /// incomparable; equal scopes are within each other.
    pub fn is_within(&self, wider: &ProfileScope) -> bool {
        match (self, wider) {
            // Reaches nothing, so it is within anything.
            (ProfileScope::Guest, _) => true,
            // Reaches everything, so only Household contains it.
            (ProfileScope::Household, ProfileScope::Household) => true,
            (ProfileScope::Household, _) => false,
            (ProfileScope::Owner(_), ProfileScope::Household) => true,
            (ProfileScope::Owner(a), ProfileScope::Owner(b)) => a == b,
            (ProfileScope::Owner(_), ProfileScope::Guest) => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProfileRequest {
    pub display_name: String,
    #[serde(default = "default_avatar")]
    pub avatar_emoji: String,
}

fn default_avatar() -> String {
    "\u{1F986}".to_string() // 🦆
}

#[cfg(test)]
mod profile_scope_tests {
    use super::*;

    #[test]
    fn owner_exposes_its_id_and_nothing_else_does() {
        assert_eq!(
            ProfileScope::Owner("jerry".into()).owner_id(),
            Some("jerry")
        );
        assert_eq!(ProfileScope::Household.owner_id(), None);
        assert_eq!(ProfileScope::Guest.owner_id(), None);
    }

    /// Branching on `owner_id()` alone would hand a Guest the whole household's memory.
    #[test]
    fn household_and_guest_are_not_interchangeable_despite_both_lacking_an_id() {
        assert_eq!(
            ProfileScope::Household.owner_id(),
            ProfileScope::Guest.owner_id()
        );
        assert!(!ProfileScope::Household.excludes_everything());
        assert!(ProfileScope::Guest.excludes_everything());
        assert!(ProfileScope::Household.allows_personal_data());
        assert!(!ProfileScope::Guest.allows_personal_data());
    }

    #[test]
    fn owner_sees_personal_data() {
        let owner = ProfileScope::Owner("jerry".into());
        assert!(!owner.excludes_everything());
        assert!(owner.allows_personal_data());
    }
}

#[cfg(test)]
mod scope_lattice_tests {
    use super::*;

    /// Every shape plus a second owner, since distinct owners are where `is_within` goes wrong.
    fn every_scope() -> Vec<ProfileScope> {
        let mut scopes = ProfileScope::every_shape();
        scopes.push(ProfileScope::Owner(SECOND_EXEMPLAR_OWNER_ID.to_string()));
        scopes
    }

    /// Covers [`every_scope`] and `orchestration::scope_inheritance_tests::candidate_scopes`.
    #[test]
    fn the_second_owner_really_is_a_different_member() {
        assert_ne!(
            SECOND_EXEMPLAR_OWNER_ID, EXEMPLAR_OWNER_ID,
            "the fixture's two owners are the same member, so every 'two members are \
             incomparable' assertion below is comparing a scope with itself -- and so is \
             PAI-6's scope-inheritance sweep, which reads the same constant"
        );
        let owners = every_scope()
            .iter()
            .filter(|s| matches!(s, ProfileScope::Owner(_)))
            .count();
        assert_eq!(
            owners, 2,
            "the lattice fixture holds {owners} owners; with fewer than two, \
             `two_owners_are_incomparable` and the refused-pair count below stop \
             exercising the partial order at all"
        );
    }

    /// Takes the variants from serde's `unknown variant` error, so misses `#[serde(skip)]` ones.
    #[test]
    fn every_shape_lists_every_variant_the_enum_has() {
        let message = serde_json::from_str::<ProfileScope>("\"definitely_not_a_variant\"")
            .expect_err("that is not a variant")
            .to_string();
        let from_the_enum: Vec<&str> = message.split('`').skip(1).step_by(2).skip(1).collect();
        assert!(
            !from_the_enum.is_empty(),
            "no variant names could be read out of serde's message {message:?}, so this test \
             can no longer see a new variant at all"
        );
        let listed: Vec<String> = ProfileScope::every_shape()
            .iter()
            .map(|s| match s {
                ProfileScope::Owner(_) => "owner".to_string(),
                ProfileScope::Household => "household".to_string(),
                ProfileScope::Guest => "guest".to_string(),
            })
            .collect();
        for variant in &from_the_enum {
            assert!(
                listed.iter().any(|l| l == variant),
                "ProfileScope has a variant `{variant}` that every_shape() does not produce. \
                 Every scope guard in this crate quantifies over that list, so an unlisted \
                 shape is one nothing is tested against"
            );
        }
        assert_eq!(
            from_the_enum.len(),
            listed.len(),
            "every_shape() and the enum disagree on how many shapes exist: {from_the_enum:?} \
             against {listed:?}"
        );
    }

    /// Reflexive: inheriting a scope unchanged is "never wider".
    #[test]
    fn every_scope_is_within_itself() {
        for s in every_scope() {
            assert!(s.is_within(&s), "{s:?} is not within itself");
        }
    }

    /// Otherwise a Guest turn could delegate to a subagent that reads the household's memory.
    #[test]
    fn household_is_within_nothing_narrower() {
        assert!(!ProfileScope::Household.is_within(&ProfileScope::Guest));
        assert!(!ProfileScope::Household.is_within(&ProfileScope::Owner("jerry".into())));
    }

    #[test]
    fn two_owners_are_incomparable() {
        let jerry = ProfileScope::Owner("jerry".into());
        let liz = ProfileScope::Owner("liz".into());
        assert!(!jerry.is_within(&liz));
        assert!(!liz.is_within(&jerry));
    }

    #[test]
    fn guest_is_within_everything_and_only_guest_is_within_guest() {
        for wider in every_scope() {
            assert!(
                ProfileScope::Guest.is_within(&wider),
                "Guest should be within {wider:?}"
            );
        }
        for narrower in every_scope() {
            let expected = matches!(narrower, ProfileScope::Guest);
            assert_eq!(
                narrower.is_within(&ProfileScope::Guest),
                expected,
                "{narrower:?} within Guest should be {expected}"
            );
        }
    }

    #[test]
    fn an_owner_is_within_the_household() {
        assert!(ProfileScope::Owner("jerry".into()).is_within(&ProfileScope::Household));
    }

    /// Vacuity control: pins how many pairs are refused, so a constant predicate fails.
    #[test]
    fn the_predicate_refuses_a_specific_number_of_pairs() {
        let scopes = every_scope();
        let refused = scopes
            .iter()
            .flat_map(|a| scopes.iter().map(move |b| (a, b)))
            .filter(|(a, b)| !a.is_within(b))
            .count();
        // 16 ordered pairs, 9 permitted (4 reflexive, Guest in 3 others, 2 owners in Household).
        assert_eq!(
            refused, 7,
            "the scope lattice changed shape; if that was deliberate, say which \
             pair moved and why it is not a widening"
        );
    }
}

#[cfg(test)]
mod scope_gating_tests {
    use super::*;

    /// The memory-injection gate in `goose_agent.rs` relies on this and cannot be tested there.
    #[test]
    fn only_a_guest_is_denied_personal_data() {
        assert!(ProfileScope::Owner("jerry".into()).allows_personal_data());
        assert!(ProfileScope::Household.allows_personal_data());
        assert!(!ProfileScope::Guest.allows_personal_data());
    }

    /// Scopes ride the serialized `AgentRequest`; a variant widening across it would go unseen.
    #[test]
    fn every_scope_round_trips_through_serde() {
        for scope in ProfileScope::every_shape() {
            let json = serde_json::to_string(&scope).expect("scope must serialize");
            let back: ProfileScope = serde_json::from_str(&json).expect("scope must deserialize");
            assert_eq!(back, scope, "round trip changed the scope: {json}");
        }
    }
}
