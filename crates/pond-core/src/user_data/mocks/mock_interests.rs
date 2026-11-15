//! In-memory [`InterestRepository`] with the SQLite adapter's rules: one count per conversation,
//! a confirmed row's fields left alone, an avoided row never touched.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::user_data::domain::interest::{
    category_of, clean_facets, clean_topic, merge_facets, picture_path, topic_key, Interest,
    InterestInput, InterestPatch, MemberPortrait, Observed, ObservedInterest, Origin, STRONG_AT,
};
use crate::user_data::domain::interest_interview::InterestInterview;
use crate::user_data::ports::interests::InterestRepository;
use crate::user_data::services::member_interview::MemberInterview;

#[derive(Default)]
pub struct InMemoryInterestRepository {
    rows: Mutex<Vec<Interest>>,
    sightings: Mutex<HashSet<(String, String)>>,
    pictures_looked_for: Mutex<HashSet<String>>,
    portraits: Mutex<HashMap<String, MemberPortrait>>,
    interviews: Mutex<HashMap<String, InterestInterview>>,
    member_interviews: Mutex<HashMap<String, MemberInterview>>,
}

impl InMemoryInterestRepository {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every member's rows, for assertions.
    pub fn all(&self) -> Vec<Interest> {
        self.rows.lock().unwrap().clone()
    }
}

fn sorted(mut rows: Vec<Interest>) -> Vec<Interest> {
    rows.sort_by(|a, b| {
        b.confirmed
            .cmp(&a.confirmed)
            .then(b.evidence.cmp(&a.evidence))
            .then(b.updated_at.cmp(&a.updated_at))
    });
    rows
}

#[async_trait]
impl InterestRepository for InMemoryInterestRepository {
    async fn list(&self, profile_id: &str) -> Result<Vec<Interest>> {
        let rows = self.rows.lock().unwrap();
        Ok(sorted(
            rows.iter()
                .filter(|r| r.profile_id == profile_id)
                .cloned()
                .collect(),
        ))
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Interest>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .find(|r| r.profile_id == profile_id && r.id == id)
            .cloned())
    }

    async fn observe(
        &self,
        profile_id: &str,
        session_id: &str,
        seen: &ObservedInterest,
    ) -> Result<Observed> {
        let Some(topic) = clean_topic(&seen.topic) else {
            return Ok(Observed::AlreadyCounted);
        };
        let key = topic_key(&topic);
        let now = Utc::now();
        let mut rows = self.rows.lock().unwrap();
        let mut sightings = self.sightings.lock().unwrap();
        match rows
            .iter_mut()
            .find(|r| r.profile_id == profile_id && topic_key(&r.topic) == key)
        {
            Some(row) if row.avoid => Ok(Observed::Avoided),
            Some(row) => {
                if !sightings.insert((row.id.clone(), session_id.to_string())) {
                    return Ok(Observed::AlreadyCounted);
                }
                row.evidence += 1;
                if !row.confirmed {
                    row.facets = merge_facets(&row.facets, &seen.facets);
                    row.updated_at = now;
                }
                Ok(Observed::Counted)
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                sightings.insert((id.clone(), session_id.to_string()));
                rows.push(Interest {
                    id,
                    profile_id: profile_id.to_string(),
                    topic,
                    category: category_of(&seen.category),
                    facets: clean_facets(seen.facets.iter().map(String::as_str)),
                    depth: Default::default(),
                    news: Default::default(),
                    avoid: false,
                    origin: Origin::Chat,
                    evidence: 1,
                    confirmed: false,
                    picture: None,
                    updated_at: now,
                });
                Ok(Observed::Created)
            }
        }
    }

    async fn confirm(
        &self,
        profile_id: &str,
        input: &InterestInput,
        origin: Origin,
    ) -> Result<Interest> {
        let topic = clean_topic(&input.topic).ok_or_else(|| anyhow::anyhow!("empty topic"))?;
        let key = topic_key(&topic);
        let now = Utc::now();
        let facets = clean_facets(input.facets.iter().flatten().map(String::as_str));
        let mut rows = self.rows.lock().unwrap();
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.profile_id == profile_id && topic_key(&r.topic) == key)
        {
            row.topic = topic;
            if let Some(category) = &input.category {
                row.category = category_of(category);
            }
            row.facets = merge_facets(&row.facets, &facets);
            row.depth = input.depth.unwrap_or(row.depth);
            row.news = input.news.unwrap_or(row.news);
            row.avoid = input.avoid.unwrap_or(row.avoid);
            row.origin = origin;
            row.confirmed = true;
            row.updated_at = now;
            return Ok(row.clone());
        }
        let row = Interest {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            topic,
            category: category_of(input.category.as_deref().unwrap_or("other")),
            facets,
            depth: input.depth.unwrap_or_default(),
            news: input.news.unwrap_or_default(),
            avoid: input.avoid.unwrap_or(false),
            origin,
            evidence: 1,
            confirmed: true,
            picture: None,
            updated_at: now,
        };
        rows.push(row.clone());
        Ok(row)
    }

    async fn patch(
        &self,
        profile_id: &str,
        id: &str,
        patch: &InterestPatch,
    ) -> Result<Option<Interest>> {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows
            .iter_mut()
            .find(|r| r.profile_id == profile_id && r.id == id)
        else {
            return Ok(None);
        };
        if let Some(topic) = patch.topic.as_deref().and_then(clean_topic) {
            row.topic = topic;
        }
        if let Some(category) = &patch.category {
            row.category = category_of(category);
        }
        if let Some(facets) = &patch.facets {
            row.facets = clean_facets(facets.iter().map(String::as_str));
        }
        if let Some(depth) = patch.depth {
            row.depth = depth;
        }
        if let Some(news) = patch.news {
            row.news = news;
        }
        if let Some(avoid) = patch.avoid {
            row.avoid = avoid;
        }
        if let Some(confirmed) = patch.confirmed {
            row.confirmed = confirmed;
        }
        row.updated_at = Utc::now();
        Ok(Some(row.clone()))
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool> {
        let mut rows = self.rows.lock().unwrap();
        let before = rows.len();
        rows.retain(|r| !(r.profile_id == profile_id && r.id == id));
        Ok(rows.len() < before)
    }

    async fn last_changed(&self, profile_id: &str) -> Result<Option<DateTime<Utc>>> {
        let rows = self.rows.lock().unwrap();
        Ok(rows
            .iter()
            .filter(|r| r.profile_id == profile_id && (r.confirmed || r.evidence >= STRONG_AT))
            .map(|r| r.updated_at)
            .max())
    }

    async fn without_picture(&self, profile_id: &str, limit: usize) -> Result<Vec<Interest>> {
        let looked = self.pictures_looked_for.lock().unwrap();
        let rows = self.rows.lock().unwrap();
        Ok(sorted(
            rows.iter()
                .filter(|r| r.profile_id == profile_id && !looked.contains(&r.id))
                .cloned()
                .collect(),
        )
        .into_iter()
        .take(limit)
        .collect())
    }

    async fn set_picture(&self, profile_id: &str, id: &str, key: Option<&str>) -> Result<()> {
        self.pictures_looked_for
            .lock()
            .unwrap()
            .insert(id.to_string());
        let mut rows = self.rows.lock().unwrap();
        if let Some(row) = rows
            .iter_mut()
            .find(|r| r.profile_id == profile_id && r.id == id)
        {
            row.picture = key.map(picture_path);
        }
        Ok(())
    }

    async fn portrait(&self, profile_id: &str) -> Result<Option<MemberPortrait>> {
        Ok(self.portraits.lock().unwrap().get(profile_id).cloned())
    }

    async fn save_portrait(&self, profile_id: &str, portrait: &MemberPortrait) -> Result<()> {
        self.portraits
            .lock()
            .unwrap()
            .insert(profile_id.to_string(), portrait.clone());
        Ok(())
    }

    async fn interview(&self, profile_id: &str) -> Result<Option<InterestInterview>> {
        Ok(self.interviews.lock().unwrap().get(profile_id).cloned())
    }

    async fn save_interview(&self, profile_id: &str, interview: &InterestInterview) -> Result<()> {
        let mut stamped = interview.clone();
        stamped.updated_at = Some(Utc::now());
        self.interviews
            .lock()
            .unwrap()
            .insert(profile_id.to_string(), stamped);
        Ok(())
    }
    async fn member_interview(&self, profile_id: &str) -> Result<Option<MemberInterview>> {
        Ok(self
            .member_interviews
            .lock()
            .unwrap()
            .get(profile_id)
            .cloned())
    }

    async fn save_member_interview(
        &self,
        profile_id: &str,
        interview: &MemberInterview,
    ) -> Result<()> {
        let mut stamped = interview.clone();
        stamped.updated_at = Some(Utc::now());
        self.member_interviews
            .lock()
            .unwrap()
            .insert(profile_id.to_string(), stamped);
        Ok(())
    }
}
