//! SQLite-backed [`InterestRepository`]: the `interests`, `member_portraits`, `interest_sightings`
//! and `interest_interviews` tables. Facets, portrait lists and interview turns are JSON.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::interest::{
    category_of, clean_facets, clean_topic, merge_facets, picture_path, topic_key, Depth, Interest,
    InterestInput, InterestPatch, MemberPortrait, NewsMode, Observed, ObservedInterest, Origin,
    STRONG_AT,
};
use pond_core::user_data::domain::interest_interview::{InterestInterview, InterviewStatus};
use pond_core::user_data::ports::interests::InterestRepository;
use pond_core::user_data::services::member_interview::MemberInterview;

pub struct SqliteInterestRepository {
    pool: Pool<Sqlite>,
}

impl SqliteInterestRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, topic, category, facets, depth, news, avoid, origin, \
     evidence, confirmed, picture_key, updated_at";

/// Now, at the second the column keeps, so a returned row equals the stored one.
fn now_secs() -> DateTime<Utc> {
    use chrono::SubsecRound;
    Utc::now().trunc_subsecs(0)
}

pub(crate) fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// RFC 3339 as written here, or SQLite's own `datetime('now')`.
pub(crate) fn parse_time(raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(at) = DateTime::parse_from_rfc3339(raw) {
        return Ok(at.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .map(|naive| naive.and_utc())
        .with_context(|| format!("unreadable timestamp {raw:?}"))
}

fn strings(raw: &str) -> Vec<String> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn json_of<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".to_string())
}

fn interest_of(row: &SqliteRow) -> Result<Interest> {
    let facets: String = row.try_get("facets")?;
    let depth: String = row.try_get("depth")?;
    let news: String = row.try_get("news")?;
    let origin: String = row.try_get("origin")?;
    let evidence: i64 = row.try_get("evidence")?;
    let updated_at: String = row.try_get("updated_at")?;
    let picture_key: Option<String> = row.try_get("picture_key")?;
    Ok(Interest {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        topic: row.try_get("topic")?,
        category: row.try_get("category")?,
        facets: strings(&facets),
        depth: Depth::parse(&depth).unwrap_or_default(),
        news: NewsMode::parse(&news).unwrap_or_default(),
        avoid: row.try_get::<i64, _>("avoid")? != 0,
        origin: Origin::parse(&origin),
        evidence: evidence.max(0) as u32,
        confirmed: row.try_get::<i64, _>("confirmed")? != 0,
        // An empty key records a look that found nothing.
        picture: picture_key
            .filter(|key| !key.is_empty())
            .map(|key| picture_path(&key)),
        updated_at: parse_time(&updated_at)?,
    })
}

impl SqliteInterestRepository {
    async fn by_key(&self, profile_id: &str, key: &str) -> Result<Option<Interest>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM interests WHERE profile_id = ? AND topic_key = ?"
        ))
        .bind(profile_id)
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .context("read an interest by topic")?;
        row.as_ref().map(interest_of).transpose()
    }

    /// Writes every field of `row`, which already exists.
    async fn write(&self, row: &Interest) -> Result<()> {
        sqlx::query(
            "UPDATE interests SET topic = ?, topic_key = ?, category = ?, facets = ?, depth = ?, \
             news = ?, avoid = ?, origin = ?, evidence = ?, confirmed = ?, updated_at = ? \
             WHERE id = ? AND profile_id = ?",
        )
        .bind(&row.topic)
        .bind(topic_key(&row.topic))
        .bind(&row.category)
        .bind(json_of(&row.facets))
        .bind(row.depth.as_str())
        .bind(row.news.as_str())
        .bind(row.avoid as i64)
        .bind(row.origin.as_str())
        .bind(row.evidence as i64)
        .bind(row.confirmed as i64)
        .bind(stamp(row.updated_at))
        .bind(&row.id)
        .bind(&row.profile_id)
        .execute(&self.pool)
        .await
        .context("write an interest")?;
        Ok(())
    }

    async fn insert(&self, row: &Interest) -> Result<()> {
        sqlx::query(
            "INSERT INTO interests (id, profile_id, topic, topic_key, category, facets, depth, \
             news, avoid, origin, evidence, confirmed, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&row.id)
        .bind(&row.profile_id)
        .bind(&row.topic)
        .bind(topic_key(&row.topic))
        .bind(&row.category)
        .bind(json_of(&row.facets))
        .bind(row.depth.as_str())
        .bind(row.news.as_str())
        .bind(row.avoid as i64)
        .bind(row.origin.as_str())
        .bind(row.evidence as i64)
        .bind(row.confirmed as i64)
        .bind(stamp(row.updated_at))
        .bind(stamp(row.updated_at))
        .execute(&self.pool)
        .await
        .context("insert an interest")?;
        Ok(())
    }

    /// `true` when this conversation had not yet been counted for the row.
    async fn sighted(&self, interest_id: &str, session_id: &str) -> Result<bool> {
        let done = sqlx::query(
            "INSERT OR IGNORE INTO interest_sightings (interest_id, session_id, seen_at) \
             VALUES (?, ?, ?)",
        )
        .bind(interest_id)
        .bind(session_id)
        .bind(stamp(Utc::now()))
        .execute(&self.pool)
        .await
        .context("note which conversation an interest rests on")?;
        Ok(done.rows_affected() > 0)
    }
}

#[async_trait]
impl InterestRepository for SqliteInterestRepository {
    async fn list(&self, profile_id: &str) -> Result<Vec<Interest>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM interests WHERE profile_id = ? \
             ORDER BY confirmed DESC, evidence DESC, updated_at DESC, rowid DESC"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's interests")?;
        rows.iter().map(interest_of).collect()
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Interest>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM interests WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read an interest")?;
        row.as_ref().map(interest_of).transpose()
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
        let now = now_secs();
        let existing = match self.by_key(profile_id, &key).await? {
            Some(row) => row,
            None => {
                let row = Interest {
                    id: uuid::Uuid::new_v4().to_string(),
                    profile_id: profile_id.to_string(),
                    topic,
                    category: category_of(&seen.category),
                    facets: clean_facets(seen.facets.iter().map(String::as_str)),
                    depth: Depth::Curious,
                    news: NewsMode::Both,
                    avoid: false,
                    origin: Origin::Chat,
                    evidence: 1,
                    confirmed: false,
                    picture: None,
                    updated_at: now,
                };
                match self.insert(&row).await {
                    Ok(()) => {
                        self.sighted(&row.id, session_id).await?;
                        return Ok(Observed::Created);
                    }
                    // Another window wrote the same topic first; count against that row.
                    Err(_) => self
                        .by_key(profile_id, &key)
                        .await?
                        .context("an interest that failed to insert is not there either")?,
                }
            }
        };
        if existing.avoid {
            return Ok(Observed::Avoided);
        }
        if !self.sighted(&existing.id, session_id).await? {
            return Ok(Observed::AlreadyCounted);
        }
        if existing.confirmed {
            sqlx::query("UPDATE interests SET evidence = evidence + 1 WHERE id = ?")
                .bind(&existing.id)
                .execute(&self.pool)
                .await
                .context("count a conversation behind a confirmed interest")?;
        } else {
            sqlx::query(
                "UPDATE interests SET evidence = evidence + 1, facets = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(json_of(&merge_facets(&existing.facets, &seen.facets)))
            .bind(stamp(now))
            .bind(&existing.id)
            .execute(&self.pool)
            .await
            .context("count a conversation behind a guessed interest")?;
        }
        Ok(Observed::Counted)
    }

    async fn confirm(
        &self,
        profile_id: &str,
        input: &InterestInput,
        origin: Origin,
    ) -> Result<Interest> {
        let topic = clean_topic(&input.topic).context("an interest needs a topic")?;
        let key = topic_key(&topic);
        let facets = clean_facets(input.facets.iter().flatten().map(String::as_str));
        let now = now_secs();
        if let Some(mut row) = self.by_key(profile_id, &key).await? {
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
            self.write(&row).await?;
            return Ok(row);
        }
        let avoid = input.avoid.unwrap_or(false);
        let row = Interest {
            id: uuid::Uuid::new_v4().to_string(),
            profile_id: profile_id.to_string(),
            topic,
            category: category_of(input.category.as_deref().unwrap_or("other")),
            facets,
            depth: input.depth.unwrap_or_default(),
            news: input
                .news
                .unwrap_or(if avoid { NewsMode::Off } else { NewsMode::Both }),
            avoid,
            origin,
            evidence: 1,
            confirmed: true,
            picture: None,
            updated_at: now,
        };
        self.insert(&row).await?;
        Ok(row)
    }

    async fn patch(
        &self,
        profile_id: &str,
        id: &str,
        patch: &InterestPatch,
    ) -> Result<Option<Interest>> {
        let Some(mut row) = self.get(profile_id, id).await? else {
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
        row.updated_at = now_secs();
        self.write(&row).await?;
        Ok(Some(row))
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM interests WHERE profile_id = ? AND id = ?")
            .bind(profile_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete an interest")?;
        Ok(done.rows_affected() > 0)
    }

    async fn last_changed(&self, profile_id: &str) -> Result<Option<DateTime<Utc>>> {
        let latest: Option<String> = sqlx::query_scalar(
            "SELECT MAX(updated_at) FROM interests WHERE profile_id = ? \
             AND (confirmed = 1 OR evidence >= ?)",
        )
        .bind(profile_id)
        .bind(STRONG_AT as i64)
        .fetch_one(&self.pool)
        .await
        .context("read when a member's interests last changed")?;
        latest.as_deref().map(parse_time).transpose()
    }

    async fn without_picture(&self, profile_id: &str, limit: usize) -> Result<Vec<Interest>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM interests WHERE profile_id = ? AND picture_key IS NULL \
             ORDER BY confirmed DESC, evidence DESC, updated_at DESC LIMIT ?"
        ))
        .bind(profile_id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .context("list interests with no picture looked for")?;
        rows.iter().map(interest_of).collect()
    }

    async fn set_picture(&self, profile_id: &str, id: &str, key: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE interests SET picture_key = ? WHERE profile_id = ? AND id = ?")
            .bind(key.unwrap_or(""))
            .bind(profile_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("record an interest's picture")?;
        Ok(())
    }

    async fn portrait(&self, profile_id: &str) -> Result<Option<MemberPortrait>> {
        let row = sqlx::query(
            "SELECT summary, into_json, working_on_json, likes_json, updated_at \
             FROM member_portraits WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await
        .context("read a member's portrait")?;
        let Some(row) = row else {
            return Ok(None);
        };
        let into: String = row.try_get("into_json")?;
        let working_on: String = row.try_get("working_on_json")?;
        let likes: String = row.try_get("likes_json")?;
        let updated_at: String = row.try_get("updated_at")?;
        Ok(Some(MemberPortrait {
            summary: row.try_get("summary")?,
            into: strings(&into),
            working_on: strings(&working_on),
            likes: strings(&likes),
            updated_at: parse_time(&updated_at)?,
        }))
    }

    async fn save_portrait(&self, profile_id: &str, portrait: &MemberPortrait) -> Result<()> {
        sqlx::query(
            "INSERT INTO member_portraits \
             (profile_id, summary, into_json, working_on_json, likes_json, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET summary = excluded.summary, \
             into_json = excluded.into_json, working_on_json = excluded.working_on_json, \
             likes_json = excluded.likes_json, updated_at = excluded.updated_at",
        )
        .bind(profile_id)
        .bind(&portrait.summary)
        .bind(json_of(&portrait.into))
        .bind(json_of(&portrait.working_on))
        .bind(json_of(&portrait.likes))
        .bind(stamp(portrait.updated_at))
        .execute(&self.pool)
        .await
        .context("save a member's portrait")?;
        Ok(())
    }

    async fn interview(&self, profile_id: &str) -> Result<Option<InterestInterview>> {
        let row = sqlx::query(
            "SELECT status, turns_json, draft_json, asked, planned, round, updated_at \
             FROM interest_interviews WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await
        .context("read a member's interests interview")?;
        let Some(row) = row else {
            return Ok(None);
        };
        let status: String = row.try_get("status")?;
        let turns: String = row.try_get("turns_json")?;
        let draft: String = row.try_get("draft_json")?;
        let updated_at: String = row.try_get("updated_at")?;
        Ok(Some(InterestInterview {
            status: InterviewStatus::parse(&status),
            turns: serde_json::from_str(&turns).unwrap_or_default(),
            draft: serde_json::from_str::<Vec<Interest>>(&draft).unwrap_or_default(),
            asked: row.try_get::<i64, _>("asked")?.max(0) as u32,
            planned: row.try_get::<i64, _>("planned")?.max(0) as u32,
            round: row.try_get::<i64, _>("round")?.max(1) as u32,
            updated_at: Some(parse_time(&updated_at)?),
        }))
    }

    async fn save_interview(&self, profile_id: &str, interview: &InterestInterview) -> Result<()> {
        sqlx::query(
            "INSERT INTO interest_interviews \
             (profile_id, status, turns_json, draft_json, asked, planned, round, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET status = excluded.status, \
             turns_json = excluded.turns_json, draft_json = excluded.draft_json, \
             asked = excluded.asked, planned = excluded.planned, round = excluded.round, \
             updated_at = excluded.updated_at",
        )
        .bind(profile_id)
        .bind(interview.status.as_str())
        .bind(json_of(&interview.turns))
        .bind(json_of(&interview.draft))
        .bind(interview.asked as i64)
        .bind(interview.planned as i64)
        .bind(interview.round.max(1) as i64)
        .bind(stamp(Utc::now()))
        .execute(&self.pool)
        .await
        .context("save a member's interests interview")?;
        Ok(())
    }

    async fn member_interview(&self, profile_id: &str) -> Result<Option<MemberInterview>> {
        let row = sqlx::query(
            "SELECT status, body_json, updated_at FROM member_interviews WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await
        .context("read a member's interview")?;
        let Some(row) = row else {
            return Ok(None);
        };
        let status: String = row.try_get("status")?;
        let body: String = row.try_get("body_json")?;
        let updated_at: String = row.try_get("updated_at")?;
        // A body this build cannot read is an interview to begin again, not an error.
        let mut interview: MemberInterview = serde_json::from_str(&body).unwrap_or_default();
        if interview.status != InterviewStatus::NotStarted {
            interview.status = InterviewStatus::parse(&status);
        }
        interview.updated_at = Some(parse_time(&updated_at)?);
        Ok(Some(interview))
    }

    async fn save_member_interview(
        &self,
        profile_id: &str,
        interview: &MemberInterview,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO member_interviews (profile_id, status, body_json, updated_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET status = excluded.status, \
             body_json = excluded.body_json, updated_at = excluded.updated_at",
        )
        .bind(profile_id)
        .bind(interview.status.as_str())
        .bind(json_of(interview))
        .bind(stamp(Utc::now()))
        .execute(&self.pool)
        .await
        .context("save a member's interview")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::user_data::domain::interest::Signal;

    async fn repo() -> SqliteInterestRepository {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::migrate!("migrations/system")
            .run(&pool)
            .await
            .unwrap();
        for member in ["jerry", "liz"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(member)
                .bind(member)
                .execute(&pool)
                .await
                .unwrap();
        }
        SqliteInterestRepository::new(pool)
    }

    fn seen(topic: &str, facets: &[&str]) -> ObservedInterest {
        ObservedInterest {
            topic: topic.to_string(),
            category: "cars".to_string(),
            facets: facets.iter().map(|f| f.to_string()).collect(),
            signal: Signal::Does,
        }
    }

    #[tokio::test]
    async fn a_conversation_adds_one_to_an_interest_however_many_windows_mention_it() {
        let repo = repo().await;
        let fielder = seen("Toyota Fielder", &["Kilimall"]);
        assert_eq!(
            repo.observe("jerry", "s1", &fielder).await.unwrap(),
            Observed::Created
        );
        assert_eq!(
            repo.observe("jerry", "s1", &seen("toyota fielder", &["Jiji"]))
                .await
                .unwrap(),
            Observed::AlreadyCounted
        );
        assert_eq!(
            repo.observe("jerry", "s2", &seen("Toyota Fielder", &["Autochek"]))
                .await
                .unwrap(),
            Observed::Counted
        );
        let rows = repo.list("jerry").await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].evidence, 2);
        assert!(!rows[0].confirmed);
        assert_eq!(rows[0].origin, Origin::Chat);
        assert_eq!(rows[0].facets, vec!["Kilimall", "Autochek"]);
        assert!(
            repo.list("liz").await.unwrap().is_empty(),
            "never another member's"
        );
    }

    #[tokio::test]
    async fn a_confirmed_row_gains_evidence_and_nothing_else() {
        let repo = repo().await;
        let confirmed = repo
            .confirm(
                "jerry",
                &InterestInput {
                    topic: "Toyota Fielder".into(),
                    category: Some("cars".into()),
                    facets: Some(vec!["under KSh 1.2M".into()]),
                    ..Default::default()
                },
                Origin::You,
            )
            .await
            .unwrap();
        repo.observe("jerry", "s9", &seen("Toyota Fielder", &["Jiji"]))
            .await
            .unwrap();
        let after = repo.get("jerry", &confirmed.id).await.unwrap().unwrap();
        assert_eq!(after.evidence, confirmed.evidence + 1);
        assert_eq!(after.facets, vec!["under KSh 1.2M"], "facets untouched");
        assert_eq!(after.updated_at, confirmed.updated_at);
    }

    #[tokio::test]
    async fn an_avoided_row_is_never_resurrected() {
        let repo = repo().await;
        let row = repo
            .confirm(
                "jerry",
                &InterestInput {
                    topic: "Celebrity gossip".into(),
                    avoid: Some(true),
                    ..Default::default()
                },
                Origin::Interview,
            )
            .await
            .unwrap();
        assert_eq!(row.news, NewsMode::Off);
        assert_eq!(
            repo.observe("jerry", "s1", &seen("celebrity gossip", &[]))
                .await
                .unwrap(),
            Observed::Avoided
        );
        let after = repo.get("jerry", &row.id).await.unwrap().unwrap();
        assert_eq!(after.evidence, row.evidence);
        assert!(after.avoid);
    }

    #[tokio::test]
    async fn approving_a_guess_confirms_the_same_row() {
        let repo = repo().await;
        repo.observe("jerry", "s1", &seen("Gen V", &["Marie Moreau"]))
            .await
            .unwrap();
        let confirmed = repo
            .confirm(
                "jerry",
                &InterestInput {
                    topic: "gen v".into(),
                    category: Some("film_tv".into()),
                    ..Default::default()
                },
                Origin::Interview,
            )
            .await
            .unwrap();
        let rows = repo.list("jerry").await.unwrap();
        assert_eq!(rows.len(), 1, "merged by topic, not added beside the guess");
        assert!(confirmed.confirmed);
        assert_eq!(confirmed.origin, Origin::Interview);
        assert_eq!(confirmed.category, "film_tv");
        assert_eq!(confirmed.facets, vec!["Marie Moreau"]);
    }

    #[tokio::test]
    async fn a_picture_is_looked_for_once_and_served_as_a_path() {
        let repo = repo().await;
        repo.observe("jerry", "s1", &seen("Toyota Fielder", &[]))
            .await
            .unwrap();
        repo.observe("jerry", "s1", &seen("Snacks", &[]))
            .await
            .unwrap();
        let missing = repo.without_picture("jerry", 10).await.unwrap();
        assert_eq!(missing.len(), 2);
        let fielder = missing
            .iter()
            .find(|r| r.topic == "Toyota Fielder")
            .unwrap();
        let snacks = missing.iter().find(|r| r.topic == "Snacks").unwrap();
        repo.set_picture("jerry", &fielder.id, Some("abc123"))
            .await
            .unwrap();
        repo.set_picture("jerry", &snacks.id, None).await.unwrap();
        assert!(repo.without_picture("jerry", 10).await.unwrap().is_empty());
        let rows = repo.list("jerry").await.unwrap();
        let fielder = rows.iter().find(|r| r.topic == "Toyota Fielder").unwrap();
        assert_eq!(
            fielder.picture.as_deref(),
            Some("/api/v1/report-images/abc123")
        );
        let snacks = rows.iter().find(|r| r.topic == "Snacks").unwrap();
        assert_eq!(
            snacks.picture, None,
            "nothing found is null, not an empty path"
        );
    }

    #[tokio::test]
    async fn the_portrait_and_the_interview_round_trip() {
        let repo = repo().await;
        assert!(repo.portrait("jerry").await.unwrap().is_none());
        let portrait = MemberPortrait {
            summary: "Runs a company.".into(),
            into: vec!["Formula 1".into()],
            working_on: vec!["a talk".into()],
            likes: vec!["teal".into()],
            updated_at: Utc::now(),
        };
        repo.save_portrait("jerry", &portrait).await.unwrap();
        assert_eq!(
            repo.portrait("jerry").await.unwrap().unwrap().into,
            vec!["Formula 1"]
        );

        let mut interview = InterestInterview::default();
        interview.status = InterviewStatus::Thinking;
        repo.save_interview("jerry", &interview).await.unwrap();
        let back = repo.interview("jerry").await.unwrap().unwrap();
        assert_eq!(back.status, InterviewStatus::Thinking);
        assert!(back.updated_at.is_some());
    }

    #[tokio::test]
    async fn the_member_interview_round_trips_with_its_answers() {
        use pond_core::user_data::services::member_interview::{answer, begin, AnswerIn};
        let repo = repo().await;
        assert!(repo.member_interview("jerry").await.unwrap().is_none());
        let (started, _) = begin(None, Utc::now());
        let midway = answer(
            started,
            AnswerIn {
                choices: vec!["Retired".into()],
                ..AnswerIn::default()
            },
            Utc::now(),
        )
        .unwrap();
        repo.save_member_interview("jerry", &midway).await.unwrap();
        let back = repo.member_interview("jerry").await.unwrap().unwrap();
        assert_eq!(back.status, InterviewStatus::Asking);
        assert_eq!(back.section, 1);
        assert_eq!(back.answers, midway.answers);
        assert_eq!(back.turns.len(), midway.turns.len());
        assert!(back.updated_at.is_some());
    }
}
