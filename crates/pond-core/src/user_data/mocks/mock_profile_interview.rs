//! In-memory mock implementation of `ProfileInterviewRepository`.

use crate::user_data::domain::profile_interview::Interview;
use crate::user_data::ports::profile_interview::ProfileInterviewRepository;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::RwLock;

#[derive(Default)]
pub struct MockProfileInterviewRepository {
    interviews: RwLock<HashMap<String, Interview>>,
}

#[async_trait]
impl ProfileInterviewRepository for MockProfileInterviewRepository {
    async fn get(&self, profile_id: &str) -> Result<Option<Interview>> {
        Ok(self.interviews.read().await.get(profile_id).cloned())
    }

    async fn get_by_session(&self, session_id: &str) -> Result<Option<Interview>> {
        Ok(self
            .interviews
            .read()
            .await
            .values()
            .find(|i| i.session_id == session_id)
            .cloned())
    }

    async fn save(&self, interview: &Interview) -> Result<()> {
        self.interviews
            .write()
            .await
            .insert(interview.profile_id.clone(), interview.clone());
        Ok(())
    }

    async fn delete(&self, profile_id: &str) -> Result<()> {
        self.interviews.write().await.remove(profile_id);
        Ok(())
    }
}
