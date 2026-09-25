//! SQLite-backed implementation of [`DraftRepository`].

use anyhow::Result;
use async_trait::async_trait;
use pond_core::user_data::domain::draft::{Draft, DraftStatus};
use pond_core::user_data::domain::session::IdentificationSource;
use pond_core::user_data::ports::draft::DraftRepository;
use sqlx::{Pool, Sqlite};

/// Row shape shared by every SELECT below, so the tuple arity is stated once.
type DraftRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

const DRAFT_COLUMNS: &str = "id, session_id, profile_id, identification_source, \
     kind, summary, payload, status, created_at, expires_at";

pub struct SqliteDraftRepository {
    pool: Pool<Sqlite>,
}

impl SqliteDraftRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl DraftRepository for SqliteDraftRepository {
    async fn save(&self, draft: Draft) -> Result<()> {
        sqlx::query(
            "INSERT INTO drafts (id, session_id, profile_id, identification_source, \
             kind, summary, payload, status, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&draft.id)
        .bind(&draft.session_id)
        .bind(&draft.profile_id)
        .bind(draft.identification_source.map(|s| s.as_str().to_string()))
        .bind(&draft.kind)
        .bind(&draft.summary)
        .bind(&draft.payload)
        .bind(draft.status.to_string())
        .bind(draft.created_at.to_rfc3339())
        // Seconds + `Z`, as in `sqlite_proposal.rs`: 0041's triggers compare it via `datetime()`.
        .bind(
            draft
                .expires_at
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_pending(&self, session_id: &str) -> Result<Vec<Draft>> {
        // Expired drafts are filtered here, not only by a sweeper. Fails closed: an unreadable
        // expiry makes `datetime()` NULL, so the row counts as expired.
        let sql = format!(
            "SELECT {DRAFT_COLUMNS} FROM drafts WHERE session_id = ? AND status = 'pending' \
             AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
             ORDER BY created_at DESC"
        );
        let rows: Vec<DraftRow> = sqlx::query_as(&sql)
            .bind(session_id)
            .fetch_all(&self.pool)
            .await?;

        rows.into_iter().map(row_to_draft).collect()
    }

    async fn get(&self, id: &str) -> Result<Option<Draft>> {
        let sql = format!("SELECT {DRAFT_COLUMNS} FROM drafts WHERE id = ?");
        let row: Option<DraftRow> = sqlx::query_as(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        match row {
            Some(r) => Ok(Some(row_to_draft(r)?)),
            None => Ok(None),
        }
    }

    async fn update_status(&self, id: &str, status: DraftStatus) -> Result<()> {
        let result = sqlx::query("UPDATE drafts SET status = ? WHERE id = ?")
            .bind(status.to_string())
            .bind(id)
            .execute(&self.pool)
            .await?;

        if result.rows_affected() == 0 {
            anyhow::bail!("draft not found: {id}");
        }
        Ok(())
    }
}

fn row_to_draft(row: DraftRow) -> Result<Draft> {
    let (
        id,
        session_id,
        profile_id,
        identification_source,
        kind,
        summary,
        payload,
        status,
        created_at,
        expires_at,
    ) = row;
    let status: DraftStatus = status
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{}", e))?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now());
    // Unreadable expiry -> epoch (expired), never `None`, which means "never expires".
    let expires_at = expires_at.map(|raw| {
        chrono::DateTime::parse_from_rfc3339(&raw)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(|_| chrono::DateTime::UNIX_EPOCH)
    });

    Ok(Draft {
        id,
        session_id,
        profile_id,
        // `parse` is total: an unreadable value degrades to the weakest claim.
        identification_source: identification_source
            .as_deref()
            .map(IdentificationSource::parse),
        kind,
        summary,
        payload,
        status,
        created_at,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str, owner: Option<&str>) -> Draft {
        Draft {
            id: id.to_string(),
            session_id: "20260805_1".to_string(),
            profile_id: owner.map(str::to_string),
            identification_source: owner.map(|_| IdentificationSource::Explicit),
            kind: "shell_command".to_string(),
            summary: "remove the scratch directory".to_string(),
            payload: "{}".to_string(),
            status: DraftStatus::Pending,
            created_at: chrono::Utc::now(),
            expires_at: None,
        }
    }

    /// Exercises migration 0038's trigger; the other member's draft must stay untouched.
    #[tokio::test]
    async fn a_deleted_members_pending_draft_is_expired_and_released() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let repo = SqliteDraftRepository::new(db.system.clone());

        for id in ["liz", "jerry"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(id)
                .execute(&db.system)
                .await
                .unwrap();
        }
        repo.save(pending("d-liz", Some("liz"))).await.unwrap();
        repo.save(pending("d-jerry", Some("jerry"))).await.unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&db.system)
            .await
            .unwrap();

        let hers = repo.get("d-liz").await.unwrap().unwrap();
        assert_eq!(
            hers.status,
            DraftStatus::Expired,
            "a departed member's pending action must not stay approvable"
        );
        assert_eq!(hers.profile_id, None);
        assert_eq!(hers.identification_source, None);

        let theirs = repo.get("d-jerry").await.unwrap().unwrap();
        assert_eq!(theirs.status, DraftStatus::Pending);
        assert_eq!(theirs.profile_id.as_deref(), Some("jerry"));
        assert_eq!(
            theirs.identification_source,
            Some(IdentificationSource::Explicit),
            "the source must survive the round trip: PAI-1 invariant 3"
        );
    }

    /// An unowned draft (no session identified yet) round-trips as NULL, not "unknown".
    #[tokio::test]
    async fn an_unowned_draft_round_trips_as_unowned() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let repo = SqliteDraftRepository::new(db.system.clone());

        repo.save(pending("d1", None)).await.unwrap();
        let back = repo.get("d1").await.unwrap().unwrap();
        assert_eq!(back.profile_id, None);
        assert_eq!(back.identification_source, None);

        let listed = repo.list_pending("20260805_1").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "d1");
        assert!(repo
            .list_pending("another-session")
            .await
            .unwrap()
            .is_empty());
    }

    /// Nothing sweeps here: `list_pending` itself must filter expired drafts.
    #[tokio::test]
    async fn an_expired_draft_is_not_listed_and_a_live_one_is() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let repo = SqliteDraftRepository::new(db.system.clone());

        let mut stale = pending("stale", None);
        stale.expires_at = Some(chrono::Utc::now() - chrono::Duration::hours(1));
        let mut fresh = pending("fresh", None);
        fresh.expires_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
        let forever = pending("forever", None);
        for d in [stale, fresh, forever] {
            repo.save(d).await.unwrap();
        }

        let mut listed: Vec<String> = repo
            .list_pending("20260805_1")
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.id)
            .collect();
        listed.sort();
        assert_eq!(
            listed,
            vec!["forever".to_string(), "fresh".to_string()],
            "an expired draft must not be listed; one with no expiry must be"
        );

        // Still pending: the read filter, not a status change, hid it.
        let status: String = sqlx::query_scalar("SELECT status FROM drafts WHERE id = 'stale'")
            .fetch_one(&db.system)
            .await
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn an_unreadable_expiry_degrades_to_expired_not_to_immortal() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let repo = SqliteDraftRepository::new(db.system.clone());

        repo.save(pending("d1", None)).await.unwrap();
        sqlx::query("UPDATE drafts SET expires_at = 'not-a-time' WHERE id = 'd1'")
            .execute(&db.system)
            .await
            .unwrap();

        let back = repo.get("d1").await.unwrap().unwrap();
        assert!(
            !back.is_live_at(chrono::Utc::now()),
            "an unreadable expiry must not become `None`, which means never expires"
        );
        assert!(
            repo.list_pending("20260805_1").await.unwrap().is_empty(),
            "SQLite cannot parse it either, so the SQL filter must exclude it too"
        );
    }
}
