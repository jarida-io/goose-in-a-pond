//! Turns stored reminders into proposals, best effort: one with no member to address stays
//! `Pending`, since [`ProposalAudience::from_scope`] refuses `Household` and `Guest`.

use crate::user_data::domain::proposal::{BusEventRef, Proposal, ProposalAudience};
use crate::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use crate::user_data::domain::schedule::TaskKind;
use crate::user_data::ports::proposal::ProposalRepository;
use crate::user_data::ports::reminder_repository::ReminderRepository;
use crate::user_data::services::proactive_review::{MAX_PROPOSALS_PER_DAY, PROPOSAL_TTL};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;

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

/// What one promotion run came to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PromotionReport {
    /// Pending reminders the run looked at.
    pub considered: usize,
    /// Reminders that became a proposal and were moved off `Pending`.
    pub proposed: usize,
    /// Reminders with no member to address, left pending; expected, not a fault.
    pub unaddressable: usize,
    /// Reminders held back by the daily cap, left pending.
    pub capped: usize,
    /// Stopped by a store error or a malformed proposal; the one outcome that is a fault.
    pub failed: usize,
    /// The proposals counted in `proposed`, each now waiting for its member's decision, so the
    /// caller can tell that member's phone.
    pub made: Vec<Proposal>,
}

/// Promote what can be promoted and report the rest, which stays `Pending`.
pub async fn promote_pending_reminders(
    reminders: &dyn ReminderRepository,
    proposals: &dyn ProposalRepository,
    limit: usize,
    now: DateTime<Utc>,
) -> Result<PromotionReport> {
    // `Household`, the only caller that should: rows leave only as proposals to their owner.
    let pending = reminders
        .list_pending(
            &crate::user_data::domain::profile::ProfileScope::Household,
            limit,
        )
        .await?;
    let mut report = PromotionReport {
        considered: pending.len(),
        ..Default::default()
    };
    // profile_id -> proposals made to them in the last day, this run's included.
    let mut made_today: BTreeMap<String, usize> = BTreeMap::new();

    for reminder in pending {
        let proposal = match proposal_from_reminder(&reminder, uuid_v4(), now) {
            Ok(proposal) => proposal,
            Err(skip) => {
                record_skip(&mut report, &reminder, &skip);
                continue;
            }
        };
        let profile_id = proposal.audience().profile_id().to_string();

        // Shares the reviewer tick's count and window (one budget); a failed read fails closed.
        let made = match made_today.get(&profile_id) {
            Some(made) => *made,
            None => match proposals
                .count_made_since(&profile_id, now - Duration::days(1))
                .await
            {
                Ok(made) => {
                    made_today.insert(profile_id.clone(), made);
                    made
                }
                Err(e) => {
                    report.failed += 1;
                    tracing::warn!(
                        reminder_id = %reminder.id,
                        "[reminders] could not read this member's proposal count, so the \
                         reminder stays pending: {e}"
                    );
                    continue;
                }
            },
        };
        if made >= MAX_PROPOSALS_PER_DAY {
            record_skip(
                &mut report,
                &reminder,
                &ReminderProposalSkip::DailyCapReached {
                    made,
                    cap: MAX_PROPOSALS_PER_DAY,
                },
            );
            continue;
        }

        if let Err(e) = proposals.save(&proposal).await {
            report.failed += 1;
            tracing::warn!(
                reminder_id = %reminder.id,
                proposal_id = %proposal.id(),
                "[reminders] the proposal could not be saved, so the reminder stays pending: {e}"
            );
            continue;
        }
        made_today.insert(profile_id, made + 1);

        // The disposition stops the next run re-proposing this row; not moving it is a failure.
        match reminders
            .set_disposition(
                &reminder.id,
                &crate::user_data::domain::profile::ProfileScope::Household,
                ReminderDisposition::Proposed,
                now,
            )
            .await
        {
            Ok(true) => {
                report.proposed += 1;
                report.made.push(proposal);
            }
            Ok(false) => {
                report.failed += 1;
                // Dismissed since the list; the proposal just saved is one nobody asked for.
                tracing::warn!(
                    reminder_id = %reminder.id,
                    proposal_id = %proposal.id(),
                    "[reminders] a proposal was made for a reminder that stopped being \
                     pending underneath it"
                );
            }
            Err(e) => {
                report.failed += 1;
                tracing::warn!(
                    reminder_id = %reminder.id,
                    proposal_id = %proposal.id(),
                    "[reminders] a proposal was made but the reminder could not be marked \
                     proposed; it may be proposed again: {e}"
                );
            }
        }
    }

    Ok(report)
}

/// Count a refusal and log why at INFO; the reminder's private words never go above DEBUG.
fn record_skip(
    report: &mut PromotionReport,
    reminder: &CapturedReminder,
    skip: &ReminderProposalSkip,
) {
    match skip {
        ReminderProposalSkip::Unaddressable { .. } => report.unaddressable += 1,
        ReminderProposalSkip::DailyCapReached { .. } => report.capped += 1,
        ReminderProposalSkip::Malformed { .. } => report.failed += 1,
    }
    tracing::info!(
        target: "giap::trace",
        kind = "reminder_not_proposed",
        reminder_id = %reminder.id,
        skip = skip.as_str(),
        reason = %skip.reason(),
        "a stored reminder did not become a proposal"
    );
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::proposal::ProposalDecision;
    use crate::user_data::mocks::mock_reminder::MockReminderRepository;
    use std::sync::Mutex;

    /// In-memory proposal store; only the three methods this module calls do anything.
    #[derive(Default)]
    struct FakeProposals {
        saved: Mutex<Vec<Proposal>>,
        already_made: usize,
        save_fails: bool,
    }

    #[async_trait::async_trait]
    impl ProposalRepository for FakeProposals {
        async fn save(&self, proposal: &Proposal) -> Result<()> {
            if self.save_fails {
                anyhow::bail!("the drafts table is locked");
            }
            self.saved.lock().unwrap().push(proposal.clone());
            Ok(())
        }

        async fn list_live_for(
            &self,
            _profile_id: &str,
            _now: DateTime<Utc>,
        ) -> Result<Vec<Proposal>> {
            Ok(vec![])
        }

        async fn get_live(&self, _id: &str, _now: DateTime<Utc>) -> Result<Option<Proposal>> {
            Ok(None)
        }

        async fn expire_due(&self, _now: DateTime<Utc>) -> Result<u64> {
            Ok(0)
        }

        async fn count_made_since(
            &self,
            _profile_id: &str,
            _since: DateTime<Utc>,
        ) -> Result<usize> {
            Ok(self.already_made + self.saved.lock().unwrap().len())
        }

        async fn decisions_since(
            &self,
            _profile_id: &str,
            _since: DateTime<Utc>,
        ) -> Result<Vec<ProposalDecision>> {
            Ok(vec![])
        }
    }

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
    async fn a_reminder_with_no_member_stays_a_reminder() {
        let reminders = MockReminderRepository::new();
        reminders.capture(&reminder("r1", None)).await.unwrap();
        let proposals = FakeProposals::default();

        let report = promote_pending_reminders(&reminders, &proposals, 50, Utc::now())
            .await
            .unwrap();

        assert_eq!(report.considered, 1);
        assert_eq!(report.unaddressable, 1);
        assert_eq!(report.proposed, 0);
        assert_eq!(
            report.failed, 0,
            "an unaddressable reminder is not a failure"
        );
        assert!(proposals.saved.lock().unwrap().is_empty());
        assert_eq!(
            reminders.rows()[0].disposition,
            ReminderDisposition::Pending,
            "it must still be promotable once the member is identified"
        );
    }

    #[tokio::test]
    async fn a_reminder_with_a_member_becomes_a_proposal_once() {
        let reminders = MockReminderRepository::new();
        reminders
            .capture(&reminder("r1", Some("profile-jerry")))
            .await
            .unwrap();
        let proposals = FakeProposals::default();

        let first = promote_pending_reminders(&reminders, &proposals, 50, Utc::now())
            .await
            .unwrap();
        assert_eq!(first.proposed, 1);
        assert_eq!(
            reminders.rows()[0].disposition,
            ReminderDisposition::Proposed
        );
        // Handed back so the member's phone can be told; it is the proposal that was saved.
        assert_eq!(first.made.len(), 1);
        assert_eq!(
            first.made[0].audience().profile_id(),
            "profile-jerry",
            "the notice goes to whose reminder it is"
        );
        assert_eq!(first.made[0].id(), proposals.saved.lock().unwrap()[0].id());

        // The disposition is what stops the second one.
        let second = promote_pending_reminders(&reminders, &proposals, 50, Utc::now())
            .await
            .unwrap();
        assert_eq!(second.considered, 0);
        assert_eq!(second.proposed, 0);
        assert!(second.made.is_empty(), "nothing new to tell anybody");
        assert_eq!(proposals.saved.lock().unwrap().len(), 1);
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

    #[tokio::test]
    async fn the_daily_cap_is_not_bypassed() {
        let reminders = MockReminderRepository::new();
        for i in 0..4 {
            let mut r = reminder(&format!("r{i}"), Some("profile-jerry"));
            r.about = format!("thing {i}");
            reminders.capture(&r).await.unwrap();
        }
        let proposals = FakeProposals {
            already_made: MAX_PROPOSALS_PER_DAY - 2,
            ..Default::default()
        };

        let report = promote_pending_reminders(&reminders, &proposals, 50, Utc::now())
            .await
            .unwrap();

        assert_eq!(report.proposed, 2, "only the remaining budget is spent");
        assert_eq!(report.capped, 2);
        assert_eq!(report.failed, 0);
    }

    #[tokio::test]
    async fn a_store_that_refuses_the_write_is_a_failure_not_a_skip() {
        let reminders = MockReminderRepository::new();
        reminders
            .capture(&reminder("r1", Some("profile-jerry")))
            .await
            .unwrap();
        let proposals = FakeProposals {
            save_fails: true,
            ..Default::default()
        };

        let report = promote_pending_reminders(&reminders, &proposals, 50, Utc::now())
            .await
            .unwrap();

        assert_eq!(report.failed, 1);
        assert_eq!(report.proposed, 0);
        assert_eq!(
            reminders.rows()[0].disposition,
            ReminderDisposition::Pending,
            "a failed proposal must leave the reminder promotable"
        );
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
