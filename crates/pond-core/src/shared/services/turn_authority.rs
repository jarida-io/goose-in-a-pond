//! Lets an MCP tool handler find the live turn's [`DelegationAuthority`] by engine session id.
//! The model can't pick that id: Goose drops any caller-supplied `agent-session-id` and stamps
//! its own, so a subagent carries the child's id. Leases revoke on drop and `None` means refuse.
//! Never persisted: a second source of truth for an authorisation input is how one widens.

use crate::shared::domain::orchestration::DelegationAuthority;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, RwLock, Weak};
use tokio_util::sync::CancellationToken;

/// Cap so a leaked lease can't grow the map forever (the SSE semaphore keeps it far below).
/// Eviction only ever removes authority, so a wrong eviction just refuses a delegation.
pub const MAX_TRACKED_TURNS: usize = 64;

/// A live turn's delegation authority, plus the token that ends it.
#[derive(Clone)]
struct TurnEntry {
    authority: DelegationAuthority,
    cancel: CancellationToken,
}

#[derive(Default)]
struct Entries {
    by_engine_session: HashMap<String, TurnEntry>,
    order: VecDeque<String>,
}

/// Live turns' [`DelegationAuthority`], keyed by the engine session id their tool calls carry.
#[derive(Default)]
pub struct TurnAuthorityRegistry {
    entries: RwLock<Entries>,
}

impl TurnAuthorityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes `authority` until the returned lease drops.
    /// `cancel` backs [`parent_turn_token`](Self::parent_turn_token): children die with the turn.
    pub fn publish(
        self: &Arc<Self>,
        engine_session_id: &str,
        authority: DelegationAuthority,
        cancel: CancellationToken,
    ) -> TurnAuthorityLease {
        {
            let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
            let entry = TurnEntry { authority, cancel };
            if entries
                .by_engine_session
                .insert(engine_session_id.to_string(), entry)
                .is_none()
            {
                entries.order.push_back(engine_session_id.to_string());
            }
            while entries.order.len() > MAX_TRACKED_TURNS {
                if let Some(oldest) = entries.order.pop_front() {
                    entries.by_engine_session.remove(&oldest);
                }
            }
        }
        TurnAuthorityLease {
            registry: Arc::downgrade(self),
            engine_session_id: engine_session_id.to_string(),
        }
    }

    /// Same `None` for a subagent's session, an ended turn or an unknown one; all must refuse.
    pub fn authority_for_engine_session(
        &self,
        engine_session_id: &str,
    ) -> Option<DelegationAuthority> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_engine_session
            .get(engine_session_id)
            .map(|entry| entry.authority.clone())
    }

    /// The live turn's token by GIAP session id, which is what a `TaskSpec` carries.
    /// A linear scan is fine: at most [`MAX_TRACKED_TURNS`] entries, once per spawn.
    pub fn parent_turn_token(&self, giap_session_id: &str) -> Option<CancellationToken> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_engine_session
            .values()
            .find(|entry| entry.authority.session_id() == giap_session_id)
            .map(|entry| entry.cancel.clone())
    }

    /// Live turns currently published. Diagnostics and tests only.
    pub fn tracked_turns(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .by_engine_session
            .len()
    }

    fn revoke(&self, engine_session_id: &str) {
        let mut entries = self.entries.write().unwrap_or_else(|e| e.into_inner());
        if entries
            .by_engine_session
            .remove(engine_session_id)
            .is_some()
        {
            entries.order.retain(|id| id != engine_session_id);
        }
    }
}

/// Revokes its authority on drop; deliberately cannot be extended, refreshed or detached.
pub struct TurnAuthorityLease {
    registry: Weak<TurnAuthorityRegistry>,
    engine_session_id: String,
}

impl Drop for TurnAuthorityLease {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            registry.revoke(&self.engine_session_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::profile::ProfileScope;

    fn authority(session_id: &str, scope: ProfileScope) -> DelegationAuthority {
        DelegationAuthority::for_turn(
            session_id,
            scope,
            ["giap-weather__get_forecast", "giap-knowledge__lookup"],
        )
    }

    #[test]
    fn a_published_authority_is_found_by_the_engine_session_id() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let _lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );

        let found = registry
            .authority_for_engine_session("goose-1")
            .expect("the live turn's authority must be findable");
        assert_eq!(found.session_id(), "giap-1");
        assert_eq!(found.profile_scope(), &ProfileScope::Household);
        assert!(found.tool_groups().contains("giap-weather"));
    }

    #[test]
    fn an_unknown_or_finished_turn_has_no_authority() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        assert!(registry
            .authority_for_engine_session("never-seen")
            .is_none());

        let lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );
        // A subagent asking under its own id gets nothing, even while the parent's turn is live.
        assert!(registry
            .authority_for_engine_session("goose-1-child")
            .is_none());

        drop(lease);
        assert!(
            registry.authority_for_engine_session("goose-1").is_none(),
            "an authority outlived its turn - a stale answer to an authorisation question"
        );
        assert_eq!(registry.tracked_turns(), 0);
    }

    /// Vacuity control for the test above: the lookup does work while the lease lives.
    #[test]
    fn revocation_removes_only_the_turn_that_ended() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let lease_a = registry.publish(
            "goose-a",
            authority("giap-a", ProfileScope::Household),
            CancellationToken::new(),
        );
        let _lease_b = registry.publish(
            "goose-b",
            authority("giap-b", ProfileScope::Guest),
            CancellationToken::new(),
        );
        assert_eq!(registry.tracked_turns(), 2);

        drop(lease_a);
        assert!(registry.authority_for_engine_session("goose-a").is_none());
        assert!(
            registry.authority_for_engine_session("goose-b").is_some(),
            "revoking one turn took another turn's authority with it"
        );
        assert_eq!(registry.tracked_turns(), 1);
    }

    #[test]
    fn the_parents_turn_token_is_reachable_by_its_giap_session_id() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let parent = CancellationToken::new();
        let _lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            parent.clone(),
        );

        let found = registry
            .parent_turn_token("giap-1")
            .expect("a live parent turn must expose its token");
        let child = found.child_token();
        assert!(!child.is_cancelled());
        parent.cancel();
        assert!(
            child.is_cancelled(),
            "cancelling the parent did not cancel a token derived from it"
        );
        assert!(registry.parent_turn_token("giap-other").is_none());
    }

    /// Accumulating instead would let one long conversation evict every other turn.
    #[test]
    fn republishing_a_session_replaces_its_entry() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let first = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );
        let _second = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Guest),
            CancellationToken::new(),
        );
        assert_eq!(registry.tracked_turns(), 1);
        assert_eq!(
            registry
                .authority_for_engine_session("goose-1")
                .unwrap()
                .profile_scope(),
            &ProfileScope::Guest,
            "the newer turn's scope must win"
        );
        // The superseded lease still revokes on drop: a refused delegation beats a stale one.
        drop(first);
        assert!(registry.authority_for_engine_session("goose-1").is_none());
    }

    #[test]
    fn the_map_is_bounded_and_eviction_only_ever_removes() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let mut leases = Vec::new();
        for i in 0..(MAX_TRACKED_TURNS + 10) {
            leases.push(registry.publish(
                &format!("goose-{i}"),
                authority(&format!("giap-{i}"), ProfileScope::Household),
                CancellationToken::new(),
            ));
        }
        assert_eq!(registry.tracked_turns(), MAX_TRACKED_TURNS);
        assert!(
            registry.authority_for_engine_session("goose-0").is_none(),
            "the oldest entry should have been evicted"
        );
        assert!(registry
            .authority_for_engine_session(&format!("goose-{}", MAX_TRACKED_TURNS + 9))
            .is_some());
    }

    /// Pins `Weak` over `Arc`: an `Arc` in the lease would keep the registry alive forever.
    #[test]
    fn a_lease_outliving_its_registry_is_inert() {
        let registry = Arc::new(TurnAuthorityRegistry::new());
        let lease = registry.publish(
            "goose-1",
            authority("giap-1", ProfileScope::Household),
            CancellationToken::new(),
        );
        let weak = Arc::downgrade(&registry);
        drop(registry);
        assert!(
            weak.upgrade().is_none(),
            "the lease is holding a strong reference and the registry can never be freed"
        );
        drop(lease);
    }
}
