//! Port for proactive proposals, stored as `drafts` rows to reuse the draft approval flow.
//! Every read takes `now` and filters expiry itself; `expire_due` is only tidying.

use crate::user_data::domain::proposal::{Proposal, ProposalDecision};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Repository for proactive proposals, stored as rows in the `drafts` table.
#[async_trait]
pub trait ProposalRepository: Send + Sync {
    /// Persist a new proposal in the pending state.
    async fn save(&self, proposal: &Proposal) -> Result<()>;

    /// Live, pending proposals for one member, newest first; expired ones are never returned.
    /// No list-all exists on purpose: only a broadcast would want one.
    async fn list_live_for(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Vec<Proposal>>;

    /// One proposal by id if still pending and live, else `None` whatever the reason:
    /// deciding goes through the draft path, which has the ownership gate.
    async fn get_live(&self, id: &str, now: DateTime<Utc>) -> Result<Option<Proposal>>;

    /// Mark overdue pending proposals `expired`; returns the count. Tidying, not enforcement.
    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64>;

    /// Proposals made to one member since `since`, whatever their fate: the
    /// `MAX_PROPOSALS_PER_DAY` count, since dismissed ones still interrupted someone.
    async fn count_made_since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<usize>;

    /// One member's decisions on proposals made since `since`, expiries included and pending
    /// rows excluded (`ProposalDecision::recorded` refuses them).
    async fn decisions_since(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProposalDecision>>;
}
