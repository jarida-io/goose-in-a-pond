//! In-memory [`AgentPrefillRegistry`] with `sqlite_agent_prefill`'s semantics.

use crate::shared::domain::agent_prefill::PrefillRecord;
use crate::shared::ports::agent_prefill::AgentPrefillRegistry;
use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
pub struct MockAgentPrefillRegistry {
    rows: Mutex<HashMap<String, PrefillRecord>>,
}

impl MockAgentPrefillRegistry {
    pub fn new() -> Self {
        Self::default()
    }
}

fn checked(agent: &str) -> Result<()> {
    if agent.trim().is_empty() {
        bail!("a prefill row names its agent");
    }
    Ok(())
}

#[async_trait]
impl AgentPrefillRegistry for MockAgentPrefillRegistry {
    async fn get_all(&self) -> Result<HashMap<String, PrefillRecord>> {
        Ok(self.rows.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    async fn note_compiled(
        &self,
        agent: &str,
        definition_hash: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        checked(agent)?;
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let row = rows.entry(agent.to_string()).or_insert(PrefillRecord {
            definition_hash: String::new(),
            compiled_at: None,
            last_used_at: None,
        });
        row.definition_hash = definition_hash.to_string();
        row.compiled_at = Some(at);
        Ok(())
    }

    async fn note_used(&self, agent: &str, at: DateTime<Utc>) -> Result<()> {
        checked(agent)?;
        let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        rows.entry(agent.to_string())
            .or_insert(PrefillRecord {
                definition_hash: String::new(),
                compiled_at: None,
                last_used_at: None,
            })
            .last_used_at = Some(at);
        Ok(())
    }

    async fn forget(&self, agent: &str) -> Result<bool> {
        Ok(self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(agent)
            .is_some())
    }
}
