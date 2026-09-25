use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::RwLock;

use crate::models::domain::model_record::{ModelCategory, ModelRecord, ModelRoleAssignment};
use crate::models::ports::model_repository::ModelRepository;

/// In-memory model catalog and role assignment store.
pub struct MockModelRepository {
    models: Arc<RwLock<HashMap<String, ModelRecord>>>,
    assignments: Arc<RwLock<HashMap<String, String>>>, // role → model_id
}

impl MockModelRepository {
    pub fn new() -> Self {
        Self {
            models: Arc::new(RwLock::new(HashMap::new())),
            assignments: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for MockModelRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ModelRepository for MockModelRepository {
    async fn list_all(&self) -> Result<Vec<ModelRecord>> {
        let map = self.models.read().await;
        let mut records: Vec<ModelRecord> = map.values().cloned().collect();
        records.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(records)
    }

    async fn list_by_category(&self, category: &ModelCategory) -> Result<Vec<ModelRecord>> {
        let map = self.models.read().await;
        let mut records: Vec<ModelRecord> = map
            .values()
            .filter(|r| &r.category == category)
            .cloned()
            .collect();
        records.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(records)
    }

    async fn get_by_id(&self, id: &str) -> Result<Option<ModelRecord>> {
        Ok(self.models.read().await.get(id).cloned())
    }

    async fn upsert(&self, model: &ModelRecord) -> Result<()> {
        let mut map = self.models.write().await;
        // Never downgrade is_custom — mirror the SQLite `ON CONFLICT` rule.
        if let Some(existing) = map.get(&model.id) {
            if existing.is_custom && !model.is_custom {
                let mut preserved = model.clone();
                preserved.is_custom = true;
                map.insert(model.id.clone(), preserved);
                return Ok(());
            }
        }
        map.insert(model.id.clone(), model.clone());
        Ok(())
    }

    async fn set_downloaded(&self, id: &str, downloaded: bool) -> Result<()> {
        let mut map = self.models.write().await;
        if let Some(record) = map.get_mut(id) {
            record.downloaded = downloaded;
        }
        Ok(())
    }

    async fn list_assignments(&self) -> Result<Vec<ModelRoleAssignment>> {
        let map = self.assignments.read().await;
        Ok(map
            .iter()
            .map(|(role, model_id)| ModelRoleAssignment {
                role: role.clone(),
                model_id: model_id.clone(),
            })
            .collect())
    }

    async fn get_assignment(&self, role: &str) -> Result<Option<ModelRoleAssignment>> {
        let map = self.assignments.read().await;
        Ok(map.get(role).map(|model_id| ModelRoleAssignment {
            role: role.to_string(),
            model_id: model_id.clone(),
        }))
    }

    async fn set_assignment(&self, role: &str, model_id: &str) -> Result<()> {
        self.assignments
            .write()
            .await
            .insert(role.to_string(), model_id.to_string());
        Ok(())
    }

    async fn clear_assignment(&self, role: &str) -> Result<()> {
        self.assignments.write().await.remove(role);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::model_record::ModelCategory;

    fn gguf_record(name: &str) -> ModelRecord {
        ModelRecord {
            id: format!("gguf/{name}"),
            category: ModelCategory::Gguf,
            name: name.to_string(),
            filename: Some(format!("{name}.gguf")),
            description: format!("{name} test model"),
            size_mb: 100,
            url: Some(format!("https://example.com/{name}.gguf")),
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        }
    }

    #[tokio::test]
    async fn upsert_and_get_by_id() {
        let repo = MockModelRepository::new();
        let r = gguf_record("llama3");
        repo.upsert(&r).await.unwrap();
        let got = repo.get_by_id("gguf/llama3").await.unwrap().unwrap();
        assert_eq!(got.name, "llama3");
    }

    #[tokio::test]
    async fn set_downloaded_flips_flag() {
        let repo = MockModelRepository::new();
        repo.upsert(&gguf_record("llama3")).await.unwrap();
        repo.set_downloaded("gguf/llama3", true).await.unwrap();
        let got = repo.get_by_id("gguf/llama3").await.unwrap().unwrap();
        assert!(got.downloaded);
    }

    #[tokio::test]
    async fn is_custom_never_downgraded() {
        let repo = MockModelRepository::new();
        let mut custom = gguf_record("custom-model");
        custom.is_custom = true;
        repo.upsert(&custom).await.unwrap();

        let mut registry_version = gguf_record("custom-model");
        registry_version.is_custom = false;
        repo.upsert(&registry_version).await.unwrap();

        let got = repo.get_by_id("gguf/custom-model").await.unwrap().unwrap();
        assert!(got.is_custom, "is_custom must not be downgraded");
    }

    #[tokio::test]
    async fn assignments_roundtrip() {
        let repo = MockModelRepository::new();
        repo.upsert(&gguf_record("llama3")).await.unwrap();
        repo.set_assignment("chat", "gguf/llama3").await.unwrap();

        let a = repo.get_assignment("chat").await.unwrap().unwrap();
        assert_eq!(a.model_id, "gguf/llama3");

        repo.clear_assignment("chat").await.unwrap();
        assert!(repo.get_assignment("chat").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_assignments_returns_all() {
        let repo = MockModelRepository::new();
        repo.upsert(&gguf_record("chat-model")).await.unwrap();
        repo.upsert(&gguf_record("think-model")).await.unwrap();
        repo.set_assignment("chat", "gguf/chat-model")
            .await
            .unwrap();
        repo.set_assignment("think", "gguf/think-model")
            .await
            .unwrap();

        let list = repo.list_assignments().await.unwrap();
        assert_eq!(list.len(), 2);
    }
}
