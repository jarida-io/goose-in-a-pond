//! Middleware blocking protected API routes until onboarding completes.

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};

use serde_json::json;
use std::sync::Arc;

use crate::AppState;

use pond_core::user_data::domain::onboarding::OnboardingStep;
use pond_core::user_data::services::onboarding::OnboardingService;

/// Responds 403 `onboarding_required` until onboarding completes.
pub async fn require_onboarding_complete(
    State(state): State<Arc<AppState>>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, Response> {
    if state.skip_onboarding {
        return Ok(next.run(req).await);
    }

    let service = OnboardingService::new(state.onboarding_repo.clone());

    let step: Option<OnboardingStep> = service.status().await;

    match step {
        Some(OnboardingStep::Completed) => Ok(next.run(req).await),

        None | Some(_) => Err((
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "onboarding_required",
                "message": "Complete onboarding before using this feature"
            })),
        )
            .into_response()),
    }
}
