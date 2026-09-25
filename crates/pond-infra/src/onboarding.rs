//! SQLx implementation of the OnboardingRepository port.

use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::ports::onboarding::OnboardingRepository;
use sqlx::{Pool, Sqlite};
use std::str::FromStr;

pub struct SqlxOnboardingRepository {
    pool: Pool<Sqlite>,
}

impl SqlxOnboardingRepository {
    pub fn new(pool: Pool<Sqlite>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl OnboardingRepository for SqlxOnboardingRepository {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT current_step FROM onboarding_state WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .ok()??;

        OnboardingStep::from_str(row.0.as_str()).ok()
    }

    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()> {
        let step_str = step.to_string();

        sqlx::query(
            r#"
            INSERT INTO onboarding_state (id, current_step)
            VALUES (1, ?)
            ON CONFLICT(id)
            DO UPDATE SET
                current_step = excluded.current_step,
                updated_at = datetime('now')
            "#,
        )
        .bind(step_str)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    async fn reset(&self) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM onboarding_state WHERE id = 1")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Like `get_current_step` but propagates DB errors: the auth allowlist opens onboarding
    /// write routes while not onboarded, so a failed read must not look like "not started".
    async fn is_complete(&self) -> anyhow::Result<bool> {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT current_step FROM onboarding_state WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(match row {
            Some((step,)) => matches!(
                OnboardingStep::from_str(step.as_str()),
                Ok(OnboardingStep::Completed)
            ),
            None => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use tempfile::tempdir;

    #[tokio::test]
    async fn onboarding_repo_persists_state() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();

        let repo = SqlxOnboardingRepository::new(db.system.clone());

        repo.save_step(OnboardingStep::Basics).await.unwrap();

        let step = repo.get_current_step().await;

        assert_eq!(step, Some(OnboardingStep::Basics));
    }

    #[tokio::test]
    async fn is_complete_reports_a_read_failure_instead_of_answering_not_onboarded() {
        let tmp = tempdir().unwrap();
        let db = Database::init(tmp.path()).await.unwrap();
        let repo = SqlxOnboardingRepository::new(db.system.clone());

        repo.save_step(OnboardingStep::Completed).await.unwrap();
        assert!(repo.is_complete().await.unwrap());

        sqlx::query("DROP TABLE onboarding_state")
            .execute(&db.system)
            .await
            .unwrap();

        assert!(
            repo.get_current_step().await.is_none(),
            "precondition: the old path reports an unreadable table as 'not started', \
             which is the widest possible answer to the question the auth gate asks"
        );
        assert!(
            repo.is_complete().await.is_err(),
            "an unreadable onboarding table must be an error, not 'not onboarded'"
        );
    }
}
