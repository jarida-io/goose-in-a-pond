//! POST forms confirmed on the member's own phone. A read page's POST form is asked for, kept
//! waiting, and sent only on the member's yes, at most once. A no, or no answer within
//! [`ANSWER_WITHIN_MINUTES`], leaves it unsent, and the conversation hears which.

use std::sync::{Arc, OnceLock};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};

use crate::shared::domain::web_form::{
    FormSubmission, SubmissionState, ANSWER_WITHIN_MINUTES, KEPT_FOR_HOURS,
};
use crate::shared::ports::web_form::{FormSender, FormSubmissionStore, SendFailure};

#[derive(Debug, thiserror::Error)]
pub enum ConfirmError {
    /// No such form, or another member's: whose forms exist is no business of this phone.
    #[error("no such form")]
    NotFound,
    #[error("the form waited {ANSWER_WITHIN_MINUTES} minutes for an answer and was not sent")]
    TimedOut,
    #[error("the form was already answered ({0})")]
    AlreadyAnswered(&'static str),
    #[error(transparent)]
    Store(#[from] anyhow::Error),
}

pub struct FormConfirmations {
    store: Arc<dyn FormSubmissionStore>,
    sender: Arc<dyn FormSender>,
}

/// What a form asks to send, before it is kept.
pub struct FormRequest {
    pub owner: String,
    pub page: String,
    pub action: String,
    pub values: Vec<(String, String)>,
    pub depth: u8,
    pub session_id: Option<String>,
}

impl FormConfirmations {
    pub fn new(store: Arc<dyn FormSubmissionStore>, sender: Arc<dyn FormSender>) -> Self {
        Self { store, sender }
    }

    /// Keeps a new form, waiting for its owner's answer. Forms past their keeping go first.
    pub async fn ask(&self, request: FormRequest, now: DateTime<Utc>) -> Result<FormSubmission> {
        if let Err(e) = self
            .store
            .prune(now - Duration::hours(KEPT_FOR_HOURS))
            .await
        {
            tracing::warn!(target: "giap::trace", kind = "web_form_prune_failed", error = %e);
        }
        let submission = FormSubmission {
            id: uuid::Uuid::new_v4().to_string(),
            owner: request.owner,
            page: request.page,
            action: request.action,
            values: request.values,
            depth: request.depth,
            session_id: request.session_id,
            state: SubmissionState::Waiting,
            asked_at: now,
            decided_at: None,
        };
        self.store.create(&submission).await?;
        Ok(submission)
    }

    /// The member's own form, as it stands at `now`.
    pub async fn get(
        &self,
        id: &str,
        member: &str,
        now: DateTime<Utc>,
    ) -> Result<FormSubmission, ConfirmError> {
        let submission = self
            .store
            .get(id)
            .await?
            .filter(|s| s.owner == member)
            .ok_or(ConfirmError::NotFound)?;
        Ok(self.settle(submission, now).await?)
    }

    /// The owner's newest form sending exactly `values` to `action`, while it is kept.
    pub async fn latest(
        &self,
        owner: &str,
        action: &str,
        values: &[(String, String)],
        now: DateTime<Utc>,
    ) -> Result<Option<FormSubmission>> {
        let recent = self
            .store
            .recent_for(owner, now - Duration::hours(KEPT_FOR_HOURS))
            .await?;
        match recent.into_iter().find(|s| s.same_request(action, values)) {
            Some(s) => Ok(Some(self.settle(s, now).await?)),
            None => Ok(None),
        }
    }

    /// The member's yes: the form is sent now, once, and what the site answered is kept.
    pub async fn confirm(
        &self,
        id: &str,
        member: &str,
        now: DateTime<Utc>,
    ) -> Result<FormSubmission, ConfirmError> {
        let submission = self.waiting(id, member, now).await?;
        let claimed = self
            .store
            .advance(
                id,
                &SubmissionState::Waiting,
                &SubmissionState::Sending,
                now,
            )
            .await?;
        if !claimed {
            return Err(self.already(id, member, now).await);
        }
        let outcome = match self.sender.send(&submission).await {
            Ok(page) => SubmissionState::Sent {
                status: page.status,
                address: page.address,
                title: page.title,
            },
            Err(SendFailure::NotSent(why)) => SubmissionState::Failed {
                why: format!("{why} Nothing was sent."),
            },
            Err(SendFailure::AnswerLost(why)) => SubmissionState::Failed {
                why: format!("{why} The site may have received the form."),
            },
        };
        tracing::info!(
            target: "giap::trace",
            kind = "web_form_sent",
            outcome = outcome.name(),
            status = match &outcome { SubmissionState::Sent { status, .. } => *status, _ => 0 },
        );
        self.store
            .advance(id, &SubmissionState::Sending, &outcome, Utc::now())
            .await?;
        self.get(id, member, Utc::now()).await
    }

    /// The member's no: the form is never sent.
    pub async fn decline(
        &self,
        id: &str,
        member: &str,
        now: DateTime<Utc>,
    ) -> Result<FormSubmission, ConfirmError> {
        self.waiting(id, member, now).await?;
        let declined = self
            .store
            .advance(
                id,
                &SubmissionState::Waiting,
                &SubmissionState::Declined,
                now,
            )
            .await?;
        if !declined {
            return Err(self.already(id, member, now).await);
        }
        self.get(id, member, now).await
    }

    /// Ends a form that never reached the member's phone, so it doesn't wait for a yes.
    pub async fn abandon(&self, id: &str, why: &str, now: DateTime<Utc>) -> Result<()> {
        let failed = SubmissionState::Failed { why: why.into() };
        self.store
            .advance(id, &SubmissionState::Waiting, &failed, now)
            .await?;
        Ok(())
    }

    async fn waiting(
        &self,
        id: &str,
        member: &str,
        now: DateTime<Utc>,
    ) -> Result<FormSubmission, ConfirmError> {
        let submission = self.get(id, member, now).await?;
        match submission.state {
            SubmissionState::Waiting => Ok(submission),
            SubmissionState::TimedOut => Err(ConfirmError::TimedOut),
            ref other => Err(ConfirmError::AlreadyAnswered(other.name())),
        }
    }

    async fn already(&self, id: &str, member: &str, now: DateTime<Utc>) -> ConfirmError {
        match self.get(id, member, now).await {
            Ok(s) if s.state == SubmissionState::TimedOut => ConfirmError::TimedOut,
            Ok(s) => ConfirmError::AlreadyAnswered(s.state.name()),
            Err(e) => e,
        }
    }

    /// Marks a form that waited too long as timed out, and one a restart cut off mid-send as
    /// failed, so every reader sees the same end.
    async fn settle(
        &self,
        submission: FormSubmission,
        now: DateTime<Utc>,
    ) -> Result<FormSubmission> {
        let (from, to, at) = if submission.lapsed(now) {
            (
                SubmissionState::Waiting,
                SubmissionState::TimedOut,
                submission.answer_by(),
            )
        } else if submission.cut_off(now) {
            (
                SubmissionState::Sending,
                SubmissionState::Failed {
                    why: "The pond stopped while sending the form. The site may have received it."
                        .into(),
                },
                now,
            )
        } else {
            return Ok(submission);
        };
        self.store.advance(&submission.id, &from, &to, at).await?;
        Ok(self.store.get(&submission.id).await?.unwrap_or(submission))
    }
}

static INSTALLED: OnceLock<Arc<FormConfirmations>> = OnceLock::new();

/// Installed once at startup by each binary that runs tools or serves the phone's routes.
pub fn install(confirmations: Arc<FormConfirmations>) {
    let _ = INSTALLED.set(confirmations);
}

pub fn installed() -> Option<Arc<FormConfirmations>> {
    INSTALLED.get().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::ports::web_form::SentPage;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryStore(Mutex<Vec<FormSubmission>>);

    #[async_trait]
    impl FormSubmissionStore for MemoryStore {
        async fn create(&self, submission: &FormSubmission) -> Result<()> {
            self.0.lock().unwrap().push(submission.clone());
            Ok(())
        }
        async fn get(&self, id: &str) -> Result<Option<FormSubmission>> {
            Ok(self.0.lock().unwrap().iter().find(|s| s.id == id).cloned())
        }
        async fn recent_for(
            &self,
            owner: &str,
            since: DateTime<Utc>,
        ) -> Result<Vec<FormSubmission>> {
            let mut found: Vec<FormSubmission> = self
                .0
                .lock()
                .unwrap()
                .iter()
                .filter(|s| s.owner == owner && s.asked_at >= since)
                .cloned()
                .collect();
            found.sort_by(|a, b| b.asked_at.cmp(&a.asked_at));
            Ok(found)
        }
        async fn advance(
            &self,
            id: &str,
            from: &SubmissionState,
            to: &SubmissionState,
            at: DateTime<Utc>,
        ) -> Result<bool> {
            let mut all = self.0.lock().unwrap();
            let Some(s) = all
                .iter_mut()
                .find(|s| s.id == id && s.state.name() == from.name())
            else {
                return Ok(false);
            };
            s.state = to.clone();
            s.decided_at.get_or_insert(at);
            Ok(true)
        }
        async fn prune(&self, before: DateTime<Utc>) -> Result<u64> {
            let mut all = self.0.lock().unwrap();
            let had = all.len();
            all.retain(|s| s.asked_at >= before);
            Ok((had - all.len()) as u64)
        }
    }

    struct Sender {
        sent: AtomicUsize,
        answer: std::result::Result<SentPage, SendFailure>,
    }

    impl Sender {
        fn answering(answer: std::result::Result<SentPage, SendFailure>) -> Arc<Self> {
            Arc::new(Self {
                sent: AtomicUsize::new(0),
                answer,
            })
        }
    }

    #[async_trait]
    impl FormSender for Sender {
        async fn send(
            &self,
            _submission: &FormSubmission,
        ) -> std::result::Result<SentPage, SendFailure> {
            self.sent.fetch_add(1, Ordering::SeqCst);
            self.answer.clone()
        }
    }

    fn ok_page() -> std::result::Result<SentPage, SendFailure> {
        Ok(SentPage {
            status: 200,
            address: "https://cars.example/results?id=9".into(),
            title: "3 cars found".into(),
        })
    }

    fn request() -> FormRequest {
        FormRequest {
            owner: "jerry".into(),
            page: "https://cars.example/".into(),
            action: "https://cars.example/search".into(),
            values: vec![("q".into(), "forester".into())],
            depth: 0,
            session_id: Some("s1".into()),
        }
    }

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap() + Duration::minutes(minutes)
    }

    fn service(sender: Arc<Sender>) -> FormConfirmations {
        FormConfirmations::new(Arc::new(MemoryStore::default()), sender)
    }

    #[tokio::test]
    async fn a_yes_sends_the_form_once_and_keeps_the_answer() {
        let sender = Sender::answering(ok_page());
        let forms = service(sender.clone());
        let asked = forms.ask(request(), at(0)).await.unwrap();
        let sent = forms.confirm(&asked.id, "jerry", at(1)).await.unwrap();
        assert_eq!(
            sent.state,
            SubmissionState::Sent {
                status: 200,
                address: "https://cars.example/results?id=9".into(),
                title: "3 cars found".into()
            }
        );
        let again = forms.confirm(&asked.id, "jerry", at(2)).await.unwrap_err();
        assert!(
            matches!(again, ConfirmError::AlreadyAnswered("sent")),
            "{again:?}"
        );
        assert_eq!(sender.sent.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn another_members_form_reads_as_missing() {
        let forms = service(Sender::answering(ok_page()));
        let asked = forms.ask(request(), at(0)).await.unwrap();
        for result in [
            forms.get(&asked.id, "amina", at(1)).await,
            forms.confirm(&asked.id, "amina", at(1)).await,
            forms.decline(&asked.id, "amina", at(1)).await,
        ] {
            assert!(matches!(result, Err(ConfirmError::NotFound)), "{result:?}");
        }
    }

    #[tokio::test]
    async fn a_no_or_no_answer_sends_nothing() {
        let sender = Sender::answering(ok_page());
        let forms = service(sender.clone());
        let declined = forms.ask(request(), at(0)).await.unwrap();
        let after = forms.decline(&declined.id, "jerry", at(1)).await.unwrap();
        assert_eq!(after.state, SubmissionState::Declined);
        assert!(matches!(
            forms.confirm(&declined.id, "jerry", at(2)).await,
            Err(ConfirmError::AlreadyAnswered("declined"))
        ));

        let lapsed = forms.ask(request(), at(10)).await.unwrap();
        let late = at(10 + ANSWER_WITHIN_MINUTES);
        assert!(matches!(
            forms.confirm(&lapsed.id, "jerry", late).await,
            Err(ConfirmError::TimedOut)
        ));
        let seen = forms.get(&lapsed.id, "jerry", late).await.unwrap();
        assert_eq!(seen.state, SubmissionState::TimedOut);
        assert_eq!(seen.decided_at, Some(lapsed.answer_by()));
        assert_eq!(sender.sent.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_failed_send_says_whether_the_site_may_have_it() {
        for (failure, words) in [
            (
                SendFailure::NotSent("The form is no longer on the page.".into()),
                "Nothing was sent.",
            ),
            (
                SendFailure::AnswerLost("The site did not answer.".into()),
                "may have received",
            ),
        ] {
            let forms = service(Sender::answering(Err(failure)));
            let asked = forms.ask(request(), at(0)).await.unwrap();
            let after = forms.confirm(&asked.id, "jerry", at(1)).await.unwrap();
            match after.state {
                SubmissionState::Failed { why } => assert!(why.contains(words), "{why}"),
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn the_conversation_finds_its_form_by_what_it_sends() {
        let forms = service(Sender::answering(ok_page()));
        forms.ask(request(), at(0)).await.unwrap();
        let swapped = vec![("q".to_string(), "forester".to_string())];
        let found = forms
            .latest("jerry", "https://cars.example/search", &swapped, at(1))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.state, SubmissionState::Waiting);
        let other = vec![("q".to_string(), "outback".to_string())];
        assert!(forms
            .latest("jerry", "https://cars.example/search", &other, at(1))
            .await
            .unwrap()
            .is_none());
        assert!(forms
            .latest("amina", "https://cars.example/search", &swapped, at(1))
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_send_cut_off_by_a_restart_reads_as_failed() {
        let store = Arc::new(MemoryStore::default());
        let forms = FormConfirmations::new(store.clone(), Sender::answering(ok_page()));
        let asked = forms.ask(request(), at(0)).await.unwrap();
        store
            .advance(
                &asked.id,
                &SubmissionState::Waiting,
                &SubmissionState::Sending,
                at(1),
            )
            .await
            .unwrap();
        let later = forms.get(&asked.id, "jerry", at(10)).await.unwrap();
        match later.state {
            SubmissionState::Failed { why } => assert!(why.contains("may have received"), "{why}"),
            other => panic!("{other:?}"),
        }
    }
}
