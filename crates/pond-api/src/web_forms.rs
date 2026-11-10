//! A member's own phone answers a POST form the pond was asked to send: it reads what would be
//! sent, then confirms or declines. Only a yes sends it, once; the member is the one the paired
//! phone belongs to, never a request field.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use pond_core::security::ports::policy::Principal;
use pond_core::shared::domain::web_form::{FormSubmission, SubmissionState};
use pond_core::shared::services::web_form_confirm::{self, ConfirmError, FormConfirmations};
use serde_json::{json, Value};

use crate::rides::{phone_member, Refusal};
use crate::AppState;

fn refuse(status: StatusCode, message: &str) -> Refusal {
    (status, Json(json!({ "error": message })))
}

fn confirmations() -> Result<Arc<FormConfirmations>, Refusal> {
    web_form_confirm::installed().ok_or_else(|| {
        refuse(
            StatusCode::SERVICE_UNAVAILABLE,
            "sending forms is not set up on this pond",
        )
    })
}

async fn member(state: &AppState, principal: Option<&Principal>) -> Result<String, Refusal> {
    phone_member(
        state,
        principal,
        "a form is confirmed from the member's own paired phone",
    )
    .await
}

fn refusal(error: ConfirmError) -> Refusal {
    match error {
        ConfirmError::NotFound => refuse(StatusCode::NOT_FOUND, "no such form"),
        ConfirmError::TimedOut => refuse(StatusCode::GONE, &error.to_string()),
        ConfirmError::AlreadyAnswered(_) => refuse(StatusCode::CONFLICT, &error.to_string()),
        ConfirmError::Store(e) => {
            tracing::warn!(error = %format!("{e:#}"), "web forms: the store failed");
            refuse(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not read the form just now",
            )
        }
    }
}

/// A form as the phone shows it: where it goes, what it sends, and how it stands.
pub fn view(submission: &FormSubmission) -> Value {
    let (result, message) = match &submission.state {
        SubmissionState::Sent {
            status,
            address,
            title,
        } => (
            Some(json!({ "status": status, "address": address, "title": title })),
            None,
        ),
        SubmissionState::Failed { why } => (None, Some(why.as_str())),
        _ => (None, None),
    };
    json!({
        "id": submission.id,
        "site": submission.site(),
        "page": submission.page,
        "action": submission.action,
        "fields": submission
            .values
            .iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect::<Vec<_>>(),
        "state": submission.state.name(),
        "asked_at": submission.asked_at,
        "answer_by": submission.answer_by(),
        "decided_at": submission.decided_at,
        "result": result,
        "message": message,
    })
}

/// `GET /api/v1/web-forms/{id}`
pub async fn get(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refusal> {
    let member = member(&state, principal.as_deref()).await?;
    let submission = confirmations()?
        .get(&id, &member, chrono::Utc::now())
        .await
        .map_err(refusal)?;
    Ok(Json(view(&submission)))
}

/// `POST /api/v1/web-forms/{id}/confirm` — the member's yes: the form is sent now. 502 when it
/// could not be sent, or its answer was lost; `message` says which.
pub async fn confirm(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<Value>), Refusal> {
    let member = member(&state, principal.as_deref()).await?;
    let submission = confirmations()?
        .confirm(&id, &member, chrono::Utc::now())
        .await
        .map_err(refusal)?;
    let status = match submission.state {
        SubmissionState::Failed { .. } => StatusCode::BAD_GATEWAY,
        _ => StatusCode::OK,
    };
    Ok((status, Json(view(&submission))))
}

/// `POST /api/v1/web-forms/{id}/decline` — the member's no: the form is never sent.
pub async fn decline(
    State(state): State<Arc<AppState>>,
    principal: Option<Extension<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, Refusal> {
    let member = member(&state, principal.as_deref()).await?;
    confirmations()?
        .decline(&id, &member, chrono::Utc::now())
        .await
        .map_err(refusal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_form_shows_its_site_fields_and_answer() {
        let at = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let submission = FormSubmission {
            id: "f1".into(),
            owner: "jerry".into(),
            page: "https://www.cars.example/used/9".into(),
            action: "https://www.cars.example/enquire".into(),
            values: vec![("message".into(), "Still for sale?".into())],
            depth: 1,
            session_id: None,
            state: SubmissionState::Sent {
                status: 200,
                address: "https://www.cars.example/thanks".into(),
                title: "Message sent".into(),
            },
            asked_at: at,
            decided_at: Some(at),
        };
        let shown = view(&submission);
        assert_eq!(shown["site"], "cars.example");
        assert_eq!(shown["state"], "sent");
        assert_eq!(shown["fields"][0]["name"], "message");
        assert_eq!(shown["result"]["title"], "Message sent");
        assert!(shown["message"].is_null());
        assert_eq!(shown["answer_by"], json!(submission.answer_by()));
    }

    #[test]
    fn each_refusal_has_its_status() {
        assert_eq!(refusal(ConfirmError::NotFound).0, StatusCode::NOT_FOUND);
        assert_eq!(refusal(ConfirmError::TimedOut).0, StatusCode::GONE);
        assert_eq!(
            refusal(ConfirmError::AlreadyAnswered("sent")).0,
            StatusCode::CONFLICT
        );
    }
}
