//! Driven Port: who is deciding a draft, and how hard the policy bites.
//!
//! Builtin MCP servers are process-wide singletons (Goose's `SpawnServerFn` takes no session,
//! and `add_extension` reuses an unchanged config), and the global `set_current_session_id`
//! races across the four `sse_semaphore` turns. So identity comes from the per-call engine
//! session id in the MCP request `_meta` (`agent-session-id`).

use crate::security::ports::policy::{PolicyDecision, PolicyMode};
use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::session::IdentificationSource;
use async_trait::async_trait;

#[async_trait]
pub trait DraftAuthority: Send + Sync {
    /// The mode in force, read fresh per decision: an operator's flip must take effect at once.
    async fn policy_mode(&self) -> PolicyMode;

    /// Who is speaking in this call's engine session; callers must treat `None` as a refusal.
    async fn actor_for_engine_session(
        &self,
        engine_session_id: &str,
    ) -> Option<(ProfileScope, IdentificationSource)>;

    /// Record a decision; must never fail the caller. Takes the whole decision: under `audit`, a
    /// refusal's effect is "permitted". `action` is a bare verb (`draft_approve`), no verdict.
    async fn audit(&self, engine_session_id: &str, action: &str, decision: &PolicyDecision);
}
