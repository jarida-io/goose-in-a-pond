//! In-memory [`AgentSuggestionLedger`] with `sqlite_agent_suggestion`'s semantics.

use crate::user_data::ports::agent_suggestion::AgentSuggestionLedger;
use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

#[derive(Default)]
pub struct MockAgentSuggestionLedger {
    rows: Mutex<BTreeMap<(String, String), DateTime<Utc>>>,
}

impl MockAgentSuggestionLedger {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl AgentSuggestionLedger for MockAgentSuggestionLedger {
    async fn dismiss(&self, template: &str, profile_id: &str, at: DateTime<Utc>) -> Result<()> {
        if template.trim().is_empty() || profile_id.trim().is_empty() {
            bail!("a dismissal names a template and a member");
        }
        self.rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((template.to_string(), profile_id.to_string()), at);
        Ok(())
    }

    async fn dismissed(&self, profile_id: &str) -> Result<BTreeSet<String>> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .filter(|(_, member)| member == profile_id)
            .map(|(template, _)| template.clone())
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_dismissal_is_one_members_and_dismissing_twice_keeps_one() {
        let ledger = MockAgentSuggestionLedger::new();
        let at = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        ledger.dismiss("trip-planner", "jerry", at).await.unwrap();
        ledger.dismiss("trip-planner", "jerry", at).await.unwrap();
        assert_eq!(
            ledger.dismissed("jerry").await.unwrap(),
            ["trip-planner".to_string()].into_iter().collect()
        );
        assert!(ledger.dismissed("liz").await.unwrap().is_empty());
        assert!(ledger.dismiss(" ", "jerry", at).await.is_err());
    }
}
