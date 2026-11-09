//! A POST form waits for its member's own phone: only their yes sends it, once, and another
//! member's phone never sees it.

#[path = "support/ride_phone.rs"]
mod ride_phone;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

use anyhow::Result;
use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use pond_core::shared::domain::web_form::{FormSubmission, SubmissionState};
use pond_core::shared::ports::web_form::{FormSender, FormSubmissionStore, SendFailure, SentPage};
use pond_core::shared::services::web_form_confirm::{self, FormConfirmations, FormRequest};
use ride_phone::{member_and_phone, member_with_phone, phone_of_nobody, send};

#[derive(Default)]
struct Store(Mutex<Vec<FormSubmission>>);

#[async_trait]
impl FormSubmissionStore for Store {
    async fn create(&self, submission: &FormSubmission) -> Result<()> {
        self.0.lock().unwrap().push(submission.clone());
        Ok(())
    }
    async fn get(&self, id: &str) -> Result<Option<FormSubmission>> {
        Ok(self.0.lock().unwrap().iter().find(|s| s.id == id).cloned())
    }
    async fn recent_for(&self, owner: &str, since: DateTime<Utc>) -> Result<Vec<FormSubmission>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.owner == owner && s.asked_at >= since)
            .cloned()
            .collect())
    }
    async fn advance(
        &self,
        id: &str,
        from: &SubmissionState,
        to: &SubmissionState,
        at: DateTime<Utc>,
    ) -> Result<bool> {
        let mut all = self.0.lock().unwrap();
        match all
            .iter_mut()
            .find(|s| s.id == id && s.state.name() == from.name())
        {
            Some(s) => {
                s.state = to.clone();
                s.decided_at.get_or_insert(at);
                Ok(true)
            }
            None => Ok(false),
        }
    }
    async fn prune(&self, _before: DateTime<Utc>) -> Result<u64> {
        Ok(0)
    }
}

/// A stand-in site: counts what it was sent, per member.
#[derive(Default)]
struct Site(Mutex<Vec<String>>);

impl Site {
    fn sent_for(&self, owner: &str) -> usize {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|o| *o == owner)
            .count()
    }
}

#[async_trait]
impl FormSender for Site {
    async fn send(&self, submission: &FormSubmission) -> Result<SentPage, SendFailure> {
        self.0.lock().unwrap().push(submission.owner.clone());
        Ok(SentPage {
            status: 200,
            address: "https://cars.example/thanks".into(),
            title: "Message sent".into(),
        })
    }
}

/// One set of forms for the whole binary: `web_form_confirm::install` is process-wide.
static SITE: LazyLock<Arc<Site>> = LazyLock::new(|| {
    let site = Arc::new(Site::default());
    web_form_confirm::install(Arc::new(FormConfirmations::new(
        Arc::new(Store::default()),
        site.clone(),
    )));
    site
});

static ASKED: AtomicUsize = AtomicUsize::new(0);

async fn pond() -> ride_phone::Pond {
    LazyLock::force(&SITE);
    ride_phone::pond().await
}

async fn ask(owner: &str) -> String {
    ASKED.fetch_add(1, Ordering::SeqCst);
    web_form_confirm::installed()
        .unwrap()
        .ask(
            FormRequest {
                owner: owner.into(),
                page: "https://cars.example/used/9".into(),
                action: "https://cars.example/enquire".into(),
                values: vec![("message".into(), "Still for sale?".into())],
                depth: 1,
                session_id: None,
            },
            Utc::now(),
        )
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn the_members_phone_reads_and_confirms_their_form_once() {
    let p = pond().await;
    let (liz, phone) = member_and_phone(&p, "liz").await;
    let id = ask(&liz).await;

    let (status, shown) = send(
        &p.lan,
        Method::GET,
        &format!("/api/v1/web-forms/{id}"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shown}");
    assert_eq!(shown["state"], "waiting");
    assert_eq!(shown["site"], "cars.example");
    assert_eq!(shown["fields"][0]["value"], "Still for sale?");
    assert_eq!(SITE.sent_for(&liz), 0, "reading the form sent it");

    let (status, sent) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/web-forms/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sent}");
    assert_eq!(sent["state"], "sent");
    assert_eq!(sent["result"]["title"], "Message sent");

    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/web-forms/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "a second yes was accepted");
    assert_eq!(SITE.sent_for(&liz), 1);
}

#[tokio::test]
async fn another_members_phone_or_none_cannot_touch_the_form() {
    let p = pond().await;
    let (amina, _) = member_and_phone(&p, "amina").await;
    let other = member_with_phone(&p, "baraka").await;
    let nobodys = phone_of_nobody(&p, "kiosk").await;
    let id = ask(&amina).await;

    for (method, path) in [
        (Method::GET, format!("/api/v1/web-forms/{id}")),
        (Method::POST, format!("/api/v1/web-forms/{id}/confirm")),
        (Method::POST, format!("/api/v1/web-forms/{id}/decline")),
    ] {
        let (status, _) = send(&p.lan, method.clone(), &path, Some(&other), None).await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "{path} answered another member"
        );
        let (status, _) = send(&p.lan, method, &path, Some(&nobodys), None).await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "{path} answered a phone of nobody's"
        );
    }
    assert_eq!(SITE.sent_for(&amina), 0);
}

#[tokio::test]
async fn a_declined_form_is_never_sent() {
    let p = pond().await;
    let (zawadi, phone) = member_and_phone(&p, "zawadi").await;
    let id = ask(&zawadi).await;
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/web-forms/{id}/decline"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &p.lan,
        Method::POST,
        &format!("/api/v1/web-forms/{id}/confirm"),
        Some(&phone),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(SITE.sent_for(&zawadi), 0);
}
