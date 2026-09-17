//! SQLite-backed [`SuggestionQueueRepository`] — the rows migration 0058 added.
//!
//! # The insert is `ON CONFLICT DO NOTHING`, and that is the deduplication
//!
//! Not a read followed by a write. Generation runs on the inference lane, a
//! restart can overlap two ticks, and a check-then-insert would be a race that
//! queues two questions about one note on exactly the pond that is idle
//! longest. The partial unique index is the guard, the database enforces it,
//! and `rows_affected` is how the caller learns which happened.
//!
//! `DO NOTHING` rather than `DO UPDATE` for the same reason the reminders
//! adapter chose it: the live row may be one the household has been looking at
//! for a week, and an upsert would swap the question under them between two
//! glances at the same screen.
//!
//! # `offerable` joins `memory_fragments`, on purpose
//!
//! A queued question whose note has been deleted, archived or superseded is an
//! offer whose reason is false — and the reason is the only thing that makes the
//! offer falsifiable. The join is the check, and it uses the SAME liveness
//! predicate as `sqlite_memory`'s own reads (`lifecycle IS NULL OR lifecycle =
//! 'active'`), so a memory the household can no longer retrieve cannot be one
//! the panel is still asking about.
//!
//! No foreign key: `memory_fragments` rows are removed by the household and by
//! decay, and a CASCADE would empty this table with nothing anywhere recording
//! that it had. A join is visible in a query plan and testable from a test.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::ports::suggestion_queue::{
    QueuedSuggestion, Settled, SuggestionQueueRepository,
};
use pond_core::user_data::services::suggestion_generation::GeneratedSuggestion;
use sqlx::{Pool, Sqlite};

/// Stated once, so a column added to one query and not another shifts a tuple
/// field at compile time rather than at runtime.
const QUEUE_COLUMNS: &str = "q.id, q.profile_id, q.prompt, q.reason, q.source_memory_id, \
     q.created_at";

/// `(id, profile_id, prompt, reason, source_memory_id, created_at)`.
type QueueRow = (String, Option<String>, String, String, String, String);

/// The liveness predicate, copied verbatim from `sqlite_memory`'s reads.
///
/// Verbatim and not "equivalent": if that file's definition of a live memory
/// ever changes, this one has to change with it, and an identical string is
/// what makes a grep find both.
const MEMORY_IS_LIVE: &str = "(m.lifecycle IS NULL OR m.lifecycle = 'active')";

pub struct SqliteSuggestionQueue {
    pool: Pool<Sqlite>,
}

impl SqliteSuggestionQueue {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Timestamps written by this adapter are RFC3339; the column default is
/// SQLite's `datetime('now')`, which is not. Both are read, because a row
/// inserted by a future path that leans on the default must not be unreadable.
fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }
    chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .map(|naive| naive.and_utc())
        .with_context(|| format!("unreadable suggestion timestamp: {raw}"))
}

/// The same shape `sqlite_memory::scope_sql` uses, against this table's own
/// alias. An `Owner` sees their own rows and the unattributed ones; a `Guest`
/// sees nothing and the predicate says so rather than relying on the caller to
/// short-circuit.
fn scope_sql(scope: &ProfileScope) -> (&'static str, Option<&str>) {
    match scope {
        ProfileScope::Owner(id) => (
            "AND (q.profile_id = ? OR q.profile_id IS NULL)",
            Some(id.as_str()),
        ),
        ProfileScope::Household => ("", None),
        ProfileScope::Guest => ("AND 1 = 0", None),
    }
}

fn row_to_suggestion(row: QueueRow) -> Result<QueuedSuggestion> {
    let (id, profile_id, prompt, reason, source_memory_id, created_at) = row;
    Ok(QueuedSuggestion {
        id,
        profile_id,
        prompt,
        reason,
        source_memory_id,
        created_at: parse_ts(&created_at)?,
    })
}

#[async_trait]
impl SuggestionQueueRepository for SqliteSuggestionQueue {
    async fn queue(&self, suggestions: &[GeneratedSuggestion]) -> Result<usize> {
        let mut stored = 0usize;
        for s in suggestions {
            // The id is the pond's, never the model's. A model-supplied id
            // would be a value the household's own rows are keyed on, written
            // by something that cannot be trusted to make it unique.
            let id = uuid::Uuid::new_v4().to_string();
            let result = sqlx::query(
                "INSERT INTO suggestion_queue \
                 (id, profile_id, prompt, reason, source_memory_id, answered_by, created_at, state) \
                 VALUES (?, ?, ?, ?, ?, 'giap-memory', ?, 'queued') \
                 ON CONFLICT DO NOTHING",
            )
            .bind(&id)
            .bind(s.profile_id.as_deref())
            .bind(&s.prompt)
            .bind(&s.reason)
            .bind(&s.source_memory_id)
            .bind(sql_ts(Utc::now()))
            .execute(&self.pool)
            .await
            .context("queue a composed suggestion")?;
            stored += result.rows_affected() as usize;
        }
        Ok(stored)
    }

    async fn offerable(&self, scope: &ProfileScope, limit: usize) -> Result<Vec<QueuedSuggestion>> {
        let (filter, bind) = scope_sql(scope);
        let sql = format!(
            "SELECT {QUEUE_COLUMNS} FROM suggestion_queue q \
             JOIN memory_fragments m ON m.id = q.source_memory_id \
             WHERE q.state = 'queued' AND {MEMORY_IS_LIVE} {filter} \
             ORDER BY q.created_at DESC LIMIT ?"
        );
        let mut query = sqlx::query_as::<_, QueueRow>(&sql);
        if let Some(value) = bind {
            query = query.bind(value);
        }
        let rows = query
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .context("read the offerable suggestions")?;
        rows.into_iter().map(row_to_suggestion).collect()
    }

    async fn settle(&self, id: &str, outcome: Settled) -> Result<bool> {
        // `state = 'queued'` in the predicate is what makes this answer the
        // question the caller asked. Without it a second tap would report
        // success, and "already taken" would be indistinguishable from "no such
        // suggestion" -- one is a household being quick on a touch panel and
        // the other is a bug.
        let result =
            sqlx::query("UPDATE suggestion_queue SET state = ? WHERE id = ? AND state = 'queued'")
                .bind(outcome.as_str())
                .bind(id)
                .execute(&self.pool)
                .await
                .context("settle a suggestion")?;
        Ok(result.rows_affected() > 0)
    }

    async fn live_memory_ids(&self) -> Result<Vec<String>> {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT source_memory_id FROM suggestion_queue WHERE state = 'queued'")
                .fetch_all(&self.pool)
                .await
                .context("read which memories already carry a suggestion")?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn queue_with_memories(memories: &[(&str, Option<&str>, &str)]) -> SqliteSuggestionQueue {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect(":memory:")
            .await
            .unwrap();
        sqlx::migrate!("migrations/system")
            .run(&pool)
            .await
            .unwrap();
        // `memory_fragments.profile_id` is a real foreign key, so a member has
        // to exist before a note can belong to them. Inserting the member is
        // part of the fixture rather than something to work around: a test that
        // disabled the constraint would not be testing this database.
        for member in ["jerry", "liz"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(member)
                .bind(member)
                .execute(&pool)
                .await
                .unwrap();
        }
        for (id, profile_id, lifecycle) in memories {
            sqlx::query(
                "INSERT INTO memory_fragments (id, profile_id, content, source, created_at, lifecycle) \
                 VALUES (?, ?, 'a note', 'extraction', datetime('now'), ?)",
            )
            .bind(id)
            .bind(*profile_id)
            .bind(lifecycle)
            .execute(&pool)
            .await
            .unwrap();
        }
        SqliteSuggestionQueue::new(pool)
    }

    fn generated(memory_id: &str, profile_id: Option<&str>, prompt: &str) -> GeneratedSuggestion {
        GeneratedSuggestion {
            source_memory_id: memory_id.to_string(),
            profile_id: profile_id.map(str::to_string),
            prompt: prompt.to_string(),
            reason: "From something you told me, saved 3 days ago.".to_string(),
        }
    }

    #[tokio::test]
    async fn a_queued_suggestion_comes_back_offerable() {
        let repo = queue_with_memories(&[("m1", None, "active")]).await;
        assert_eq!(
            repo.queue(&[generated("m1", None, "What time should I leave?")])
                .await
                .unwrap(),
            1
        );

        let offered = repo.offerable(&ProfileScope::Household, 10).await.unwrap();
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].prompt, "What time should I leave?");
        assert_eq!(offered[0].source_memory_id, "m1");
    }

    /// The whole reason the reason is checkable. A question about a note the
    /// household has deleted is an offer whose sentence underneath is false.
    #[tokio::test]
    async fn a_suggestion_whose_memory_is_gone_is_not_offered() {
        let repo = queue_with_memories(&[("m1", None, "active")]).await;
        repo.queue(&[generated("m1", None, "What time should I leave?")])
            .await
            .unwrap();
        // Control: it IS offered while the note is live.
        assert_eq!(
            repo.offerable(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            1
        );

        sqlx::query("DELETE FROM memory_fragments WHERE id = 'm1'")
            .execute(&repo.pool)
            .await
            .unwrap();

        assert!(
            repo.offerable(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .is_empty(),
            "the note is gone, so the question about it is not an offer any more"
        );
    }

    /// Archived and superseded are not deleted, and a household cannot retrieve
    /// them — so the panel must not still be asking about them. Same predicate
    /// `sqlite_memory`'s own reads use.
    #[tokio::test]
    async fn a_suggestion_about_an_archived_memory_is_not_offered() {
        for lifecycle in ["archived", "superseded"] {
            let repo = queue_with_memories(&[("m1", None, lifecycle)]).await;
            repo.queue(&[generated("m1", None, "What time should I leave?")])
                .await
                .unwrap();
            assert!(
                repo.offerable(&ProfileScope::Household, 10)
                    .await
                    .unwrap()
                    .is_empty(),
                "{lifecycle} memories are not live"
            );
        }
    }

    /// A composed question carries whatever its note carried, so scope is not
    /// optional here. `Guest` is the one that must never see anything.
    #[tokio::test]
    async fn scope_keeps_a_members_question_off_a_shared_screen() {
        let repo =
            queue_with_memories(&[("m1", Some("jerry"), "active"), ("m2", None, "active")]).await;
        repo.queue(&[
            generated("m1", Some("jerry"), "How did the swim go?"),
            generated("m2", None, "Is the bread in?"),
        ])
        .await
        .unwrap();

        let guest = repo.offerable(&ProfileScope::Guest, 10).await.unwrap();
        assert!(
            guest.is_empty(),
            "a guest sees no composed suggestion at all"
        );

        let jerry = repo
            .offerable(&ProfileScope::Owner("jerry".into()), 10)
            .await
            .unwrap();
        assert_eq!(jerry.len(), 2, "their own, plus the unattributed one");

        let liz = repo
            .offerable(&ProfileScope::Owner("liz".into()), 10)
            .await
            .unwrap();
        assert_eq!(
            liz.len(),
            1,
            "somebody else's note is not theirs to be asked about"
        );
        assert_eq!(liz[0].source_memory_id, "m2");
    }

    /// The partial unique index is the deduplication, and it is what stops a
    /// pass that runs every idle period from queueing a hundred variations of
    /// one note.
    #[tokio::test]
    async fn one_memory_carries_one_live_question() {
        let repo = queue_with_memories(&[("m1", None, "active")]).await;
        assert_eq!(
            repo.queue(&[generated("m1", None, "First question?")])
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            repo.queue(&[generated("m1", None, "A second, different question?")])
                .await
                .unwrap(),
            0,
            "the second is refused by the index rather than by a read-then-write race"
        );

        let offered = repo.offerable(&ProfileScope::Household, 10).await.unwrap();
        assert_eq!(offered.len(), 1);
        assert_eq!(
            offered[0].prompt, "First question?",
            "DO NOTHING, not DO UPDATE: the question must not change under somebody reading it"
        );
    }

    /// Once settled, a memory may carry a NEW question — a later pass sees more
    /// of the household's history than an earlier one did. What must not happen
    /// is two at once.
    #[tokio::test]
    async fn a_settled_memory_may_be_asked_about_again() {
        let repo = queue_with_memories(&[("m1", None, "active")]).await;
        repo.queue(&[generated("m1", None, "First question?")])
            .await
            .unwrap();
        let id = repo.offerable(&ProfileScope::Household, 10).await.unwrap()[0]
            .id
            .clone();
        assert!(repo.settle(&id, Settled::Taken).await.unwrap());

        assert_eq!(
            repo.queue(&[generated("m1", None, "A later question?")])
                .await
                .unwrap(),
            1
        );
        let offered = repo.offerable(&ProfileScope::Household, 10).await.unwrap();
        assert_eq!(offered.len(), 1, "still only one live at a time");
        assert_eq!(offered[0].prompt, "A later question?");
    }

    /// A double tap on a touch panel is a real event. "Already taken" and "no
    /// such suggestion" must be different answers: the first is a household
    /// being quick, the second is a bug.
    #[tokio::test]
    async fn settling_twice_reports_that_it_changed_nothing() {
        let repo = queue_with_memories(&[("m1", None, "active")]).await;
        repo.queue(&[generated("m1", None, "First question?")])
            .await
            .unwrap();
        let id = repo.offerable(&ProfileScope::Household, 10).await.unwrap()[0]
            .id
            .clone();

        assert!(repo.settle(&id, Settled::Taken).await.unwrap());
        assert!(!repo.settle(&id, Settled::Taken).await.unwrap());
        assert!(!repo
            .settle("never-existed", Settled::Dismissed)
            .await
            .unwrap());
        assert!(repo
            .offerable(&ProfileScope::Household, 10)
            .await
            .unwrap()
            .is_empty());
    }

    /// What the generation pass subtracts before choosing what to show the
    /// model, so a pass spends its slots on notes nobody has been asked about.
    #[tokio::test]
    async fn the_pass_can_see_which_memories_already_have_a_question() {
        let repo = queue_with_memories(&[("m1", None, "active"), ("m2", None, "active")]).await;
        repo.queue(&[generated("m1", None, "First question?")])
            .await
            .unwrap();
        assert_eq!(
            repo.live_memory_ids().await.unwrap(),
            vec!["m1".to_string()]
        );

        let id = repo.offerable(&ProfileScope::Household, 10).await.unwrap()[0]
            .id
            .clone();
        repo.settle(&id, Settled::Dismissed).await.unwrap();
        assert!(
            repo.live_memory_ids().await.unwrap().is_empty(),
            "a settled row no longer blocks its memory"
        );
    }
}
