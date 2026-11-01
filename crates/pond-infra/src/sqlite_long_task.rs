//! SQLite-backed [`LongTaskRepository`], over the tables migration 0062 added.
//!
//! State moves are compare-and-set on the stored state, so a concurrent move loses with an error
//! instead of overwriting; a replan runs in one `BEGIN IMMEDIATE` transaction.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat, Utc};
use pond_core::shared::domain::long_task::{
    AnswerShape, Artifact, ArtifactKind, ArtifactParts, ArtifactSummary, LongTask, LongTaskKind,
    LongTaskParts, LongTaskState, PlanStep, PlanStepParts, StepKey, StepState, StepStateRefused,
    ValidatedPlan,
};
use pond_core::shared::ports::long_task::{LongTaskOwnerFilter, LongTaskRepository};
use pond_core::user_data::domain::profile::ProfileScope;
use sqlx::{Pool, Sqlite};

/// Column order must match `TaskRow`.
const TASK_COLUMNS: &str = "id, owner_profile_id, scope, goal, origin_session_id, state, \
     max_steps, deadline, created_at, updated_at, finished_at, kind, answer";

/// `(id, owner_profile_id, scope, goal, origin_session_id, state, max_steps, deadline,
/// created_at, updated_at, finished_at, kind, answer)`.
type TaskRow = (
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    i64,
    Option<String>,
    String,
    String,
    Option<String>,
    String,
    String,
);

/// Column order must match `StepRow`.
const STEP_COLUMNS: &str = "task_id, step_key, ordinal, role, instructions, inputs, depends_on, \
     acceptance, state, attempts, summary, failure, started_at, finished_at, agent";

/// `(task_id, step_key, ordinal, role, instructions, inputs, depends_on, acceptance, state,
/// attempts, summary, failure, started_at, finished_at, agent)`.
type StepRow = (
    String,
    String,
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    String,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// Every column but `body`, so a listing never reads one. Order must match `ArtifactSummaryRow`.
const ARTIFACT_SUMMARY_COLUMNS: &str =
    "id, task_id, step_key, kind, title, summary, tokens, created_at";

/// `(id, task_id, step_key, kind, title, summary, tokens, created_at)`.
type ArtifactSummaryRow = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    i64,
    String,
);

/// Column order must match `ArtifactRow`; `tokens` is recomputed from the body.
const ARTIFACT_COLUMNS: &str = "id, task_id, step_key, kind, title, summary, body, created_at";

/// `(id, task_id, step_key, kind, title, summary, body, created_at)`.
type ArtifactRow = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
);

pub struct SqliteLongTaskRepository {
    pool: Pool<Sqlite>,
}

impl SqliteLongTaskRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

/// Seconds, UTC, `Z`: one format in every column, so text order is time order.
fn sql_ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn parse_ts(raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .with_context(|| format!("unreadable long-task timestamp: {raw}"))
}

fn parse_opt_ts(raw: Option<String>) -> Result<Option<DateTime<Utc>>> {
    raw.as_deref().map(parse_ts).transpose()
}

fn scope_column(scope: &ProfileScope) -> &'static str {
    match scope {
        ProfileScope::Owner(_) => "owner",
        ProfileScope::Household => "household",
        ProfileScope::Guest => "guest",
    }
}

/// The member of an `owner` scope is `owner_profile_id`; there is no second copy to disagree.
fn scope_from_columns(scope: &str, owner: Option<&str>) -> Result<ProfileScope> {
    match (scope, owner) {
        ("owner", Some(id)) => Ok(ProfileScope::Owner(id.to_string())),
        ("household", _) => Ok(ProfileScope::Household),
        ("guest", _) => Ok(ProfileScope::Guest),
        (other, owner) => bail!("unreadable long-task scope {other:?} with owner {owner:?}"),
    }
}

/// Built from the domain, so the SQL list cannot drift from `is_terminal`.
fn terminal_states_sql() -> String {
    LongTaskState::ALL
        .into_iter()
        .filter(|state| state.is_terminal())
        .map(|state| format!("'{}'", state.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Built from the domain, so the SQL list cannot drift from `survives_replan`.
fn open_step_states_sql() -> String {
    StepState::ALL
        .into_iter()
        .filter(|state| !state.survives_replan())
        .map(|state| format!("'{}'", state.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn u32_column(value: i64, column: &str) -> Result<u32> {
    u32::try_from(value).with_context(|| format!("long-task column {column} out of range: {value}"))
}

fn json_list(raw: &str, column: &str) -> Result<Vec<String>> {
    serde_json::from_str(raw).with_context(|| format!("unreadable long-task {column}: {raw}"))
}

fn row_to_task(row: TaskRow) -> Result<LongTask> {
    let (
        id,
        owner_profile_id,
        scope,
        goal,
        origin_session_id,
        state,
        max_steps,
        deadline,
        created_at,
        updated_at,
        finished_at,
        kind,
        answer,
    ) = row;
    let scope = scope_from_columns(&scope, owner_profile_id.as_deref())?;
    let state = LongTaskState::parse(&state)
        .with_context(|| format!("unreadable state on long task {id}: {state}"))?;
    let kind = LongTaskKind::parse(&kind)
        .with_context(|| format!("unreadable kind on long task {id}: {kind}"))?;
    let answer = AnswerShape::parse(&answer)
        .with_context(|| format!("unreadable answer on long task {id}: {answer}"))?;
    let parts = LongTaskParts {
        owner_profile_id,
        scope,
        goal,
        origin_session_id,
        state,
        max_steps: u32_column(max_steps, "max_steps")?,
        deadline: parse_opt_ts(deadline)?,
        kind,
        answer,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
        finished_at: parse_opt_ts(finished_at)?,
        id,
    };
    Ok(LongTask::from_parts(parts)?)
}

fn row_to_step(row: StepRow) -> Result<PlanStep> {
    let (
        task_id,
        step_key,
        ordinal,
        role,
        instructions,
        inputs,
        depends_on,
        acceptance,
        state,
        attempts,
        summary,
        failure,
        started_at,
        finished_at,
        agent,
    ) = row;
    let state = StepState::parse(&state)
        .with_context(|| format!("unreadable state on step {step_key} of {task_id}: {state}"))?;
    let context = format!("step {step_key} of long task {task_id}");
    let parts = PlanStepParts {
        ordinal: u32_column(ordinal, "ordinal")?,
        role,
        instructions,
        inputs: json_list(&inputs, "inputs")?,
        depends_on: json_list(&depends_on, "depends_on")?,
        acceptance,
        state,
        attempts: u32_column(attempts, "attempts")?,
        summary,
        failure,
        started_at: parse_opt_ts(started_at)?,
        finished_at: parse_opt_ts(finished_at)?,
        agent: agent
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .with_context(|| format!("unreadable agent on {context}"))?,
        task_id,
        key: step_key,
    };
    PlanStep::from_parts(parts).context(context)
}

fn parse_kind(raw: &str, id: &str) -> Result<ArtifactKind> {
    ArtifactKind::parse(raw).with_context(|| format!("unreadable kind on artifact {id}: {raw}"))
}

fn row_to_artifact(row: ArtifactRow) -> Result<Artifact> {
    let (id, task_id, step_key, kind, title, summary, body, created_at) = row;
    let kind = parse_kind(&kind, &id)?;
    let created_at = parse_ts(&created_at)?;
    Ok(Artifact::from_parts(ArtifactParts {
        id,
        task_id,
        step_key,
        kind,
        title,
        summary,
        body,
        created_at,
    })?)
}

fn row_to_artifact_summary(row: ArtifactSummaryRow) -> Result<ArtifactSummary> {
    let (id, task_id, step_key, kind, title, summary, tokens, created_at) = row;
    let step_key = step_key
        .as_deref()
        .map(StepKey::parse)
        .transpose()
        .with_context(|| format!("unreadable step key on artifact {id}"))?;
    Ok(ArtifactSummary {
        kind: parse_kind(&kind, &id)?,
        tokens: u32_column(tokens, "tokens")?,
        created_at: parse_ts(&created_at)?,
        id,
        task_id,
        step_key,
        title,
        summary,
    })
}

#[async_trait]
impl LongTaskRepository for SqliteLongTaskRepository {
    async fn create_task(&self, task: &LongTask) -> Result<()> {
        sqlx::query(
            "INSERT INTO long_tasks \
             (id, owner_profile_id, scope, goal, origin_session_id, state, max_steps, deadline, \
              created_at, updated_at, finished_at, kind, answer) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(task.id())
        .bind(task.owner_profile_id())
        .bind(scope_column(task.scope()))
        .bind(task.goal())
        .bind(task.origin_session_id())
        .bind(task.state().as_str())
        .bind(i64::from(task.budget().max_steps()))
        .bind(task.budget().deadline().map(sql_ts))
        .bind(sql_ts(task.created_at()))
        .bind(sql_ts(task.updated_at()))
        .bind(task.finished_at().map(sql_ts))
        .bind(task.kind().as_str())
        .bind(task.answer().as_str())
        .execute(&self.pool)
        .await
        .with_context(|| format!("storing long task {}", task.id()))?;
        Ok(())
    }

    async fn get_task(&self, id: &str) -> Result<Option<LongTask>> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM long_tasks WHERE id = ?");
        let row: Option<TaskRow> = sqlx::query_as(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("reading long task {id}"))?;
        row.map(row_to_task).transpose()
    }

    async fn list_tasks(
        &self,
        owner: LongTaskOwnerFilter<'_>,
        include_finished: bool,
    ) -> Result<Vec<LongTask>> {
        let (owner_predicate, owner_id) = match owner {
            LongTaskOwnerFilter::Everyone => ("", None),
            LongTaskOwnerFilter::Member(id) => ("AND owner_profile_id = ?", Some(id)),
            LongTaskOwnerFilter::Unowned => ("AND owner_profile_id IS NULL", None),
        };
        let state_predicate = if include_finished {
            String::new()
        } else {
            format!("AND state NOT IN ({})", terminal_states_sql())
        };
        let sql = format!(
            "SELECT {TASK_COLUMNS} FROM long_tasks WHERE 1 = 1 {owner_predicate} \
             {state_predicate} ORDER BY created_at DESC, rowid DESC"
        );
        let mut query = sqlx::query_as::<_, TaskRow>(&sql);
        if let Some(owner_id) = owner_id {
            query = query.bind(owner_id);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .context("listing long tasks")?;

        // Skip, don't fail: one bad row must not hide a member's other tasks.
        Ok(rows
            .into_iter()
            .filter_map(|row| match row_to_task(row) {
                Ok(task) => Some(task),
                Err(e) => {
                    tracing::warn!(
                        target: "giap::trace",
                        kind = "long_task_row_unreadable",
                        error = %e,
                        "a stored long task did not survive validation and was not listed"
                    );
                    None
                }
            })
            .collect())
    }

    async fn decide_task_answer(&self, id: &str, answer: AnswerShape) -> Result<bool> {
        if answer == AnswerShape::Auto {
            return Ok(false);
        }
        let result =
            sqlx::query("UPDATE long_tasks SET answer = ? WHERE id = ? AND answer = 'auto'")
                .bind(answer.as_str())
                .bind(id)
                .execute(&self.pool)
                .await
                .with_context(|| format!("recording long task {id}'s answer"))?;
        Ok(result.rows_affected() > 0)
    }

    async fn set_task_state(
        &self,
        id: &str,
        state: LongTaskState,
        at: DateTime<Utc>,
    ) -> Result<Option<LongTask>> {
        let Some(mut task) = self.get_task(id).await? else {
            return Ok(None);
        };
        let from = task.state();
        task.transition_to(state, at)?;
        let result = sqlx::query(
            "UPDATE long_tasks SET state = ?, updated_at = ?, finished_at = ? \
             WHERE id = ? AND state = ?",
        )
        .bind(task.state().as_str())
        .bind(sql_ts(task.updated_at()))
        .bind(task.finished_at().map(sql_ts))
        .bind(id)
        .bind(from.as_str())
        .execute(&self.pool)
        .await
        .with_context(|| format!("moving long task {id} to {state}"))?;
        if result.rows_affected() == 0 {
            if self.get_task(id).await?.is_none() {
                return Ok(None);
            }
            bail!("long task {id} moved while it was being set to {state}; read it again");
        }
        Ok(Some(task))
    }

    async fn replace_open_steps(&self, plan: &ValidatedPlan) -> Result<()> {
        let task_id = plan.task_id();
        // IMMEDIATE takes the write lock before the reads the checks depend on.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;

        let task_state: Option<(String,)> =
            sqlx::query_as("SELECT state FROM long_tasks WHERE id = ?")
                .bind(task_id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((task_state,)) = task_state else {
            bail!("there is no long task {task_id} to plan");
        };
        let task_state = LongTaskState::parse(&task_state)
            .with_context(|| format!("unreadable state on long task {task_id}: {task_state}"))?;
        if task_state.is_terminal() {
            bail!("long task {task_id} is {task_state} and takes no new plan");
        }

        let stored: Vec<(String, String)> =
            sqlx::query_as("SELECT step_key, state FROM long_task_steps WHERE task_id = ?")
                .bind(task_id)
                .fetch_all(&mut *tx)
                .await?;
        let stored = stored
            .into_iter()
            .map(|(key, state)| {
                let parsed = StepState::parse(&state).with_context(|| {
                    format!("unreadable state on step {key} of {task_id}: {state}")
                })?;
                Ok((key, parsed))
            })
            .collect::<Result<Vec<(String, StepState)>>>()?;
        plan.check_current(stored.iter().map(|(key, state)| (key.as_str(), *state)))?;

        sqlx::query(&format!(
            "DELETE FROM long_task_steps WHERE task_id = ? AND state IN ({})",
            open_step_states_sql()
        ))
        .bind(task_id)
        .execute(&mut *tx)
        .await
        .with_context(|| format!("clearing the open steps of long task {task_id}"))?;

        for step in plan.steps() {
            let depends_on: Vec<&str> = step.depends_on().iter().map(StepKey::as_str).collect();
            sqlx::query(
                "INSERT INTO long_task_steps \
                 (task_id, step_key, ordinal, role, instructions, inputs, depends_on, acceptance, \
                  state, attempts, summary, failure, started_at, finished_at, agent) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(step.task_id())
            .bind(step.key().as_str())
            .bind(i64::from(step.ordinal()))
            .bind(step.role())
            .bind(step.instructions())
            .bind(serde_json::to_string(step.inputs())?)
            .bind(serde_json::to_string(&depends_on)?)
            .bind(step.acceptance())
            .bind(step.state().as_str())
            .bind(i64::from(step.attempts()))
            .bind(step.summary())
            .bind(step.failure())
            .bind(step.started_at().map(sql_ts))
            .bind(step.finished_at().map(sql_ts))
            .bind(step.agent().map(serde_json::to_string).transpose()?)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("storing step {} of long task {task_id}", step.key()))?;
        }

        tx.commit().await?;
        Ok(())
    }

    async fn list_steps(&self, task_id: &str) -> Result<Vec<PlanStep>> {
        let sql = format!(
            "SELECT {STEP_COLUMNS} FROM long_task_steps WHERE task_id = ? \
             ORDER BY ordinal ASC, rowid ASC"
        );
        let rows: Vec<StepRow> = sqlx::query_as(&sql)
            .bind(task_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("listing the steps of long task {task_id}"))?;
        rows.into_iter().map(row_to_step).collect()
    }

    async fn update_step(&self, step: &PlanStep) -> Result<()> {
        let task_id = step.task_id();
        let key = step.key().as_str();
        let stored: Option<(String,)> =
            sqlx::query_as("SELECT state FROM long_task_steps WHERE task_id = ? AND step_key = ?")
                .bind(task_id)
                .bind(key)
                .fetch_optional(&self.pool)
                .await?;
        let Some((stored,)) = stored else {
            bail!("there is no step `{key}` in long task {task_id}");
        };
        let from = StepState::parse(&stored)
            .with_context(|| format!("unreadable state on step {key} of {task_id}: {stored}"))?;
        if !from.can_become(step.state()) {
            return Err(StepStateRefused {
                step: key.to_string(),
                from,
                to: step.state(),
            }
            .into());
        }
        let result = sqlx::query(
            "UPDATE long_task_steps \
             SET state = ?, attempts = ?, summary = ?, failure = ?, started_at = ?, finished_at = ? \
             WHERE task_id = ? AND step_key = ? AND state = ?",
        )
        .bind(step.state().as_str())
        .bind(i64::from(step.attempts()))
        .bind(step.summary())
        .bind(step.failure())
        .bind(step.started_at().map(sql_ts))
        .bind(step.finished_at().map(sql_ts))
        .bind(task_id)
        .bind(key)
        .bind(from.as_str())
        .execute(&self.pool)
        .await
        .with_context(|| format!("updating step {key} of long task {task_id}"))?;
        if result.rows_affected() == 0 {
            bail!("step `{key}` of long task {task_id} moved while it was being updated; read it again");
        }
        Ok(())
    }

    async fn insert_artifact(&self, artifact: &Artifact) -> Result<()> {
        sqlx::query(
            "INSERT INTO long_task_artifacts \
             (id, task_id, step_key, kind, title, summary, body, tokens, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(artifact.id())
        .bind(artifact.task_id())
        .bind(artifact.step_key().map(StepKey::as_str))
        .bind(artifact.kind().as_str())
        .bind(artifact.title())
        .bind(artifact.summary())
        .bind(artifact.body())
        .bind(i64::from(artifact.tokens()))
        .bind(sql_ts(artifact.created_at()))
        .execute(&self.pool)
        .await
        .with_context(|| format!("storing artifact {}", artifact.id()))?;
        Ok(())
    }

    async fn get_artifact(&self, task_id: &str, artifact_id: &str) -> Result<Option<Artifact>> {
        let sql = format!(
            "SELECT {ARTIFACT_COLUMNS} FROM long_task_artifacts WHERE id = ? AND task_id = ?"
        );
        let row: Option<ArtifactRow> = sqlx::query_as(&sql)
            .bind(artifact_id)
            .bind(task_id)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("reading artifact {artifact_id}"))?;
        row.map(row_to_artifact).transpose()
    }

    async fn list_artifacts(&self, task_id: &str) -> Result<Vec<ArtifactSummary>> {
        let sql = format!(
            "SELECT {ARTIFACT_SUMMARY_COLUMNS} FROM long_task_artifacts WHERE task_id = ? \
             ORDER BY created_at ASC, rowid ASC"
        );
        let rows: Vec<ArtifactSummaryRow> = sqlx::query_as(&sql)
            .bind(task_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("listing the artifacts of long task {task_id}"))?;
        Ok(rows
            .into_iter()
            .filter_map(|row| match row_to_artifact_summary(row) {
                Ok(summary) => Some(summary),
                Err(e) => {
                    tracing::warn!(
                        target: "giap::trace",
                        kind = "long_task_artifact_unreadable",
                        error = %e,
                        "a stored artifact did not survive validation and was not listed"
                    );
                    None
                }
            })
            .collect())
    }

    async fn delete_task(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM long_tasks WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("deleting long task {id}"))?;
        Ok(result.rows_affected() > 0)
    }

    async fn delete_artifact(&self, task_id: &str, artifact_id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM long_task_artifacts WHERE id = ? AND task_id = ?")
            .bind(artifact_id)
            .bind(task_id)
            .execute(&self.pool)
            .await
            .with_context(|| format!("deleting artifact {artifact_id}"))?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::shared::domain::long_task::{
        validate_plan, LongTaskBudget, LongTaskStateRefused, OutcomeKind, StalePlan, StepDraft,
        StepOutcome,
    };

    async fn db() -> (tempfile::TempDir, Pool<Sqlite>) {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::db::Database::init(tmp.path()).await.unwrap();
        let pool = db.system.clone();
        (tmp, pool)
    }

    async fn repo() -> (tempfile::TempDir, Pool<Sqlite>, SqliteLongTaskRepository) {
        let (tmp, pool) = db().await;
        let repo = SqliteLongTaskRepository::new(pool.clone());
        (tmp, pool, repo)
    }

    /// Whole seconds, since the columns store seconds.
    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    fn household_task(at: DateTime<Utc>) -> LongTask {
        LongTask::start(
            ProfileScope::Household,
            None,
            "compare three plumbers",
            None,
            LongTaskBudget::new(8, None).unwrap(),
            at,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_step_keeps_the_agent_its_plan_made() {
        use pond_core::shared::domain::long_task::StepAgent;
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        let made = StepDraft {
            role: String::new(),
            new_agent: Some(StepAgent {
                name: "Fare Checker".into(),
                instructions: "Compare matatu and SGR fares.".into(),
                tools: vec!["giap-knowledge".into()],
            }),
            ..draft("fares", &[])
        };
        plan(&repo, &task, vec![made, draft("write", &["fares"])]).await;
        let fares = step(&repo, &task, "fares").await;
        assert_eq!(fares.role(), "new:Fare Checker");
        assert_eq!(
            fares.agent().unwrap().tools,
            vec!["giap-knowledge".to_string()]
        );
        assert!(step(&repo, &task, "write").await.agent().is_none());
    }

    #[tokio::test]
    async fn the_planner_decides_a_tasks_answer_only_when_nobody_chose() {
        let (_tmp, pool, repo) = repo().await;
        let auto = household_task(t(0));
        repo.create_task(&auto).await.unwrap();
        assert_eq!(
            repo.get_task(auto.id()).await.unwrap().unwrap().answer(),
            AnswerShape::Auto
        );
        assert!(!repo
            .decide_task_answer(auto.id(), AnswerShape::Auto)
            .await
            .unwrap());
        assert!(repo
            .decide_task_answer(auto.id(), AnswerShape::Short)
            .await
            .unwrap());
        assert!(!repo
            .decide_task_answer(auto.id(), AnswerShape::Report)
            .await
            .unwrap());
        assert_eq!(
            repo.get_task(auto.id()).await.unwrap().unwrap().answer(),
            AnswerShape::Short
        );

        let chosen = household_task(t(1)).with_answer(AnswerShape::Report);
        repo.create_task(&chosen).await.unwrap();
        assert!(!repo
            .decide_task_answer(chosen.id(), AnswerShape::Short)
            .await
            .unwrap());
        assert_eq!(
            repo.get_task(chosen.id()).await.unwrap().unwrap().answer(),
            AnswerShape::Report
        );
        assert!(
            sqlx::query("UPDATE long_tasks SET answer = 'essay' WHERE id = ?")
                .bind(chosen.id())
                .execute(&pool)
                .await
                .is_err(),
            "the column accepts only the shapes the domain knows"
        );
    }

    #[tokio::test]
    async fn a_task_keeps_its_kind_and_an_unknown_one_is_refused() {
        let (_tmp, pool, repo) = repo().await;
        let task = household_task(t(0)).with_kind(LongTaskKind::Research);
        repo.create_task(&task).await.unwrap();
        let stored = repo.get_task(task.id()).await.unwrap().unwrap();
        assert_eq!(stored.kind(), LongTaskKind::Research);

        let plain = household_task(t(1));
        repo.create_task(&plain).await.unwrap();
        assert_eq!(
            repo.get_task(plain.id()).await.unwrap().unwrap().kind(),
            LongTaskKind::General
        );
        assert!(
            sqlx::query("UPDATE long_tasks SET kind = 'gossip' WHERE id = ?")
                .bind(plain.id())
                .execute(&pool)
                .await
                .is_err(),
            "the column accepts only the kinds the domain knows"
        );
    }

    async fn member(pool: &Pool<Sqlite>, id: &str) {
        sqlx::query("INSERT INTO profiles (id, display_name) VALUES (?, ?)")
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn session(pool: &Pool<Sqlite>, id: &str) {
        sqlx::query("INSERT INTO sessions (id) VALUES (?)")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    fn draft(key: &str, deps: &[&str]) -> StepDraft {
        StepDraft {
            key: key.to_string(),
            role: "researcher".to_string(),
            instructions: format!("do {key}"),
            depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
            ..StepDraft::default()
        }
    }

    async fn plan(repo: &SqliteLongTaskRepository, task: &LongTask, drafts: Vec<StepDraft>) {
        let current = repo.list_steps(task.id()).await.unwrap();
        let plan = validate_plan(task, drafts, &current).unwrap();
        repo.replace_open_steps(&plan).await.unwrap();
    }

    async fn step(repo: &SqliteLongTaskRepository, task: &LongTask, key: &str) -> PlanStep {
        repo.list_steps(task.id())
            .await
            .unwrap()
            .into_iter()
            .find(|step| step.key().as_str() == key)
            .unwrap_or_else(|| panic!("no step {key}"))
    }

    async fn count(pool: &Pool<Sqlite>, table: &str, task_id: &str) -> i64 {
        let (n,): (i64,) =
            sqlx::query_as(&format!("SELECT COUNT(*) FROM {table} WHERE task_id = ?"))
                .bind(task_id)
                .fetch_one(pool)
                .await
                .unwrap();
        n
    }

    fn note(task: &LongTask, step: Option<&str>, body: &str, at: DateTime<Utc>) -> Artifact {
        Artifact::new(
            task.id(),
            step.map(|key| StepKey::parse(key).unwrap()),
            ArtifactKind::Findings,
            "Plumber quotes",
            "three quotes, the cheapest is Otieno",
            body.to_string(),
            at,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn a_task_round_trips_every_field() {
        let (_tmp, pool, repo) = repo().await;
        member(&pool, "liz").await;
        session(&pool, "conv-1").await;
        let task = LongTask::start(
            ProfileScope::Owner("liz".into()),
            Some("liz".into()),
            "compare three plumbers",
            Some("conv-1".into()),
            LongTaskBudget::new(7, Some(t(86_400))).unwrap(),
            t(0),
        )
        .unwrap();
        repo.create_task(&task).await.unwrap();
        assert_eq!(repo.get_task(task.id()).await.unwrap(), Some(task.clone()));

        for scope in [ProfileScope::Household, ProfileScope::Guest] {
            let other = LongTask::start(
                scope,
                None,
                "tidy the photos",
                None,
                LongTaskBudget::new(2, None).unwrap(),
                t(1),
            )
            .unwrap();
            repo.create_task(&other).await.unwrap();
            assert_eq!(repo.get_task(other.id()).await.unwrap(), Some(other));
        }
        assert_eq!(repo.get_task("nobody").await.unwrap(), None);
        assert!(
            repo.create_task(&task).await.is_err(),
            "a duplicate id was stored"
        );
    }

    #[tokio::test]
    async fn every_task_state_the_domain_reaches_is_one_the_table_accepts() {
        let (_tmp, _pool, repo) = repo().await;
        for target in LongTaskState::ALL {
            let task = household_task(t(0));
            repo.create_task(&task).await.unwrap();
            let path: Vec<LongTaskState> = match target {
                LongTaskState::Planning => vec![],
                LongTaskState::AwaitingApproval => {
                    vec![LongTaskState::Running, LongTaskState::AwaitingApproval]
                }
                direct => vec![direct],
            };
            for (i, next) in path.into_iter().enumerate() {
                repo.set_task_state(task.id(), next, t(i as i64 + 1))
                    .await
                    .unwrap_or_else(|e| panic!("the table refused {next}: {e}"))
                    .unwrap();
            }
            let back = repo.get_task(task.id()).await.unwrap().unwrap();
            assert_eq!(back.state(), target);
            assert_eq!(
                back.finished_at().is_some(),
                target.is_terminal(),
                "{target}"
            );
        }
    }

    #[tokio::test]
    async fn a_refused_move_leaves_the_row_as_it_was() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        let cancelled = repo
            .set_task_state(task.id(), LongTaskState::Cancelled, t(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.finished_at(), Some(t(5)));

        let refused = repo
            .set_task_state(task.id(), LongTaskState::Running, t(6))
            .await
            .unwrap_err();
        assert_eq!(
            refused.downcast_ref::<LongTaskStateRefused>(),
            Some(&LongTaskStateRefused {
                task_id: task.id().to_string(),
                from: LongTaskState::Cancelled,
                to: LongTaskState::Running,
            })
        );
        assert_eq!(repo.get_task(task.id()).await.unwrap(), Some(cancelled));
        assert_eq!(
            repo.set_task_state("nobody", LongTaskState::Running, t(6))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_move_is_judged_against_the_stored_state_not_the_callers_copy() {
        let (_tmp, pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        // Another writer finishes it; the caller's copy still says Planning, which may run.
        sqlx::query("UPDATE long_tasks SET state = 'completed' WHERE id = ?")
            .bind(task.id())
            .execute(&pool)
            .await
            .unwrap();
        assert!(task.state().can_become(LongTaskState::Running));
        let refused = repo
            .set_task_state(task.id(), LongTaskState::Running, t(1))
            .await
            .unwrap_err();
        assert!(
            refused.downcast_ref::<LongTaskStateRefused>().is_some(),
            "{refused}"
        );

        // Control: from a stored state that may run, the same move lands.
        sqlx::query("UPDATE long_tasks SET state = 'paused' WHERE id = ?")
            .bind(task.id())
            .execute(&pool)
            .await
            .unwrap();
        let moved = repo
            .set_task_state(task.id(), LongTaskState::Running, t(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(moved.state(), LongTaskState::Running);
        assert_eq!(moved.updated_at(), t(1));
    }

    #[tokio::test]
    async fn listing_filters_by_owner_and_finish_and_breaks_ties_by_insertion() {
        let (_tmp, pool, repo) = repo().await;
        member(&pool, "liz").await;
        member(&pool, "jerry").await;
        let liz = |at| {
            LongTask::start(
                ProfileScope::Owner("liz".into()),
                None,
                "her task",
                None,
                LongTaskBudget::new(3, None).unwrap(),
                at,
            )
            .unwrap()
        };
        let first = liz(t(10));
        let second = liz(t(10));
        let older = liz(t(0));
        let jerrys = LongTask::start(
            ProfileScope::Owner("jerry".into()),
            None,
            "his task",
            None,
            LongTaskBudget::new(3, None).unwrap(),
            t(20),
        )
        .unwrap();
        let shared = household_task(t(5));
        for task in [&first, &second, &older, &jerrys, &shared] {
            repo.create_task(task).await.unwrap();
        }
        repo.set_task_state(older.id(), LongTaskState::Completed, t(11))
            .await
            .unwrap();

        let ids = |tasks: Vec<LongTask>| -> Vec<String> {
            tasks.iter().map(|task| task.id().to_string()).collect()
        };
        assert_eq!(
            ids(repo
                .list_tasks(LongTaskOwnerFilter::Member("liz"), true)
                .await
                .unwrap()),
            vec![second.id(), first.id(), older.id()],
            "newest first, and the same second falls back to insertion order"
        );
        assert_eq!(
            ids(repo
                .list_tasks(LongTaskOwnerFilter::Member("liz"), false)
                .await
                .unwrap()),
            vec![second.id(), first.id()],
            "a finished task was listed as open"
        );
        assert_eq!(
            ids(repo
                .list_tasks(LongTaskOwnerFilter::Unowned, true)
                .await
                .unwrap()),
            vec![shared.id()]
        );
        assert_eq!(
            ids(repo
                .list_tasks(LongTaskOwnerFilter::Everyone, false)
                .await
                .unwrap()),
            vec![jerrys.id(), second.id(), first.id(), shared.id()]
        );
    }

    #[tokio::test]
    async fn steps_round_trip_and_list_by_ordinal() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        let mut fetch = draft("fetch", &[]);
        fetch.inputs = vec!["art-1".into(), "art-2".into()];
        fetch.acceptance = Some("names three plumbers".into());
        let drafts = vec![
            fetch,
            draft("compare", &["fetch"]),
            draft("write", &["compare", "fetch"]),
        ];
        let plan = validate_plan(&task, drafts, &[]).unwrap();
        repo.replace_open_steps(&plan).await.unwrap();
        assert_eq!(repo.list_steps(task.id()).await.unwrap(), plan.steps());
        assert!(repo.list_steps("nobody").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn steps_with_the_same_ordinal_list_in_insertion_order() {
        let (_tmp, pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        for key in ["zeta", "alpha", "mid"] {
            sqlx::query(
                "INSERT INTO long_task_steps \
                 (task_id, step_key, ordinal, role, instructions, state) \
                 VALUES (?, ?, 1, 'researcher', 'go', 'pending')",
            )
            .bind(task.id())
            .bind(key)
            .execute(&pool)
            .await
            .unwrap();
        }
        let keys: Vec<String> = repo
            .list_steps(task.id())
            .await
            .unwrap()
            .iter()
            .map(|step| step.key().to_string())
            .collect();
        assert_eq!(keys, vec!["zeta", "alpha", "mid"]);
    }

    #[tokio::test]
    async fn a_replan_keeps_finished_and_running_steps_and_replaces_the_rest() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        plan(
            &repo,
            &task,
            vec![
                draft("done", &[]),
                draft("busy", &[]),
                draft("dropped", &[]),
                draft("retry", &[]),
            ],
        )
        .await;

        let mut done = step(&repo, &task, "done").await;
        done.start(t(1)).unwrap();
        repo.update_step(&done).await.unwrap();
        done.record(
            &StepOutcome::new(
                OutcomeKind::Completed,
                "found three",
                vec!["art-1".into()],
                None,
            ),
            t(2),
        )
        .unwrap();
        repo.update_step(&done).await.unwrap();

        let mut busy = step(&repo, &task, "busy").await;
        busy.start(t(3)).unwrap();
        repo.update_step(&busy).await.unwrap();

        let mut retry = step(&repo, &task, "retry").await;
        retry.start(t(3)).unwrap();
        repo.update_step(&retry).await.unwrap();
        retry.fail("the site was down", t(4)).unwrap();
        repo.update_step(&retry).await.unwrap();

        let mut replacement = draft("retry", &["done"]);
        replacement.instructions = "try the other site".into();
        plan(
            &repo,
            &task,
            vec![replacement, draft("report", &["retry", "busy"])],
        )
        .await;

        let steps = repo.list_steps(task.id()).await.unwrap();
        let keys: Vec<&str> = steps.iter().map(|step| step.key().as_str()).collect();
        assert_eq!(keys, vec!["done", "busy", "retry", "report"]);
        assert_eq!(steps[0], done, "a completed step was rewritten");
        assert_eq!(steps[1], busy, "a running step was rewritten");
        assert_eq!(steps[2].instructions(), "try the other site");
        assert_eq!(steps[2].state(), StepState::Pending);
        assert_eq!(steps[2].attempts(), 0);
        assert_eq!(
            steps[2].failure(),
            None,
            "the replaced step kept its old run"
        );
        assert_eq!(steps[3].ordinal(), 4);
    }

    #[tokio::test]
    async fn a_stale_replan_writes_nothing() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        plan(&repo, &task, vec![draft("fetch", &[]), draft("later", &[])]).await;
        let mut fetch = step(&repo, &task, "fetch").await;
        fetch.start(t(1)).unwrap();
        repo.update_step(&fetch).await.unwrap();

        let current = repo.list_steps(task.id()).await.unwrap();
        let stale = validate_plan(&task, vec![draft("write", &["fetch"])], &current).unwrap();
        // A household turn pre-empts `fetch` after the plan was checked.
        fetch.preempt().unwrap();
        repo.update_step(&fetch).await.unwrap();

        let refused = repo.replace_open_steps(&stale).await.unwrap_err();
        assert_eq!(
            refused
                .downcast_ref::<StalePlan>()
                .map(|stale| stale.step.as_str()),
            Some("fetch"),
            "{refused}"
        );
        let keys: Vec<String> = repo
            .list_steps(task.id())
            .await
            .unwrap()
            .iter()
            .map(|step| step.key().to_string())
            .collect();
        assert_eq!(
            keys,
            vec!["fetch", "later"],
            "the refused plan deleted or wrote a step"
        );
    }

    #[tokio::test]
    async fn a_finished_task_takes_no_replan() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        let plan = validate_plan(&task, vec![draft("a", &[])], &[]).unwrap();
        repo.set_task_state(task.id(), LongTaskState::Cancelled, t(1))
            .await
            .unwrap();
        assert!(repo.replace_open_steps(&plan).await.is_err());
        assert!(repo.list_steps(task.id()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_step_moves_only_along_the_allowed_edges() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        plan(&repo, &task, vec![draft("a", &[])]).await;

        // Pending -> Failed in one write is not an edge.
        let mut skipped_ahead = step(&repo, &task, "a").await;
        skipped_ahead.start(t(1)).unwrap();
        skipped_ahead.fail("no network", t(2)).unwrap();
        let refused = repo.update_step(&skipped_ahead).await.unwrap_err();
        assert_eq!(
            refused.downcast_ref::<StepStateRefused>(),
            Some(&StepStateRefused {
                step: "a".into(),
                from: StepState::Pending,
                to: StepState::Failed,
            })
        );
        assert_eq!(step(&repo, &task, "a").await.state(), StepState::Pending);

        // Control: the same end state, one edge at a time, lands with every run field.
        let mut a = step(&repo, &task, "a").await;
        a.start(t(1)).unwrap();
        repo.update_step(&a).await.unwrap();
        a.record(
            &StepOutcome::new(
                OutcomeKind::NeedsApproval,
                "ready to book",
                vec![],
                Some("it pays"),
            ),
            t(2),
        )
        .unwrap();
        repo.update_step(&a).await.unwrap();
        assert_eq!(step(&repo, &task, "a").await, a);
    }

    #[tokio::test]
    async fn every_step_state_the_domain_reaches_is_one_the_table_accepts() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        let keys = [
            "pending",
            "running",
            "completed",
            "failed",
            "skipped",
            "parked",
        ];
        plan(
            &repo,
            &task,
            keys.iter().map(|key| draft(key, &[])).collect(),
        )
        .await;

        for key in &keys[1..4] {
            let mut s = step(&repo, &task, key).await;
            s.start(t(1)).unwrap();
            repo.update_step(&s).await.unwrap();
        }
        let mut completed = step(&repo, &task, "completed").await;
        completed
            .record(
                &StepOutcome::new(OutcomeKind::Completed, "ok", vec![], None),
                t(2),
            )
            .unwrap();
        repo.update_step(&completed).await.unwrap();
        let mut failed = step(&repo, &task, "failed").await;
        failed.fail("broke", t(2)).unwrap();
        repo.update_step(&failed).await.unwrap();
        let mut skipped = step(&repo, &task, "skipped").await;
        skipped.skip(t(2)).unwrap();
        repo.update_step(&skipped).await.unwrap();
        let mut parked = step(&repo, &task, "parked").await;
        parked.await_approval().unwrap();
        repo.update_step(&parked).await.unwrap();

        let mut states: Vec<StepState> = repo
            .list_steps(task.id())
            .await
            .unwrap()
            .iter()
            .map(PlanStep::state)
            .collect();
        states.sort_by_key(|state| state.as_str());
        let mut every: Vec<StepState> = StepState::ALL.to_vec();
        every.sort_by_key(|state| state.as_str());
        assert_eq!(states, every);
    }

    #[tokio::test]
    async fn an_artifact_round_trips_and_its_listing_carries_no_body() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        let body = "Otieno: 4,500 KES. Wanjiru: 5,200 KES. 水 ".repeat(20);
        let artifact = note(&task, Some("fetch"), &body, t(3));
        repo.insert_artifact(&artifact).await.unwrap();

        let back = repo
            .get_artifact(task.id(), artifact.id())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(back, artifact);
        assert_eq!(back.body(), body);
        assert_eq!(
            repo.list_artifacts(task.id()).await.unwrap(),
            vec![artifact.summary_view()]
        );
    }

    #[tokio::test]
    async fn an_artifact_is_read_only_through_its_own_task() {
        let (_tmp, _pool, repo) = repo().await;
        let mine = household_task(t(0));
        let theirs = household_task(t(0));
        repo.create_task(&mine).await.unwrap();
        repo.create_task(&theirs).await.unwrap();
        let artifact = note(&theirs, None, "their findings", t(1));
        repo.insert_artifact(&artifact).await.unwrap();

        assert_eq!(
            repo.get_artifact(mine.id(), artifact.id()).await.unwrap(),
            None
        );
        assert!(repo.list_artifacts(mine.id()).await.unwrap().is_empty());
        // Control: the owning task reads it.
        assert!(repo
            .get_artifact(theirs.id(), artifact.id())
            .await
            .unwrap()
            .is_some());
        let orphan = Artifact::new(
            "no-such-task",
            None,
            ArtifactKind::Note,
            "t",
            "s",
            "b".into(),
            t(1),
        )
        .unwrap();
        assert!(
            repo.insert_artifact(&orphan).await.is_err(),
            "the foreign key let it in"
        );
    }

    #[tokio::test]
    async fn an_artifact_is_deleted_only_through_its_own_task() {
        let (_tmp, _pool, repo) = repo().await;
        let mine = household_task(t(0));
        let theirs = household_task(t(0));
        repo.create_task(&mine).await.unwrap();
        repo.create_task(&theirs).await.unwrap();
        let kept = note(&theirs, None, "kept", t(1));
        let gone = note(&theirs, None, "gone", t(2));
        repo.insert_artifact(&kept).await.unwrap();
        repo.insert_artifact(&gone).await.unwrap();

        assert!(!repo.delete_artifact(mine.id(), gone.id()).await.unwrap());
        assert!(repo.delete_artifact(theirs.id(), gone.id()).await.unwrap());
        assert!(!repo.delete_artifact(theirs.id(), gone.id()).await.unwrap());
        let left: Vec<String> = repo
            .list_artifacts(theirs.id())
            .await
            .unwrap()
            .into_iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(left, [kept.id().to_string()]);
        assert!(repo.get_task(theirs.id()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn artifacts_list_oldest_first_with_ties_in_insertion_order() {
        let (_tmp, _pool, repo) = repo().await;
        let task = household_task(t(0));
        repo.create_task(&task).await.unwrap();
        let later = note(&task, None, "later", t(9));
        let tie_a = note(&task, None, "tie a", t(5));
        let tie_b = note(&task, None, "tie b", t(5));
        for artifact in [&later, &tie_a, &tie_b] {
            repo.insert_artifact(artifact).await.unwrap();
        }
        let ids: Vec<String> = repo
            .list_artifacts(task.id())
            .await
            .unwrap()
            .into_iter()
            .map(|summary| summary.id)
            .collect();
        assert_eq!(ids, vec![tie_a.id(), tie_b.id(), later.id()]);
    }

    #[tokio::test]
    async fn deleting_a_task_cascades_to_its_steps_and_artifacts_only() {
        let (_tmp, pool, repo) = repo().await;
        let gone = household_task(t(0));
        let kept = household_task(t(0));
        for task in [&gone, &kept] {
            repo.create_task(task).await.unwrap();
            plan(&repo, task, vec![draft("a", &[]), draft("b", &["a"])]).await;
            repo.insert_artifact(&note(task, Some("a"), "body", t(1)))
                .await
                .unwrap();
        }

        assert!(repo.delete_task(gone.id()).await.unwrap());
        assert_eq!(repo.get_task(gone.id()).await.unwrap(), None);
        assert_eq!(count(&pool, "long_task_steps", gone.id()).await, 0);
        assert_eq!(count(&pool, "long_task_artifacts", gone.id()).await, 0);
        // Control: the other task lost nothing, so the counts above are the cascade's doing.
        assert_eq!(count(&pool, "long_task_steps", kept.id()).await, 2);
        assert_eq!(count(&pool, "long_task_artifacts", kept.id()).await, 1);
        assert!(!repo.delete_task(gone.id()).await.unwrap());
    }

    #[tokio::test]
    async fn a_departed_member_takes_their_tasks_and_a_deleted_conversation_does_not() {
        let (_tmp, pool, repo) = repo().await;
        member(&pool, "liz").await;
        session(&pool, "conv-1").await;
        let hers = LongTask::start(
            ProfileScope::Owner("liz".into()),
            None,
            "her task",
            Some("conv-1".into()),
            LongTaskBudget::new(3, None).unwrap(),
            t(0),
        )
        .unwrap();
        let shared = LongTask::start(
            ProfileScope::Household,
            None,
            "shared task",
            Some("conv-1".into()),
            LongTaskBudget::new(3, None).unwrap(),
            t(0),
        )
        .unwrap();
        repo.create_task(&hers).await.unwrap();
        repo.create_task(&shared).await.unwrap();
        plan(&repo, &hers, vec![draft("a", &[])]).await;

        sqlx::query("DELETE FROM sessions WHERE id = 'conv-1'")
            .execute(&pool)
            .await
            .unwrap();
        let orphaned = repo.get_task(shared.id()).await.unwrap().unwrap();
        assert_eq!(orphaned.origin_session_id(), None);
        assert_eq!(orphaned.state(), LongTaskState::Planning);

        sqlx::query("DELETE FROM profiles WHERE id = 'liz'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(repo.get_task(hers.id()).await.unwrap(), None);
        assert_eq!(count(&pool, "long_task_steps", hers.id()).await, 0);
        assert!(repo.get_task(shared.id()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn the_table_refuses_a_scope_that_disagrees_with_its_owner() {
        let (_tmp, pool, _repo) = repo().await;
        member(&pool, "liz").await;
        let insert = |scope: &'static str, owner: Option<&'static str>| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO long_tasks (id, owner_profile_id, scope, goal, state, max_steps, \
                     created_at, updated_at) \
                     VALUES (lower(hex(randomblob(8))), ?, ?, 'g', 'planning', 3, 'x', 'x')",
                )
                .bind(owner)
                .bind(scope)
                .execute(&pool)
                .await
            }
        };
        assert!(insert("owner", None).await.is_err());
        assert!(insert("guest", Some("liz")).await.is_err());
        assert!(insert("member", Some("liz")).await.is_err());
        // Controls: the agreeing shapes are stored.
        assert!(insert("owner", Some("liz")).await.is_ok());
        assert!(insert("household", Some("liz")).await.is_ok());
        assert!(insert("household", None).await.is_ok());
        assert!(insert("guest", None).await.is_ok());
    }
}
