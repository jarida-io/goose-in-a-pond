//! SQLite-backed [`ReminderRepository`]. Dedup is `ON CONFLICT (window_id, about_key) DO NOTHING`:
//! check-then-insert races on overlapping re-walks, and `DO UPDATE` would revive dismissed rows.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use sqlx::{Pool, Sqlite};

/// Shared by every SELECT so each matches `ReminderRow`'s field order.
const REMINDER_COLUMNS: &str = "id, about, when_said, session_id, window_id, subject, \
     profile_id, said_at, captured_at, disposition";

/// One row of `REMINDER_COLUMNS`, in order.
type ReminderRow = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    String,
    String,
);

pub struct SqliteReminderRepository {
    pool: Pool<Sqlite>,
}

impl SqliteReminderRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

/// Must match the profile-delete trigger's `strftime('%Y-%m-%dT%H:%M:%SZ', 'now')`.
fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("unreadable reminder timestamp: {raw}"))
}

/// An unreadable disposition is an error, not `Pending`: that default would resurface the row.
fn row_to_reminder(row: ReminderRow) -> Result<CapturedReminder> {
    let (
        id,
        about,
        when_said,
        session_id,
        window_id,
        subject,
        profile_id,
        said_at,
        captured_at,
        disposition,
    ) = row;

    let disposition = ReminderDisposition::parse(&disposition)
        .with_context(|| format!("unreadable disposition on reminder {id}: {disposition}"))?;

    Ok(CapturedReminder {
        id,
        about,
        when_said,
        session_id,
        window_id,
        subject,
        profile_id,
        said_at: parse_ts(&said_at)?,
        captured_at: parse_ts(&captured_at)?,
        disposition,
    })
}

/// Same predicate shape as `sqlite_memory`; `Guest` is refused in SQL, so it fails closed.
fn scope_sql(scope: &ProfileScope) -> (&'static str, Option<&str>) {
    match scope {
        ProfileScope::Owner(id) => (
            "AND (profile_id = ? OR profile_id IS NULL)",
            Some(id.as_str()),
        ),
        ProfileScope::Household => ("", None),
        ProfileScope::Guest => ("AND 1 = 0", None),
    }
}

#[async_trait]
impl ReminderRepository for SqliteReminderRepository {
    async fn capture(&self, reminder: &CapturedReminder) -> Result<bool> {
        let result = sqlx::query(
            "INSERT INTO reminders \
             (id, about, when_said, about_key, session_id, window_id, subject, profile_id, \
              said_at, captured_at, disposition) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (window_id, about_key) DO NOTHING",
        )
        .bind(&reminder.id)
        .bind(&reminder.about)
        .bind(&reminder.when_said)
        .bind(reminder.dedup_key())
        .bind(&reminder.session_id)
        .bind(&reminder.window_id)
        .bind(&reminder.subject)
        .bind(&reminder.profile_id)
        .bind(sql_ts(reminder.said_at))
        .bind(sql_ts(reminder.captured_at))
        .bind(reminder.disposition.as_str())
        .execute(&self.pool)
        .await
        .with_context(|| format!("storing reminder {}", reminder.id))?;

        Ok(result.rows_affected() > 0)
    }

    async fn list_pending(
        &self,
        scope: &ProfileScope,
        limit: usize,
    ) -> Result<Vec<CapturedReminder>> {
        let (predicate, owner) = scope_sql(scope);
        let sql = format!(
            "SELECT {REMINDER_COLUMNS} FROM reminders \
             WHERE disposition = 'pending' {predicate} ORDER BY said_at DESC LIMIT ?"
        );
        let mut query = sqlx::query_as::<_, ReminderRow>(&sql);
        if let Some(owner) = owner {
            query = query.bind(owner);
        }
        let rows: Vec<ReminderRow> = query
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
            .context("listing pending reminders")?;

        // Skip unreadable rows rather than fail the read: one bad row must not hide the rest.
        Ok(rows
            .into_iter()
            .filter_map(|row| match row_to_reminder(row) {
                Ok(reminder) => Some(reminder),
                Err(e) => {
                    tracing::warn!("[reminders] skipping an unreadable row: {e}");
                    None
                }
            })
            .collect())
    }

    /// `disposition = 'pending'` in the WHERE makes a race's loser get `false`, not overwrite.
    async fn set_disposition(
        &self,
        id: &str,
        scope: &ProfileScope,
        disposition: ReminderDisposition,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let (predicate, owner) = scope_sql(scope);
        let sql = format!(
            "UPDATE reminders SET disposition = ?, decided_at = ? \
             WHERE id = ? AND disposition = 'pending' {predicate}"
        );
        let mut query = sqlx::query(&sql)
            .bind(disposition.as_str())
            .bind(sql_ts(at))
            .bind(id);
        if let Some(owner) = owner {
            query = query.bind(owner);
        }
        let result = query
            .execute(&self.pool)
            .await
            .with_context(|| format!("disposing of reminder {id}"))?;
        Ok(result.rows_affected() > 0)
    }

    async fn offer(&self, id: &str, at: DateTime<Utc>) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE reminders SET disposition = 'offered', decided_at = ?, offered_at = ? \
             WHERE id = ? AND disposition = 'pending' AND profile_id IS NOT NULL",
        )
        .bind(sql_ts(at))
        .bind(sql_ts(at))
        .bind(id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("offering reminder {id}"))?;
        Ok(result.rows_affected() > 0)
    }

    async fn open_to(&self, profile_id: &str) -> Result<Vec<CapturedReminder>> {
        let sql = format!(
            "SELECT {REMINDER_COLUMNS} FROM reminders \
             WHERE disposition IN ('offered', 'pending') AND profile_id = ? \
             ORDER BY said_at DESC, rowid DESC"
        );
        let rows: Vec<ReminderRow> = sqlx::query_as::<_, ReminderRow>(&sql)
            .bind(profile_id)
            .fetch_all(&self.pool)
            .await
            .context("listing open reminders")?;
        Ok(rows
            .into_iter()
            .filter_map(|row| match row_to_reminder(row) {
                Ok(reminder) => Some(reminder),
                Err(e) => {
                    tracing::warn!("[reminders] skipping an unreadable row: {e}");
                    None
                }
            })
            .collect())
    }

    /// The disposition in the WHERE: an Add and a No racing, or an answer and the offer job, settle
    /// it once.
    async fn settle_offer(
        &self,
        profile_id: &str,
        id: &str,
        accepted: bool,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE reminders SET disposition = ?, decided_at = ? \
             WHERE id = ? AND profile_id = ? AND disposition IN ('offered', 'pending')",
        )
        .bind(if accepted { "accepted" } else { "dismissed" })
        .bind(sql_ts(at))
        .bind(id)
        .bind(profile_id)
        .execute(&self.pool)
        .await
        .with_context(|| format!("settling the offer of reminder {id}"))?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn db() -> (tempfile::TempDir, Pool<Sqlite>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        (tmp, pool)
    }

    fn reminder(id: &str, window: &str, about: &str) -> CapturedReminder {
        CapturedReminder {
            id: id.into(),
            about: about.into(),
            when_said: "next Tuesday".into(),
            session_id: "sess-1".into(),
            window_id: window.into(),
            subject: "Jerry".into(),
            profile_id: None,
            said_at: DateTime::from_timestamp(1_785_000_000, 0).unwrap(),
            captured_at: DateTime::from_timestamp(1_785_600_000, 0).unwrap(),
            disposition: ReminderDisposition::Pending,
        }
    }

    /// `profile_id` has no foreign key, so the fixture needs no profile row.
    fn reminder_of(id: &str, about: &str, owner: Option<&str>) -> CapturedReminder {
        CapturedReminder {
            profile_id: owner.map(str::to_string),
            window_id: format!("w-{id}"),
            ..reminder(id, "w", about)
        }
    }

    async fn seeded() -> (tempfile::TempDir, SqliteReminderRepository) {
        let (tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);
        for r in [
            reminder_of("r-liz", "the clinic", Some("liz")),
            reminder_of("r-jerry", "the dentist", Some("jerry")),
            reminder_of("r-anyone", "the bins", None),
        ] {
            assert!(repo.capture(&r).await.unwrap());
        }
        (tmp, repo)
    }

    fn ids(rows: &[CapturedReminder]) -> Vec<&str> {
        let mut v: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        v.sort_unstable();
        v
    }

    #[tokio::test]
    async fn an_owner_reads_their_own_and_the_unattributed_but_not_another_members() {
        let (_tmp, repo) = seeded().await;
        let jerry = repo
            .list_pending(&ProfileScope::Owner("jerry".into()), 10)
            .await
            .unwrap();
        assert_eq!(
            ids(&jerry),
            vec!["r-anyone", "r-jerry"],
            "Liz's clinic date reached Jerry"
        );
    }

    #[tokio::test]
    async fn a_guest_reads_no_reminder_at_all() {
        let (_tmp, repo) = seeded().await;
        assert!(repo
            .list_pending(&ProfileScope::Guest, 10)
            .await
            .unwrap()
            .is_empty());
    }

    /// Control for the two above: an adapter returning nothing would pass both.
    #[tokio::test]
    async fn the_household_read_sees_every_members_reminder() {
        let (_tmp, repo) = seeded().await;
        let all = repo
            .list_pending(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(ids(&all), vec!["r-anyone", "r-jerry", "r-liz"]);
    }

    #[tokio::test]
    async fn one_member_cannot_dismiss_another_members_reminder() {
        let (_tmp, repo) = seeded().await;
        let jerry = ProfileScope::Owner("jerry".into());

        assert!(
            !repo
                .set_disposition("r-liz", &jerry, ReminderDisposition::Dismissed, Utc::now())
                .await
                .unwrap(),
            "Jerry dismissed Liz's reminder"
        );
        assert!(
            !repo
                .set_disposition(
                    "r-liz",
                    &ProfileScope::Guest,
                    ReminderDisposition::Dismissed,
                    Utc::now()
                )
                .await
                .unwrap(),
            "a guest dismissed Liz's reminder"
        );
        let liz = repo
            .list_pending(&ProfileScope::Owner("liz".into()), 10)
            .await
            .unwrap();
        assert!(
            ids(&liz).contains(&"r-liz"),
            "the refused dismiss moved the row anyway"
        );

        // Control: the owner can still dismiss their own.
        assert!(repo
            .set_disposition(
                "r-jerry",
                &jerry,
                ReminderDisposition::Dismissed,
                Utc::now()
            )
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn a_captured_reminder_round_trips() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);

        assert!(repo
            .capture(&reminder("r-1", "w-1", "the dentist"))
            .await
            .unwrap());

        let back = repo
            .list_pending(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(
            back[0],
            reminder("r-1", "w-1", "the dentist"),
            "every field must survive the row, not just the id"
        );
        assert_eq!(
            back[0].when_said, "next Tuesday",
            "the subject's own words about the timing are what this table exists to keep"
        );
    }

    #[tokio::test]
    async fn re_capturing_the_same_window_writes_no_second_row() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);

        assert!(repo
            .capture(&reminder("r-1", "w-1", "the dentist"))
            .await
            .unwrap());
        // Fresh id and drifted spelling, as a second model call over the same messages produces.
        assert!(
            !repo
                .capture(&reminder("r-2", "w-1", "The dentist."))
                .await
                .unwrap(),
            "a re-walk must be told the row was already there"
        );

        let back = repo
            .list_pending(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "r-1", "the first row is the one that is kept");
    }

    #[tokio::test]
    async fn one_window_may_file_two_different_reminders() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);

        assert!(repo
            .capture(&reminder("r-1", "w-1", "the dentist"))
            .await
            .unwrap());
        assert!(repo
            .capture(&reminder("r-2", "w-1", "the school run"))
            .await
            .unwrap());
        assert_eq!(
            repo.list_pending(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn the_same_words_in_another_window_are_another_reminder() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);

        assert!(repo
            .capture(&reminder("r-1", "w-1", "the dentist"))
            .await
            .unwrap());
        assert!(repo
            .capture(&reminder("r-2", "w-2", "the dentist"))
            .await
            .unwrap());
        assert_eq!(
            repo.list_pending(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_dismissed_reminder_is_not_revived_by_a_re_walk() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool.clone());

        repo.capture(&reminder("r-1", "w-1", "the dentist"))
            .await
            .unwrap();
        repo.set_disposition(
            "r-1",
            &ProfileScope::Household,
            ReminderDisposition::Dismissed,
            Utc::now(),
        )
        .await
        .unwrap();

        assert!(!repo
            .capture(&reminder("r-2", "w-1", "the dentist"))
            .await
            .unwrap());
        assert!(
            repo.list_pending(&ProfileScope::Household, 10)
                .await
                .unwrap()
                .is_empty(),
            "DO NOTHING, not DO UPDATE -- an upsert would put a dismissed reminder back"
        );

        let (disposition,): (String,) =
            sqlx::query_as("SELECT disposition FROM reminders WHERE id = 'r-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(disposition, "dismissed");
    }

    /// Covers the migration's profile-delete trigger, not Rust code.
    #[tokio::test]
    async fn deleting_a_member_expires_their_pending_reminders() {
        let (_tmp, pool) = db().await;
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES ('liz', 'Liz')")
            .execute(&pool)
            .await
            .unwrap();
        let repo = SqliteReminderRepository::new(pool.clone());

        let mut hers = reminder("r-1", "w-1", "the dentist");
        hers.profile_id = Some("liz".into());
        repo.capture(&hers).await.unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&pool)
            .await
            .unwrap();

        assert!(repo
            .list_pending(&ProfileScope::Household, 10)
            .await
            .unwrap()
            .is_empty());
        let (disposition, profile_id, session_id): (String, Option<String>, String) =
            sqlx::query_as(
                "SELECT disposition, profile_id, session_id FROM reminders WHERE id = 'r-1'",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(disposition, "expired");
        assert_eq!(profile_id, None, "the row is released, not deleted");
        assert_eq!(
            session_id, "sess-1",
            "and it keeps saying where it came from"
        );
    }

    /// A row each: only a `pending` row moves, so one shared row would test only the first value.
    #[tokio::test]
    async fn every_disposition_the_domain_can_write_is_one_the_table_accepts() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);

        for (i, disposition) in [
            ReminderDisposition::Pending,
            ReminderDisposition::Proposed,
            ReminderDisposition::Dismissed,
            ReminderDisposition::Expired,
        ]
        .into_iter()
        .enumerate()
        {
            let id = format!("r-{i}");
            repo.capture(&reminder(&id, &format!("w-{i}"), "the dentist"))
                .await
                .unwrap();
            let moved = repo
                .set_disposition(&id, &ProfileScope::Household, disposition, Utc::now())
                .await
                .unwrap_or_else(|e| panic!("the table refused {}: {e}", disposition.as_str()));
            assert!(moved, "{} moved no row", disposition.as_str());
        }
    }

    #[tokio::test]
    async fn disposing_says_whether_there_was_anything_to_dispose_of() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);
        repo.capture(&reminder("r-1", "w-1", "the dentist"))
            .await
            .unwrap();

        assert!(repo
            .set_disposition(
                "r-1",
                &ProfileScope::Household,
                ReminderDisposition::Dismissed,
                Utc::now()
            )
            .await
            .unwrap());
        assert!(
            !repo
                .set_disposition(
                    "r-1",
                    &ProfileScope::Household,
                    ReminderDisposition::Proposed,
                    Utc::now()
                )
                .await
                .unwrap(),
            "a decided reminder is not decided again"
        );
        assert!(
            !repo
                .set_disposition(
                    "nobody",
                    &ProfileScope::Household,
                    ReminderDisposition::Dismissed,
                    Utc::now()
                )
                .await
                .unwrap(),
            "an unknown id moves nothing"
        );
    }
}
