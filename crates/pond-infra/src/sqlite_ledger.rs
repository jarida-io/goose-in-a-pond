//! SQLite-backed [`LedgerRepository`] over the `ledger_entries` table. `occurred_on` is a
//! `YYYY-MM-DD` date, so a month is a text range.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::NaiveDate;
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::ledger::LedgerEntry;
use pond_core::user_data::ports::ledger::LedgerRepository;

use crate::sqlite_rows::{stamp, time_of};

pub struct SqliteLedgerRepository {
    pool: Pool<Sqlite>,
}

impl SqliteLedgerRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, occurred_on, payee, amount_minor, currency, category, \
     anchor_id, note, created_at";

const DAY: &str = "%Y-%m-%d";

fn day(on: NaiveDate) -> String {
    on.format(DAY).to_string()
}

fn entry_of(row: &SqliteRow) -> Result<LedgerEntry> {
    let occurred_on: String = row.try_get("occurred_on")?;
    Ok(LedgerEntry {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        occurred_on: NaiveDate::parse_from_str(&occurred_on, DAY)
            .with_context(|| format!("unreadable ledger day {occurred_on:?}"))?,
        payee: row.try_get("payee")?,
        amount_minor: row.try_get("amount_minor")?,
        currency: row.try_get("currency")?,
        category: row.try_get("category")?,
        anchor_id: row.try_get("anchor_id")?,
        note: row.try_get("note")?,
        created_at: time_of(row, "created_at")?,
    })
}

#[async_trait]
impl LedgerRepository for SqliteLedgerRepository {
    async fn add(&self, entries: &[LedgerEntry]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for entry in entries {
            sqlx::query(
                "INSERT INTO ledger_entries (id, profile_id, occurred_on, payee, amount_minor, \
                 currency, category, anchor_id, note, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&entry.id)
            .bind(&entry.profile_id)
            .bind(day(entry.occurred_on))
            .bind(&entry.payee)
            .bind(entry.amount_minor)
            .bind(&entry.currency)
            .bind(&entry.category)
            .bind(&entry.anchor_id)
            .bind(&entry.note)
            .bind(stamp(entry.created_at))
            .execute(&mut *tx)
            .await
            .context("keep a ledger entry")?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<LedgerEntry>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM ledger_entries WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read a ledger entry")?;
        row.as_ref().map(entry_of).transpose()
    }

    async fn list(
        &self,
        profile_id: &str,
        from: NaiveDate,
        until: NaiveDate,
    ) -> Result<Vec<LedgerEntry>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM ledger_entries \
             WHERE profile_id = ? AND occurred_on >= ? AND occurred_on < ? \
             ORDER BY occurred_on DESC, created_at DESC, rowid DESC"
        ))
        .bind(profile_id)
        .bind(day(from))
        .bind(day(until))
        .fetch_all(&self.pool)
        .await
        .context("list a member's ledger entries")?;
        rows.iter().map(entry_of).collect()
    }

    async fn save(&self, entry: &LedgerEntry) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE ledger_entries SET occurred_on = ?, payee = ?, amount_minor = ?, \
             currency = ?, category = ?, anchor_id = ?, note = ? WHERE id = ? AND profile_id = ?",
        )
        .bind(day(entry.occurred_on))
        .bind(&entry.payee)
        .bind(entry.amount_minor)
        .bind(&entry.currency)
        .bind(&entry.category)
        .bind(&entry.anchor_id)
        .bind(&entry.note)
        .bind(&entry.id)
        .bind(&entry.profile_id)
        .execute(&self.pool)
        .await
        .context("save a ledger entry")?;
        Ok(done.rows_affected() > 0)
    }

    async fn delete(&self, profile_id: &str, id: &str) -> Result<bool> {
        let done = sqlx::query("DELETE FROM ledger_entries WHERE profile_id = ? AND id = ?")
            .bind(profile_id)
            .bind(id)
            .execute(&self.pool)
            .await
            .context("delete a ledger entry")?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use pond_core::user_data::domain::ledger::{month_bounds, month_totals};
    use pond_core::user_data::mocks::mock_ledger::InMemoryLedgerRepository;

    fn entry(member: &str, on: (i32, u32, u32), payee: &str, amount_minor: i64) -> LedgerEntry {
        LedgerEntry::new(
            member,
            NaiveDate::from_ymd_opt(on.0, on.1, on.2).unwrap(),
            payee,
            amount_minor,
            "KES",
            "groceries",
            noon(),
        )
        .unwrap()
    }

    fn payees(found: &[LedgerEntry]) -> Vec<&str> {
        found.iter().map(|e| e.payee.as_str()).collect()
    }

    async fn a_month_of_entries_round_trips_newest_day_first(repo: &dyn LedgerRepository) {
        let mut naivas = entry("jerry", (2026, 11, 3), "Naivas", 420_000);
        naivas.note = Some("weekly shop".into());
        naivas.anchor_id = Some("a-budget".into());
        let last_day = entry("jerry", (2026, 11, 30), "Carrefour", 180_050);
        let october = entry("jerry", (2026, 10, 31), "KPLC", 350_000);
        let december = entry("jerry", (2026, 12, 1), "Zuku", 499_900);
        repo.add(&[naivas.clone(), last_day.clone(), october, december])
            .await
            .unwrap();
        assert_eq!(
            repo.get("jerry", &naivas.id).await.unwrap(),
            Some(naivas.clone())
        );
        let (from, until) = month_bounds("2026-11").unwrap();
        let november = repo.list("jerry", from, until).await.unwrap();
        assert_eq!(payees(&november), ["Carrefour", "Naivas"]);
        assert_eq!(month_totals(&november)[0].amount_minor, 600_050);
        assert!(
            repo.add(&[last_day]).await.is_err(),
            "an entry is kept once"
        );
    }

    async fn a_member_never_reads_or_changes_anothers_ledger(repo: &dyn LedgerRepository) {
        let mut mine = entry("jerry", (2026, 11, 3), "Naivas", 420_000);
        let theirs = entry("liz", (2026, 11, 4), "Carrefour", 99_000);
        repo.add(&[mine.clone(), theirs.clone()]).await.unwrap();
        let (from, until) = month_bounds("2026-11").unwrap();
        assert_eq!(
            payees(&repo.list("jerry", from, until).await.unwrap()),
            ["Naivas"]
        );
        assert_eq!(repo.get("liz", &mine.id).await.unwrap(), None);
        assert!(!repo.delete("liz", &mine.id).await.unwrap());
        let mut forged = mine.clone();
        forged.profile_id = "liz".into();
        forged.amount_minor = 1;
        assert!(!repo.save(&forged).await.unwrap());

        mine.amount_minor = 430_000;
        mine.category = "household".into();
        assert!(repo.save(&mine).await.unwrap());
        assert_eq!(
            repo.get("jerry", &mine.id).await.unwrap(),
            Some(mine.clone())
        );
        assert!(repo.delete("jerry", &mine.id).await.unwrap());
        assert!(repo.list("jerry", from, until).await.unwrap().is_empty());
        assert_eq!(repo.list("liz", from, until).await.unwrap(), vec![theirs]);
    }

    async fn sqlite() -> SqliteLedgerRepository {
        SqliteLedgerRepository::new(pool().await)
    }

    #[tokio::test]
    async fn a_month_of_ledger_entries_round_trips_newest_day_first() {
        a_month_of_entries_round_trips_newest_day_first(&sqlite().await).await;
        a_month_of_entries_round_trips_newest_day_first(&InMemoryLedgerRepository::new()).await;
    }

    #[tokio::test]
    async fn one_member_never_reads_or_changes_anothers_ledger() {
        a_member_never_reads_or_changes_anothers_ledger(&sqlite().await).await;
        a_member_never_reads_or_changes_anothers_ledger(&InMemoryLedgerRepository::new()).await;
    }

    #[tokio::test]
    async fn entries_are_kept_together_or_not_at_all() {
        let repo = sqlite().await;
        let good = entry("jerry", (2026, 11, 3), "Naivas", 420_000);
        let orphan = entry("nobody", (2026, 11, 3), "Carrefour", 1);
        assert!(repo.add(&[good.clone(), orphan]).await.is_err());
        assert_eq!(repo.get("jerry", &good.id).await.unwrap(), None);
    }
}
