//! Repository-backed [`DraftAuthority`].
//!
//! Reuses [`identity_resolution::resolve`] rather than re-deriving a scope: a second
//! resolution disagreeing with the one the turn already made would silently win here.

use std::sync::Arc;

use async_trait::async_trait;

use crate::security::ports::draft_authority::DraftAuthority;
use crate::security::ports::policy::{
    scopes, PolicyDecision, PolicyMode, Principal, SecurityPolicy,
};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::session::{IdentificationSource, SessionIdentity};
use crate::user_data::ports::profile::ProfileRepository;
use crate::user_data::ports::session_storage::SessionStorage;
use crate::user_data::ports::settings::SettingsRepository;
use crate::user_data::services::identity_resolution;

pub struct RepoDraftAuthority {
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    sessions: Arc<dyn SessionStorage>,
    profiles: Arc<dyn ProfileRepository + Send + Sync>,
    policy: Option<Arc<dyn SecurityPolicy>>,
}

impl RepoDraftAuthority {
    pub fn new(
        settings: Arc<dyn SettingsRepository + Send + Sync>,
        sessions: Arc<dyn SessionStorage>,
        profiles: Arc<dyn ProfileRepository + Send + Sync>,
        policy: Option<Arc<dyn SecurityPolicy>>,
    ) -> Self {
        Self {
            settings,
            sessions,
            profiles,
            policy,
        }
    }
}

#[async_trait]
impl DraftAuthority for RepoDraftAuthority {
    async fn policy_mode(&self) -> PolicyMode {
        match self.settings.get().await {
            Ok(s) => PolicyMode::parse(&s.security_policy_mode),
            // A failed settings read must neither start blocking nor silently disable auditing.
            Err(e) => {
                tracing::warn!(error = %e, "could not read security_policy_mode; assuming audit");
                PolicyMode::Audit
            }
        }
    }

    async fn actor_for_engine_session(
        &self,
        engine_session_id: &str,
    ) -> Option<(ProfileScope, IdentificationSource)> {
        if engine_session_id.trim().is_empty() {
            return None;
        }
        // The engine's id is not GIAP's. Walk engine_session_map backwards.
        let giap_sid = match self
            .sessions
            .get_session_id_for_engine(engine_session_id)
            .await
        {
            Ok(Some(sid)) => sid,
            // Unmapped or unreadable: unresolvable, which narrows.
            Ok(None) => return None,
            Err(e) => {
                tracing::warn!(error = %e, engine_session_id, "could not map engine session");
                return None;
            }
        };

        let identity = self
            .sessions
            .get_session_identity(&giap_sid)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = %e,
                    session_id = %giap_sid,
                    "could not read session identity"
                );
                SessionIdentity::unknown()
            });

        let household_has_multiple_members = match self.profiles.list().await {
            Ok(p) => p.len() > 1,
            // Assume >1: an unidentified speaker then resolves to Guest, not Household.
            Err(e) => {
                tracing::warn!(error = %e, "could not count household members; assuming >1");
                true
            }
        };

        let resolved = identity_resolution::resolve(&identity_resolution::ResolutionInputs {
            // No rung to resolve from: nothing links a paired device to a member.
            paired_device_profile: None,
            session: &identity,
            household_has_multiple_members,
        });
        Some((resolved.scope, resolved.source))
    }

    async fn audit(&self, engine_session_id: &str, action: &str, decision: &PolicyDecision) {
        let Some(policy) = &self.policy else {
            return;
        };
        // A model tool call is in-process work on a session's behalf; `Principal` has no
        // agent-turn kind, so the session rides the action string.
        let principal = Principal::internal();
        policy
            .audit(
                &principal,
                &format!("{action} session={engine_session_id}"),
                scopes::DRAFT,
                decision,
            )
            .await;
    }
}
