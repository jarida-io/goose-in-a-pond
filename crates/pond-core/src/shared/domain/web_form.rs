//! A POST form a member asked the pond to send. It waits for their yes on their own phone, and
//! is sent at most once.

use chrono::{DateTime, Duration, Utc};

/// How long a form waits for the member's answer before it lapses unsent.
pub const ANSWER_WITHIN_MINUTES: i64 = 15;

/// How long a form is kept after it was asked, for the conversation to hear how it went.
pub const KEPT_FOR_HOURS: i64 = 24;

/// Longer than any one send takes, which is a page read and a post of at most 10 s each. A form
/// still sending past this was cut off by a restart.
pub const SENDING_AT_MOST_SECS: i64 = 120;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionState {
    /// Waiting for the member's yes or no.
    Waiting,
    /// Confirmed and being sent; claimed first, so two answers can't send it twice.
    Sending,
    /// Sent: the site answered `status` with the page at `address`.
    Sent {
        status: u16,
        address: String,
        title: String,
    },
    Declined,
    /// No answer came in time, so nothing was sent.
    TimedOut,
    /// `why` is a whole sentence that says whether anything reached the site.
    Failed {
        why: String,
    },
}

impl SubmissionState {
    /// The stored name, as `web_form_submissions.state` holds it.
    pub fn name(&self) -> &'static str {
        match self {
            SubmissionState::Waiting => "waiting",
            SubmissionState::Sending => "sending",
            SubmissionState::Sent { .. } => "sent",
            SubmissionState::Declined => "declined",
            SubmissionState::TimedOut => "timed_out",
            SubmissionState::Failed { .. } => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormSubmission {
    pub id: String,
    /// The member who asked; only their own phone may answer.
    pub owner: String,
    /// The page the form sits on, read again just before sending for the form's fresh hidden
    /// values and the site's cookies.
    pub page: String,
    /// Where the form sends, without a query.
    pub action: String,
    /// The fields the member confirms, as the model filled them in.
    pub values: Vec<(String, String)>,
    /// Clicks from an address given outright to the page the form sits on.
    pub depth: u8,
    pub session_id: Option<String>,
    pub state: SubmissionState,
    pub asked_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
}

impl FormSubmission {
    pub fn answer_by(&self) -> DateTime<Utc> {
        self.asked_at + Duration::minutes(ANSWER_WITHIN_MINUTES)
    }

    /// Waiting past its time.
    pub fn lapsed(&self, now: DateTime<Utc>) -> bool {
        self.state == SubmissionState::Waiting && now >= self.answer_by()
    }

    /// Sending for longer than a send takes: the pond stopped mid-way.
    pub fn cut_off(&self, now: DateTime<Utc>) -> bool {
        self.state == SubmissionState::Sending
            && self
                .decided_at
                .is_none_or(|at| now >= at + Duration::seconds(SENDING_AT_MOST_SECS))
    }

    /// Whether this sends exactly `values` to `action`, in any order.
    pub fn same_request(&self, action: &str, values: &[(String, String)]) -> bool {
        let mut mine = self.values.clone();
        let mut theirs = values.to_vec();
        mine.sort();
        theirs.sort();
        self.action == action && mine == theirs
    }

    /// The site the form sends to, `www.` aside, as the phone names it.
    pub fn site(&self) -> String {
        url::Url::parse(&self.action)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .map(|h| h.strip_prefix("www.").map(str::to_string).unwrap_or(h))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn submission() -> FormSubmission {
        let at = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        FormSubmission {
            id: "f1".into(),
            owner: "jerry".into(),
            page: "https://www.cars.example/".into(),
            action: "https://www.cars.example/search".into(),
            values: vec![
                ("q".into(), "forester".into()),
                ("make".into(), "subaru".into()),
            ],
            depth: 0,
            session_id: None,
            state: SubmissionState::Waiting,
            asked_at: at,
            decided_at: None,
        }
    }

    #[test]
    fn a_form_lapses_once_its_answer_time_has_passed() {
        let s = submission();
        assert!(!s.lapsed(s.answer_by() - Duration::seconds(1)));
        assert!(s.lapsed(s.answer_by()));
        let declined = FormSubmission {
            state: SubmissionState::Declined,
            ..s.clone()
        };
        assert!(!declined.lapsed(s.answer_by() + Duration::hours(1)));
    }

    #[test]
    fn the_same_request_ignores_field_order_only() {
        let s = submission();
        let action = "https://www.cars.example/search";
        let swapped = [
            ("make".to_string(), "subaru".to_string()),
            ("q".to_string(), "forester".to_string()),
        ];
        assert!(s.same_request(action, &swapped));
        assert!(!s.same_request(action, &swapped[..1]));
        assert!(!s.same_request("https://www.cars.example/other", &swapped));
        assert_eq!(s.site(), "cars.example");
    }

    #[test]
    fn a_send_running_past_its_time_was_cut_off() {
        let s = submission();
        let started = s.asked_at + Duration::minutes(1);
        let sending = FormSubmission {
            state: SubmissionState::Sending,
            decided_at: Some(started),
            ..s
        };
        assert!(!sending.cut_off(started + Duration::seconds(SENDING_AT_MOST_SECS - 1)));
        assert!(sending.cut_off(started + Duration::seconds(SENDING_AT_MOST_SECS)));
    }
}
