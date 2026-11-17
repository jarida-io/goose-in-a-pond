//! SQLite-backed [`FactSheetRepository`] over the `fact_sheet` table. The rules of a write are
//! `pond_core`'s (`fact_sheet::with_line`, `rebuilt`); this stores what they return.

use std::collections::HashMap;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::fact_sheet::{
    rank, rebuilt, with_line, Confidence, FactCategory, FactLine, FactSource, FactSourceKind,
    FactState,
};
use pond_core::user_data::ports::fact_sheet::FactSheetRepository;

use crate::sqlite_rows::{count_in, flag_in, json_in, json_of, stamp, time_of, word_in};

pub struct SqliteFactSheetRepository {
    pool: Pool<Sqlite>,
}

impl SqliteFactSheetRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const COLUMNS: &str = "id, profile_id, category, fact, confidence, since, source_kind, \
     source_ref, tools_json, pinned, score, used, state, created_at, updated_at";

fn line_of(row: &SqliteRow) -> Result<FactLine> {
    let category: String = row.try_get("category")?;
    Ok(FactLine {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        category: FactCategory::parse(&category),
        fact: row.try_get("fact")?,
        confidence: word_in(row, "confidence", Confidence::parse)?,
        since: row.try_get("since")?,
        source: FactSource {
            kind: word_in(row, "source_kind", FactSourceKind::parse)?,
            reference: row.try_get("source_ref")?,
        },
        tools: json_in(row, "tools_json")?,
        pinned: flag_in(row, "pinned")?,
        score: row.try_get("score")?,
        used: count_in(row, "used")?,
        state: word_in(row, "state", FactState::parse)?,
        created_at: time_of(row, "created_at")?,
        updated_at: time_of(row, "updated_at")?,
    })
}

impl SqliteFactSheetRepository {
    /// The member's rows in every state.
    async fn rows(&self, profile_id: &str) -> Result<Vec<FactLine>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fact_sheet WHERE profile_id = ? ORDER BY rowid"
        ))
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("read a member's fact sheet")?;
        rows.iter().map(line_of).collect()
    }

    /// Writes the rows of `after` that differ from `before`, together.
    async fn persist(&self, before: &[FactLine], after: &[FactLine]) -> Result<()> {
        let held: HashMap<&str, &FactLine> = before.iter().map(|r| (r.id.as_str(), r)).collect();
        let mut tx = self.pool.begin().await?;
        for row in after {
            if held.get(row.id.as_str()).is_some_and(|was| *was == row) {
                continue;
            }
            sqlx::query(
                "INSERT INTO fact_sheet (id, profile_id, category, fact, fact_key, confidence, \
                 since, source_kind, source_ref, tools_json, pinned, score, used, state, \
                 created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET category = excluded.category, \
                 fact = excluded.fact, fact_key = excluded.fact_key, \
                 confidence = excluded.confidence, since = excluded.since, \
                 source_kind = excluded.source_kind, source_ref = excluded.source_ref, \
                 tools_json = excluded.tools_json, pinned = excluded.pinned, \
                 score = excluded.score, used = excluded.used, state = excluded.state, \
                 updated_at = excluded.updated_at",
            )
            .bind(&row.id)
            .bind(&row.profile_id)
            .bind(row.category.as_str())
            .bind(&row.fact)
            .bind(row.key())
            .bind(row.confidence.as_str())
            .bind(&row.since)
            .bind(row.source.kind.as_str())
            .bind(&row.source.reference)
            .bind(json_of(&row.tools)?)
            .bind(row.pinned as i64)
            .bind(row.score)
            .bind(i64::from(row.used))
            .bind(row.state.as_str())
            .bind(stamp(row.created_at))
            .bind(stamp(row.updated_at))
            .execute(&mut *tx)
            .await
            .context("write a fact line")?;
        }
        tx.commit().await?;
        Ok(())
    }
}

#[async_trait]
impl FactSheetRepository for SqliteFactSheetRepository {
    async fn list(&self, profile_id: &str, state: FactState) -> Result<Vec<FactLine>> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fact_sheet WHERE profile_id = ? AND state = ?"
        ))
        .bind(profile_id)
        .bind(state.as_str())
        .fetch_all(&self.pool)
        .await
        .context("list a member's fact lines")?;
        let mut lines = rows.iter().map(line_of).collect::<Result<Vec<_>>>()?;
        rank(&mut lines);
        Ok(lines)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<FactLine>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fact_sheet WHERE profile_id = ? AND id = ?"
        ))
        .bind(profile_id)
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .context("read a fact line")?;
        row.as_ref().map(line_of).transpose()
    }

    async fn by_key(&self, profile_id: &str, fact_key: &str) -> Result<Option<FactLine>> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM fact_sheet WHERE profile_id = ? AND fact_key = ?"
        ))
        .bind(profile_id)
        .bind(fact_key)
        .fetch_optional(&self.pool)
        .await
        .context("read a fact line by its key")?;
        row.as_ref().map(line_of).transpose()
    }

    async fn write(&self, line: &FactLine) -> Result<Option<FactLine>> {
        let before = self.rows(&line.profile_id).await?;
        let Some((after, stored)) = with_line(before.clone(), line.clone(), line.updated_at) else {
            return Ok(None);
        };
        self.persist(&before, &after).await?;
        self.get(&line.profile_id, &stored.id).await
    }

    async fn replace(
        &self,
        profile_id: &str,
        incoming: &[FactLine],
        now: DateTime<Utc>,
    ) -> Result<Vec<FactLine>> {
        let incoming = incoming
            .iter()
            .filter(|line| line.profile_id == profile_id)
            .cloned()
            .collect();
        let before = self.rows(profile_id).await?;
        let after = rebuilt(before.clone(), incoming, now);
        self.persist(&before, &after).await?;
        self.list(profile_id, FactState::Active).await
    }

    async fn save(&self, line: &FactLine) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE fact_sheet SET category = ?, fact = ?, fact_key = ?, confidence = ?, \
             since = ?, source_kind = ?, source_ref = ?, tools_json = ?, pinned = ?, score = ?, \
             used = ?, state = ?, updated_at = ? WHERE id = ? AND profile_id = ?",
        )
        .bind(line.category.as_str())
        .bind(&line.fact)
        .bind(line.key())
        .bind(line.confidence.as_str())
        .bind(&line.since)
        .bind(line.source.kind.as_str())
        .bind(&line.source.reference)
        .bind(json_of(&line.tools)?)
        .bind(line.pinned as i64)
        .bind(line.score)
        .bind(i64::from(line.used))
        .bind(line.state.as_str())
        .bind(stamp(line.updated_at))
        .bind(&line.id)
        .bind(&line.profile_id)
        .execute(&self.pool)
        .await
        .context("save a fact line; its wording may be another line's")?;
        Ok(done.rows_affected() > 0)
    }

    async fn remove(&self, profile_id: &str, id: &str, now: DateTime<Utc>) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE fact_sheet SET state = 'removed', pinned = 0, updated_at = ? \
             WHERE profile_id = ? AND id = ? AND state != 'removed'",
        )
        .bind(stamp(now))
        .bind(profile_id)
        .bind(id)
        .execute(&self.pool)
        .await
        .context("remove a fact line")?;
        Ok(done.rows_affected() > 0)
    }

    async fn count_used(&self, profile_id: &str, ids: &[String]) -> Result<()> {
        for id in ids {
            sqlx::query("UPDATE fact_sheet SET used = used + 1 WHERE profile_id = ? AND id = ?")
                .bind(profile_id)
                .bind(id)
                .execute(&self.pool)
                .await
                .context("count a fact line's use")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::user_data::domain::fact_sheet::FACT_SHEET_LIMIT;
    use pond_core::user_data::mocks::mock_fact_sheet::InMemoryFactSheetRepository;

    fn guess(member: &str, fact: &str, score: f64) -> FactLine {
        let mut line = FactLine::new(
            member,
            FactCategory::Work,
            fact,
            Confidence::Likely,
            FactSource::new(FactSourceKind::Memory, Some("m1")),
            noon(),
        )
        .unwrap();
        line.score = score;
        line.tools = vec!["get_weather".into()];
        line
    }

    fn facts(lines: &[FactLine]) -> Vec<&str> {
        lines.iter().map(|l| l.fact.as_str()).collect()
    }

    async fn a_line_round_trips_and_one_key_is_one_row(repo: &dyn FactSheetRepository) {
        let first = guess("jerry", "Runs Jarida", 3.0);
        let stored = repo.write(&first).await.unwrap().unwrap();
        assert_eq!(stored, first);
        assert_eq!(
            repo.get("jerry", &first.id).await.unwrap(),
            Some(first.clone())
        );

        let again = repo
            .write(&guess("jerry", "runs  JARIDA!", 8.0))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.id, first.id, "the same key is the same row");
        assert_eq!(again.score, 8.0);
        let sheet = repo.list("jerry", FactState::Active).await.unwrap();
        assert_eq!(sheet.len(), 1);
        assert_eq!(
            repo.by_key("jerry", "runs jarida")
                .await
                .unwrap()
                .unwrap()
                .id,
            first.id
        );
    }

    async fn a_member_never_reads_or_changes_anothers_lines(repo: &dyn FactSheetRepository) {
        let mine = repo
            .write(&guess("jerry", "Runs Jarida", 3.0))
            .await
            .unwrap()
            .unwrap();
        repo.write(&guess("liz", "Runs Jarida", 3.0))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repo.list("jerry", FactState::Active).await.unwrap().len(),
            1
        );
        assert_eq!(repo.get("liz", &mine.id).await.unwrap(), None);
        assert!(!repo.remove("liz", &mine.id, noon()).await.unwrap());
        let mut hijacked = mine.clone();
        hijacked.profile_id = "liz".into();
        hijacked.fact = "Something else".into();
        assert!(!repo.save(&hijacked).await.unwrap());
        repo.count_used("liz", &[mine.id.clone()]).await.unwrap();
        assert_eq!(repo.get("jerry", &mine.id).await.unwrap(), Some(mine));
    }

    async fn a_removed_line_keeps_its_key_and_is_never_written_again(
        repo: &dyn FactSheetRepository,
    ) {
        let line = repo
            .write(&guess("jerry", "Likes duck recipes", 3.0))
            .await
            .unwrap()
            .unwrap();
        assert!(repo.remove("jerry", &line.id, noon()).await.unwrap());
        assert!(!repo.remove("jerry", &line.id, noon()).await.unwrap());
        assert!(repo
            .list("jerry", FactState::Active)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repo.write(&guess("jerry", "likes duck recipes", 9.0))
                .await
                .unwrap(),
            None
        );
        let sheet = repo
            .replace(
                "jerry",
                &[guess("jerry", "Likes duck recipes", 9.0)],
                noon(),
            )
            .await
            .unwrap();
        assert!(sheet.is_empty());
        let removed = repo.list("jerry", FactState::Removed).await.unwrap();
        assert_eq!(facts(&removed), ["Likes duck recipes"]);
    }

    async fn a_rebuild_keeps_pinned_and_own_lines_and_retires_what_it_left_out(
        repo: &dyn FactSheetRepository,
    ) {
        let own =
            FactLine::written_by_member("jerry", FactCategory::Home, "Lives in Kilimani", noon())
                .unwrap();
        repo.write(&own).await.unwrap().unwrap();
        let mut pinned = repo
            .write(&guess("jerry", "Allergic to peanuts", 1.0))
            .await
            .unwrap()
            .unwrap();
        pinned.pinned = true;
        assert!(repo.save(&pinned).await.unwrap());
        repo.write(&guess("jerry", "Asked about rain once", 2.0))
            .await
            .unwrap()
            .unwrap();
        repo.count_used("jerry", &[pinned.id.clone()])
            .await
            .unwrap();

        let later = noon() + Duration::days(1);
        let sheet = repo
            .replace(
                "jerry",
                &[
                    guess("jerry", "Preparing a talk for December", 6.0),
                    guess("liz", "Not Jerry's line", 9.0),
                ],
                later,
            )
            .await
            .unwrap();
        assert_eq!(
            facts(&sheet),
            [
                "Allergic to peanuts",
                "Lives in Kilimani",
                "Preparing a talk for December"
            ]
        );
        assert_eq!(sheet[0].used, 1);
        let retired = repo.list("jerry", FactState::Retired).await.unwrap();
        assert_eq!(facts(&retired), ["Asked about rain once"]);
        assert!(repo
            .list("liz", FactState::Active)
            .await
            .unwrap()
            .is_empty());
    }

    async fn the_sheet_never_holds_more_than_its_limit(repo: &dyn FactSheetRepository) {
        let incoming: Vec<FactLine> = (0..FACT_SHEET_LIMIT + 5)
            .map(|i| guess("jerry", &format!("fact number {i}"), i as f64))
            .collect();
        let sheet = repo.replace("jerry", &incoming, noon()).await.unwrap();
        assert_eq!(sheet.len(), FACT_SHEET_LIMIT);
        repo.write(&guess("jerry", "One more strong fact", 500.0))
            .await
            .unwrap()
            .unwrap();
        let sheet = repo.list("jerry", FactState::Active).await.unwrap();
        assert_eq!(sheet.len(), FACT_SHEET_LIMIT);
        assert_eq!(sheet[0].fact, "One more strong fact");
        assert_eq!(
            repo.list("jerry", FactState::Retired).await.unwrap().len(),
            6
        );
    }

    async fn rewording_a_line_into_anothers_key_fails(repo: &dyn FactSheetRepository) {
        repo.write(&guess("jerry", "Runs Jarida", 3.0))
            .await
            .unwrap()
            .unwrap();
        let mut other = repo
            .write(&guess("jerry", "Drives a Fielder", 3.0))
            .await
            .unwrap()
            .unwrap();
        other.fact = "runs jarida".into();
        assert!(repo.save(&other).await.is_err());
    }

    async fn sqlite() -> SqliteFactSheetRepository {
        SqliteFactSheetRepository::new(pool().await)
    }

    #[tokio::test]
    async fn a_fact_line_round_trips_and_one_key_is_one_row() {
        a_line_round_trips_and_one_key_is_one_row(&sqlite().await).await;
        a_line_round_trips_and_one_key_is_one_row(&InMemoryFactSheetRepository::new()).await;
    }

    #[tokio::test]
    async fn one_member_never_reads_or_changes_anothers_fact_lines() {
        a_member_never_reads_or_changes_anothers_lines(&sqlite().await).await;
        a_member_never_reads_or_changes_anothers_lines(&InMemoryFactSheetRepository::new()).await;
    }

    #[tokio::test]
    async fn a_removed_fact_keeps_its_key_and_is_never_written_again() {
        a_removed_line_keeps_its_key_and_is_never_written_again(&sqlite().await).await;
        a_removed_line_keeps_its_key_and_is_never_written_again(
            &InMemoryFactSheetRepository::new(),
        )
        .await;
    }

    #[tokio::test]
    async fn a_rebuild_keeps_pinned_and_member_written_lines_and_retires_the_rest() {
        a_rebuild_keeps_pinned_and_own_lines_and_retires_what_it_left_out(&sqlite().await).await;
        a_rebuild_keeps_pinned_and_own_lines_and_retires_what_it_left_out(
            &InMemoryFactSheetRepository::new(),
        )
        .await;
    }

    #[tokio::test]
    async fn the_fact_sheet_never_holds_more_than_a_hundred_active_lines() {
        the_sheet_never_holds_more_than_its_limit(&sqlite().await).await;
        the_sheet_never_holds_more_than_its_limit(&InMemoryFactSheetRepository::new()).await;
    }

    #[tokio::test]
    async fn rewording_a_fact_into_another_lines_key_is_refused() {
        rewording_a_line_into_anothers_key_fails(&sqlite().await).await;
        rewording_a_line_into_anothers_key_fails(&InMemoryFactSheetRepository::new()).await;
    }

    #[tokio::test]
    async fn a_members_fact_sheet_goes_with_the_member() {
        let repo = sqlite().await;
        repo.write(&guess("liz", "Runs Jarida", 3.0))
            .await
            .unwrap()
            .unwrap();
        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&repo.pool)
            .await
            .unwrap();
        assert!(repo.rows("liz").await.unwrap().is_empty());
    }
}
