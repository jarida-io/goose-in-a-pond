//! SQLite-backed [`IdeaRepository`]: the `ideas` and `idea_runs` tables, the seeds the topic map
//! (`member_topics`) offers, and the to-dos an idea's steps become in `todos`.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::idea::{
    card_label, GooseCan, GoosePlan, Grounding, Idea, IdeaFact, IdeaKind, IdeaProgress, IdeaSource,
    IdeaState,
};
use pond_core::user_data::ports::ideas::{IdeaRepository, IdeaRun, IdeaTodo, TopicSeed};

use crate::sqlite_interests::{parse_time, stamp};

pub struct SqliteIdeaRepository {
    pool: Pool<Sqlite>,
}

impl SqliteIdeaRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }

    async fn sync_finished(&self, profile_id: &str) {
        let _ = sqlx::query(
            "UPDATE ideas SET state = 'done', updated_at = datetime('now') \
             WHERE profile_id = ? AND state = 'working' AND task_id IS NOT NULL \
             AND task_id IN (SELECT id FROM long_tasks WHERE state IN ('completed', 'failed', 'cancelled'))",
        )
        .bind(profile_id)
        .execute(&self.pool)
        .await;

        let _ = sqlx::query(
            "UPDATE ideas SET state = 'done', updated_at = datetime('now') \
             WHERE profile_id = ? AND state = 'working' AND task_id IS NULL \
             AND id IN (SELECT source_ref FROM todos WHERE source = 'idea') \
             AND NOT EXISTS (SELECT 1 FROM todos WHERE source = 'idea' AND source_ref = ideas.id AND state = 'open')",
        )
        .bind(profile_id)
        .execute(&self.pool)
        .await;
    }
}

const COLUMNS: &str = "id, profile_id, topic, kind, title, hook, pitch, why, facts_json, notes, \
     grounded_json, sources_json, picture, goose_can_json, goose_plan_json, you_can_json, state, \
     task_id, created_at";

fn idea_of(row: &SqliteRow) -> Result<Idea> {
    let grounded: String = row.try_get("grounded_json")?;
    let sources: String = row.try_get("sources_json")?;
    let goose_can: Option<String> = row.try_get("goose_can_json")?;
    let goose_plan: Option<String> = row.try_get("goose_plan_json")?;
    let you_can: String = row.try_get("you_can_json")?;
    let state: String = row.try_get("state")?;
    let task_id: Option<String> = row.try_get("task_id")?;
    let created_at: String = row.try_get("created_at")?;
    let kind: String = row.try_get("kind")?;
    let kind = IdeaKind::parse(&kind).unwrap_or_default();
    let facts: String = row.try_get("facts_json")?;
    let pitch: String = row.try_get("pitch")?;
    let hook: String = row.try_get("hook")?;
    let grounded_in = serde_json::from_str::<Vec<Grounding>>(&grounded).unwrap_or_default();
    Ok(Idea {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        topic: row.try_get("topic")?,
        kind,
        kind_label: card_label(kind, &grounded_in).to_string(),
        title: row.try_get("title")?,
        // An idea written before cards had a line of their own shows its pitch.
        hook: if hook.is_empty() { pitch.clone() } else { hook },
        pitch,
        why: row.try_get("why")?,
        facts: serde_json::from_str::<Vec<IdeaFact>>(&facts).unwrap_or_default(),
        notes: row.try_get("notes")?,
        grounded_in,
        sources: serde_json::from_str::<Vec<IdeaSource>>(&sources).unwrap_or_default(),
        picture: row.try_get("picture")?,
        goose_can: goose_can.and_then(|g| serde_json::from_str::<GooseCan>(&g).ok()),
        goose_plan: goose_plan
            .and_then(|g| serde_json::from_str::<GoosePlan>(&g).ok())
            .unwrap_or_default(),
        you_can: serde_json::from_str(&you_can).unwrap_or_default(),
        state: IdeaState::parse(&state),
        progress: task_id.map(|task_id| IdeaProgress { task_id }),
        created_at: parse_time(&created_at)?,
    })
}

fn time_or_none(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.as_deref().and_then(|r| parse_time(r).ok())
}

#[async_trait]
impl IdeaRepository for SqliteIdeaRepository {
    async fn list(&self, profile_id: &str) -> Result<Vec<Idea>> {
        self.sync_finished(profile_id).await;
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM ideas WHERE profile_id = ? AND state != 'dismissed' \
             ORDER BY created_at DESC, rowid DESC LIMIT 50"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's ideas")?;
        rows.iter().map(idea_of).collect()
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Idea>> {
        self.sync_finished(profile_id).await;
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM ideas WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read an idea")?;
        row.as_ref().map(idea_of).transpose()
    }

    async fn add(&self, idea: &Idea) -> Result<()> {
        let now = stamp(Utc::now());
        sqlx::query(
            "INSERT INTO ideas (id, profile_id, topic, kind, title, hook, pitch, why, \
             facts_json, notes, grounded_json, sources_json, picture, goose_can_json, \
             goose_plan_json, you_can_json, state, task_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&idea.id)
        .bind(&idea.profile_id)
        .bind(&idea.topic)
        .bind(idea.kind.as_str())
        .bind(&idea.title)
        .bind(&idea.hook)
        .bind(&idea.pitch)
        .bind(&idea.why)
        .bind(serde_json::to_string(&idea.facts)?)
        .bind(&idea.notes)
        .bind(serde_json::to_string(&idea.grounded_in)?)
        .bind(serde_json::to_string(&idea.sources)?)
        .bind(idea.picture.as_deref())
        .bind(
            idea.goose_can
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
        )
        .bind(serde_json::to_string(&idea.goose_plan)?)
        .bind(serde_json::to_string(&idea.you_can)?)
        .bind(idea.state.as_str())
        .bind(idea.progress.as_ref().map(|p| p.task_id.as_str()))
        .bind(stamp(idea.created_at))
        .bind(now)
        .execute(&self.pool)
        .await
        .context("store an idea")?;
        Ok(())
    }

    async fn set_state(
        &self,
        profile_id: &str,
        id: &str,
        state: IdeaState,
        task_id: Option<&str>,
    ) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE ideas SET state = ?, task_id = COALESCE(?, task_id), updated_at = ? \
             WHERE profile_id = ? AND id = ?",
        )
        .bind(state.as_str())
        .bind(task_id)
        .bind(stamp(Utc::now()))
        .bind(profile_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("change an idea's state")?;
        Ok(done.rows_affected() > 0)
    }

    async fn topics_since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<Vec<String>> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT topic, created_at FROM ideas WHERE profile_id = ?")
                .bind(profile_id)
                .fetch_all(&self.pool)
                .await
                .context("read the topics a member's ideas built on")?;
        Ok(rows
            .into_iter()
            .filter(|(_, at)| parse_time(at).is_ok_and(|at| at >= since))
            .map(|(topic, _)| topic)
            .collect())
    }

    async fn declined_since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<Vec<String>> {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT topic, created_at FROM ideas WHERE profile_id = ? AND state = 'dismissed'",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("read the topics of the ideas a member turned down")?;
        Ok(rows
            .into_iter()
            .filter(|(_, at)| parse_time(at).is_ok_and(|at| at >= since))
            .map(|(topic, _)| topic)
            .collect())
    }

    async fn run(&self, profile_id: &str) -> Result<IdeaRun> {
        let row: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT requested_at, started_at, finished_at FROM idea_runs WHERE profile_id = ?",
        )
        .bind(profile_id)
        .fetch_optional(&self.pool)
        .await
        .context("read a member's idea runs")?;
        Ok(match row {
            None => IdeaRun::default(),
            Some((requested, started, finished)) => IdeaRun {
                requested_at: time_or_none(requested),
                started_at: time_or_none(started),
                finished_at: time_or_none(finished),
            },
        })
    }

    async fn mark_requested(&self, profile_id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO idea_runs (profile_id, requested_at) VALUES (?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET requested_at = excluded.requested_at",
        )
        .bind(profile_id)
        .bind(stamp(at))
        .execute(&self.pool)
        .await
        .context("note that a member asked for ideas")?;
        Ok(())
    }

    async fn mark_started(&self, profile_id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO idea_runs (profile_id, started_at) VALUES (?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET started_at = excluded.started_at",
        )
        .bind(profile_id)
        .bind(stamp(at))
        .execute(&self.pool)
        .await
        .context("note that ideas are being thought of")?;
        Ok(())
    }

    async fn mark_finished(&self, profile_id: &str, at: DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO idea_runs (profile_id, finished_at) VALUES (?, ?) \
             ON CONFLICT(profile_id) DO UPDATE SET finished_at = excluded.finished_at",
        )
        .bind(profile_id)
        .bind(stamp(at))
        .execute(&self.pool)
        .await
        .context("note that ideas were thought of")?;
        Ok(())
    }

    async fn research_seeds(
        &self,
        profile_id: &str,
        unused_since: DateTime<Utc>,
    ) -> Result<Vec<TopicSeed>> {
        type SeedRow = (String, String, String, String, f64, String, Option<String>);
        let rows: Vec<SeedRow> = match sqlx::query_as(
            "SELECT id, area, subject, query, score, why, used_at FROM member_topics \
             WHERE profile_id = ? AND newsworthy = 0 ORDER BY score DESC, rowid ASC",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        {
            Ok(rows) => rows,
            // The topic map's table arrives with the news; until then ideas seed from interests.
            Err(e) if e.to_string().contains("no such table") => return Ok(Vec::new()),
            Err(e) => return Err(e).context("read a member's topic map"),
        };
        Ok(rows
            .into_iter()
            .filter(|row| time_or_none(row.6.clone()).is_none_or(|used| used < unused_since))
            .map(|(id, area, subject, query, score, why, _)| TopicSeed {
                id,
                area,
                subject,
                query,
                score,
                why,
            })
            .collect())
    }

    async fn mark_seed_used(
        &self,
        profile_id: &str,
        seed_id: &str,
        at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query("UPDATE member_topics SET used_at = ? WHERE profile_id = ? AND id = ?")
            .bind(stamp(at))
            .bind(profile_id)
            .bind(seed_id)
            .execute(&self.pool)
            .await
            .context("mark a topic used by an idea")?;
        Ok(())
    }

    async fn add_todo(&self, profile_id: &str, idea_id: &str, text: &str) -> Result<IdeaTodo> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO todos (id, profile_id, kind, text, ask, source, source_ref, created_at, \
             updated_at) VALUES (?, ?, 'todo', ?, 1, 'idea', ?, ?, ?)",
        )
        .bind(&id)
        .bind(profile_id)
        .bind(text.trim())
        .bind(idea_id)
        .bind(stamp(now))
        .bind(stamp(now))
        .execute(&self.pool)
        .await
        .context("add a to-do from an idea")?;
        Ok(IdeaTodo {
            id,
            text: text.trim().to_string(),
            created_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The news engineer's DDL, exactly; it arrives on their branch, not in this one's migration.
    const MEMBER_TOPICS: &str = "CREATE TABLE IF NOT EXISTS member_topics (id TEXT PRIMARY KEY, \
        profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE, area TEXT NOT NULL, \
        subject TEXT NOT NULL, query TEXT NOT NULL, newsworthy INTEGER NOT NULL, score REAL NOT \
        NULL, why TEXT NOT NULL DEFAULT '', built_at TEXT NOT NULL, used_at TEXT);";

    async fn pool() -> Pool<Sqlite> {
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
        pool
    }

    fn idea(id: &str, owner: &str, topic: &str) -> Idea {
        Idea {
            id: id.to_string(),
            profile_id: owner.to_string(),
            topic: topic.to_string(),
            kind: IdeaKind::Outing,
            kind_label: IdeaKind::Outing.label().to_string(),
            title: "Catch the January dip on Fielders".to_string(),
            hook: "Prices dip in January.".to_string(),
            pitch: "Prices soften after the import rush.".to_string(),
            why: "You are shopping for a Toyota Fielder under KSh 1.2M.".to_string(),
            facts: vec![IdeaFact {
                label: "Where".into(),
                value: "Mombasa Road yards".into(),
            }],
            notes: "Yards open at 8.".to_string(),
            grounded_in: vec![Grounding {
                kind: "interest".into(),
                label: "Toyota Fielder".into(),
            }],
            sources: vec![IdeaSource {
                title: "Used car prices".into(),
                url: "https://example.com/prices".into(),
                site: "example.com".into(),
            }],
            picture: None,
            goose_can: Some(GooseCan {
                label: "Watch the listings".into(),
                kind: pond_core::user_data::domain::idea::GooseKind::Watch,
            }),
            goose_plan: GoosePlan {
                prompt: None,
                listing_url: Some("https://example.com/fielders".into()),
            },
            you_can: vec!["Set a budget ceiling".into()],
            state: IdeaState::New,
            progress: None,
            created_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn an_idea_round_trips_and_stays_its_owners() {
        let repo = SqliteIdeaRepository::new(pool().await);
        repo.add(&idea("i1", "jerry", "Toyota Fielder"))
            .await
            .unwrap();
        let back = repo.get("jerry", "i1").await.unwrap().unwrap();
        assert_eq!(
            back.goose_plan.listing_url.as_deref(),
            Some("https://example.com/fielders")
        );
        assert_eq!(back.sources.len(), 1);
        assert_eq!(back.kind, IdeaKind::Outing);
        assert_eq!(back.kind_label, "Get outside");
        assert_eq!(back.hook, "Prices dip in January.");
        assert_eq!(back.facts[0].value, "Mombasa Road yards");
        assert_eq!(back.notes, "Yards open at 8.");
        assert!(repo.get("liz", "i1").await.unwrap().is_none());
        assert!(repo.list("liz").await.unwrap().is_empty());

        assert!(repo
            .set_state("jerry", "i1", IdeaState::Working, Some("task-9"))
            .await
            .unwrap());
        let working = repo.get("jerry", "i1").await.unwrap().unwrap();
        assert_eq!(working.progress.unwrap().task_id, "task-9");

        let week = Utc::now() - chrono::Duration::days(7);
        assert!(repo.declined_since("jerry", week).await.unwrap().is_empty());
        repo.set_state("jerry", "i1", IdeaState::Dismissed, None)
            .await
            .unwrap();
        assert!(repo.list("jerry").await.unwrap().is_empty());
        assert_eq!(
            repo.declined_since("jerry", week).await.unwrap(),
            vec!["Toyota Fielder"],
            "what they turned down is remembered as a topic"
        );
        assert!(repo.declined_since("liz", week).await.unwrap().is_empty());
        assert_eq!(
            repo.topics_since("jerry", Utc::now() - chrono::Duration::days(7))
                .await
                .unwrap(),
            vec!["Toyota Fielder"],
            "a dismissed idea's topic still counts as used"
        );
    }

    #[tokio::test]
    async fn a_run_is_pending_from_the_ask_until_it_finishes() {
        let repo = SqliteIdeaRepository::new(pool().await);
        assert!(!repo.run("jerry").await.unwrap().is_pending());
        let t0 = Utc::now() - chrono::Duration::minutes(5);
        repo.mark_requested("jerry", t0).await.unwrap();
        assert!(repo.run("jerry").await.unwrap().is_pending());
        repo.mark_started("jerry", t0 + chrono::Duration::minutes(1))
            .await
            .unwrap();
        repo.mark_finished("jerry", t0 + chrono::Duration::minutes(2))
            .await
            .unwrap();
        assert!(!repo.run("jerry").await.unwrap().is_pending());
    }

    #[tokio::test]
    async fn seeds_are_the_unused_topics_the_news_does_not_cover_best_first() {
        let pool = pool().await;
        let repo = SqliteIdeaRepository::new(pool.clone());
        assert!(
            repo.research_seeds("jerry", Utc::now())
                .await
                .unwrap()
                .is_empty(),
            "no topic map yet is no seeds, not an error"
        );
        sqlx::query(MEMBER_TOPICS).execute(&pool).await.unwrap();
        let long_ago = stamp(Utc::now() - chrono::Duration::days(30));
        let yesterday = stamp(Utc::now() - chrono::Duration::days(1));
        for (id, owner, newsworthy, score, used) in [
            ("t1", "jerry", 0, 0.6, None),
            ("t2", "jerry", 0, 0.9, Some(long_ago.as_str())),
            ("t3", "jerry", 1, 0.99, None),
            ("t4", "jerry", 0, 0.95, Some(yesterday.as_str())),
            ("t5", "liz", 0, 0.99, None),
        ] {
            sqlx::query(
                "INSERT INTO member_topics (id, profile_id, area, subject, query, newsworthy, \
                 score, why, built_at, used_at) VALUES (?, ?, 'cars', 'Fielder', 'q', ?, ?, '', \
                 ?, ?)",
            )
            .bind(id)
            .bind(owner)
            .bind(newsworthy)
            .bind(score)
            .bind(stamp(Utc::now()))
            .bind(used)
            .execute(&pool)
            .await
            .unwrap();
        }
        let week_ago = Utc::now() - chrono::Duration::days(7);
        let seeds = repo.research_seeds("jerry", week_ago).await.unwrap();
        assert_eq!(
            seeds.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["t2", "t1"]
        );
        repo.mark_seed_used("jerry", "t2", Utc::now())
            .await
            .unwrap();
        let seeds = repo.research_seeds("jerry", week_ago).await.unwrap();
        assert_eq!(seeds.len(), 1);
    }

    #[tokio::test]
    async fn a_step_becomes_an_open_todo_from_the_idea() {
        let pool = pool().await;
        let repo = SqliteIdeaRepository::new(pool.clone());
        let todo = repo
            .add_todo("jerry", "i1", "  Compare three listings  ")
            .await
            .unwrap();
        assert_eq!(todo.text, "Compare three listings");
        let (source, source_ref, state, ask): (String, String, String, i64) =
            sqlx::query_as("SELECT source, source_ref, state, ask FROM todos WHERE id = ?")
                .bind(&todo.id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (source.as_str(), source_ref.as_str(), state.as_str(), ask),
            ("idea", "i1", "open", 1)
        );
    }

    #[tokio::test]
    async fn working_idea_auto_syncs_to_done_when_task_or_todos_complete() {
        let pool = pool().await;
        let repo = SqliteIdeaRepository::new(pool.clone());
        let mut i1 = idea("i-task", "jerry", "ai");
        i1.state = IdeaState::Working;
        repo.add(&i1).await.unwrap();
        repo.set_state("jerry", "i-task", IdeaState::Working, Some("t-99"))
            .await
            .unwrap();

        let got = repo.get("jerry", "i-task").await.unwrap().unwrap();
        assert_eq!(got.state, IdeaState::Working);

        sqlx::query(
            "INSERT INTO long_tasks (id, owner_profile_id, scope, goal, state, max_steps, created_at, updated_at) \
             VALUES ('t-99', 'jerry', 'owner', 'research', 'completed', 10, datetime('now'), datetime('now'))",
        )
        .execute(&pool)
        .await
        .unwrap();

        let list = repo.list("jerry").await.unwrap();
        assert_eq!(list[0].state, IdeaState::Done);
    }
}
