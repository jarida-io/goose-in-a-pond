//! SQLite-backed [`TodoRepository`] and [`HookEventQueue`], over migrations 0087 and 0088, and the
//! member's rows a hook may name (cart items, mail and calendar accounts, webhooks). Every time is
//! written as RFC 3339 UTC to the second, so the due queries compare text.

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use sqlx::sqlite::SqliteRow;
use sqlx::{Pool, Row, Sqlite};

use pond_core::shared::domain::todo::{
    rfc3339, Hook, HookEvent, HookSource, HookSpec, Place, StateFilter, Todo, TodoKind, TodoSource,
    TodoState, MAX_ASKS,
};
use pond_core::shared::ports::todo::{HookEventQueue, TodoRepository};

pub struct SqliteTodoRepository {
    pool: Pool<Sqlite>,
}

impl SqliteTodoRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

const TODO_COLUMNS: &str = "id, profile_id, kind, text, notes, state, due_at, ask, source, \
     source_ref, last_asked_at, asked, snoozed_until, created_at, updated_at, done_at";
const HOOK_COLUMNS: &str =
    "id, todo_id, kind, spec, label, next_fire_at, last_fired_at, created_at";

/// A to-do Goose has asked about as often as it asks is due for no further question.
const ASKING_OVER: &str = "(kind = 'todo' AND ask = 1 AND asked >= ?)";

/// Reads what this adapter wrote, and SQLite's own `datetime('now')` form from other writers.
fn time(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S").map(|t| t.and_utc()))
        .with_context(|| format!("bad to-do timestamp {raw:?}"))
}

fn maybe_time(row: &SqliteRow, column: &str) -> Result<Option<DateTime<Utc>>> {
    row.try_get::<Option<String>, _>(column)?
        .as_deref()
        .map(time)
        .transpose()
}

fn todo_of(row: &SqliteRow) -> Result<Todo> {
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let state: String = row.try_get("state")?;
    let source: String = row.try_get("source")?;
    Ok(Todo {
        kind: TodoKind::parse(&kind).with_context(|| format!("to-do {id}: kind {kind}"))?,
        state: TodoState::parse(&state).with_context(|| format!("to-do {id}: state {state}"))?,
        source: TodoSource::parse(&source).unwrap_or(TodoSource::You),
        owner: row.try_get("profile_id")?,
        text: row.try_get("text")?,
        notes: row.try_get("notes")?,
        due_at: maybe_time(row, "due_at")?,
        ask: row.try_get::<i64, _>("ask")? != 0,
        source_ref: row.try_get("source_ref")?,
        last_asked_at: maybe_time(row, "last_asked_at")?,
        asked: row.try_get::<i64, _>("asked")?.max(0) as u32,
        snoozed_until: maybe_time(row, "snoozed_until")?,
        created_at: time(&row.try_get::<String, _>("created_at")?)?,
        updated_at: time(&row.try_get::<String, _>("updated_at")?)?,
        done_at: maybe_time(row, "done_at")?,
        hooks: Vec::new(),
        id,
    })
}

fn hook_of(row: &SqliteRow) -> Result<(String, Hook)> {
    let id: String = row.try_get("id")?;
    let kind: String = row.try_get("kind")?;
    let spec: Value = serde_json::from_str(&row.try_get::<String, _>("spec")?)
        .with_context(|| format!("hook {id}: spec is not JSON"))?;
    let spec = HookSpec::parse(&kind, &spec).with_context(|| format!("hook {id}"))?;
    Ok((
        row.try_get("todo_id")?,
        Hook {
            spec,
            label: row.try_get("label")?,
            next_fire_at: maybe_time(row, "next_fire_at")?,
            last_fired_at: maybe_time(row, "last_fired_at")?,
            created_at: time(&row.try_get::<String, _>("created_at")?)?,
            id,
        },
    ))
}

fn place_of(row: &SqliteRow) -> Result<Place> {
    Ok(Place {
        id: row.try_get("id")?,
        owner: row.try_get("profile_id")?,
        label: row.try_get("label")?,
        lat: row.try_get("lat")?,
        lng: row.try_get("lng")?,
        radius_m: row.try_get("radius_m")?,
        created_at: time(&row.try_get::<String, _>("created_at")?)?,
    })
}

fn event_of(row: &SqliteRow) -> Result<HookEvent> {
    let payload: String = row.try_get("payload")?;
    Ok(HookEvent {
        id: row.try_get("id")?,
        profile_id: row.try_get("profile_id")?,
        source_kind: row.try_get("source_kind")?,
        source_id: row.try_get("source_id")?,
        event: row.try_get("event")?,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        // An unreadable time reads as now, so the event is matched rather than stuck.
        at: time(&row.try_get::<String, _>("at")?).unwrap_or_else(|_| Utc::now()),
    })
}

/// A value bound into one of the load queries.
#[derive(Clone)]
enum Bind {
    Text(String),
    Int(i64),
}

impl SqliteTodoRepository {
    /// To-dos matching `predicate` (over `todos`), each with every hook, in `order`.
    async fn load(&self, predicate: &str, binds: &[Bind], order: &str) -> Result<Vec<Todo>> {
        let sql = format!("SELECT {TODO_COLUMNS} FROM todos WHERE {predicate} ORDER BY {order}");
        let mut query = sqlx::query(&sql);
        for bind in binds {
            query = match bind {
                Bind::Text(t) => query.bind(t.clone()),
                Bind::Int(i) => query.bind(*i),
            };
        }
        let rows = query.fetch_all(&self.pool).await?;
        let mut todos = Vec::with_capacity(rows.len());
        for row in &rows {
            match todo_of(row) {
                Ok(todo) => todos.push(todo),
                Err(e) => tracing::warn!(error = %e, "[todos] skipping an unreadable to-do"),
            }
        }
        if todos.is_empty() {
            return Ok(todos);
        }
        let marks = vec!["?"; todos.len()].join(", ");
        let sql = format!(
            "SELECT {HOOK_COLUMNS} FROM todo_hooks WHERE todo_id IN ({marks}) \
             ORDER BY created_at, rowid"
        );
        let mut query = sqlx::query(&sql);
        for todo in &todos {
            query = query.bind(todo.id.clone());
        }
        for row in query.fetch_all(&self.pool).await? {
            match hook_of(&row) {
                Ok((todo_id, hook)) => {
                    if let Some(todo) = todos.iter_mut().find(|t| t.id == todo_id) {
                        todo.hooks.push(hook);
                    }
                }
                Err(e) => tracing::warn!(error = %e, "[todos] skipping an unreadable hook"),
            }
        }
        Ok(todos)
    }

    async fn insert_hooks(
        tx: &mut sqlx::Transaction<'_, Sqlite>,
        todo_id: &str,
        hooks: &[Hook],
    ) -> Result<()> {
        for hook in hooks {
            sqlx::query(&format!(
                "INSERT INTO todo_hooks ({HOOK_COLUMNS}) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
            ))
            .bind(&hook.id)
            .bind(todo_id)
            .bind(hook.spec.kind())
            .bind(hook.spec.to_json().to_string())
            .bind(&hook.label)
            .bind(hook.next_fire_at.map(rfc3339))
            .bind(hook.last_fired_at.map(rfc3339))
            .bind(rfc3339(hook.created_at))
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// The member's watched links, as sources a hook may name.
    pub async fn cart_sources(&self, owner: &str) -> Result<Vec<HookSource>> {
        let rows = sqlx::query(
            "SELECT id, title, site, url FROM cart_items WHERE profile_id = ? \
             ORDER BY created_at, rowid",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(HookSource::cart(
                    &r.try_get::<String, _>("id")?,
                    &r.try_get::<String, _>("title")?,
                    &r.try_get::<String, _>("site")?,
                    &r.try_get::<String, _>("url")?,
                ))
            })
            .collect()
    }

    /// The member's mail and calendar accounts and their webhooks, as connectors a hook may name.
    pub async fn connector_sources(&self, owner: &str) -> Result<Vec<HookSource>> {
        let mut sources = Vec::new();
        let rows = sqlx::query(
            "SELECT id, kind, provider FROM context_sources \
             WHERE profile_id = ? AND kind IN ('mail', 'calendar') ORDER BY created_at, rowid",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        for r in &rows {
            let id: String = r.try_get("id")?;
            let provider: String = r.try_get("provider")?;
            sources.push(match r.try_get::<String, _>("kind")?.as_str() {
                "mail" => HookSource::mail(&id, &provider),
                _ => HookSource::calendar(&id, &provider),
            });
        }
        let rows = sqlx::query(
            "SELECT id, name FROM connector_webhooks WHERE profile_id = ? \
             ORDER BY created_at, rowid",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        for r in &rows {
            sources.push(HookSource::webhook(
                &r.try_get::<String, _>("id")?,
                &r.try_get::<String, _>("name")?,
            ));
        }
        Ok(sources)
    }
}

const LIST_ORDER: &str = "state <> 'open', \
     CASE WHEN state = 'open' THEN due_at IS NULL END, \
     CASE WHEN state = 'open' THEN due_at END, \
     COALESCE(done_at, updated_at) DESC, created_at DESC, rowid DESC";
const DUE_ORDER: &str = "created_at, rowid";

#[async_trait]
impl TodoRepository for SqliteTodoRepository {
    async fn create(&self, todo: &Todo) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(&format!(
            "INSERT INTO todos ({TODO_COLUMNS}) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(&todo.id)
        .bind(&todo.owner)
        .bind(todo.kind.as_str())
        .bind(&todo.text)
        .bind(&todo.notes)
        .bind(todo.state.as_str())
        .bind(todo.due_at.map(rfc3339))
        .bind(i64::from(todo.ask))
        .bind(todo.source.as_str())
        .bind(&todo.source_ref)
        .bind(todo.last_asked_at.map(rfc3339))
        .bind(i64::from(todo.asked))
        .bind(todo.snoozed_until.map(rfc3339))
        .bind(rfc3339(todo.created_at))
        .bind(rfc3339(todo.updated_at))
        .bind(todo.done_at.map(rfc3339))
        .execute(&mut *tx)
        .await
        .context("saving a to-do")?;
        Self::insert_hooks(&mut tx, &todo.id, &todo.hooks).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn get(&self, owner: &str, id: &str) -> Result<Option<Todo>> {
        Ok(self
            .load(
                "profile_id = ? AND id = ?",
                &[Bind::Text(owner.into()), Bind::Text(id.into())],
                DUE_ORDER,
            )
            .await?
            .pop())
    }

    async fn list(&self, owner: &str, filter: StateFilter) -> Result<Vec<Todo>> {
        let state = match filter {
            StateFilter::All => "",
            StateFilter::Open => " AND state = 'open'",
            StateFilter::Done => " AND state = 'done'",
            StateFilter::Dropped => " AND state = 'dropped'",
        };
        self.load(
            &format!("profile_id = ?{state}"),
            &[Bind::Text(owner.into())],
            LIST_ORDER,
        )
        .await
    }

    async fn update(&self, todo: &Todo, replace_hooks: bool) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE todos SET kind = ?, text = ?, notes = ?, state = ?, due_at = ?, ask = ?, \
             last_asked_at = ?, asked = ?, snoozed_until = ?, updated_at = ?, done_at = ? \
             WHERE id = ? AND profile_id = ?",
        )
        .bind(todo.kind.as_str())
        .bind(&todo.text)
        .bind(&todo.notes)
        .bind(todo.state.as_str())
        .bind(todo.due_at.map(rfc3339))
        .bind(i64::from(todo.ask))
        .bind(todo.last_asked_at.map(rfc3339))
        .bind(i64::from(todo.asked))
        .bind(todo.snoozed_until.map(rfc3339))
        .bind(rfc3339(todo.updated_at))
        .bind(todo.done_at.map(rfc3339))
        .bind(&todo.id)
        .bind(&todo.owner)
        .execute(&mut *tx)
        .await
        .context("updating a to-do")?;
        if replace_hooks {
            sqlx::query("DELETE FROM todo_hooks WHERE todo_id = ?")
                .bind(&todo.id)
                .execute(&mut *tx)
                .await?;
            Self::insert_hooks(&mut tx, &todo.id, &todo.hooks).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn delete(&self, owner: &str, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let gone = sqlx::query("DELETE FROM todos WHERE id = ? AND profile_id = ?")
            .bind(id)
            .bind(owner)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        if gone {
            sqlx::query("DELETE FROM todo_hooks WHERE todo_id = ?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(gone)
    }

    async fn due_clock_hooks(&self, now: DateTime<Utc>) -> Result<Vec<Todo>> {
        self.load(
            "state = 'open' AND EXISTS (SELECT 1 FROM todo_hooks h WHERE h.todo_id = todos.id \
             AND h.kind IN ('time', 'repeat') AND h.next_fire_at IS NOT NULL \
             AND h.next_fire_at <= ?)",
            &[Bind::Text(rfc3339(now))],
            DUE_ORDER,
        )
        .await
    }

    async fn due_dates(&self, now: DateTime<Utc>) -> Result<Vec<Todo>> {
        self.load(
            &format!(
                "state = 'open' AND due_at IS NOT NULL AND due_at <= ? \
                 AND (last_asked_at IS NULL OR last_asked_at < due_at) \
                 AND NOT EXISTS (SELECT 1 FROM todo_hooks h WHERE h.todo_id = todos.id) \
                 AND NOT {ASKING_OVER}"
            ),
            &[Bind::Text(rfc3339(now)), Bind::Int(i64::from(MAX_ASKS))],
            DUE_ORDER,
        )
        .await
    }

    async fn follow_ups_due(&self, now: DateTime<Utc>) -> Result<Vec<Todo>> {
        self.load(
            &format!(
                "state = 'open' AND snoozed_until IS NOT NULL AND snoozed_until <= ? \
                 AND NOT {ASKING_OVER}"
            ),
            &[Bind::Text(rfc3339(now)), Bind::Int(i64::from(MAX_ASKS))],
            DUE_ORDER,
        )
        .await
    }

    async fn listening(&self) -> Result<Vec<Todo>> {
        self.load(
            "state = 'open' AND EXISTS (SELECT 1 FROM todo_hooks h WHERE h.todo_id = todos.id \
             AND h.kind IN ('place', 'event', 'connector'))",
            &[],
            DUE_ORDER,
        )
        .await
    }

    async fn hook_fired(
        &self,
        hook_id: &str,
        fired_at: DateTime<Utc>,
        next_fire_at: Option<DateTime<Utc>>,
    ) -> Result<()> {
        sqlx::query("UPDATE todo_hooks SET last_fired_at = ?, next_fire_at = ? WHERE id = ?")
            .bind(rfc3339(fired_at))
            .bind(next_fire_at.map(rfc3339))
            .bind(hook_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn pushed(&self, todo_id: &str, at: DateTime<Utc>, asked: u32) -> Result<()> {
        sqlx::query(
            "UPDATE todos SET last_asked_at = ?, asked = ?, snoozed_until = NULL WHERE id = ?",
        )
        .bind(rfc3339(at))
        .bind(i64::from(asked))
        .bind(todo_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn places(&self, owner: &str) -> Result<Vec<Place>> {
        let rows = sqlx::query(
            "SELECT id, profile_id, label, lat, lng, radius_m, created_at FROM member_places \
             WHERE profile_id = ? ORDER BY created_at, rowid",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(place_of).collect()
    }

    async fn add_place(&self, place: &Place) -> Result<()> {
        sqlx::query(
            "INSERT INTO member_places (id, profile_id, label, lat, lng, radius_m, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&place.id)
        .bind(&place.owner)
        .bind(&place.label)
        .bind(place.lat)
        .bind(place.lng)
        .bind(place.radius_m)
        .bind(rfc3339(place.created_at))
        .execute(&self.pool)
        .await
        .context("saving a place")?;
        Ok(())
    }

    async fn delete_place(&self, owner: &str, id: &str) -> Result<bool> {
        let mut tx = self.pool.begin().await?;
        let gone = sqlx::query("DELETE FROM member_places WHERE id = ? AND profile_id = ?")
            .bind(id)
            .bind(owner)
            .execute(&mut *tx)
            .await?
            .rows_affected()
            > 0;
        if gone {
            let rows = sqlx::query(
                "SELECT h.id, h.spec FROM todo_hooks h JOIN todos t ON t.id = h.todo_id \
                 WHERE t.profile_id = ? AND h.kind = 'place'",
            )
            .bind(owner)
            .fetch_all(&mut *tx)
            .await?;
            for row in rows {
                let spec: Value =
                    serde_json::from_str(&row.try_get::<String, _>("spec")?).unwrap_or_default();
                if spec.get("place_id").and_then(Value::as_str) == Some(id) {
                    sqlx::query("DELETE FROM todo_hooks WHERE id = ?")
                        .bind(row.try_get::<String, _>("id")?)
                        .execute(&mut *tx)
                        .await?;
                }
            }
        }
        tx.commit().await?;
        Ok(gone)
    }
}

#[async_trait]
impl HookEventQueue for SqliteTodoRepository {
    async fn raise(&self, event: &HookEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO hook_events (id, profile_id, source_kind, source_id, event, payload, at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event.id)
        .bind(&event.profile_id)
        .bind(&event.source_kind)
        .bind(&event.source_id)
        .bind(&event.event)
        .bind(event.payload.to_string())
        .bind(rfc3339(event.at))
        .execute(&self.pool)
        .await
        .context("raising a hook event")?;
        Ok(())
    }

    async fn unhandled(&self, limit: usize) -> Result<Vec<HookEvent>> {
        let rows = sqlx::query(
            "SELECT id, profile_id, source_kind, source_id, event, payload, at FROM hook_events \
             WHERE handled_at IS NULL ORDER BY at, rowid LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(event_of).collect()
    }

    async fn mark_handled(&self, ids: &[String], at: DateTime<Utc>) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        for id in ids {
            sqlx::query("UPDATE hook_events SET handled_at = ? WHERE id = ?")
                .bind(rfc3339(at))
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn prune_handled(&self, before: DateTime<Utc>) -> Result<u64> {
        Ok(
            sqlx::query("DELETE FROM hook_events WHERE handled_at IS NOT NULL AND handled_at < ?")
                .bind(rfc3339(before))
                .execute(&self.pool)
                .await?
                .rows_affected(),
        )
    }
}

#[cfg(test)]
mod tests;
