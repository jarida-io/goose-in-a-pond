//! The proposals stored reminders became before they were offered as to-dos instead
//! (`shared::services::todo_offers`): how one is built, and the conversation it names, which
//! such a proposal still waiting in the queue needs.

use crate::user_data::domain::proposal::{BusEventRef, Proposal, ProposalAudience};
use crate::user_data::domain::reminder::CapturedReminder;
use crate::user_data::domain::schedule::TaskKind;
use crate::user_data::services::proactive_review::PROPOSAL_TTL;
use chrono::{DateTime, Utc};

/// Trigger `kind` of reminder-born proposals; stable, since recorded decisions match on it.
pub const REMINDER_TRIGGER_KIND: &str = "reminder";

/// Confidence of reminder-born proposals, not a model score; below 1.0 as extraction can err.
pub const REMINDER_PROPOSAL_CONFIDENCE: f32 = 0.9;

/// Why one reminder did not become a proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderProposalSkip {
    /// No `profile_id` to address: normal on a pond without profile rows, not a failure.
    Unaddressable { subject: String },
    /// This member has had their day's proposals.
    DailyCapReached { made: usize, cap: usize },
    /// The domain refused the proposal; the table's CHECKs should make this unreachable.
    Malformed { reason: String },
}

impl ReminderProposalSkip {
    /// Stable label for structured logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unaddressable { .. } => "unaddressable",
            Self::DailyCapReached { .. } => "daily_cap_reached",
            Self::Malformed { .. } => "malformed",
        }
    }

    /// The same fact as a human-readable sentence.
    pub fn reason(&self) -> String {
        match self {
            Self::Unaddressable { subject } => format!(
                "nothing on this pond says which household member {subject} is, so there is \
                 no one to address a proposal to; the reminder is kept and can be proposed \
                 once they are identified"
            ),
            Self::DailyCapReached { made, cap } => format!(
                "this member has already had {made} of {cap} proposals today; the reminder \
                 is kept and can be proposed tomorrow"
            ),
            Self::Malformed { reason } => {
                format!("the proposal could not be built from this reminder: {reason}")
            }
        }
    }
}

/// Build the proposal one stored reminder becomes; pure, so the daily cap is not checked here.
pub fn proposal_from_reminder(
    reminder: &CapturedReminder,
    id: impl Into<String>,
    now: DateTime<Utc>,
) -> Result<Proposal, ReminderProposalSkip> {
    let Some(profile_id) = reminder.profile_id.as_deref() else {
        return Err(ReminderProposalSkip::Unaddressable {
            subject: reminder.subject.clone(),
        });
    };
    let audience =
        ProposalAudience::for_member(profile_id).map_err(|e| ReminderProposalSkip::Malformed {
            reason: e.to_string(),
        })?;

    // Identity is conversation + thing, not row: a re-walk can't re-ask someone who said no.
    // Observed at `said_at`, not walk time: the feedback loop reasons about the conversation.
    let trigger = BusEventRef::new(
        REMINDER_TRIGGER_KIND,
        Some(reminder.session_id.clone()),
        Some(reminder.dedup_key()),
        reminder.said_at,
    )
    .map_err(|e| ReminderProposalSkip::Malformed {
        reason: e.to_string(),
    })?;

    // The subject's words, quoted and never parsed: the pond can't know which Tuesday was meant.
    let rationale = format!(
        "{} mentioned {} -- \"{}\" -- in conversation. A one-off date is never kept as a \
         memory, because read back months later it would be false, so it is held here instead. \
         The timing is quoted as it was said; the pond has not worked out a date.",
        reminder.subject, reminder.about, reminder.when_said
    );

    let proposed_action = TaskKind::AgentPrompt {
        prompt: format!(
            "Remind {} about {} -- they said \"{}\".",
            reminder.subject, reminder.about, reminder.when_said
        ),
    };

    Proposal::expiring_after(
        id,
        trigger,
        rationale,
        proposed_action,
        audience,
        REMINDER_PROPOSAL_CONFIDENCE,
        now,
        PROPOSAL_TTL,
    )
    .map_err(|e| ReminderProposalSkip::Malformed {
        reason: e.to_string(),
    })
}

/// The conversation a reminder-born proposal was heard in: the trigger's `source_id`, which
/// [`proposal_from_reminder`] fills with the reminder's session. `None` for a proposal from any
/// other trigger (a sensor, a camera, the reviewer), which has no conversation behind it.
pub fn reminder_session_id(proposal: &Proposal) -> Option<&str> {
    let trigger = proposal.trigger();
    if trigger.kind() == REMINDER_TRIGGER_KIND {
        trigger.source_id()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::reminder::ReminderDisposition;
    use chrono::Duration;

    fn reminder(id: &str, profile_id: Option<&str>) -> CapturedReminder {
        CapturedReminder {
            id: id.into(),
            about: "the dentist".into(),
            when_said: "next Tuesday".into(),
            session_id: "sess-1".into(),
            window_id: format!("win-{id}"),
            subject: "Jerry".into(),
            profile_id: profile_id.map(str::to_string),
            said_at: Utc::now() - Duration::hours(3),
            captured_at: Utc::now(),
            disposition: ReminderDisposition::Pending,
        }
    }

    #[tokio::test]
    async fn the_timing_words_reach_the_proposal_verbatim() {
        let mut r = reminder("r1", Some("profile-jerry"));
        r.when_said = "after the rains".into();
        let proposal = proposal_from_reminder(&r, "p1", Utc::now()).unwrap();

        assert!(proposal.rationale().contains("after the rains"));
        assert!(proposal.summary().contains("after the rains"));
        assert_eq!(proposal.trigger().kind(), REMINDER_TRIGGER_KIND);
        assert_eq!(proposal.trigger().signal(), Some("the dentist"));
        assert_eq!(
            proposal.trigger().observed_at(),
            r.said_at,
            "the feedback loop reasons about when the conversation happened"
        );
    }

    #[test]
    fn a_reminder_proposal_names_the_conversation_it_was_heard_in() {
        let r = reminder("r1", Some("profile-jerry"));
        let proposal = proposal_from_reminder(&r, "p1", Utc::now()).unwrap();
        assert_eq!(reminder_session_id(&proposal), Some(r.session_id.as_str()));
    }

    #[test]
    fn a_proposal_from_another_trigger_names_no_conversation() {
        let proposal = Proposal::expiring_after(
            "p2",
            BusEventRef::new(
                "sensor",
                Some("plant-1".into()),
                Some("moisture".into()),
                Utc::now(),
            )
            .unwrap(),
            "the soil is dry",
            TaskKind::AgentPrompt {
                prompt: "Water the plants".into(),
            },
            ProposalAudience::for_member("profile-jerry").unwrap(),
            0.9,
            Utc::now(),
            PROPOSAL_TTL,
        )
        .unwrap();
        assert_eq!(reminder_session_id(&proposal), None);
    }

    #[test]
    fn every_refusal_carries_its_reason() {
        for skip in [
            ReminderProposalSkip::Unaddressable {
                subject: "Jerry".into(),
            },
            ReminderProposalSkip::DailyCapReached { made: 6, cap: 6 },
            ReminderProposalSkip::Malformed {
                reason: "blank rationale".into(),
            },
        ] {
            assert!(!skip.as_str().is_empty());
            assert!(skip.reason().len() > 20, "{:?} has no explanation", skip);
        }
    }
}
