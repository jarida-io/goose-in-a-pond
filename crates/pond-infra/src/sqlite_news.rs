//! SQLite-backed `NewsStore`: a member's news settings, stories, digests and topic map
//! (migration 0090), and the interests and portrait it reads (0086). Times are RFC 3339 in UTC to
//! the millisecond, so they sort as text.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::shared::domain::news::{
    Digest, DigestEntry, DigestSection, InterestRef, InterestRow, NewsBook, NewsSettings, NewsUse,
    Portrait, Reference, Story, StoryKind, Topic, Vote,
};
use pond_core::shared::ports::news::NewsStore;
use pond_core::shared::services::news::dedupe::{headline_key, url_key};

pub struct SqliteNewsStore {
    pool: Pool<Sqlite>,
}

impl SqliteNewsStore {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A stored time: this store's own RFC 3339, or SQLite's `datetime('now')` from another writer.
fn time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw.trim())
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw.trim(), "%Y-%m-%d %H:%M:%S")
                .ok()
                .map(|t| t.and_utc())
        })
}

fn opt_time(row: &SqliteRow, column: &str) -> Result<Option<DateTime<Utc>>> {
    Ok(row
        .try_get::<Option<String>, _>(column)?
        .as_deref()
        .and_then(time))
}

fn req_time(row: &SqliteRow, column: &str) -> Result<DateTime<Utc>> {
    let raw: String = row.try_get(column)?;
    time(&raw).with_context(|| format!("bad news timestamp {raw:?} in {column}"))
}

fn json_list(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

const STORY_COLUMNS: &str = "id, profile_id, kind, interest_id, interest_topic, headline, dek, \
     summary, why_you, url, site, source_title, further_sources, picture_key, found_by, \
     published_at, written_at, read_at, vote";

/// Lists leave out what the member called wrong.
const LISTED: &str = "(vote IS NULL OR vote <> 'wrong')";

fn story_of(row: &SqliteRow) -> Result<Story> {
    let further: String = row.try_get("further_sources")?;
    Ok(Story {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        kind: StoryKind::parse(&row.try_get::<String, _>("kind")?),
        interest: InterestRef {
            id: row.try_get("interest_id")?,
            topic: row.try_get("interest_topic")?,
        },
        headline: row.try_get("headline")?,
        dek: row.try_get("dek")?,
        summary: row.try_get("summary")?,
        why_you: row.try_get("why_you")?,
        url: row.try_get("url")?,
        site: row.try_get("site")?,
        source_title: row.try_get("source_title")?,
        further_sources: serde_json::from_str::<Vec<Reference>>(&further).unwrap_or_default(),
        picture_key: row.try_get("picture_key")?,
        found_by: row.try_get("found_by")?,
        published_at: opt_time(row, "published_at")?,
        written_at: req_time(row, "written_at")?,
        read_at: opt_time(row, "read_at")?,
        vote: row
            .try_get::<Option<String>, _>("vote")?
            .as_deref()
            .and_then(Vote::parse),
    })
}

fn digest_of(row: &SqliteRow) -> Result<Digest> {
    let day: String = row.try_get("day")?;
    let sections: String = row.try_get("sections")?;
    Ok(Digest {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        day: NaiveDate::parse_from_str(&day, "%Y-%m-%d")
            .with_context(|| format!("bad digest day {day:?}"))?,
        title: row.try_get("title")?,
        intro: row.try_get("intro")?,
        sections: serde_json::from_str::<Vec<DigestSection>>(&sections)
            .context("a digest's sections are unreadable")?,
        written_at: req_time(row, "written_at")?,
    })
}

fn topic_of(row: &SqliteRow) -> Result<Topic> {
    Ok(Topic {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        area: row.try_get("area")?,
        subject: row.try_get("subject")?,
        query: row.try_get("query")?,
        newsworthy: row.try_get::<i64, _>("newsworthy")? != 0,
        score: row.try_get("score")?,
        why: row.try_get("why")?,
        built_at: req_time(row, "built_at")?,
        used_at: opt_time(row, "used_at")?,
    })
}

impl SqliteNewsStore {
    /// Sets one bookkeeping column, making the member's row with the default settings if needed.
    async fn set_book(&self, member: &str, column: &str, value: Option<String>) -> Result<()> {
        let sql = format!(
            "INSERT INTO news_settings (profile_id, {column}, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET {column} = excluded.{column}"
        );
        sqlx::query(&sql)
            .bind(member)
            .bind(value)
            .bind(stamp(Utc::now()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn stories_where(
        &self,
        filter: &str,
        binds: &[String],
        tail: &str,
    ) -> Result<Vec<Story>> {
        let sql = format!("SELECT {STORY_COLUMNS} FROM news_stories WHERE {filter} {tail}");
        let mut query = sqlx::query(&sql);
        for bind in binds {
            query = query.bind(bind);
        }
        query
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(story_of)
            .collect()
    }
}

#[async_trait]
impl NewsStore for SqliteNewsStore {
    async fn settings(&self, member: &str) -> Result<NewsSettings> {
        let row = sqlx::query(
            "SELECT tiles, digest, digest_time, spoken_on_speaker, timezone FROM news_settings \
             WHERE profile_id = ?",
        )
        .bind(member)
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(NewsSettings::default());
        };
        Ok(NewsSettings {
            tiles: row.try_get::<i64, _>("tiles")? != 0,
            digest: row.try_get::<i64, _>("digest")? != 0,
            digest_time: row.try_get("digest_time")?,
            spoken_on_speaker: row.try_get::<i64, _>("spoken_on_speaker")? != 0,
            timezone: row.try_get("timezone")?,
        })
    }

    async fn save_settings(&self, member: &str, settings: &NewsSettings) -> Result<()> {
        sqlx::query(
            "INSERT INTO news_settings (profile_id, tiles, digest, digest_time, spoken_on_speaker, \
             timezone, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET tiles = excluded.tiles, \
             digest = excluded.digest, digest_time = excluded.digest_time, \
             spoken_on_speaker = excluded.spoken_on_speaker, timezone = excluded.timezone, \
             updated_at = excluded.updated_at",
        )
        .bind(member)
        .bind(settings.tiles as i64)
        .bind(settings.digest as i64)
        .bind(&settings.digest_time)
        .bind(settings.spoken_on_speaker as i64)
        .bind(&settings.timezone)
        .bind(stamp(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn book(&self, member: &str) -> Result<NewsBook> {
        let row = sqlx::query(
            "SELECT \
               (SELECT refresh_requested_at FROM news_settings WHERE profile_id = ?1) AS refresh, \
               (SELECT writing_since FROM news_settings WHERE profile_id = ?1) AS writing, \
               (SELECT stories_written_at FROM news_settings WHERE profile_id = ?1) AS stories, \
               (SELECT spoken_on FROM news_settings WHERE profile_id = ?1) AS spoken, \
               (SELECT MAX(built_at) FROM member_topics WHERE profile_id = ?1) AS built, \
               (SELECT MAX(updated_at) FROM interests WHERE profile_id = ?1) AS changed",
        )
        .bind(member)
        .fetch_one(&self.pool)
        .await?;
        Ok(NewsBook {
            refresh_requested_at: opt_time(&row, "refresh")?,
            writing_since: opt_time(&row, "writing")?,
            stories_written_at: opt_time(&row, "stories")?,
            spoken_on: row
                .try_get::<Option<String>, _>("spoken")?
                .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok()),
            map_built_at: opt_time(&row, "built")?,
            interests_changed_at: opt_time(&row, "changed")?,
        })
    }

    async fn request_refresh(&self, member: &str, at: DateTime<Utc>) -> Result<()> {
        self.set_book(member, "refresh_requested_at", Some(stamp(at)))
            .await
    }

    async fn begin_writing(&self, member: &str, at: DateTime<Utc>) -> Result<()> {
        self.set_book(member, "writing_since", Some(stamp(at)))
            .await?;
        self.set_book(member, "refresh_requested_at", None).await
    }

    async fn end_writing(&self, member: &str, stories_done: Option<DateTime<Utc>>) -> Result<()> {
        self.set_book(member, "writing_since", None).await?;
        if let Some(done) = stories_done {
            self.set_book(member, "stories_written_at", Some(stamp(done)))
                .await?;
        }
        Ok(())
    }

    async fn mark_spoken(&self, member: &str, day: NaiveDate) -> Result<()> {
        self.set_book(
            member,
            "spoken_on",
            Some(day.format("%Y-%m-%d").to_string()),
        )
        .await
    }

    async fn members(&self) -> Result<Vec<String>> {
        Ok(
            sqlx::query_scalar::<_, String>("SELECT id FROM profiles ORDER BY created_at, rowid")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    async fn interests(&self, member: &str) -> Result<Vec<InterestRow>> {
        let rows = sqlx::query(
            "SELECT id, topic, category, facets, depth, news, avoid, evidence, confirmed \
             FROM interests WHERE profile_id = ? ORDER BY confirmed DESC, evidence DESC, rowid",
        )
        .bind(member)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(InterestRow {
                    id: row.try_get("id")?,
                    topic: row.try_get("topic")?,
                    category: row.try_get("category")?,
                    facets: json_list(&row.try_get::<String, _>("facets")?),
                    depth: row.try_get("depth")?,
                    news: NewsUse::parse(&row.try_get::<String, _>("news")?),
                    avoid: row.try_get::<i64, _>("avoid")? != 0,
                    evidence: row.try_get::<i64, _>("evidence")?.max(0) as u32,
                    confirmed: row.try_get::<i64, _>("confirmed")? != 0,
                })
            })
            .collect()
    }

    async fn portrait(&self, member: &str) -> Result<Option<Portrait>> {
        let row = sqlx::query(
            "SELECT summary, into_json, working_on_json, likes_json FROM member_portraits \
             WHERE profile_id = ?",
        )
        .bind(member)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(Portrait {
                summary: row.try_get("summary")?,
                into: json_list(&row.try_get::<String, _>("into_json")?),
                working_on: json_list(&row.try_get::<String, _>("working_on_json")?),
                likes: json_list(&row.try_get::<String, _>("likes_json")?),
            })
        })
        .transpose()
    }

    async fn topics(&self, member: &str) -> Result<Vec<Topic>> {
        sqlx::query(
            "SELECT id, profile_id, area, subject, query, newsworthy, score, why, built_at, \
             used_at FROM member_topics WHERE profile_id = ? ORDER BY score DESC, rowid",
        )
        .bind(member)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(topic_of)
        .collect()
    }

    async fn replace_topics(&self, member: &str, topics: &[Topic]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let old = sqlx::query("SELECT id, query, used_at FROM member_topics WHERE profile_id = ?")
            .bind(member)
            .fetch_all(&mut *tx)
            .await?;
        let old: Vec<(String, String, Option<String>)> = old
            .iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("query")?, r.try_get("used_at")?)))
            .collect::<Result<_>>()?;
        sqlx::query("DELETE FROM member_topics WHERE profile_id = ?")
            .bind(member)
            .execute(&mut *tx)
            .await?;
        for topic in topics {
            let kept = old
                .iter()
                .find(|(_, query, _)| query.trim().eq_ignore_ascii_case(topic.query.trim()));
            let id = kept.map_or(topic.id.clone(), |(id, _, _)| id.clone());
            let used_at = kept
                .and_then(|(_, _, used)| used.clone())
                .or_else(|| topic.used_at.map(stamp));
            sqlx::query(
                "INSERT OR IGNORE INTO member_topics (id, profile_id, area, subject, query, \
                 newsworthy, score, why, built_at, used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(member)
            .bind(&topic.area)
            .bind(&topic.subject)
            .bind(&topic.query)
            .bind(topic.newsworthy as i64)
            .bind(topic.score)
            .bind(&topic.why)
            .bind(stamp(topic.built_at))
            .bind(used_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn mark_topics_used(
        &self,
        member: &str,
        ids: &[String],
        at: DateTime<Utc>,
    ) -> Result<()> {
        for id in ids {
            sqlx::query("UPDATE member_topics SET used_at = ? WHERE profile_id = ? AND id = ?")
                .bind(stamp(at))
                .bind(member)
                .bind(id)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }

    async fn add_story(&self, story: &Story) -> Result<bool> {
        let done = sqlx::query(
            "INSERT OR IGNORE INTO news_stories (id, profile_id, kind, interest_id, interest_topic, \
             headline, headline_key, dek, summary, why_you, url, url_key, site, source_title, \
             further_sources, picture_key, found_by, published_at, written_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&story.id)
        .bind(&story.profile_id)
        .bind(story.kind.as_str())
        .bind(&story.interest.id)
        .bind(&story.interest.topic)
        .bind(&story.headline)
        .bind(headline_key(&story.headline))
        .bind(&story.dek)
        .bind(&story.summary)
        .bind(&story.why_you)
        .bind(&story.url)
        .bind(url_key(&story.url))
        .bind(&story.site)
        .bind(&story.source_title)
        .bind(serde_json::to_string(&story.further_sources)?)
        .bind(&story.picture_key)
        .bind(&story.found_by)
        .bind(story.published_at.map(stamp))
        .bind(stamp(story.written_at))
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn stories_since(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>> {
        self.stories_where(
            &format!("profile_id = ? AND written_at >= ? AND {LISTED}"),
            &[member.to_string(), stamp(since)],
            "ORDER BY written_at DESC, rowid DESC",
        )
        .await
    }

    async fn interest_stories(
        &self,
        member: &str,
        interest_id: &str,
        limit: usize,
    ) -> Result<Vec<Story>> {
        self.stories_where(
            &format!("profile_id = ? AND interest_id = ? AND {LISTED}"),
            &[member.to_string(), interest_id.to_string()],
            &format!(
                "ORDER BY written_at DESC, rowid DESC LIMIT {}",
                limit.min(500)
            ),
        )
        .await
    }

    async fn read_stories(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>> {
        self.stories_where(
            &format!("profile_id = ? AND read_at >= ? AND {LISTED}"),
            &[member.to_string(), stamp(since)],
            "ORDER BY read_at DESC, rowid DESC",
        )
        .await
    }

    async fn voted_stories(&self, member: &str, since: DateTime<Utc>) -> Result<Vec<Story>> {
        self.stories_where(
            "profile_id = ? AND voted_at >= ? AND vote IN ('up', 'down')",
            &[member.to_string(), stamp(since)],
            "ORDER BY voted_at DESC, rowid DESC LIMIT 40",
        )
        .await
    }

    async fn story(&self, member: &str, id: &str) -> Result<Option<Story>> {
        Ok(self
            .stories_where(
                "profile_id = ? AND id = ?",
                &[member.to_string(), id.to_string()],
                "",
            )
            .await?
            .into_iter()
            .next())
    }

    async fn mark_read(&self, member: &str, id: &str, at: DateTime<Utc>) -> Result<bool> {
        let done =
            sqlx::query("UPDATE news_stories SET read_at = ? WHERE profile_id = ? AND id = ?")
                .bind(stamp(at))
                .bind(member)
                .bind(id)
                .execute(&self.pool)
                .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn set_vote(
        &self,
        member: &str,
        id: &str,
        vote: Vote,
        note: Option<&str>,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE news_stories SET vote = ?, vote_note = ?, voted_at = ? \
             WHERE profile_id = ? AND id = ?",
        )
        .bind(vote.as_str())
        .bind(note)
        .bind(stamp(at))
        .bind(member)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() == 1)
    }

    async fn has_page(&self, member: &str, key: &str) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM news_stories WHERE profile_id = ? AND url_key = ?",
        )
        .bind(member)
        .bind(key)
        .fetch_one(&self.pool)
        .await?
            > 0)
    }

    async fn count_stories(
        &self,
        member: &str,
        kind: StoryKind,
        since: DateTime<Utc>,
    ) -> Result<u32> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM news_stories WHERE profile_id = ? AND kind = ? AND written_at >= ?",
        )
        .bind(member)
        .bind(kind.as_str())
        .bind(stamp(since))
        .fetch_one(&self.pool)
        .await?
        .max(0) as u32)
    }

    async fn prune_stories(&self, before: DateTime<Utc>) -> Result<u64> {
        Ok(sqlx::query("DELETE FROM news_stories WHERE written_at < ?")
            .bind(stamp(before))
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    async fn add_digest(&self, digest: &Digest) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO news_digests (id, profile_id, day, title, intro, sections, \
             written_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&digest.id)
        .bind(&digest.profile_id)
        .bind(digest.day.format("%Y-%m-%d").to_string())
        .bind(&digest.title)
        .bind(&digest.intro)
        .bind(serde_json::to_string(&digest.sections)?)
        .bind(stamp(digest.written_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn digest(&self, member: &str, id: &str) -> Result<Option<Digest>> {
        sqlx::query("SELECT * FROM news_digests WHERE profile_id = ? AND id = ?")
            .bind(member)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(digest_of)
            .transpose()
    }

    async fn digest_on(&self, member: &str, day: NaiveDate) -> Result<Option<Digest>> {
        sqlx::query("SELECT * FROM news_digests WHERE profile_id = ? AND day = ?")
            .bind(member)
            .bind(day.format("%Y-%m-%d").to_string())
            .fetch_optional(&self.pool)
            .await?
            .as_ref()
            .map(digest_of)
            .transpose()
    }

    async fn latest_digest(&self, member: &str) -> Result<Option<Digest>> {
        sqlx::query(
            "SELECT * FROM news_digests WHERE profile_id = ? ORDER BY written_at DESC, rowid DESC \
             LIMIT 1",
        )
        .bind(member)
        .fetch_optional(&self.pool)
        .await?
        .as_ref()
        .map(digest_of)
        .transpose()
    }

    async fn digests(&self, member: &str) -> Result<Vec<DigestEntry>> {
        sqlx::query(
            "SELECT id, title, written_at FROM news_digests WHERE profile_id = ? \
             ORDER BY written_at DESC, rowid DESC",
        )
        .bind(member)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|row| {
            Ok(DigestEntry {
                id: row.try_get("id")?,
                title: row.try_get("title")?,
                written_at: req_time(row, "written_at")?,
            })
        })
        .collect()
    }
}

#[cfg(test)]
#[path = "sqlite_news_tests.rs"]
mod tests;
