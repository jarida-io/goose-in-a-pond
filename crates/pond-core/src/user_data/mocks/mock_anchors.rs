//! In-memory [`AnchorRepository`] with the SQLite adapter's rules: one anchor per title and
//! owner, one deliverable per slot, and a member's reads limited to anchors they own or are
//! shared.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::anchor::{sort_anchors, Anchor, AnchorState, Deliverable};
use crate::user_data::ports::anchors::AnchorRepository;

#[derive(Default)]
pub struct InMemoryAnchorRepository {
    anchors: Mutex<Vec<Anchor>>,
    deliverables: Mutex<Vec<Deliverable>>,
}

impl InMemoryAnchorRepository {
    pub fn new() -> Self {
        Self::default()
    }

    fn readable(&self, profile_id: &str, anchor_id: &str) -> bool {
        self.anchors
            .lock()
            .unwrap()
            .iter()
            .any(|a| a.id == anchor_id && a.is_readable_by(profile_id))
    }
}

#[async_trait]
impl AnchorRepository for InMemoryAnchorRepository {
    async fn add(&self, anchor: &Anchor) -> Result<bool> {
        let mut anchors = self.anchors.lock().unwrap();
        let key = anchor.key();
        if anchors
            .iter()
            .any(|a| a.id == anchor.id || (a.profile_id == anchor.profile_id && a.key() == key))
        {
            return Ok(false);
        }
        anchors.push(Anchor {
            shared_with: Vec::new(),
            ..anchor.clone()
        });
        Ok(true)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Anchor>> {
        let anchors = self.anchors.lock().unwrap();
        Ok(anchors
            .iter()
            .find(|a| a.id == id && a.is_readable_by(profile_id))
            .cloned())
    }

    async fn by_title(&self, profile_id: &str, title_key: &str) -> Result<Option<Anchor>> {
        let anchors = self.anchors.lock().unwrap();
        Ok(anchors
            .iter()
            .find(|a| a.profile_id == profile_id && a.key() == title_key)
            .cloned())
    }

    async fn list(&self, profile_id: &str, states: &[AnchorState]) -> Result<Vec<Anchor>> {
        let mut found: Vec<Anchor> = self
            .anchors
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.is_readable_by(profile_id) && states.contains(&a.state))
            .cloned()
            .collect();
        sort_anchors(&mut found);
        Ok(found)
    }

    async fn save(&self, anchor: &Anchor) -> Result<bool> {
        let mut anchors = self.anchors.lock().unwrap();
        match anchors
            .iter_mut()
            .find(|a| a.id == anchor.id && a.profile_id == anchor.profile_id)
        {
            Some(held) => {
                let shared_with = std::mem::take(&mut held.shared_with);
                *held = Anchor {
                    shared_with,
                    ..anchor.clone()
                };
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn set_shares(
        &self,
        owner_id: &str,
        anchor_id: &str,
        profile_ids: &[String],
    ) -> Result<bool> {
        let mut anchors = self.anchors.lock().unwrap();
        match anchors
            .iter_mut()
            .find(|a| a.id == anchor_id && a.profile_id == owner_id)
        {
            Some(held) => {
                let mut shared: Vec<String> = profile_ids
                    .iter()
                    .filter(|id| id.as_str() != owner_id)
                    .cloned()
                    .collect();
                shared.sort();
                shared.dedup();
                held.shared_with = shared;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn find(&self, id: &str) -> Result<Option<Anchor>> {
        Ok(self
            .anchors
            .lock()
            .unwrap()
            .iter()
            .find(|a| a.id == id)
            .cloned())
    }

    async fn candidates_older_than(&self, before: DateTime<Utc>) -> Result<Vec<Anchor>> {
        let anchors = self.anchors.lock().unwrap();
        Ok(anchors
            .iter()
            .filter(|a| a.state == AnchorState::Candidate && a.created_at < before)
            .cloned()
            .collect())
    }

    async fn active_over_before(&self, before: DateTime<Utc>) -> Result<Vec<Anchor>> {
        let anchors = self.anchors.lock().unwrap();
        Ok(anchors
            .iter()
            .filter(|a| {
                a.state == AnchorState::Active && a.over_at().is_some_and(|over| over < before)
            })
            .cloned()
            .collect())
    }

    async fn add_deliverable(&self, deliverable: &Deliverable) -> Result<bool> {
        let mut deliverables = self.deliverables.lock().unwrap();
        if deliverables.iter().any(|d| {
            d.id == deliverable.id
                || (d.anchor_id == deliverable.anchor_id && d.slot == deliverable.slot)
        }) {
            return Ok(false);
        }
        deliverables.push(deliverable.clone());
        Ok(true)
    }

    async fn deliverable(&self, profile_id: &str, id: &str) -> Result<Option<Deliverable>> {
        let found = self.find_deliverable(id).await?;
        Ok(found.filter(|d| self.readable(profile_id, &d.anchor_id)))
    }

    async fn deliverables(&self, profile_id: &str, anchor_id: &str) -> Result<Vec<Deliverable>> {
        if !self.readable(profile_id, anchor_id) {
            return Ok(Vec::new());
        }
        self.deliverables_of(anchor_id).await
    }

    async fn unseen(&self, profile_id: &str) -> Result<Vec<Deliverable>> {
        let mut found: Vec<Deliverable> = self
            .deliverables
            .lock()
            .unwrap()
            .iter()
            .rev()
            .filter(|d| d.is_unseen() && self.readable(profile_id, &d.anchor_id))
            .cloned()
            .collect();
        found.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(found)
    }

    async fn find_deliverable(&self, id: &str) -> Result<Option<Deliverable>> {
        Ok(self
            .deliverables
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.id == id)
            .cloned())
    }

    async fn deliverables_of(&self, anchor_id: &str) -> Result<Vec<Deliverable>> {
        let deliverables = self.deliverables.lock().unwrap();
        Ok(deliverables
            .iter()
            .filter(|d| d.anchor_id == anchor_id)
            .cloned()
            .collect())
    }

    async fn save_deliverable(&self, deliverable: &Deliverable) -> Result<bool> {
        let mut deliverables = self.deliverables.lock().unwrap();
        match deliverables.iter_mut().find(|d| d.id == deliverable.id) {
            Some(held) => {
                *held = deliverable.clone();
                Ok(true)
            }
            None => Ok(false),
        }
    }

    async fn deliverables_due(&self, now: DateTime<Utc>) -> Result<Vec<Deliverable>> {
        let active: Vec<String> = self
            .anchors
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.state == AnchorState::Active)
            .map(|a| a.id.clone())
            .collect();
        let mut due: Vec<Deliverable> = self
            .deliverables
            .lock()
            .unwrap()
            .iter()
            .filter(|d| d.refresh_is_due(now) && active.contains(&d.anchor_id))
            .cloned()
            .collect();
        due.sort_by_key(|d| d.refresh_at);
        Ok(due)
    }
}
