//! SQLite-backed [`ProposalRepository`]; proposals are rows in the `drafts` table.
//!
//! Every "what may this member act on" read filters expiry in SQL, so none relies on a
//! sweeper; `count_made_since` and `decisions_since` deliberately include expired rows.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::user_data::domain::draft::DraftStatus;
use pond_core::user_data::domain::proposal::{
    Proposal, ProposalAudience, ProposalDecision, ProposalPayload, ProposalShape,
    PROPOSAL_DRAFT_KIND, PROPOSAL_ORIGIN, PROPOSAL_SESSION_ID,
};
use pond_core::user_data::ports::proposal::ProposalRepository;
use sqlx::{Pool, Sqlite};

/// Column order must match `ProposalRow`.
const PROPOSAL_COLUMNS: &str = "id, profile_id, payload, rationale, created_at, expires_at";

/// `(id, profile_id, payload, rationale, created_at, expires_at)`.
type ProposalRow = (
    String,
    Option<String>,
    String,
    Option<String>,
    String,
    Option<String>,
);

/// The `WHERE` every live read shares. `datetime()` accepts both `Z` and `+00:00` and yields
/// NULL on garbage, so an unparseable expiry reads as expired.
const LIVE_PREDICATE: &str = "origin = ? AND status = 'pending' \
     AND expires_at IS NOT NULL AND datetime(expires_at) > datetime(?)";

pub struct SqliteProposalRepository {
    pool: Pool<Sqlite>,
}

impl SqliteProposalRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

/// Seconds, UTC, `Z`. Must stay `datetime()`-parseable or every proposal silently expires.
fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("unreadable proposal timestamp: {raw}"))
}

/// Deliberately goes through the validating constructor: a row that fails it is an error.
fn row_to_proposal(row: ProposalRow) -> Result<Proposal> {
    let (id, profile_id, payload, rationale, created_at, expires_at) = row;

    let audience = ProposalAudience::for_member(profile_id.unwrap_or_default())?;
    let payload: ProposalPayload = serde_json::from_str(&payload)
        .with_context(|| format!("unreadable proposal payload for {id}"))?;
    let expires_at = expires_at.ok_or_else(|| anyhow::anyhow!("proposal {id} has no expiry"))?;

    Ok(Proposal::from_parts(
        id,
        payload.trigger,
        rationale.unwrap_or_default(),
        payload.proposed_action,
        audience,
        payload.confidence,
        parse_ts(&created_at)?,
        parse_ts(&expires_at)?,
    )?)
}

#[async_trait]
impl ProposalRepository for SqliteProposalRepository {
    async fn save(&self, proposal: &Proposal) -> Result<()> {
        let payload = serde_json::to_string(&proposal.payload())?;
        sqlx::query(
            "INSERT INTO drafts \
             (id, session_id, profile_id, identification_source, kind, summary, payload, \
              status, created_at, origin, expires_at, rationale) \
             VALUES (?, ?, ?, NULL, ?, ?, ?, 'pending', ?, ?, ?, ?)",
        )
        .bind(proposal.id())
        // Namespaced so no real session's `list_drafts` can pick this proposal up.
        .bind(PROPOSAL_SESSION_ID)
        // Always one member's profile id; no `ProposalAudience` value means "everybody".
        .bind(proposal.audience().profile_id())
        .bind(PROPOSAL_DRAFT_KIND)
        .bind(proposal.summary())
        .bind(payload)
        .bind(sql_ts(proposal.created_at()))
        .bind(PROPOSAL_ORIGIN)
        .bind(sql_ts(proposal.expires_at()))
        .bind(proposal.rationale())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_live_for(&self, profile_id: &str, now: DateTime<Utc>) -> Result<Vec<Proposal>> {
        let sql = format!(
            "SELECT {PROPOSAL_COLUMNS} FROM drafts \
             WHERE {LIVE_PREDICATE} AND profile_id = ? ORDER BY created_at DESC"
        );
        let rows: Vec<ProposalRow> = sqlx::query_as(&sql)
            .bind(PROPOSAL_ORIGIN)
            .bind(sql_ts(now))
            .bind(profile_id)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows
            .into_iter()
            .filter_map(|row| match row_to_proposal(row) {
                Ok(p) => Some(p),
                Err(e) => {
                    // Skip, don't fail: one bad row must not hide the member's other proposals.
                    tracing::warn!(
                        target: "giap::trace",
                        kind = "proposal_row_unreadable",
                        error = %e,
                        "a stored proposal did not survive validation and was not shown"
                    );
                    None
                }
            })
            .collect())
    }

    async fn get_live(&self, id: &str, now: DateTime<Utc>) -> Result<Option<Proposal>> {
        let sql =
            format!("SELECT {PROPOSAL_COLUMNS} FROM drafts WHERE {LIVE_PREDICATE} AND id = ?");
        let row: Option<ProposalRow> = sqlx::query_as(&sql)
            .bind(PROPOSAL_ORIGIN)
            .bind(sql_ts(now))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        match row {
            Some(r) => Ok(Some(row_to_proposal(r)?)),
            None => Ok(None),
        }
    }

    async fn expire_due(&self, now: DateTime<Utc>) -> Result<u64> {
        // Spelled out, not negated: a missing or unparseable expiry must count as due.
        let result = sqlx::query(
            "UPDATE drafts SET status = 'expired' \
             WHERE origin = ? AND status = 'pending' \
             AND (expires_at IS NULL OR datetime(expires_at) IS NULL \
                  OR datetime(expires_at) <= datetime(?))",
        )
        .bind(PROPOSAL_ORIGIN)
        .bind(sql_ts(now))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    async fn count_made_since(&self, profile_id: &str, since: DateTime<Utc>) -> Result<usize> {
        // Unfiltered on purpose: dismissed and expired proposals still count toward the daily cap.
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM drafts \
             WHERE origin = ? AND profile_id = ? AND datetime(created_at) >= datetime(?)",
        )
        .bind(PROPOSAL_ORIGIN)
        .bind(profile_id)
        .bind(sql_ts(since))
        .fetch_one(&self.pool)
        .await
        .context("counting proposals made to a member")?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    async fn decisions_since(
        &self,
        profile_id: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProposalDecision>> {
        let sql = format!(
            "SELECT {PROPOSAL_COLUMNS}, status FROM drafts \
             WHERE origin = ? AND profile_id = ? AND status != 'pending' \
             AND datetime(created_at) >= datetime(?) ORDER BY created_at DESC"
        );
        let rows: Vec<(
            String,
            Option<String>,
            String,
            Option<String>,
            String,
            Option<String>,
            String,
        )> = sqlx::query_as(&sql)
            .bind(PROPOSAL_ORIGIN)
            .bind(profile_id)
            .bind(sql_ts(since))
            .fetch_all(&self.pool)
            .await
            .context("reading a member's proposal decisions")?;

        Ok(rows
            .into_iter()
            .filter_map(
                |(id, pid, payload, rationale, created_at, expires_at, status)| {
                    let created = parse_ts(&created_at).ok()?;
                    let proposal =
                        row_to_proposal((id, pid, payload, rationale, created_at, expires_at))
                            .ok()?;
                    let status: DraftStatus = status.parse().ok()?;
                    // No decided-at column, so `created_at` stands in; a suppression may end
                    // up to PROPOSAL_TTL (12h) early.
                    ProposalDecision::recorded(ProposalShape::of(&proposal), status, created).ok()
                },
            )
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use pond_core::user_data::domain::proposal::BusEventRef;
    use pond_core::user_data::domain::schedule::TaskKind;

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_785_000_000 + secs, 0).unwrap()
    }

    fn proposal(id: &str, owner: &str, created: DateTime<Utc>, ttl: Duration) -> Proposal {
        Proposal::expiring_after(
            id,
            BusEventRef::new(
                "camera",
                Some("front-door".into()),
                Some("person".into()),
                created,
            )
            .unwrap(),
            "a parcel has been at the door for an hour",
            TaskKind::AgentPrompt {
                prompt: "remind me about the parcel".into(),
            },
            ProposalAudience::for_member(owner).unwrap(),
            0.72,
            created,
            ttl,
        )
        .unwrap()
    }

    /// Raw INSERT bypassing `save`, binding the real constants rather than SQL literals.
    async fn insert_raw(
        pool: &Pool<Sqlite>,
        id: &str,
        expires_at: Option<&str>,
        rationale: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO drafts (id, session_id, kind, summary, payload, status, \
             created_at, origin, expires_at, rationale) \
             VALUES (?, ?, ?, 's', '{}', 'pending', '2026-08-10T00:00:00Z', ?, ?, ?)",
        )
        .bind(id)
        .bind(PROPOSAL_SESSION_ID)
        .bind(PROPOSAL_DRAFT_KIND)
        .bind(PROPOSAL_ORIGIN)
        .bind(expires_at)
        .bind(rationale)
        .execute(pool)
        .await
        .map(|_| ())
    }

    /// A live expiry for [`insert_raw`], in the spelling `sql_ts` writes.
    fn raw_expiry() -> String {
        sql_ts(Utc::now() + Duration::hours(2))
    }

    const MIGRATION_0041: &str = include_str!("../migrations/system/0041_proposals.sql");
    const MIGRATION_0042: &str = include_str!("../migrations/system/0042_proposal_expiry.sql");

    /// 0042's expiry triggers, so the upgrade test drops exactly what the file creates.
    const EXPIRY_TRIGGERS: [&str; 2] = [
        "trg_drafts_proposal_needs_an_expiry_on_insert",
        "trg_drafts_proposal_needs_an_expiry_on_update",
    ];

    async fn db() -> (tempfile::TempDir, Pool<Sqlite>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        for id in ["liz", "jerry"] {
            sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
                .bind(id)
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }
        (tmp, pool)
    }

    #[tokio::test]
    async fn a_proposal_round_trips_through_the_drafts_table() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        let p = proposal("prop-1", "liz", created, Duration::hours(2));
        repo.save(&p).await.unwrap();

        let back = repo.get_live("prop-1", created).await.unwrap().unwrap();
        assert_eq!(back, p, "every field must survive the row, not just the id");
        assert_eq!(
            back.rationale(),
            "a parcel has been at the door for an hour"
        );
        assert_eq!(back.audience().profile_id(), "liz");

        let (kind, session, origin): (String, String, String) =
            sqlx::query_as("SELECT kind, session_id, origin FROM drafts WHERE id = 'prop-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, PROPOSAL_DRAFT_KIND);
        assert_eq!(session, PROPOSAL_SESSION_ID);
        assert_eq!(origin, PROPOSAL_ORIGIN);
    }

    #[tokio::test]
    async fn a_proposal_is_visible_only_to_the_member_it_is_addressed_to() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool);
        let created = at(0);
        repo.save(&proposal("for-liz", "liz", created, Duration::hours(2)))
            .await
            .unwrap();
        repo.save(&proposal("for-jerry", "jerry", created, Duration::hours(2)))
            .await
            .unwrap();

        let hers = repo.list_live_for("liz", created).await.unwrap();
        assert_eq!(hers.len(), 1);
        assert_eq!(hers[0].id(), "for-liz");
        assert!(repo
            .list_live_for("nobody", created)
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn an_expired_proposal_is_not_returned_even_though_nothing_swept_it() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("prop-1", "liz", created, Duration::hours(1)))
            .await
            .unwrap();

        let just_before = created + Duration::minutes(59);
        assert!(repo
            .get_live("prop-1", just_before)
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            repo.list_live_for("liz", just_before).await.unwrap().len(),
            1
        );

        let after = created + Duration::hours(1) + Duration::seconds(1);
        assert!(
            repo.get_live("prop-1", after).await.unwrap().is_none(),
            "an expired proposal must not be readable: invariant 7 does not depend on a sweeper"
        );
        assert!(repo.list_live_for("liz", after).await.unwrap().is_empty());

        // Still pending, so the refusal came from the read filter, not a status change.
        let status: String = sqlx::query_scalar("SELECT status FROM drafts WHERE id = 'prop-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(status, "pending");
    }

    #[tokio::test]
    async fn the_daily_cap_counts_what_was_said_and_not_what_is_still_pending() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = Utc::now();
        let midnight = created - Duration::hours(12);

        for id in ["one", "two", "three"] {
            repo.save(&proposal(id, "liz", created, Duration::hours(6)))
                .await
                .unwrap();
        }
        repo.save(&proposal("theirs", "ada", created, Duration::hours(6)))
            .await
            .unwrap();
        repo.save(&proposal(
            "yesterday",
            "liz",
            midnight - Duration::hours(1),
            Duration::hours(6),
        ))
        .await
        .unwrap();

        assert_eq!(repo.count_made_since("liz", midnight).await.unwrap(), 3);

        sqlx::query("UPDATE drafts SET status = 'approved' WHERE id = 'one'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE drafts SET status = 'rejected' WHERE id = 'two'")
            .execute(&pool)
            .await
            .unwrap();
        repo.expire_due(created + Duration::hours(7)).await.unwrap();

        assert_eq!(
            repo.list_live_for("liz", created + Duration::hours(7))
                .await
                .unwrap()
                .len(),
            0,
            "vacuity control: nothing of liz's is live any more, so a count that agreed \
             with `list_live_for` would now read zero"
        );
        assert_eq!(
            repo.count_made_since("liz", midnight).await.unwrap(),
            3,
            "the pond spoke to liz three times today whatever she did about it"
        );
    }

    /// Uses `Utc::now()`, not `at(0)`: 0041's approve trigger checks SQLite's `datetime('now')`.
    #[tokio::test]
    async fn a_proposal_the_member_already_decided_is_never_shown_again() {
        for decided in ["approved", "rejected", "expired"] {
            let (_tmp, pool) = db().await;
            let repo = SqliteProposalRepository::new(pool.clone());
            let created = Utc::now();
            repo.save(&proposal("decided", "liz", created, Duration::hours(6)))
                .await
                .unwrap();
            repo.save(&proposal(
                "still-pending",
                "liz",
                created,
                Duration::hours(6),
            ))
            .await
            .unwrap();

            assert_eq!(
                repo.list_live_for("liz", created).await.unwrap().len(),
                2,
                "{decided}: both proposals must be live before one is decided, or this \
                 test proves nothing"
            );

            sqlx::query("UPDATE drafts SET status = ? WHERE id = 'decided'")
                .bind(decided)
                .execute(&pool)
                .await
                .unwrap();

            assert!(
                repo.get_live("decided", created).await.unwrap().is_none(),
                "a proposal already marked '{decided}' must not be readable: invariant 7 \
                 is broken by re-surfacing a suggestion the member has answered, not only \
                 by re-surfacing a stale one"
            );
            let live = repo.list_live_for("liz", created).await.unwrap();
            assert_eq!(
                live.iter().map(|p| p.id()).collect::<Vec<_>>(),
                vec!["still-pending"],
                "list_live_for must drop the '{decided}' proposal and keep the pending one"
            );
        }
    }

    #[tokio::test]
    async fn the_stored_expiry_format_is_the_one_sql_compares() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("prop-1", "liz", created, Duration::hours(2)))
            .await
            .unwrap();

        let parsed: Option<String> =
            sqlx::query_scalar("SELECT datetime(expires_at) FROM drafts WHERE id = 'prop-1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            parsed.is_some(),
            "SQLite cannot parse the expiry this repository writes, so 0041's triggers \
             and every read filter compare against NULL"
        );
        // Vacuity control: `datetime()` really does return NULL for garbage.
        let nonsense: Option<String> = sqlx::query_scalar("SELECT datetime('not-a-time')")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(nonsense.is_none());
    }

    #[tokio::test]
    async fn sweeping_gives_a_due_proposal_a_terminal_status_and_leaves_live_ones_alone() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("short", "liz", created, Duration::hours(1)))
            .await
            .unwrap();
        repo.save(&proposal("long", "liz", created, Duration::hours(6)))
            .await
            .unwrap();

        let swept = repo.expire_due(created + Duration::hours(2)).await.unwrap();
        assert_eq!(swept, 1);

        let statuses: Vec<(String, String)> =
            sqlx::query_as("SELECT id, status FROM drafts ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            statuses,
            vec![
                ("long".to_string(), "pending".to_string()),
                ("short".to_string(), "expired".to_string())
            ]
        );
    }

    #[tokio::test]
    async fn sqlite_refuses_to_store_a_proposal_without_a_rationale() {
        let (_tmp, pool) = db().await;
        let expiry = raw_expiry();
        for blank in [None, Some(""), Some("   ")] {
            let err = insert_raw(&pool, "x", Some(expiry.as_str()), blank)
                .await
                .expect_err("a rationale-less proposal must not be storable");
            assert!(
                err.to_string().contains("must carry a rationale"),
                "the refusal must name the defect, got: {err}"
            );
        }

        // Vacuity control: with a rationale the same INSERT succeeds.
        insert_raw(&pool, "x", Some(expiry.as_str()), Some("because"))
            .await
            .unwrap();
    }

    /// Drives both doors: an INSERT-only trigger misses a legacy row relabelled by UPDATE.
    #[tokio::test]
    async fn sqlite_refuses_to_store_a_proposal_without_an_expiry() {
        let (_tmp, pool) = db().await;

        let err = insert_raw(&pool, "no-expiry", None, Some("because"))
            .await
            .expect_err("a proposal with no expiry must not be storable: it can never expire");
        assert!(
            err.to_string().contains("must carry an expiry"),
            "the refusal must name the defect, got: {err}"
        );

        // The UPDATE door: a legacy draft (no origin, no expiry) relabelled proactive.
        sqlx::query(
            "INSERT INTO drafts (id, session_id, kind, summary, payload, status, created_at) \
             VALUES ('legacy', 'sess-a', 'shell_command', 's', '{}', 'pending', \
             '2026-08-10T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .expect("a legacy draft is exactly what save_draft writes and must stay storable");

        // Supplies a rationale: SQLite doesn't define which of two matching triggers aborts first.
        let err = sqlx::query(
            "UPDATE drafts SET origin = ?, rationale = 'because' WHERE id = 'legacy'",
        )
        .bind(PROPOSAL_ORIGIN)
        .execute(&pool)
        .await
        .expect_err(
            "relabelling an expiry-less row as proactive must not mint an immortal proposal",
        );
        assert!(
            err.to_string().contains("must carry an expiry"),
            "the refusal must name the defect, got: {err}"
        );

        // Vacuity control: the same relabel succeeds once it carries an expiry.
        sqlx::query("UPDATE drafts SET origin = ?, expires_at = ?, rationale = 'because' WHERE id = 'legacy'")
            .bind(PROPOSAL_ORIGIN)
            .bind(raw_expiry())
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Upgrade-only row (proactive, no expiry): without 0042's data fix it can never be updated,
    /// so 0038's profile-delete trigger aborts and the member can't be removed.
    #[tokio::test]
    async fn migration_0042_does_not_freeze_a_row_that_predates_it() {
        let (_tmp, pool) = db().await;

        // Rewind to the 0041 world.
        for trigger in EXPIRY_TRIGGERS {
            sqlx::query(&format!("DROP TRIGGER {trigger}"))
                .execute(&pool)
                .await
                .unwrap_or_else(|e| panic!("0042 must have created {trigger}: {e}"));
        }
        insert_raw(&pool, "immortal", None, Some("because"))
            .await
            .expect("with 0042's triggers dropped, the pre-0042 row must be writable");
        sqlx::query("UPDATE drafts SET profile_id = 'liz' WHERE id = 'immortal'")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::raw_sql(MIGRATION_0042)
            .execute(&pool)
            .await
            .expect("0042 must apply to a database that already holds rows");

        let (status, expires_at): (String, Option<String>) =
            sqlx::query_as("SELECT status, expires_at FROM drafts WHERE id = 'immortal'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            (status.as_str(), expires_at.is_some()),
            ("expired", true),
            "0042's data fix did not run over the pre-upgrade row: it is still {status} with \
             expires_at = {expires_at:?}. A proactive row with no expiry is FROZEN by the \
             triggers this file creates -- every later write to it aborts, including the two \
             inside 0038's profile-delete trigger, which makes a household member unremovable. \
             Fix the data before constraining it."
        );

        // Vacuity control: re-running 0042 restored the triggers.
        let err = insert_raw(&pool, "another", None, Some("because"))
            .await
            .expect_err("0042's INSERT trigger must be in force after the file is re-run");
        assert!(
            err.to_string().contains("must carry an expiry"),
            "got: {err}"
        );

        // The half that matters: a household member can still be removed.
        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&pool)
            .await
            .expect(
                "0038's profile-delete trigger updates this row, so a frozen row blocks the \
                 deletion of a household member entirely",
            );
    }

    /// The triggers hardcode `'proactive'`: renaming `PROPOSAL_ORIGIN` would silently disable them.
    #[test]
    fn the_origin_constant_is_the_literal_the_migrations_hardcode() {
        for (name, sql, triggers) in [
            ("0041_proposals.sql", MIGRATION_0041, 2usize),
            ("0042_proposal_expiry.sql", MIGRATION_0042, 2usize),
        ] {
            let any = sql.matches("NEW.origin = '").count();
            assert_eq!(
                any, triggers,
                "{name} no longer has {triggers} triggers keyed on an origin literal, it has \
                 {any}. Either a trigger was added or removed -- in which case update this \
                 count -- or the WHEN clause was rewritten and the assertion below has \
                 stopped describing the file."
            );
            let bound = sql
                .matches(format!("NEW.origin = '{PROPOSAL_ORIGIN}'").as_str())
                .count();
            assert_eq!(
                bound, triggers,
                "{name} hardcodes an origin literal that PROPOSAL_ORIGIN ({PROPOSAL_ORIGIN:?}) \
                 no longer matches: {bound} of its {triggers} origin-keyed triggers agree with \
                 the constant. Every row SqliteProposalRepository::save writes would slip past \
                 the rest, so the storage layer under PAI-7 invariants 2 and 7 would stop \
                 applying without one test going red."
            );
        }
    }

    /// Also binds `DraftRepository::update_status`, which knows nothing about proposals.
    #[tokio::test]
    async fn sqlite_refuses_to_approve_an_expired_row() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        // Wall-clock expiry: the trigger compares against SQLite's `datetime('now')`.
        let created = Utc::now() - Duration::hours(3);
        repo.save(&proposal("stale", "liz", created, Duration::hours(2)))
            .await
            .unwrap();
        repo.save(&proposal("fresh", "liz", Utc::now(), Duration::hours(2)))
            .await
            .unwrap();

        let err = sqlx::query("UPDATE drafts SET status = 'approved' WHERE id = 'stale'")
            .execute(&pool)
            .await
            .expect_err("an expired proposal must not be approvable");
        assert!(
            err.to_string().contains("expired"),
            "the refusal must name the defect, got: {err}"
        );

        // Only approval is blocked: rejections feed memory and 0038 sets 'expired' on delete.
        sqlx::query("UPDATE drafts SET status = 'rejected' WHERE id = 'stale'")
            .execute(&pool)
            .await
            .unwrap();

        // Vacuity control: a live proposal approves cleanly.
        sqlx::query("UPDATE drafts SET status = 'approved' WHERE id = 'fresh'")
            .execute(&pool)
            .await
            .unwrap();
    }

    /// Covered by 0038's profile-delete trigger via `drafts.profile_id`.
    #[tokio::test]
    async fn a_departed_members_proposal_is_expired_and_released() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("hers", "liz", created, Duration::hours(6)))
            .await
            .unwrap();

        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&pool)
            .await
            .unwrap();

        assert!(repo.get_live("hers", created).await.unwrap().is_none());
        let (status, owner): (String, Option<String>) =
            sqlx::query_as("SELECT status, profile_id FROM drafts WHERE id = 'hers'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "expired");
        assert_eq!(owner, None);
    }

    #[tokio::test]
    async fn a_row_that_fails_validation_is_not_returned() {
        let (_tmp, pool) = db().await;
        let repo = SqliteProposalRepository::new(pool.clone());
        let created = at(0);
        repo.save(&proposal("prop-1", "liz", created, Duration::hours(2)))
            .await
            .unwrap();
        // An out-of-range confidence (9.0), written straight into the payload.
        sqlx::query("UPDATE drafts SET payload = ? WHERE id = 'prop-1'")
            .bind(r#"{"trigger":{"kind":"camera","observed_at":"2026-08-10T00:00:00Z"},"proposed_action":{"type":"agent_prompt","prompt":"x"},"confidence":9.0}"#)
            .execute(&pool)
            .await
            .unwrap();

        assert!(
            repo.get_live("prop-1", created).await.is_err(),
            "get_live must not hand back a proposal that failed validation"
        );
        assert!(
            repo.list_live_for("liz", created).await.unwrap().is_empty(),
            "list_live_for must skip a row that failed validation"
        );
    }
}
