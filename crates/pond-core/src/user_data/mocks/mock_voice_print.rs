use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::domain::voice_print::{VoicePrint, WakeTake};
use crate::user_data::ports::voice_print::VoicePrintRepository;

/// In memory, in insertion order.
#[derive(Default)]
pub struct InMemoryVoicePrints {
    prints: Mutex<Vec<VoicePrint>>,
    takes: Mutex<Vec<WakeTake>>,
}

#[async_trait]
impl VoicePrintRepository for InMemoryVoicePrints {
    async fn add_print(&self, print: &VoicePrint) -> Result<()> {
        self.prints.lock().unwrap().push(print.clone());
        Ok(())
    }

    async fn prints(&self, model_name: &str) -> Result<Vec<VoicePrint>> {
        Ok(self
            .prints
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.model_name == model_name)
            .cloned()
            .collect())
    }

    async fn delete_prints(&self, profile_id: &str) -> Result<u64> {
        let mut prints = self.prints.lock().unwrap();
        let before = prints.len();
        prints.retain(|p| p.profile_id != profile_id);
        Ok((before - prints.len()) as u64)
    }

    async fn add_wake_take(&self, take: &WakeTake) -> Result<()> {
        self.takes.lock().unwrap().push(take.clone());
        Ok(())
    }

    async fn wake_takes(&self, phrase: &str) -> Result<Vec<WakeTake>> {
        Ok(self
            .takes
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.phrase == phrase)
            .cloned()
            .collect())
    }

    async fn clear_wake_takes(&self) -> Result<u64> {
        let mut takes = self.takes.lock().unwrap();
        let n = takes.len() as u64;
        takes.clear();
        Ok(n)
    }

    async fn delete_wake_takes(&self, profile_id: &str) -> Result<u64> {
        let mut takes = self.takes.lock().unwrap();
        let before = takes.len();
        takes.retain(|t| t.profile_id.as_deref() != Some(profile_id));
        Ok((before - takes.len()) as u64)
    }

    async fn forget_wake_templates(&self, profile_id: &str) -> Result<u64> {
        let mut n = 0;
        for t in self.takes.lock().unwrap().iter_mut() {
            if t.profile_id.as_deref() == Some(profile_id) && t.template.is_some() {
                t.template = None;
                t.rest = None;
                n += 1;
            }
        }
        Ok(n)
    }
}
