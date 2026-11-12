//! In-memory news store and a scripted web, for the news services' tests.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};

use crate::shared::domain::news::{
    Digest, DigestEntry, InterestRow, NewsBook, NewsSettings, Portrait, Story, StoryKind, Topic,
    Vote,
};
use crate::shared::ports::news::{
    FoundPage, Freshness, NewsStore, NewsWeb, ReadPage, SearchBlocked, SearchFailure,
};
use crate::shared::services::news::dedupe::url_key;

#[derive(Default)]
pub struct MockNewsStore {
    pub settings: Mutex<HashMap<String, NewsSettings>>,
    pub books: Mutex<HashMap<String, NewsBook>>,
    pub members: Mutex<Vec<String>>,
    pub interests: Mutex<HashMap<String, Vec<InterestRow>>>,
    pub portraits: Mutex<HashMap<String, Portrait>>,
    pub stories: Mutex<Vec<Story>>,
    pub digests: Mutex<Vec<Digest>>,
    pub topics: Mutex<Vec<Topic>>,
}

impl MockNewsStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_interests(self, member: &str, rows: Vec<InterestRow>) -> Self {
        self.members.lock().unwrap().push(member.to_string());
        self.interests
            .lock()
            .unwrap()
            .insert(member.to_string(), rows);
        self
    }

    /// The member's stories a list shows, newest first.
    fn listed(&self, member: &str) -> Vec<Story> {
        let mut found: Vec<Story> = self
            .stories
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.profile_id == member && s.vote != Some(Vote::Wrong))
            .cloned()
            .collect();
        found.sort_by(|a, b| b.written_at.cmp(&a.written_at));
        found
    }

    pub fn stories_of(&self, member: &str) -> Vec<Story> {
        self.stories
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.profile_id == member)
            .cloned()
            .collect()
    }
}

#[async_trait]
impl NewsStore for MockNewsStore {
    async fn settings(&self, member: &str) -> Result<NewsSettings> {
        Ok(self
            .settings
            .lock()
            .unwrap()
            .get(member)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_settings(&self, member: &str, settings: &NewsSettings) -> Result<()> {
        self.settings
            .lock()
            .unwrap()
            .insert(member.to_string(), settings.clone());
        Ok(())
    }

    async fn book(&self, member: &str) -> Result<NewsBook> {
        let mut book = self
            .books
            .lock()
            .unwrap()
            .get(member)
            .cloned()
            .unwrap_or_default();
        book.map_built_at = self
            .topics
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.profile_id == member)
            .map(|t| t.built_at)
            .max();
        Ok(book)
    }

    async fn request_refresh(&self, member: &str, at: DateTime<Utc>) -> Result<()> {
        self.books
            .lock()
            .unwrap()
            .entry(member.to_string())
            .or_default()
            .refresh_requested_at = Some(at);
        Ok(())
    }

    async fn begin_writing(&self, member: &str, at: DateTime<Utc>) -> Result<()> {
        let mut books = self.books.lock().unwrap();
        let book = books.entry(member.to_string()).or_default();
        book.writing_since = Some(at);
        book.refresh_requested_at = None;
        Ok(())
    }

    async fn end_writing(&self, member: &str, stories_done: Option<DateTime<Utc>>) -> Result<()> {
        let mut books = self.books.lock().unwrap();
        let book = books.entry(member.to_string()).or_default();
        book.writing_since = None;
        if stories_done.is_some() {
            book.stories_written_at = stories_done;
        }
        Ok(())
    }

    async fn mark_spoken(&self, member: &str, day: NaiveDate) -> Result<()> {
        self.books
            .lock()
            .unwrap()
            .entry(member.to_string())
            .or_default()
            .spoken_on = Some(day);
        Ok(())
    }

    async fn members(&self) -> Result<Vec<String>> {
        Ok(self.members.lock().unwrap().clone())
    }

    async fn interests(&self, member: &str) -> Result<Vec<InterestRow>> {
        Ok(self
            .interests
            .lock()
            .unwrap()
            .get(member)
            .cloned()
            .unwrap_or_default())
    }

    async fn portrait(&self, member: &str) -> Result<Option<Portrait>> {
        Ok(self.portraits.lock().unwrap().get(member).cloned())
    }

    async fn topics(&self, member: &str) -> Result<Vec<Topic>> {
        let mut topics: Vec<Topic> = self
            .topics
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.profile_id == member)
            .cloned()
            .collect();
        topics.sort_by(|a, b| b.score.total_cmp(&a.score));
        Ok(topics)
    }

    async fn replace_topics(&self, member: &str, topics: &[Topic]) -> Result<()> {
        let mut all = self.topics.lock().unwrap();
        let old: Vec<Topic> = all
            .iter()
            .filter(|t| t.profile_id == member)
            .cloned()
            .collect();
        all.retain(|t| t.profile_id != member);
        for topic in topics {
            let mut topic = topic.clone();
            if let Some(was) = old
                .iter()
                .find(|o| o.query.eq_ignore_ascii_case(&topic.query))
            {
                topic.id = was.id.clone();
                topic.used_at = was.used_at;
            }
            all.push(topic);
        }
        Ok(())
    }

    async fn mark_topics_used(
        &self,
        member: &str,
        ids: &[String],
        at: DateTime<Utc>,
    ) -> Result<()> {
        for topic in self.topics.lock().unwrap().iter_mut() {
            if topic.profile_id == member && ids.contains(&topic.id) {
                topic.used_at = Some(at);
            }
        }
        Ok(())
    }

    async fn count_stories(
        &self,
        member: &str,
        kind: StoryKind,
        since: DateTime<Utc>,
    ) -> Result<u32> {
        Ok(self
            .stories
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.profile_id == member && s.kind == kind && s.written_at >= since)
            .count() as u32)
    }

    async fn interest_stories(
        &self,
        member: &str,
        interest_id: &str,
        limit: usize,
    ) -> Result<Vec<Story>> {
        let mut found: Vec<Story> = self
            .listed(member)
            .into_iter()
            .filter(|s| s.interest.id == interest_id)
            .collect();
        found.truncate(limit);
        Ok(found)
    }

    async fn read_stories(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>> {
        let mut found: Vec<Story> = self
            .listed(member)
            .into_iter()
            .filter(|s| s.read_at.is_some_and(|at| at >= since))
            .collect();
        found.sort_by(|a, b| b.read_at.cmp(&a.read_at));
        Ok(found)
    }

    async fn voted_stories(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>> {
        Ok(self
            .listed(member)
            .into_iter()
            .filter(|s| matches!(s.vote, Some(Vote::Up | Vote::Down)) && s.written_at >= since)
            .collect())
    }

    async fn mark_read(&self, member: &str, id: &str, at: DateTime<Utc>) -> Result<bool> {
        let mut stories = self.stories.lock().unwrap();
        let Some(story) = stories
            .iter_mut()
            .find(|s| s.profile_id == member && s.id == id)
        else {
            return Ok(false);
        };
        story.read_at = Some(at);
        Ok(true)
    }

    async fn set_vote(
        &self,
        member: &str,
        id: &str,
        vote: Vote,
        _note: Option<&str>,
        _at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut stories = self.stories.lock().unwrap();
        let Some(story) = stories
            .iter_mut()
            .find(|s| s.profile_id == member && s.id == id)
        else {
            return Ok(false);
        };
        story.vote = Some(vote);
        Ok(true)
    }

    async fn add_story(&self, story: &Story) -> Result<bool> {
        let mut stories = self.stories.lock().unwrap();
        let key = url_key(&story.url);
        if stories
            .iter()
            .any(|s| s.profile_id == story.profile_id && url_key(&s.url) == key)
        {
            return Ok(false);
        }
        stories.push(story.clone());
        Ok(true)
    }

    async fn stories_since(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>> {
        Ok(self
            .listed(member)
            .into_iter()
            .filter(|s| s.written_at >= since)
            .collect())
    }

    async fn story(&self, member: &str, id: &str) -> Result<Option<Story>> {
        Ok(self
            .stories
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.profile_id == member && s.id == id)
            .cloned())
    }

    async fn has_page(&self, member: &str, key: &str) -> Result<bool> {
        Ok(self
            .stories
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.profile_id == member && url_key(&s.url) == key))
    }

    async fn prune_stories(&self, before: DateTime<Utc>) -> Result<u64> {
        let mut stories = self.stories.lock().unwrap();
        let had = stories.len();
        stories.retain(|s| s.written_at >= before);
        Ok((had - stories.len()) as u64)
    }

    async fn add_digest(&self, digest: &Digest) -> Result<()> {
        self.digests.lock().unwrap().push(digest.clone());
        Ok(())
    }

    async fn digest(&self, member: &str, id: &str) -> Result<Option<Digest>> {
        Ok(self
            .digests
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.profile_id == member && d.id == id)
            .cloned())
    }

    async fn digest_on(&self, member: &str, day: NaiveDate) -> Result<Option<Digest>> {
        Ok(self
            .digests
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.profile_id == member && d.day == day)
            .cloned())
    }

    async fn latest_digest(&self, member: &str) -> Result<Option<Digest>> {
        Ok(self
            .digests
            .lock()
            .unwrap()
            .iter()
            .filter(|d| d.profile_id == member)
            .max_by_key(|d| d.written_at)
            .cloned())
    }

    async fn digests(&self, member: &str) -> Result<Vec<DigestEntry>> {
        let mut entries: Vec<DigestEntry> = self
            .digests
            .lock()
            .unwrap()
            .iter()
            .filter(|d| d.profile_id == member)
            .map(|d| DigestEntry {
                id: d.id.clone(),
                title: d.title.clone(),
                written_at: d.written_at,
            })
            .collect();
        entries.sort_by(|a, b| b.written_at.cmp(&a.written_at));
        Ok(entries)
    }
}

/// A web of fixed pages: each query finds what it was given, and every page reads as given.
#[derive(Default)]
pub struct MockNewsWeb {
    pub blocked: Option<SearchBlocked>,
    pub results: Mutex<HashMap<String, Vec<FoundPage>>>,
    pub pages: Mutex<HashMap<String, ReadPage>>,
    pub pictures: Mutex<HashMap<String, String>>,
    /// Every search asked, in order.
    pub searches: Mutex<Vec<(String, Freshness)>>,
}

impl MockNewsWeb {
    pub fn new() -> Self {
        Self::default()
    }

    /// A page `query` finds, readable with `text`.
    pub fn with_page(self, query: &str, url: &str, title: &str, text: &str) -> Self {
        self.results
            .lock()
            .unwrap()
            .entry(query.to_string())
            .or_default()
            .push(FoundPage {
                url: url.to_string(),
                title: title.to_string(),
            });
        self.pages.lock().unwrap().insert(
            url.to_string(),
            ReadPage {
                url: url.to_string(),
                title: title.to_string(),
                site: url_key(url)
                    .split('/')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                lines: text.lines().map(str::to_string).collect(),
                published_at: None,
            },
        );
        self
    }

    pub fn with_picture(self, url: &str, key: &str) -> Self {
        self.pictures
            .lock()
            .unwrap()
            .insert(url.to_string(), key.to_string());
        self
    }
}

#[async_trait]
impl NewsWeb for MockNewsWeb {
    async fn blocked(&self) -> Option<SearchBlocked> {
        self.blocked
    }

    async fn find(
        &self,
        query: &str,
        freshness: Freshness,
    ) -> std::result::Result<Vec<FoundPage>, SearchFailure> {
        self.searches
            .lock()
            .unwrap()
            .push((query.to_string(), freshness));
        Ok(self
            .results
            .lock()
            .unwrap()
            .get(query)
            .cloned()
            .unwrap_or_default())
    }

    async fn read(&self, url: &str) -> Option<ReadPage> {
        self.pages.lock().unwrap().get(url).cloned()
    }

    async fn picture(&self, url: &str) -> Option<String> {
        self.pictures.lock().unwrap().get(url).cloned()
    }
}
