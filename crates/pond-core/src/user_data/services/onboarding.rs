//! Application service for onboarding use cases.

use crate::user_data::domain::onboarding::OnboardingStep;
use crate::user_data::ports::onboarding::OnboardingRepository;
use anyhow::Result;

pub struct OnboardingService<R: OnboardingRepository> {
    repo: R,
}

impl<R: OnboardingRepository> OnboardingService<R> {
    pub fn new(repo: R) -> Self {
        Self { repo }
    }

    pub async fn start(&self) -> Result<()> {
        self.repo.save_step(OnboardingStep::Welcome).await?;
        Ok(())
    }

    pub async fn advance(&self) -> Result<()> {
        if let Some(current) = self.repo.get_current_step().await {
            let next = current.next();
            self.repo.save_step(next).await?;
        }
        Ok(())
    }

    /// Record reaching `step`, never moving progress backwards; returns the step now persisted.
    pub async fn advance_to(&self, step: OnboardingStep) -> Result<OnboardingStep> {
        let target = match self.repo.get_current_step().await {
            Some(current) if current.position() >= step.position() => current,
            _ => step,
        };
        self.repo.save_step(target).await?;
        Ok(target)
    }

    pub async fn status(&self) -> Option<OnboardingStep> {
        self.repo.get_current_step().await
    }

    pub async fn reset(&self) -> Result<()> {
        self.repo.reset().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, PoisonError};

    fn mutex_error<T>(err: PoisonError<T>) -> anyhow::Error {
        anyhow::anyhow!("Mutex poisoned: {}", err)
    }

    struct MockRepo {
        step: Mutex<Option<OnboardingStep>>,
    }

    impl MockRepo {
        fn new() -> Self {
            Self {
                step: Mutex::new(None),
            }
        }
    }

    #[async_trait::async_trait]
    impl OnboardingRepository for MockRepo {
        async fn get_current_step(&self) -> Option<OnboardingStep> {
            self.step.lock().ok().map(|guard| *guard).flatten()
        }

        async fn save_step(&self, step: OnboardingStep) -> Result<()> {
            let mut guard = self.step.lock().map_err(mutex_error)?;
            *guard = Some(step);
            Ok(())
        }

        async fn reset(&self) -> Result<()> {
            let mut guard = self.step.lock().map_err(mutex_error)?;
            *guard = None;
            Ok(())
        }

        async fn is_complete(&self) -> Result<bool> {
            let guard = self.step.lock().map_err(mutex_error)?;
            Ok(matches!(*guard, Some(OnboardingStep::Completed)))
        }
    }

    #[tokio::test]
    async fn start_sets_initial_step() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await?;
        assert_eq!(service.status().await, Some(OnboardingStep::Welcome));
        Ok(())
    }

    #[tokio::test]
    async fn status_is_none_before_start() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        assert_eq!(service.status().await, None);
        Ok(())
    }

    #[tokio::test]
    async fn advance_progresses_step() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await?;
        service.advance().await?;
        assert_eq!(service.status().await, Some(OnboardingStep::Basics));
        Ok(())
    }

    #[tokio::test]
    async fn advance_through_all_steps_reaches_completed() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await?;
        // 9 advances: Welcome→Basics→Location→Accessibility→Personality
        //             →GooseIdentity→WakeWord→Model→Extensions→Completed
        for _ in 0..9 {
            service.advance().await?;
        }
        assert_eq!(service.status().await, Some(OnboardingStep::Completed));
        Ok(())
    }

    #[tokio::test]
    async fn advance_stays_at_completed() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await?;
        for _ in 0..10 {
            service.advance().await?;
        }
        assert_eq!(service.status().await, Some(OnboardingStep::Completed));
        Ok(())
    }

    #[tokio::test]
    async fn advance_before_start_is_noop() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.advance().await?;
        assert_eq!(service.status().await, None);
        Ok(())
    }

    #[tokio::test]
    async fn status_is_not_complete_mid_flow() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await?;
        assert_ne!(service.status().await, Some(OnboardingStep::Completed));
        Ok(())
    }

    #[tokio::test]
    async fn status_is_complete_after_all_steps() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await?;
        for _ in 0..9 {
            service.advance().await?;
        }
        assert_eq!(service.status().await, Some(OnboardingStep::Completed));
        Ok(())
    }

    #[tokio::test]
    async fn reset_clears_state() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.start().await?;
        service.advance().await?;
        service.reset().await?;
        assert_eq!(service.status().await, None);
        Ok(())
    }

    #[tokio::test]
    async fn advance_to_records_step_from_scratch() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        let saved = service.advance_to(OnboardingStep::WakeWord).await?;
        assert_eq!(saved, OnboardingStep::WakeWord);
        assert_eq!(service.status().await, Some(OnboardingStep::WakeWord));
        Ok(())
    }

    #[tokio::test]
    async fn advance_to_moves_forward() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.advance_to(OnboardingStep::Basics).await?;
        let saved = service.advance_to(OnboardingStep::Model).await?;
        assert_eq!(saved, OnboardingStep::Model);
        assert_eq!(service.status().await, Some(OnboardingStep::Model));
        Ok(())
    }

    #[tokio::test]
    async fn advance_to_never_regresses() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.advance_to(OnboardingStep::Model).await?;
        let saved = service.advance_to(OnboardingStep::Basics).await?;
        assert_eq!(saved, OnboardingStep::Model);
        assert_eq!(service.status().await, Some(OnboardingStep::Model));
        Ok(())
    }

    #[tokio::test]
    async fn advance_to_same_step_is_idempotent() -> Result<()> {
        let service = OnboardingService::new(MockRepo::new());
        service.advance_to(OnboardingStep::Location).await?;
        let saved = service.advance_to(OnboardingStep::Location).await?;
        assert_eq!(saved, OnboardingStep::Location);
        assert_eq!(service.status().await, Some(OnboardingStep::Location));
        Ok(())
    }
}
