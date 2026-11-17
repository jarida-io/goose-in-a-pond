//! SQLite-backed [`AnchorRepository`] over the `anchors`, `anchor_shares` and `deliverables`
//! tables. Facts, sources and objections are JSON.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::user_data::domain::anchor::{
    sort_anchors, Anchor, AnchorKind, AnchorOrigin, AnchorState, Deliverable, DeliverableKind,
    DeliverableState,
};
use pond_core::user_data::domain::specialist::SpecialistId;
use pond_core::user_data::ports::anchors::AnchorRepository;

use crate::sqlite_rows::{
    count_in, json_in, json_of, maybe_stamp, maybe_time_of, stamp, time_of, word_in,
};

pub struct SqliteAnchorRepository {
    pool: Pool<Sqlite>,
}

impl SqliteAnchorRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const ANCHORS: &str = "SELECT a.id, a.profile_id, a.kind, a.title, a.state, a.starts_at, \
     a.ends_at, a.place, a.facts_json, a.agent, a.origin, a.origin_ref, a.why, a.offered_at, \
     a.confirmed_at, a.ended_at, a.created_at, a.updated_at, \
     (SELECT json_group_array(s.profile_id) FROM anchor_shares s WHERE s.anchor_id = a.id) \
     AS shared_json FROM anchors a";

/// True of an anchor `a` the bound member owns or is shared with; binds the member twice.
const READABLE: &str = "(a.profile_id = ? OR EXISTS \
     (SELECT 1 FROM anchor_shares s WHERE s.anchor_id = a.id AND s.profile_id = ?))";

const DELIVERABLES: &str = "SELECT d.id, d.anchor_id, d.kind, d.slot, d.title, d.state, d.hook, \
     d.body, d.facts_json, d.sources_json, d.objections_json, d.changed, d.version, d.task_id, \
     d.refresh_at, d.seen_at, d.created_at, d.updated_at \
     FROM deliverables d JOIN anchors a ON a.id = d.anchor_id";

fn anchor_of(row: &SqliteRow) -> Result<Anchor> {
    let agent: Option<String> = row.try_get("agent")?;
    let mut shared_with: Vec<String> = json_in(row, "shared_json")?;
    shared_with.sort();
    Ok(Anchor {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        kind: word_in(row, "kind", AnchorKind::parse)?,
        title: row.try_get("title")?,
        state: word_in(row, "state", AnchorState::parse)?,
        starts_at: maybe_time_of(row, "starts_at")?,
        ends_at: maybe_time_of(row, "ends_at")?,
        place: row.try_get("place")?,
        facts: json_in(row, "facts_json")?,
        agent: agent.as_deref().and_then(SpecialistId::parse),
        origin: word_in(row, "origin", AnchorOrigin::parse)?,
        origin_ref: row.try_get("origin_ref")?,
        why: row.try_get("why")?,
        offered_at: maybe_time_of(row, "offered_at")?,
        confirmed_at: maybe_time_of(row, "confirmed_at")?,
        ended_at: maybe_time_of(row, "ended_at")?,
        created_at: time_of(row, "created_at")?,
        updated_at: time_of(row, "updated_at")?,
        shared_with,
    })
}

fn deliverable_of(row: &SqliteRow) -> Result<Deliverable> {
    Ok(Deliverable {
        id: row.try_get("id")?,
        anchor_id: row.try_get("anchor_id")?,
        kind: word_in(row, "kind", DeliverableKind::parse)?,
        slot: row.try_get("slot")?,
        title: row.try_get("title")?,
        state: word_in(row, "state", DeliverableState::parse)?,
        hook: row.try_get("hook")?,
        body: row.try_get("body")?,
        facts: json_in(row, "facts_json")?,
        sources: json_in(row, "sources_json")?,
        objections: json_in(row, "objections_json")?,
        changed: row.try_get("changed")?,
        version: count_in(row, "version")?,
        task_id: row.try_get("task_id")?,
        refresh_at: maybe_time_of(row, "refresh_at")?,
        seen_at: maybe_time_of(row, "seen_at")?,
        created_at: time_of(row, "created_at")?,
        updated_at: time_of(row, "updated_at")?,
    })
}

#[async_trait]
impl AnchorRepository for SqliteAnchorRepository {
    async fn add(&self, anchor: &Anchor) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO anchors (id, profile_id, kind, title, title_key, state, starts_at, \
             ends_at, place, facts_json, agent, origin, origin_ref, why, offered_at, \
             confirmed_at, ended_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&anchor.id)
        .bind(&anchor.profile_id)
        .bind(anchor.kind.as_str())
        .bind(&anchor.title)
        .bind(anchor.key())
        .bind(anchor.state.as_str())
        .bind(maybe_stamp(anchor.starts_at))
        .bind(maybe_stamp(anchor.ends_at))
        .bind(&anchor.place)
        .bind(json_of(&anchor.facts)?)
        .bind(anchor.agent.map(SpecialistId::as_str))
        .bind(anchor.origin.as_str())
        .bind(&anchor.origin_ref)
        .bind(&anchor.why)
        .bind(maybe_stamp(anchor.offered_at))
        .bind(maybe_stamp(anchor.confirmed_at))
        .bind(maybe_stamp(anchor.ended_at))
        .bind(stamp(anchor.created_at))
        .bind(stamp(anchor.updated_at))
        .execute(&self.pool)
        .await
        .context("add an anchor")?;
        Ok(done.rows_affected() > 0)
    }

    async fn get(&self, profile_id: &str, id: &str) -> Result<Option<Anchor>> {
        let row = sqlx::query(&format!("{ANCHORS} WHERE a.id = ? AND {READABLE}"))
            .bind(id)
            .bind(profile_id)
            .bind(profile_id)
            .fetch_optional(&self.pool)
            .await
            .context("read an anchor")?;
        row.as_ref().map(anchor_of).transpose()
    }

    async fn by_title(&self, profile_id: &str, title_key: &str) -> Result<Option<Anchor>> {
        let row = sqlx::query(&format!(
            "{ANCHORS} WHERE a.profile_id = ? AND a.title_key = ?"
        ))
        .bind(profile_id)
        .bind(title_key)
        .fetch_optional(&self.pool)
        .await
        .context("read an anchor by its title")?;
        row.as_ref().map(anchor_of).transpose()
    }

    async fn list(&self, profile_id: &str, states: &[AnchorState]) -> Result<Vec<Anchor>> {
        if states.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; states.len()].join(", ");
        let sql = format!("{ANCHORS} WHERE {READABLE} AND a.state IN ({marks})");
        let mut query = sqlx::query(&sql).bind(profile_id).bind(profile_id);
        for state in states {
            query = query.bind(state.as_str());
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .context("list a member's anchors")?;
        let mut anchors = rows.iter().map(anchor_of).collect::<Result<Vec<_>>>()?;
        sort_anchors(&mut anchors);
        Ok(anchors)
    }

    async fn save(&self, anchor: &Anchor) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE anchors SET kind = ?, title = ?, title_key = ?, state = ?, starts_at = ?, \
             ends_at = ?, place = ?, facts_json = ?, agent = ?, origin = ?, origin_ref = ?, \
             why = ?, offered_at = ?, confirmed_at = ?, ended_at = ?, updated_at = ? \
             WHERE id = ? AND profile_id = ?",
        )
        .bind(anchor.kind.as_str())
        .bind(&anchor.title)
        .bind(anchor.key())
        .bind(anchor.state.as_str())
        .bind(maybe_stamp(anchor.starts_at))
        .bind(maybe_stamp(anchor.ends_at))
        .bind(&anchor.place)
        .bind(json_of(&anchor.facts)?)
        .bind(anchor.agent.map(SpecialistId::as_str))
        .bind(anchor.origin.as_str())
        .bind(&anchor.origin_ref)
        .bind(&anchor.why)
        .bind(maybe_stamp(anchor.offered_at))
        .bind(maybe_stamp(anchor.confirmed_at))
        .bind(maybe_stamp(anchor.ended_at))
        .bind(stamp(anchor.updated_at))
        .bind(&anchor.id)
        .bind(&anchor.profile_id)
        .execute(&self.pool)
        .await
        .context("save an anchor; its title may be another anchor's")?;
        Ok(done.rows_affected() > 0)
    }

    async fn set_shares(
        &self,
        owner_id: &str,
        anchor_id: &str,
        profile_ids: &[String],
    ) -> Result<bool> {
        let owned: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM anchors WHERE id = ? AND profile_id = ?")
                .bind(anchor_id)
                .bind(owner_id)
                .fetch_optional(&self.pool)
                .await
                .context("check who owns an anchor")?;
        if owned.is_none() {
            return Ok(false);
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM anchor_shares WHERE anchor_id = ?")
            .bind(anchor_id)
            .execute(&mut *tx)
            .await
            .context("clear who an anchor is shared with")?;
        for profile_id in profile_ids.iter().filter(|id| id.as_str() != owner_id) {
            sqlx::query(
                "INSERT OR IGNORE INTO anchor_shares (anchor_id, profile_id) VALUES (?, ?)",
            )
            .bind(anchor_id)
            .bind(profile_id)
            .execute(&mut *tx)
            .await
            .context("share an anchor with a member")?;
        }
        tx.commit().await?;
        Ok(true)
    }

    async fn find(&self, id: &str) -> Result<Option<Anchor>> {
        let row = sqlx::query(&format!("{ANCHORS} WHERE a.id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("find an anchor")?;
        row.as_ref().map(anchor_of).transpose()
    }

    async fn candidates_older_than(&self, before: DateTime<Utc>) -> Result<Vec<Anchor>> {
        let rows = sqlx::query(&format!(
            "{ANCHORS} WHERE a.state = 'candidate' AND a.created_at < ? ORDER BY a.rowid"
        ))
        .bind(stamp(before))
        .fetch_all(&self.pool)
        .await
        .context("list candidate anchors old enough to fade")?;
        rows.iter().map(anchor_of).collect()
    }

    async fn active_over_before(&self, before: DateTime<Utc>) -> Result<Vec<Anchor>> {
        let rows = sqlx::query(&format!(
            "{ANCHORS} WHERE a.state = 'active' AND COALESCE(a.ends_at, a.starts_at) < ? \
             ORDER BY a.rowid"
        ))
        .bind(stamp(before))
        .fetch_all(&self.pool)
        .await
        .context("list active anchors whose end has passed")?;
        rows.iter().map(anchor_of).collect()
    }

    async fn add_deliverable(&self, deliverable: &Deliverable) -> Result<bool> {
        let done = sqlx::query(
            "INSERT INTO deliverables (id, anchor_id, kind, slot, title, state, hook, body, \
             facts_json, sources_json, objections_json, changed, version, task_id, refresh_at, \
             seen_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT DO NOTHING",
        )
        .bind(&deliverable.id)
        .bind(&deliverable.anchor_id)
        .bind(deliverable.kind.as_str())
        .bind(&deliverable.slot)
        .bind(&deliverable.title)
        .bind(deliverable.state.as_str())
        .bind(&deliverable.hook)
        .bind(&deliverable.body)
        .bind(json_of(&deliverable.facts)?)
        .bind(json_of(&deliverable.sources)?)
        .bind(json_of(&deliverable.objections)?)
        .bind(&deliverable.changed)
        .bind(i64::from(deliverable.version))
        .bind(&deliverable.task_id)
        .bind(maybe_stamp(deliverable.refresh_at))
        .bind(maybe_stamp(deliverable.seen_at))
        .bind(stamp(deliverable.created_at))
        .bind(stamp(deliverable.updated_at))
        .execute(&self.pool)
        .await
        .context("add a deliverable")?;
        Ok(done.rows_affected() > 0)
    }

    async fn deliverable(&self, profile_id: &str, id: &str) -> Result<Option<Deliverable>> {
        let row = sqlx::query(&format!("{DELIVERABLES} WHERE d.id = ? AND {READABLE}"))
            .bind(id)
            .bind(profile_id)
            .bind(profile_id)
            .fetch_optional(&self.pool)
            .await
            .context("read a deliverable")?;
        row.as_ref().map(deliverable_of).transpose()
    }

    async fn deliverables(&self, profile_id: &str, anchor_id: &str) -> Result<Vec<Deliverable>> {
        let rows = sqlx::query(&format!(
            "{DELIVERABLES} WHERE d.anchor_id = ? AND {READABLE} ORDER BY d.rowid"
        ))
        .bind(anchor_id)
        .bind(profile_id)
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list an anchor's deliverables for a member")?;
        rows.iter().map(deliverable_of).collect()
    }

    async fn unseen(&self, profile_id: &str) -> Result<Vec<Deliverable>> {
        let rows = sqlx::query(&format!(
            "{DELIVERABLES} WHERE {READABLE} AND d.state IN ('ready', 'unsure') \
             AND d.seen_at IS NULL ORDER BY d.updated_at DESC, d.rowid DESC"
        ))
        .bind(profile_id)
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .context("list a member's unopened deliverables")?;
        rows.iter().map(deliverable_of).collect()
    }

    async fn find_deliverable(&self, id: &str) -> Result<Option<Deliverable>> {
        let row = sqlx::query(&format!("{DELIVERABLES} WHERE d.id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .context("find a deliverable")?;
        row.as_ref().map(deliverable_of).transpose()
    }

    async fn deliverables_of(&self, anchor_id: &str) -> Result<Vec<Deliverable>> {
        let rows = sqlx::query(&format!(
            "{DELIVERABLES} WHERE d.anchor_id = ? ORDER BY d.rowid"
        ))
        .bind(anchor_id)
        .fetch_all(&self.pool)
        .await
        .context("list an anchor's deliverables")?;
        rows.iter().map(deliverable_of).collect()
    }

    async fn save_deliverable(&self, deliverable: &Deliverable) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE deliverables SET kind = ?, title = ?, state = ?, hook = ?, body = ?, \
             facts_json = ?, sources_json = ?, objections_json = ?, changed = ?, version = ?, \
             task_id = ?, refresh_at = ?, seen_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(deliverable.kind.as_str())
        .bind(&deliverable.title)
        .bind(deliverable.state.as_str())
        .bind(&deliverable.hook)
        .bind(&deliverable.body)
        .bind(json_of(&deliverable.facts)?)
        .bind(json_of(&deliverable.sources)?)
        .bind(json_of(&deliverable.objections)?)
        .bind(&deliverable.changed)
        .bind(i64::from(deliverable.version))
        .bind(&deliverable.task_id)
        .bind(maybe_stamp(deliverable.refresh_at))
        .bind(maybe_stamp(deliverable.seen_at))
        .bind(stamp(deliverable.updated_at))
        .bind(&deliverable.id)
        .execute(&self.pool)
        .await
        .context("save a deliverable")?;
        Ok(done.rows_affected() > 0)
    }

    async fn deliverables_due(&self, now: DateTime<Utc>) -> Result<Vec<Deliverable>> {
        let rows = sqlx::query(&format!(
            "{DELIVERABLES} WHERE a.state = 'active' AND d.state IN ('ready', 'unsure') \
             AND d.refresh_at IS NOT NULL AND d.refresh_at <= ? ORDER BY d.refresh_at, d.rowid"
        ))
        .bind(stamp(now))
        .fetch_all(&self.pool)
        .await
        .context("list deliverables due a refresh")?;
        rows.iter().map(deliverable_of).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_rows::testing::{noon, pool};
    use chrono::Duration;
    use pond_core::user_data::domain::anchor::{
        AnchorFact, DeliverableDraft, Objection, ObjectionKind, SourceLink,
    };
    use pond_core::user_data::mocks::mock_anchors::InMemoryAnchorRepository;

    fn anchor(member: &str, title: &str, origin: AnchorOrigin, minutes_old: i64) -> Anchor {
        Anchor::new(
            member,
            AnchorKind::Trip,
            title,
            origin,
            noon() - Duration::minutes(minutes_old),
        )
        .unwrap()
    }

    fn slot(anchor: &Anchor, slot: &str) -> Deliverable {
        Deliverable::new(
            &anchor.id,
            DeliverableKind::Plan,
            slot,
            &format!("{slot} for the trip"),
            noon(),
        )
        .unwrap()
    }

    fn draft() -> DeliverableDraft {
        DeliverableDraft {
            hook: "Three days, two stays".into(),
            body: "# Kisumu\nDay 1 ...".into(),
            facts: vec![AnchorFact::new("Budget", "KSh 80,000", "answer")],
            sources: vec![SourceLink {
                title: "Flights".into(),
                url: "https://example.org/flights".into(),
            }],
        }
    }

    fn titles(found: &[Anchor]) -> Vec<&str> {
        found.iter().map(|a| a.title.as_str()).collect()
    }

    async fn an_anchor_round_trips_and_a_title_is_held_once(repo: &dyn AnchorRepository) {
        let mut trip = anchor("jerry", "Kisumu in December", AnchorOrigin::Chat, 0);
        trip.starts_at = Some(noon() + Duration::days(20));
        trip.ends_at = Some(noon() + Duration::days(23));
        trip.place = Some("Kisumu".into());
        trip.origin_ref = Some("s1".into());
        trip.why = "You said you are going in December.".into();
        trip.set_fact(AnchorFact::new("Travellers", "2", "chat"), noon());
        assert!(repo.add(&trip).await.unwrap());
        assert_eq!(
            repo.get("jerry", &trip.id).await.unwrap(),
            Some(trip.clone())
        );
        assert_eq!(
            repo.by_title("jerry", "kisumu in december").await.unwrap(),
            Some(trip.clone())
        );

        let twin = anchor("jerry", "kisumu in December!", AnchorOrigin::You, 0);
        assert!(!repo.add(&twin).await.unwrap(), "one anchor per title");
        assert!(repo
            .add(&anchor("liz", "Kisumu in December", AnchorOrigin::You, 0))
            .await
            .unwrap());
    }

    async fn an_anchor_is_read_by_its_owner_and_whoever_it_is_shared_with(
        repo: &dyn AnchorRepository,
    ) {
        let trip = anchor("jerry", "Kisumu in December", AnchorOrigin::You, 0);
        repo.add(&trip).await.unwrap();
        let plan = slot(&trip, "plan");
        assert!(repo.add_deliverable(&plan).await.unwrap());

        assert_eq!(repo.get("liz", &trip.id).await.unwrap(), None);
        assert!(repo
            .list("liz", &AnchorState::LIVE)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(repo.deliverable("liz", &plan.id).await.unwrap(), None);
        assert!(repo.deliverables("liz", &trip.id).await.unwrap().is_empty());

        assert!(!repo
            .set_shares("liz", &trip.id, &["liz".to_string()])
            .await
            .unwrap());
        assert!(repo
            .set_shares("jerry", &trip.id, &["liz".to_string(), "jerry".to_string()])
            .await
            .unwrap());
        let shared = repo.get("liz", &trip.id).await.unwrap().unwrap();
        assert_eq!(shared.profile_id, "jerry");
        assert_eq!(shared.shared_with, ["liz"]);
        assert_eq!(
            titles(&repo.list("liz", &AnchorState::LIVE).await.unwrap()),
            ["Kisumu in December"]
        );
        assert_eq!(
            repo.deliverable("liz", &plan.id).await.unwrap(),
            Some(plan.clone())
        );
        assert_eq!(repo.deliverables("liz", &trip.id).await.unwrap().len(), 1);

        let mut renamed = shared.clone();
        renamed.profile_id = "liz".into();
        renamed.title = "Liz's trip".into();
        assert!(
            !repo.save(&renamed).await.unwrap(),
            "only its owner writes it"
        );

        assert!(repo.set_shares("jerry", &trip.id, &[]).await.unwrap());
        assert_eq!(repo.get("liz", &trip.id).await.unwrap(), None);
        assert_eq!(repo.deliverable("liz", &plan.id).await.unwrap(), None);
    }

    async fn a_members_anchors_list_dated_ones_first_and_by_state(repo: &dyn AnchorRepository) {
        let mut far = anchor("jerry", "Mombasa in March", AnchorOrigin::You, 3);
        far.starts_at = Some(noon() + Duration::days(120));
        let mut near = anchor("jerry", "AI Summit", AnchorOrigin::Chat, 2);
        near.starts_at = Some(noon() + Duration::days(4));
        let standing = anchor("jerry", "Everyday", AnchorOrigin::Goose, 9);
        let newer = anchor("jerry", "A Fielder", AnchorOrigin::You, 1);
        for one in [&far, &near, &standing, &newer] {
            assert!(repo.add(one).await.unwrap());
        }
        assert_eq!(
            titles(&repo.list("jerry", &AnchorState::LIVE).await.unwrap()),
            ["AI Summit", "Mombasa in March", "A Fielder", "Everyday"]
        );
        assert_eq!(
            titles(&repo.list("jerry", &[AnchorState::Candidate]).await.unwrap()),
            ["AI Summit"]
        );

        let mut confirmed = near.clone();
        confirmed.confirm(noon()).unwrap();
        assert!(repo.save(&confirmed).await.unwrap());
        assert_eq!(repo.find(&near.id).await.unwrap(), Some(confirmed));
        assert!(repo
            .list("jerry", &[AnchorState::Candidate])
            .await
            .unwrap()
            .is_empty());
    }

    async fn the_tick_finds_stale_candidates_and_anchors_past_their_end(
        repo: &dyn AnchorRepository,
    ) {
        let stale = anchor(
            "jerry",
            "A plan from last week",
            AnchorOrigin::Chat,
            8 * 24 * 60,
        );
        let fresh = anchor("liz", "A plan from today", AnchorOrigin::Capture, 5);
        let mut over = anchor("jerry", "Last week's summit", AnchorOrigin::You, 0);
        over.starts_at = Some(noon() - Duration::days(3));
        let mut running = anchor("jerry", "A long trip", AnchorOrigin::You, 0);
        running.starts_at = Some(noon() - Duration::days(3));
        running.ends_at = Some(noon() + Duration::days(2));
        let undated = anchor("liz", "Everyday", AnchorOrigin::Goose, 0);
        for one in [&stale, &fresh, &over, &running, &undated] {
            assert!(repo.add(one).await.unwrap());
        }
        let fading = repo
            .candidates_older_than(noon() - Duration::days(7))
            .await
            .unwrap();
        assert_eq!(titles(&fading), ["A plan from last week"]);
        let ending = repo
            .active_over_before(noon() - Duration::days(1))
            .await
            .unwrap();
        assert_eq!(titles(&ending), ["Last week's summit"]);
    }

    async fn a_deliverable_round_trips_through_its_moves_one_per_slot(repo: &dyn AnchorRepository) {
        let trip = anchor("jerry", "Kisumu in December", AnchorOrigin::You, 0);
        repo.add(&trip).await.unwrap();
        let mut plan = slot(&trip, "plan");
        assert!(repo.add_deliverable(&plan).await.unwrap());
        assert!(!repo.add_deliverable(&slot(&trip, "plan")).await.unwrap());
        assert!(repo.add_deliverable(&slot(&trip, "stays")).await.unwrap());
        assert!(
            repo.unseen("jerry").await.unwrap().is_empty(),
            "nothing shown yet"
        );

        plan.start_making("engine-1", noon()).unwrap();
        assert!(repo.save_deliverable(&plan).await.unwrap());
        plan.settle_unsure(
            draft(),
            vec![Objection::new(
                ObjectionKind::Stale,
                "the fares",
                "check today's",
            )],
            Some(noon() + Duration::days(3)),
            noon(),
        )
        .unwrap();
        assert!(repo.save_deliverable(&plan).await.unwrap());
        assert_eq!(
            repo.find_deliverable(&plan.id).await.unwrap(),
            Some(plan.clone())
        );
        assert_eq!(repo.unseen("jerry").await.unwrap(), vec![plan.clone()]);
        let slots: Vec<String> = repo
            .deliverables_of(&trip.id)
            .await
            .unwrap()
            .into_iter()
            .map(|d| d.slot)
            .collect();
        assert_eq!(slots, ["plan", "stays"]);

        assert!(repo
            .deliverables_due(noon() + Duration::days(2))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            repo.deliverables_due(noon() + Duration::days(3))
                .await
                .unwrap(),
            vec![plan.clone()]
        );
        plan.mark_seen(noon());
        repo.save_deliverable(&plan).await.unwrap();
        assert!(repo.unseen("jerry").await.unwrap().is_empty());

        let mut ended = trip.clone();
        ended.end(noon()).unwrap();
        repo.save(&ended).await.unwrap();
        assert!(
            repo.deliverables_due(noon() + Duration::days(9))
                .await
                .unwrap()
                .is_empty(),
            "an ended anchor is not refreshed"
        );
        let mut ghost = slot(&trip, "packing");
        ghost.id = "never-added".into();
        assert!(!repo.save_deliverable(&ghost).await.unwrap());
    }

    async fn sqlite() -> SqliteAnchorRepository {
        SqliteAnchorRepository::new(pool().await)
    }

    #[tokio::test]
    async fn an_anchor_round_trips_and_one_member_holds_a_title_once() {
        an_anchor_round_trips_and_a_title_is_held_once(&sqlite().await).await;
        an_anchor_round_trips_and_a_title_is_held_once(&InMemoryAnchorRepository::new()).await;
    }

    #[tokio::test]
    async fn an_anchor_is_read_by_its_owner_and_the_members_it_is_shared_with() {
        an_anchor_is_read_by_its_owner_and_whoever_it_is_shared_with(&sqlite().await).await;
        an_anchor_is_read_by_its_owner_and_whoever_it_is_shared_with(
            &InMemoryAnchorRepository::new(),
        )
        .await;
    }

    #[tokio::test]
    async fn a_members_anchors_list_dated_ones_first_and_filter_by_state() {
        a_members_anchors_list_dated_ones_first_and_by_state(&sqlite().await).await;
        a_members_anchors_list_dated_ones_first_and_by_state(&InMemoryAnchorRepository::new())
            .await;
    }

    #[tokio::test]
    async fn the_tick_finds_week_old_candidates_and_anchors_a_day_past_their_end() {
        the_tick_finds_stale_candidates_and_anchors_past_their_end(&sqlite().await).await;
        the_tick_finds_stale_candidates_and_anchors_past_their_end(
            &InMemoryAnchorRepository::new(),
        )
        .await;
    }

    #[tokio::test]
    async fn a_deliverable_round_trips_through_its_moves_with_one_per_slot() {
        a_deliverable_round_trips_through_its_moves_one_per_slot(&sqlite().await).await;
        a_deliverable_round_trips_through_its_moves_one_per_slot(&InMemoryAnchorRepository::new())
            .await;
    }

    #[tokio::test]
    async fn an_anchors_shares_and_deliverables_go_with_it_and_with_its_owner() {
        let repo = sqlite().await;
        let trip = anchor("jerry", "Kisumu in December", AnchorOrigin::You, 0);
        repo.add(&trip).await.unwrap();
        repo.add_deliverable(&slot(&trip, "plan")).await.unwrap();
        repo.set_shares("jerry", &trip.id, &["liz".to_string()])
            .await
            .unwrap();
        assert!(repo
            .set_shares("jerry", &trip.id, &["nobody".to_string()])
            .await
            .is_err());
        assert_eq!(
            repo.find(&trip.id).await.unwrap().unwrap().shared_with,
            ["liz"],
            "a share with an unknown member changes nothing"
        );
        sqlx::query("DELETE FROM profiles WHERE id = 'jerry'")
            .execute(&repo.pool)
            .await
            .unwrap();
        assert_eq!(repo.find(&trip.id).await.unwrap(), None);
        assert!(repo.deliverables_of(&trip.id).await.unwrap().is_empty());
        let shares: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM anchor_shares")
            .fetch_one(&repo.pool)
            .await
            .unwrap();
        assert_eq!(shares, 0);
    }
}
