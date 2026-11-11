//! Writing a member's news: the topic map from what is known about them, stories from pages found
//! for its newsworthy topics, an occasional long read from the rest, and the digest from the
//! day's stories. Each pass is bounded in time and in what it spends, and keeps each story as it
//! is written, so a pass cut short loses only the story in hand.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};

use super::coverage::{covered, queries_for};
use super::dedupe::{same_news, url_key};
use super::prompts::{
    self, fallback_intro, first_sentences, intro_prompt, link_citations, long_read_prompt,
    parse_intro, parse_section, parse_story, parse_written, section_prompt, story_prompt,
    text_chars, MemberContext, PageForPrompt, SectionSource, StoryRefusal, EXTRA_PAGE_CHARS,
    INTRO_SYSTEM, LONG_READ_SYSTEM, LONG_READ_WORDS, MIN_PAGE_CHARS, SECTION_SYSTEM, STORY_SYSTEM,
};
use super::schedule::{StoryPass, DIGEST_PASS_BUDGET, STORIES_PASS_BUDGET};
use super::topic_map::{
    self, apply_votes, areas_prompt, areas_system, clean, fallback_topics, parse_areas,
    parse_topics, route, topics_prompt, topics_system, Area, MapSources, Route, TARGET_TOPICS,
};
use crate::models::domain::message::ChatMessage;
use crate::models::ports::provider::LlmProvider;
use crate::shared::domain::news::{
    Digest, DigestPicture, DigestSection, InterestRef, InterestRow, NewsSettings, Reference, Story,
    StoryKind, Topic,
};
use crate::shared::ports::news::{
    FoundPage, Freshness, NewsStore, NewsWeb, ReadPage, SearchBlocked, SearchFailure,
};
use crate::user_data::ports::profile::ProfileRepository;

/// How far back a new story is compared with the member's own, by page and by headline.
pub const DEDUPE_WINDOW: chrono::Duration = chrono::Duration::days(3);
/// Votes the map reads, at most this old.
pub const VOTES_WINDOW: chrono::Duration = chrono::Duration::days(30);
/// A page older than this is not news, whatever a search thought.
const STALE_AFTER: chrono::Duration = chrono::Duration::days(7);
/// How far back the digest looks for stories when there is no earlier digest.
const DIGEST_LOOKBACK: chrono::Duration = chrono::Duration::hours(30);
/// A topic searched this recently waits behind the rest, so passes take turns through the map.
const TOPIC_RESTS_FOR: chrono::Duration = chrono::Duration::hours(10);
/// Long reads a week at most, and the shortest gap between two.
const LONG_READS_A_WEEK: u32 = 2;
const LONG_READ_GAP: chrono::Duration = chrono::Duration::days(3);
/// An evergreen topic the ideas or a long read used is left this long.
const EVERGREEN_RESTS_FOR: chrono::Duration = chrono::Duration::days(7);
/// Pages a long read is written from: at least two, at most three.
const LONG_READ_PAGES: (usize, usize) = (2, 3);
/// Stories a digest section is written from at most.
const STORIES_PER_SECTION: usize = 4;
/// Pages read for a section that has no story of its own.
const EXTRA_PAGES: usize = 2;
/// Sections a digest has at most.
const MAX_SECTIONS: usize = 6;
/// Pictures a section shows at most.
const PICTURES_PER_SECTION: usize = 2;

/// What one pass may spend.
#[derive(Debug, Clone, Copy)]
pub struct PassLimits {
    pub budget: Duration,
    pub per_interest: usize,
    pub per_pass: usize,
    /// Topics searched in one pass; the rest wait for the next.
    pub topics_per_pass: usize,
    /// One model call at most; a call past it is abandoned.
    pub call_timeout: Duration,
}

impl PassLimits {
    pub fn stories() -> Self {
        Self {
            budget: STORIES_PASS_BUDGET,
            per_interest: 3,
            per_pass: 12,
            topics_per_pass: 6,
            call_timeout: Duration::from_secs(150),
        }
    }

    pub fn digest() -> Self {
        Self {
            budget: DIGEST_PASS_BUDGET,
            ..Self::stories()
        }
    }
}

/// What a story pass did, for the trace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoryReport {
    /// The interests and areas the pass wrote for.
    pub groups: usize,
    pub searches: usize,
    pub candidates: usize,
    pub unread: usize,
    pub written: usize,
    pub refused: usize,
    pub duplicates: usize,
    pub long_read: bool,
    pub out_of_time: bool,
    pub blocked: Option<SearchBlocked>,
}

/// What a map rebuild did, for the trace.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MapReport {
    pub areas: usize,
    pub topics: usize,
    pub newsworthy: usize,
    /// The model wrote no usable map, so it was made from the interests alone.
    pub fallback: bool,
}

/// The day as prompts name it, and the member's local date.
#[derive(Debug, Clone)]
pub struct Today {
    pub label: String,
    pub weekday: String,
    pub date: NaiveDate,
}

impl Today {
    pub fn of(now: DateTime<Utc>, zone: chrono_tz::Tz) -> Self {
        let local = now.with_timezone(&zone);
        Self {
            label: local.format("%A %-d %B %Y").to_string(),
            weekday: local.format("%A").to_string(),
            date: local.date_naive(),
        }
    }
}

/// What a map is written from besides the news's own tables.
#[derive(Debug, Clone, Default)]
pub struct MapExtras {
    pub memories: Vec<String>,
    pub routines: Vec<String>,
}

pub struct NewsWriter {
    store: Arc<dyn NewsStore>,
    web: Arc<dyn NewsWeb>,
    profiles: Arc<dyn ProfileRepository + Send + Sync>,
}

/// A page a search found for one topic, waiting to be read.
struct Candidate {
    url: String,
    query: String,
}

/// The interests a pass routes by: those the news covers, and those the member avoids, so a
/// topic naming an avoided one goes nowhere.
fn routable(rows: &[InterestRow]) -> Vec<InterestRow> {
    let mut all = covered(rows);
    all.extend(rows.iter().filter(|r| r.avoid).cloned());
    all
}

impl NewsWriter {
    pub fn new(
        store: Arc<dyn NewsStore>,
        web: Arc<dyn NewsWeb>,
        profiles: Arc<dyn ProfileRepository + Send + Sync>,
    ) -> Self {
        Self {
            store,
            web,
            profiles,
        }
    }

    /// The name a prompt or the speaker uses: the member's preferred name, else their profile's.
    pub async fn member_name(&self, member: &str) -> Option<String> {
        let profile = self.profiles.get(member).await.ok().flatten()?;
        let name = profile
            .preferences
            .get("preferred_name")
            .map(|n| n.trim())
            .filter(|n| !n.is_empty())
            .unwrap_or(profile.display_name.trim())
            .to_string();
        (!name.is_empty()).then_some(name)
    }

    async fn context(&self, member: &str, interests: &[InterestRow]) -> MemberContext {
        MemberContext {
            name: self
                .member_name(member)
                .await
                .unwrap_or_else(|| "this member".to_string()),
            portrait: self.store.portrait(member).await.ok().flatten(),
            interests: interests.iter().map(|i| i.topic.clone()).collect(),
        }
    }

    /// Whether the member has any interest the news may cover.
    pub async fn has_interests(&self, member: &str) -> Result<bool> {
        Ok(!covered(&self.store.interests(member).await?).is_empty())
    }

    /// Rebuilds the member's topic map: the areas of their life first, then each area's topics,
    /// cleaned, moved by their votes and kept. A member with no interest gets no map.
    pub async fn build_map(
        &self,
        model: &dyn LlmProvider,
        member: &str,
        extras: MapExtras,
        now: DateTime<Utc>,
        today: &Today,
        limits: &PassLimits,
    ) -> Result<MapReport> {
        let started = Instant::now();
        let rows = self.store.interests(member).await?;
        let interests = covered(&rows);
        let mut report = MapReport::default();
        if interests.is_empty() {
            return Ok(report);
        }
        let context = self.context(member, &interests).await;
        let sources = MapSources {
            name: context.name.clone(),
            today: today.label.clone(),
            interests: interests.clone(),
            avoid: rows
                .iter()
                .filter(|r| r.avoid)
                .map(|r| r.topic.clone())
                .collect(),
            portrait: context.portrait.clone(),
            memories: extras.memories,
            routines: extras.routines,
            votes: self.store.voted_stories(member, now - VOTES_WINDOW).await?,
        };
        let mut areas = match self
            .ask(
                model,
                &areas_system(TARGET_TOPICS),
                areas_prompt(&sources),
                limits,
            )
            .await
        {
            Some(reply) => parse_areas(&reply, TARGET_TOPICS),
            None => Vec::new(),
        };
        if areas.is_empty() {
            areas = interests
                .iter()
                .map(|i| Area {
                    name: i.topic.clone(),
                    score: 50.0,
                    topics: (TARGET_TOPICS / interests.len()).clamp(3, 8),
                })
                .collect();
        }
        report.areas = areas.len();
        let priorities = sources.priorities();
        let mut by_area: Vec<(String, Vec<topic_map::RawTopic>)> = Vec::new();
        let mut taken: Vec<String> = Vec::new();
        for area in &areas {
            if started.elapsed() >= limits.budget {
                tracing::info!(target: "giap::trace", kind = "news_map_out_of_time", areas = by_area.len());
                break;
            }
            let Some(reply) = self
                .ask(
                    model,
                    &topics_system(area.topics, &priorities),
                    topics_prompt(&sources, area, &taken),
                    limits,
                )
                .await
            else {
                continue;
            };
            let topics = parse_topics(&reply);
            for topic in &topics {
                if !taken.contains(&topic.subject) {
                    taken.push(topic.subject.clone());
                }
            }
            by_area.push((area.name.clone(), topics));
        }
        let mut avoid = sources.avoid.clone();
        avoid.extend(curiosities(&rows, &interests));
        let mut map = apply_votes(
            clean(member, by_area, &avoid, now),
            &sources.votes,
            &routable(&rows),
        );
        if map.is_empty() {
            report.fallback = true;
            map = fallback_topics(member, &interests, now);
        }
        report.topics = map.len();
        report.newsworthy = map.iter().filter(|t| t.newsworthy).count();
        self.store.replace_topics(member, &map).await?;
        Ok(report)
    }

    /// The member's routed topics, from their map or, with none yet, their interests.
    async fn routed_topics(
        &self,
        member: &str,
        rows: &[InterestRow],
        now: DateTime<Utc>,
    ) -> Result<Vec<(Topic, Route)>> {
        let mut topics = self.store.topics(member).await?;
        if topics.is_empty() {
            topics = fallback_topics(member, &covered(rows), now);
        }
        let routable = routable(rows);
        Ok(topics
            .into_iter()
            .filter_map(|t| route(&t, &routable).map(|r| (t, r)))
            .collect())
    }

    /// One story pass for one member: search the map's newsworthy topics, read the pages, write
    /// the stories, and now and then a long read.
    #[allow(clippy::too_many_arguments)]
    pub async fn write_stories(
        &self,
        model: &dyn LlmProvider,
        member: &str,
        pass: StoryPass,
        settings: &NewsSettings,
        now: DateTime<Utc>,
        today: &Today,
        limits: &PassLimits,
    ) -> Result<StoryReport> {
        let started = Instant::now();
        let mut report = StoryReport::default();
        let rows = self.store.interests(member).await?;
        let interests = covered(&rows);
        if interests.is_empty() || !(settings.tiles || settings.digest) {
            return Ok(report);
        }
        let wanted = |r: &Route| {
            (settings.tiles && r.tiles) || (settings.digest && pass.feeds_digest() && r.digest)
        };
        let routed = self.routed_topics(member, &rows, now).await?;
        let mut news: Vec<(Topic, Route)> = routed
            .iter()
            .filter(|(t, r)| t.newsworthy && wanted(r))
            .cloned()
            .collect();
        let evergreen = routed.iter().any(|(t, r)| !t.newsworthy && r.tiles);
        if news.is_empty() && !(evergreen && settings.tiles) {
            return Ok(report);
        }
        if let Some(blocked) = self.web.blocked().await {
            report.blocked = Some(blocked);
            return Ok(report);
        }
        let context = self.context(member, &interests).await;
        let recent = self
            .store
            .stories_since(member, now - DEDUPE_WINDOW)
            .await?;
        let mut seen: Vec<String> = recent
            .iter()
            .flat_map(|s| [s.headline.clone(), s.source_title.clone()])
            .collect();

        news.sort_by(|(a, _), (b, _)| {
            let rested = |t: &Topic| t.used_at.is_some_and(|at| now - at < TOPIC_RESTS_FOR);
            rested(a).cmp(&rested(b)).then(b.score.total_cmp(&a.score))
        });
        let chosen = take_turns(news, limits.topics_per_pass);
        let mut groups: Vec<(Route, VecDeque<Candidate>, usize)> = Vec::new();
        let mut taken: Vec<String> = Vec::new();
        let mut used: Vec<String> = Vec::new();
        'topics: for (topic, route) in chosen {
            let at = match groups
                .iter()
                .position(|(r, _, _)| r.interest.id == route.interest.id)
            {
                Some(at) => at,
                None => {
                    groups.push((route.clone(), VecDeque::new(), 0));
                    groups.len() - 1
                }
            };
            // Each search costs the household's allowance: a group with enough to read is not
            // searched again, and the week is asked only when the day found nothing new.
            if groups[at].1.len() > limits.per_interest {
                continue;
            }
            let before = groups[at].1.len();
            for freshness in [Freshness::Day, Freshness::Week] {
                if groups[at].1.len() > before {
                    break;
                }
                report.searches += 1;
                let found = match self.web.find(&topic.query, freshness).await {
                    Ok(found) => found,
                    Err(SearchFailure::Blocked(blocked)) => {
                        report.blocked = Some(blocked);
                        break 'topics;
                    }
                    Err(SearchFailure::Failed(why)) => {
                        tracing::info!(target: "giap::trace", kind = "news_search_failed", why = %why);
                        continue;
                    }
                };
                for page in found {
                    if self.is_new(member, &page, &taken, &seen).await {
                        taken.push(url_key(&page.url));
                        groups[at].1.push_back(Candidate {
                            url: page.url,
                            query: topic.query.clone(),
                        });
                    }
                }
            }
            used.push(topic.id.clone());
        }
        self.store.mark_topics_used(member, &used, now).await?;
        report.groups = groups.len();
        report.candidates = groups.iter().map(|(_, q, _)| q.len()).sum();

        'rounds: loop {
            let mut tried = false;
            for (route, queue, written) in groups.iter_mut() {
                if report.written >= limits.per_pass {
                    break 'rounds;
                }
                if started.elapsed() >= limits.budget {
                    report.out_of_time = true;
                    return Ok(report);
                }
                if *written >= limits.per_interest {
                    continue;
                }
                let Some(candidate) = queue.pop_front() else {
                    continue;
                };
                tried = true;
                match self
                    .write_one(
                        model, member, route, &candidate, &context, &interests, today, now, limits,
                    )
                    .await?
                {
                    Written::Story(story) => {
                        if seen.iter().any(|h| same_news(h, &story.headline)) {
                            report.duplicates += 1;
                            continue;
                        }
                        if self.store.add_story(&story).await? {
                            seen.push(story.headline.clone());
                            seen.push(story.source_title.clone());
                            *written += 1;
                            report.written += 1;
                        } else {
                            report.duplicates += 1;
                        }
                    }
                    Written::Unread => report.unread += 1,
                    Written::Refused => report.refused += 1,
                }
            }
            if !tried {
                break;
            }
        }

        if matches!(pass, StoryPass::Midday | StoryPass::Evening)
            && settings.tiles
            && started.elapsed() < limits.budget
            && report.blocked.is_none()
            && self.long_read_due(member, now).await?
        {
            report.long_read = self
                .write_long_read(model, member, &routed, &context, today, now, limits)
                .await?;
        }
        Ok(report)
    }

    async fn is_new(
        &self,
        member: &str,
        page: &FoundPage,
        taken: &[String],
        seen: &[String],
    ) -> bool {
        let key = url_key(&page.url);
        if taken.contains(&key) {
            return false;
        }
        if self.store.has_page(member, &key).await.unwrap_or(true) {
            return false;
        }
        !seen.iter().any(|h| same_news(h, &page.title))
    }

    /// The page, if it is readable, new to the member, and recent enough to be news.
    async fn read_new(&self, member: &str, url: &str) -> Result<Option<ReadPage>> {
        let Some(page) = self.web.read(url).await else {
            return Ok(None);
        };
        if text_chars(&page.lines) < MIN_PAGE_CHARS {
            return Ok(None);
        }
        if url_key(&page.url) != url_key(url)
            && self.store.has_page(member, &url_key(&page.url)).await?
        {
            return Ok(None);
        }
        Ok(Some(page))
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_one(
        &self,
        model: &dyn LlmProvider,
        member: &str,
        route: &Route,
        candidate: &Candidate,
        context: &MemberContext,
        interests: &[InterestRow],
        today: &Today,
        now: DateTime<Utc>,
        limits: &PassLimits,
    ) -> Result<Written> {
        let Some(page) = self.read_new(member, &candidate.url).await? else {
            return Ok(Written::Unread);
        };
        if page.published_at.is_some_and(|at| now - at > STALE_AFTER) {
            return Ok(Written::Unread);
        }
        let interest = interest_for(route, interests);
        let published = page.published_at.map(|t| t.format("%-d %B %Y").to_string());
        let prompt = story_prompt(
            context,
            &interest,
            &today.label,
            &PageForPrompt {
                url: &page.url,
                title: &page.title,
                site: &page.site,
                published: published.as_deref(),
                lines: &page.lines,
            },
        );
        let Some(reply) = self.ask(model, STORY_SYSTEM, prompt, limits).await else {
            return Ok(Written::Refused);
        };
        let written = match parse_story(&reply, &page.lines, &today.label) {
            Ok(written) => written,
            Err(refusal) => {
                trace_refusal(&refusal);
                return Ok(Written::Refused);
            }
        };
        let picture_key = self.web.picture(&page.url).await;
        Ok(Written::Story(Box::new(Story {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: member.to_string(),
            kind: StoryKind::News,
            interest: route.interest.clone(),
            headline: written.headline,
            dek: written.dek,
            summary: written.summary,
            why_you: written.why_you,
            url: page.url.clone(),
            site: page.site.clone(),
            source_title: page.title.clone(),
            further_sources: Vec::new(),
            picture_key,
            found_by: Some(candidate.query.clone()),
            published_at: page.published_at,
            written_at: Utc::now(),
            read_at: None,
            vote: None,
        })))
    }

    /// Whether a long read may be written now: fewer than two this week, none in three days.
    async fn long_read_due(&self, member: &str, now: DateTime<Utc>) -> Result<bool> {
        let week = self
            .store
            .count_stories(member, StoryKind::LongRead, now - chrono::Duration::days(7))
            .await?;
        let lately = self
            .store
            .count_stories(member, StoryKind::LongRead, now - LONG_READ_GAP)
            .await?;
        Ok(week < LONG_READS_A_WEEK && lately == 0)
    }

    /// A long read on the best evergreen topic no one used this week, from two or three pages.
    #[allow(clippy::too_many_arguments)]
    async fn write_long_read(
        &self,
        model: &dyn LlmProvider,
        member: &str,
        routed: &[(Topic, Route)],
        context: &MemberContext,
        today: &Today,
        now: DateTime<Utc>,
        limits: &PassLimits,
    ) -> Result<bool> {
        let Some((topic, route)) = routed
            .iter()
            .filter(|(t, r)| !t.newsworthy && r.tiles)
            .filter(|(t, _)| t.used_at.is_none_or(|at| now - at >= EVERGREEN_RESTS_FOR))
            .max_by(|(a, _), (b, _)| a.score.total_cmp(&b.score))
        else {
            return Ok(false);
        };
        self.store
            .mark_topics_used(member, std::slice::from_ref(&topic.id), now)
            .await?;
        let found = match self.web.find(&topic.query, Freshness::Any).await {
            Ok(found) => found,
            Err(_) => return Ok(false),
        };
        let mut pages: Vec<ReadPage> = Vec::new();
        for page in found {
            if pages.len() >= LONG_READ_PAGES.1 {
                break;
            }
            if self.store.has_page(member, &url_key(&page.url)).await? {
                continue;
            }
            if let Some(read) = self.read_new(member, &page.url).await? {
                if !pages.iter().any(|p| url_key(&p.url) == url_key(&read.url)) {
                    pages.push(read);
                }
            }
        }
        if pages.len() < LONG_READ_PAGES.0 {
            return Ok(false);
        }
        let quoted: Vec<PageForPrompt<'_>> = pages
            .iter()
            .map(|p| PageForPrompt {
                url: &p.url,
                title: &p.title,
                site: &p.site,
                published: None,
                lines: &p.lines,
            })
            .collect();
        let prompt = long_read_prompt(context, &topic.subject, &topic.query, &today.label, &quoted);
        let Some(reply) = self.ask(model, LONG_READ_SYSTEM, prompt, limits).await else {
            return Ok(false);
        };
        let all_lines: Vec<String> = pages.iter().flat_map(|p| p.lines.clone()).collect();
        let written = match parse_written(&reply, &all_lines, &today.label, LONG_READ_WORDS) {
            Ok(written) => written,
            Err(refusal) => {
                trace_refusal(&refusal);
                return Ok(false);
            }
        };
        let urls: Vec<String> = pages.iter().map(|p| p.url.clone()).collect();
        let lead = &pages[0];
        let story = Story {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: member.to_string(),
            kind: StoryKind::LongRead,
            interest: route.interest.clone(),
            headline: written.headline,
            dek: written.dek,
            summary: link_citations(&written.summary, &urls),
            why_you: written.why_you,
            url: lead.url.clone(),
            site: lead.site.clone(),
            source_title: lead.title.clone(),
            further_sources: pages[1..]
                .iter()
                .map(|p| Reference {
                    title: reference_title(&p.title, &p.site),
                    url: p.url.clone(),
                    site: p.site.clone(),
                })
                .collect(),
            picture_key: self.web.picture(&lead.url).await,
            found_by: Some(topic.query.clone()),
            published_at: None,
            written_at: Utc::now(),
            read_at: None,
            vote: None,
        };
        self.store.add_story(&story).await
    }

    /// One model call within the pass's call limit; `None` on a failure.
    async fn ask(
        &self,
        model: &dyn LlmProvider,
        system: &str,
        prompt: String,
        limits: &PassLimits,
    ) -> Option<String> {
        let call = model.complete(system, vec![ChatMessage::user(prompt)]);
        match tokio::time::timeout(limits.call_timeout, call).await {
            Ok(Ok(reply)) => Some(reply.content),
            Ok(Err(e)) => {
                tracing::warn!(target: "giap::trace", kind = "news_model_failed", error = %e);
                None
            }
            Err(_) => {
                tracing::warn!(target: "giap::trace", kind = "news_model_timeout");
                None
            }
        }
    }

    /// The digest for `today`: a section per interest or area from the news stories since the
    /// last digest, with extra reading for a digest interest that has none. `None` when nothing
    /// could be written.
    pub async fn write_digest(
        &self,
        model: &dyn LlmProvider,
        member: &str,
        settings: &NewsSettings,
        now: DateTime<Utc>,
        today: &Today,
        limits: &PassLimits,
    ) -> Result<Option<Digest>> {
        let started = Instant::now();
        let rows = self.store.interests(member).await?;
        let interests = covered(&rows);
        if !settings.digest || interests.is_empty() {
            return Ok(None);
        }
        let context = self.context(member, &interests).await;
        let since = match self.store.latest_digest(member).await? {
            Some(last) => last.written_at.max(now - DIGEST_LOOKBACK),
            None => now - DIGEST_LOOKBACK,
        };
        let feeds_digest = |id: &str| match interests.iter().find(|i| i.id == id) {
            Some(i) => i.news.digest(),
            None => id.starts_with("area:"),
        };
        let stories: Vec<Story> = self
            .store
            .stories_since(member, since)
            .await?
            .into_iter()
            .filter(|s| s.kind == StoryKind::News && feeds_digest(&s.interest.id))
            .collect();

        // Groups in the order their best story came, then the digest interests with none.
        let mut groups: Vec<(InterestRef, Vec<&Story>)> = Vec::new();
        for story in &stories {
            match groups.iter_mut().find(|(i, _)| i.id == story.interest.id) {
                Some((_, list)) => list.push(story),
                None => groups.push((story.interest.clone(), vec![story])),
            }
        }
        for interest in interests.iter().filter(|i| i.news.digest()) {
            if !groups.iter().any(|(i, _)| i.id == interest.id) {
                groups.push((
                    InterestRef {
                        id: interest.id.clone(),
                        topic: interest.topic.clone(),
                    },
                    Vec::new(),
                ));
            }
        }
        let routed = self.routed_topics(member, &rows, now).await?;
        let search_blocked = self.web.blocked().await.is_some();

        let mut sections = Vec::new();
        for (group, own) in groups.into_iter().take(MAX_SECTIONS) {
            if started.elapsed() >= limits.budget {
                tracing::info!(target: "giap::trace", kind = "news_digest_out_of_time", sections = sections.len());
                break;
            }
            let own: Vec<&Story> = own.into_iter().take(STORIES_PER_SECTION).collect();
            let pages = if own.is_empty() && !search_blocked {
                let query = routed
                    .iter()
                    .filter(|(t, r)| t.newsworthy && r.interest.id == group.id)
                    .max_by(|(a, _), (b, _)| a.score.total_cmp(&b.score))
                    .map(|(t, _)| t.query.clone())
                    .or_else(|| {
                        interests
                            .iter()
                            .find(|i| i.id == group.id)
                            .and_then(|i| queries_for(i, 0).pop())
                    });
                match query {
                    Some(query) => self.extra_reading(member, &query, now).await,
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };
            let interest = interest_for(
                &Route {
                    interest: group.clone(),
                    tiles: false,
                    digest: true,
                },
                &interests,
            );
            if let Some(section) = self
                .write_section(
                    model, &context, &interest, &group, today, &own, &pages, limits,
                )
                .await
            {
                sections.push(section);
            }
        }
        if sections.is_empty() {
            return Ok(None);
        }

        let summaries: Vec<(String, String)> = sections
            .iter()
            .map(|s| (s.heading.clone(), super::speech::plain(&s.body)))
            .collect();
        let intro = match self
            .ask(
                model,
                INTRO_SYSTEM,
                intro_prompt(&context, &today.label, &summaries),
                limits,
            )
            .await
            .and_then(|reply| parse_intro(&reply))
        {
            Some(intro) => intro,
            None => fallback_intro(
                &context.name,
                &today.weekday,
                &sections
                    .iter()
                    .map(|s| s.heading.clone())
                    .collect::<Vec<_>>(),
            ),
        };
        let digest = Digest {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: member.to_string(),
            day: today.date,
            title: intro.title,
            intro: intro.intro,
            sections,
            written_at: Utc::now(),
        };
        self.store.add_digest(&digest).await?;
        Ok(Some(digest))
    }

    /// Pages read for a section that has no story since the last digest.
    async fn extra_reading(&self, member: &str, query: &str, now: DateTime<Utc>) -> Vec<ReadPage> {
        let found = match self.web.find(query, Freshness::Week).await {
            Ok(found) => found,
            Err(_) => return Vec::new(),
        };
        let mut pages = Vec::new();
        for page in found {
            if pages.len() >= EXTRA_PAGES {
                break;
            }
            if self
                .store
                .has_page(member, &url_key(&page.url))
                .await
                .unwrap_or(true)
            {
                continue;
            }
            if let Ok(Some(read)) = self.read_new(member, &page.url).await {
                if read.published_at.is_none_or(|at| now - at <= STALE_AFTER) {
                    pages.push(read);
                }
            }
        }
        pages
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_section(
        &self,
        model: &dyn LlmProvider,
        context: &MemberContext,
        interest: &InterestRow,
        group: &InterestRef,
        today: &Today,
        stories: &[&Story],
        pages: &[ReadPage],
        limits: &PassLimits,
    ) -> Option<DigestSection> {
        if stories.is_empty() && pages.is_empty() {
            return None;
        }
        let page_texts: Vec<String> = pages
            .iter()
            .map(|p| prompts::page_text(&p.lines, EXTRA_PAGE_CHARS))
            .collect();
        let story_texts: Vec<String> = stories
            .iter()
            .map(|s| format!("{}\n{}", s.dek, s.summary))
            .collect();
        let mut sources: Vec<SectionSource<'_>> = Vec::new();
        let mut references: Vec<Reference> = Vec::new();
        for (story, text) in stories.iter().zip(&story_texts) {
            sources.push(SectionSource {
                site: &story.site,
                title: &story.headline,
                text,
            });
            references.push(Reference {
                title: reference_title(&story.source_title, &story.headline),
                url: story.url.clone(),
                site: story.site.clone(),
            });
        }
        for (page, text) in pages.iter().zip(&page_texts) {
            sources.push(SectionSource {
                site: &page.site,
                title: &page.title,
                text,
            });
            references.push(Reference {
                title: reference_title(&page.title, &page.site),
                url: page.url.clone(),
                site: page.site.clone(),
            });
        }
        let urls: Vec<String> = references.iter().map(|r| r.url.clone()).collect();
        let written = self
            .ask(
                model,
                SECTION_SYSTEM,
                section_prompt(context, interest, &today.label, &sources),
                limits,
            )
            .await
            .and_then(|reply| parse_section(&reply, &urls));
        let (heading, body) = match written {
            Some(w) => (w.heading, w.body),
            None if !stories.is_empty() => fallback_section(stories),
            None => return None,
        };

        let mut pictures: Vec<DigestPicture> = stories
            .iter()
            .filter_map(|s| {
                Some(DigestPicture {
                    key: s.picture_key.clone()?,
                    caption: s.headline.clone(),
                    source_url: s.url.clone(),
                })
            })
            .take(PICTURES_PER_SECTION)
            .collect();
        for page in pages {
            if pictures.len() >= PICTURES_PER_SECTION {
                break;
            }
            if let Some(key) = self.web.picture(&page.url).await {
                pictures.push(DigestPicture {
                    key,
                    caption: reference_title(&page.title, &page.site),
                    source_url: page.url.clone(),
                });
            }
        }
        Some(DigestSection {
            interest: group.clone(),
            heading,
            body,
            pictures,
            references,
        })
    }
}

/// At most `n` topics, the groups taking turns in the order of their first topic, so one
/// interest's high scores cannot take a whole pass.
fn take_turns(sorted: Vec<(Topic, Route)>, n: usize) -> Vec<(Topic, Route)> {
    let mut groups: Vec<VecDeque<(Topic, Route)>> = Vec::new();
    for (topic, route) in sorted {
        match groups.iter_mut().find(|g| {
            g.front()
                .is_some_and(|(_, r)| r.interest.id == route.interest.id)
        }) {
            Some(group) => group.push_back((topic, route)),
            None => groups.push(VecDeque::from([(topic, route)])),
        }
    }
    let mut chosen = Vec::new();
    while chosen.len() < n && groups.iter().any(|g| !g.is_empty()) {
        for group in groups.iter_mut() {
            if chosen.len() >= n {
                break;
            }
            if let Some(next) = group.pop_front() {
                chosen.push(next);
            }
        }
    }
    chosen
}

enum Written {
    Story(Box<Story>),
    Unread,
    Refused,
}

fn trace_refusal(refusal: &StoryRefusal) {
    let why = match refusal {
        StoryRefusal::Skipped(_) => "skipped",
        StoryRefusal::Unparseable => "unparseable",
        StoryRefusal::OutOfBounds(field) => field,
        StoryRefusal::NotOnThePage(_) => "not_on_the_page",
    };
    tracing::info!(target: "giap::trace", kind = "news_story_refused", why);
}

/// The interest a route names, as a prompt describes it; an area stands in as an interest of its
/// own name.
fn interest_for(route: &Route, interests: &[InterestRow]) -> InterestRow {
    interests
        .iter()
        .find(|i| i.id == route.interest.id)
        .cloned()
        .unwrap_or_else(|| InterestRow {
            id: route.interest.id.clone(),
            topic: route.interest.topic.clone(),
            category: "other".to_string(),
            facets: Vec::new(),
            depth: "keen".to_string(),
            news: crate::shared::domain::news::NewsUse::Both,
            avoid: false,
            evidence: 0,
            confirmed: false,
        })
}

/// The guesses the member has not confirmed and has shown too seldom to count, unless an interest
/// they do follow names them: a map never builds on a question asked once or twice.
fn curiosities(rows: &[InterestRow], interests: &[InterestRow]) -> Vec<String> {
    let followed: Vec<String> = interests
        .iter()
        .flat_map(|i| std::iter::once(&i.topic).chain(i.facets.iter()))
        .map(|p| topic_map::subject_key(p))
        .collect();
    rows.iter()
        .filter(|r| !r.avoid && !interests.iter().any(|i| i.id == r.id))
        .map(|r| {
            r.topic
                .split('(')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|t| {
            let key = topic_map::subject_key(t);
            !key.is_empty()
                && !followed
                    .iter()
                    .any(|f| f.contains(&key) || key.contains(f.as_str()))
        })
        .collect()
}

fn reference_title(title: &str, fallback: &str) -> String {
    let title = title.trim();
    if title.is_empty() {
        fallback.trim().to_string()
    } else {
        title.to_string()
    }
}

/// A section from its stories alone, when the model wrote none: the lead headline, and each
/// story's dek cited.
fn fallback_section(stories: &[&Story]) -> (String, String) {
    let heading = stories[0].headline.clone();
    let body = stories
        .iter()
        .enumerate()
        .map(|(i, s)| format!("{} [{}]({})", first_sentences(&s.dek, 1), i + 1, s.url))
        .collect::<Vec<_>>()
        .join("\n\n");
    (heading, body)
}

#[cfg(test)]
#[path = "writer_tests.rs"]
mod tests;
