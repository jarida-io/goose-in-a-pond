//! SQLite-backed [`ReminderRepository`] — the rows migration 0057 added.
//!
//! # The one thing to understand before editing a query here
//!
//! **The insert is `ON CONFLICT DO NOTHING`, and that is the deduplication.**
//! Not a read followed by a write: the engine re-walks conversations, two lane
//! ticks can overlap a restart, and a check-then-insert would be a race that
//! files the same reminder twice on exactly the pond that walks its history
//! fastest. The UNIQUE constraint on `(window_id, about_key)` is the guard, the
//! database enforces it, and `rows_affected` is how the caller learns which of
//! the two happened.
//!
//! `DO NOTHING` rather than `DO UPDATE` is also deliberate: the row that is
//! already there may have been dismissed, and an upsert would quietly resurrect
//! it as pending. A second sighting of a reminder somebody already said no to is
//! not new information.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::user_data::domain::profile::ProfileScope;
use pond_core::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use sqlx::{Pool, Sqlite};

/// The columns a reminder is rebuilt from. Stated once so a column added to one
/// query and not the others shifts a tuple field at compile time rather than at
/// runtime -- the same reason `sqlite_proposal.rs` has `PROPOSAL_COLUMNS`.
const REMINDER_COLUMNS: &str = "id, about, when_said, session_id, window_id, subject, \
     profile_id, said_at, captured_at, disposition";

/// `(id, about, when_said, session_id, window_id, subject, profile_id, said_at,
/// captured_at, disposition)`.
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

/// The wire format for this table's three timestamp columns.
///
/// Seconds precision, UTC, `Z`-suffixed, matching what `sqlite_proposal.rs`
/// writes. 0057's profile-delete trigger stamps `decided_at` with
/// `strftime('%Y-%m-%dT%H:%M:%SZ', 'now')`, which is this spelling, so a row
/// disposed of by SQL and a row disposed of by Rust read back the same.
fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("unreadable reminder timestamp: {raw}"))
}

/// Rebuild a reminder from its row.
///
/// An unreadable disposition is an error rather than a default, because the
/// default anybody would reach for is `Pending` and that is the one value that
/// puts the row back in front of the household. On failure, access narrows.
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

/// The scope predicate, in the same shape `sqlite_suggestion_queue` and
/// `sqlite_memory` use: an `Owner` sees their own rows and the unattributed
/// ones, a `Household` sees everything, a `Guest` sees nothing -- and says so
/// in SQL, so a caller that forgot to check still fails closed.
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

        // A row that cannot be rebuilt is dropped from the answer rather than
        // failing the whole read: one unreadable timestamp must not hide every
        // other reminder the household is holding.
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

    /// The `disposition = 'pending'` clause is the whole of the concurrency
    /// story here, and it is in the WHERE rather than in a read before the
    /// write for the same reason `capture` leans on the UNIQUE constraint: two
    /// callers can arrive at once -- the promotion run and a person pressing
    /// dismiss -- and a check-then-update would let the second overwrite the
    /// first's decision. `rows_affected` is how the caller learns it lost.
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

    /// A reminder said in one member's conversation. `profile_id` carries no
    /// foreign key (see 0057), so no profile row is needed for the fixture.
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

    /// THE DEFECT: `list_pending` filtered only on disposition, and the batch
    /// engine stamps each reminder with the member whose conversation said it
    /// -- so any caller read every member's dated reminders.
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

    /// The control for the two above: `Household` -- the engine's own read,
    /// which routes each reminder to its owner -- sees every row. Without it,
    /// an adapter that returned nothing for everyone would pass both.
    #[tokio::test]
    async fn the_household_read_sees_every_members_reminder() {
        let (_tmp, repo) = seeded().await;
        let all = repo
            .list_pending(&ProfileScope::Household, 10)
            .await
            .unwrap();
        assert_eq!(ids(&all), vec!["r-anyone", "r-jerry", "r-liz"]);
    }

    /// A move is scoped as well as a read: dismissing somebody else's
    /// reminder deletes their date as surely as reading it discloses it.
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
        // Still pending, for the person it belongs to.
        let liz = repo
            .list_pending(&ProfileScope::Owner("liz".into()), 10)
            .await
            .unwrap();
        assert!(
            ids(&liz).contains(&"r-liz"),
            "the refused dismiss moved the row anyway"
        );

        // The control: the owner CAN dismiss their own. A move that refused
        // everybody would pass everything above.
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

    /// The re-walk case. The same window, read a second time, must not
    /// accumulate.
    #[tokio::test]
    async fn re_capturing_the_same_window_writes_no_second_row() {
        let (_tmp, pool) = db().await;
        let repo = SqliteReminderRepository::new(pool);

        assert!(repo
            .capture(&reminder("r-1", "w-1", "the dentist"))
            .await
            .unwrap());
        // A fresh id and a drifted spelling: what a second model call over the
        // same messages actually produces.
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

    /// Two different reminders out of one window are two rows.
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

    /// The same words out of a DIFFERENT window are a different reminder. A
    /// household that asks about the dentist again in March has asked again.
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

    /// A re-walk must not resurrect something somebody said no to.
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

    /// 0057's trigger, which is the layer under the Rust: a departed member's
    /// reminders stop being live, and the rows keep their provenance.
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

    /// The disposition vocabulary the table accepts is the one the domain
    /// writes. A value added to one and not the other fails here rather than at
    /// three in the morning on somebody's pond.
    ///
    /// A row each, because the move is out of `pending` and nothing else: four
    /// writes against one row would land the first and be refused three times
    /// over, and the test would pass while proving nothing about three of the
    /// four values.
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

    /// The route that dismisses a reminder has to be able to tell "there was
    /// one" from "there was not", or it answers "dismissed" for an id that does
    /// not exist.
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
