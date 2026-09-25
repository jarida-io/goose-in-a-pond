//! Port definition for onboarding persistence.

use crate::user_data::domain::onboarding::OnboardingStep;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait OnboardingRepository: Send + Sync {
    async fn get_current_step(&self) -> Option<OnboardingStep>;
    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()>;
    /// Reset onboarding state to allow starting from scratch.
    async fn reset(&self) -> anyhow::Result<()>;

    /// Whether onboarding is complete; `Err` on a failed read, since the public-route
    /// allowlist keys on this. No default body on purpose: access must narrow on failure.
    async fn is_complete(&self) -> anyhow::Result<bool>;
}

#[async_trait::async_trait]
impl OnboardingRepository for Arc<dyn OnboardingRepository + Send + Sync> {
    async fn get_current_step(&self) -> Option<OnboardingStep> {
        self.as_ref().get_current_step().await
    }

    async fn save_step(&self, step: OnboardingStep) -> anyhow::Result<()> {
        self.as_ref().save_step(step).await
    }

    async fn reset(&self) -> anyhow::Result<()> {
        self.as_ref().reset().await
    }

    /// Must stay forwarded: `AppState` calls through this `Arc` impl, so if the trait method
    /// ever gets a default, dropping this arm would silently bypass the adapter.
    async fn is_complete(&self) -> anyhow::Result<bool> {
        self.as_ref().is_complete().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnreadableRepo;

    #[async_trait::async_trait]
    impl OnboardingRepository for UnreadableRepo {
        async fn get_current_step(&self) -> Option<OnboardingStep> {
            None
        }
        async fn save_step(&self, _: OnboardingStep) -> anyhow::Result<()> {
            Ok(())
        }
        async fn reset(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn is_complete(&self) -> anyhow::Result<bool> {
            Err(anyhow::anyhow!("database is locked"))
        }
    }

    /// Guards the forwarding arm above: "not onboarded" would make onboarding writes public.
    #[tokio::test]
    async fn a_read_failure_survives_the_arc_rather_than_becoming_not_onboarded() {
        let repo: Arc<dyn OnboardingRepository + Send + Sync> = Arc::new(UnreadableRepo);
        assert!(
            repo.is_complete().await.is_err(),
            "the Arc impl swallowed the read failure and answered 'not onboarded'"
        );
    }
}
