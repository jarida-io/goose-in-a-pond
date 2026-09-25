//! Turning a stored reminder into a proposal the household already has a queue
//! for -- best effort, and honest about the times it cannot.
//!
//! # Why this is best effort and the table is not
//!
//! Migration 0057 stores the date unconditionally, because
//! [`ProposalAudience::from_scope`] refuses `Household` and `Guest` (PAI-7
//! invariant 4) and `profile_id` is NULL on every live memory row. A reminder on
//! a real pond today therefore has nobody it can be addressed to, and a design
//! that only made proposals would keep losing the date on exactly the pond it
//! was written to fix.
//!
//! So the ordering is: store first, propose when possible. This module is the
//! second half. A reminder it cannot promote is left `Pending` and stays
//! readable -- promotion is something that can happen later, when the member is
//! identified, and not a deadline the row misses once and fails.
//!
//! # Every refusal is counted and named
//!
//! [`PromotionReport`] has a field per outcome rather than a single "skipped",
//! because the three mean different things: an unaddressable reminder is a pond
//! with no profile rows, a capped one is the household being protected from
//! being pestered, and a failed one is a store that would not take the write.
//! The first is the normal state of every pond today and is not a fault; the
//! last is. Reporting them as one number would hide the last behind the first.
//!
//! # The daily cap is not bypassed
//!
//! [`MAX_PROPOSALS_PER_DAY`] is a limit on how often the pond interrupts one
//! person, counted through [`ProposalRepository::count_made_since`] over the
//! last 24 hours -- the same read and the same window `pond-server`'s reviewer
//! tick uses. A second producer of proposals that counted its own would be a
//! second budget, and the member would get both.
//!
//! # Nothing here parses a date
//!
//! [`CapturedReminder::when_said`] is the subject's own words about the timing
//! and it reaches the proposal's rationale and prompt verbatim. Nothing in this
//! pipeline knows which Tuesday was meant; a resolved date here would be a guess
//! wearing the pond's authority, and the person reading the proposal is the one
//! who knows.

use crate::user_data::domain::proposal::{BusEventRef, Proposal, ProposalAudience};
use crate::user_data::domain::reminder::{CapturedReminder, ReminderDisposition};
use crate::user_data::domain::schedule::TaskKind;
use crate::user_data::ports::proposal::ProposalRepository;
use crate::user_data::ports::reminder_repository::ReminderRepository;
use crate::user_data::services::proactive_review::{MAX_PROPOSALS_PER_DAY, PROPOSAL_TTL};
use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use std::collections::BTreeMap;

/// The trigger `kind` every reminder-born proposal carries.
///
/// A stable string, because PAI-7 P7's feedback loop compares triggers by
/// identity: changing this spelling would make every decision a member has
/// already recorded stop matching the proposals it was about.
pub const REMINDER_TRIGGER_KIND: &str = "reminder";

/// The confidence a reminder-born proposal is made at.
///
/// It is not a model score and must not be read as one. The words came from the
/// household member's own conversation, so what is uncertain here is not whether
/// they meant it -- it is whether the extraction lifted the right sentence out.
/// High, and deliberately short of 1.0, which would assert a certainty the pond
/// does not have.
pub const REMINDER_PROPOSAL_CONFIDENCE: f32 = 0.9;

/// Why one reminder did not become a proposal.
///
/// Each variant is a different fact about the pond, and they are kept apart for
/// the reason in the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReminderProposalSkip {
    /// The reminder has no `profile_id`, so there is no member to address it
    /// to. The normal state of every pond with no profile rows, which is every
    /// pond today -- not a failure, and the reason the table exists.
    Unaddressable { subject: String },
    /// This member has had their day's proposals.
    DailyCapReached { made: usize, cap: usize },
    /// The domain refused the proposal. A blank `about`, a blank `when_said`, a
    /// confidence out of range -- none of which the table's CHECKs allow, so
    /// this is here to be reported rather than expected.
    Malformed { reason: String },
}

impl ReminderProposalSkip {
    /// Short, stable label for structured logs, in the same shape as
    /// [`ReviewSkip::as_str`](crate::user_data::services::proactive_review::ReviewSkip::as_str).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unaddressable { .. } => "unaddressable",
            Self::DailyCapReached { .. } => "daily_cap_reached",
            Self::Malformed { .. } => "malformed",
        }
    }

    /// The same fact as a sentence, for the log line and for anything that
    /// shows a person why a reminder is still only a reminder.
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

/// Build the proposal one stored reminder becomes.
///
/// Pure, and fallible in the three ways above minus the cap, which needs a read.
/// The id and the clock are passed in rather than minted here so a test can name
/// the proposal it built and assert on its expiry.
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

    // The conversation and the thing, not the row. A re-walk that files the same
    // reminder again produces the same identity here, so a member who said no
    // once is not asked twice about it; a genuinely new mention, in a new
    // conversation, is a different identity and is still allowed through.
    // `observed_at` is when the CONVERSATION happened, which is the fact the
    // feedback loop reasons about -- not when the backlog walk got to it.
    let trigger = BusEventRef::new(
        REMINDER_TRIGGER_KIND,
        Some(reminder.session_id.clone()),
        Some(reminder.dedup_key()),
        reminder.said_at,
    )
    .map_err(|e| ReminderProposalSkip::Malformed {
        reason: e.to_string(),
    })?;

    // The subject's own words on both halves, quoted rather than interpreted.
    // Invariant 2 wants a rationale, and the honest one here is simply where
    // this came from and why it is not a memory.
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

/// What one promotion run came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromotionReport {
    /// Pending reminders the run looked at.
    pub considered: usize,
    /// Reminders that became a proposal and were moved off `Pending`.
    pub proposed: usize,
    /// Reminders with no member to address, left pending. Expected, and not a
    /// fault: see the module docs.
    pub unaddressable: usize,
    /// Reminders held back by the daily cap, left pending.
    pub capped: usize,
    /// Reminders a store error stopped. The one outcome here that is the POND's
    /// fault, and the reason these are four fields rather than one.
    pub failed: usize,
}

/// Promote what can be promoted, and say what could not.
///
/// Reads the pending reminders, newest conversation first, and for each one that
/// has a member to address tries to make a proposal inside that member's daily
/// budget. A reminder that cannot be promoted stays `Pending` and stays
/// readable; nothing here deletes or expires a row.
///
/// The cap is counted once per member and then tracked locally, so a run that
/// promotes three reminders for one person spends three of their six rather than
/// re-reading a count that has not been committed yet.
pub async fn promote_pending_reminders(
    reminders: &dyn ReminderRepository,
    proposals: &dyn ProposalRepository,
    limit: usize,
    now: DateTime<Utc>,
) -> Result<PromotionReport> {
    // `Household`, deliberately and alone among callers: this pass routes each
    // reminder to its OWN member's proposal queue, which requires reading every
    // member's. Nothing it reads leaves this function except as a proposal
    // addressed to the reminder's owner.
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
    // profile_id -> proposals already made to that member in the last day,
    // including the ones this run has just made.
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

        // One read per member per run. An error here is not a reason to skip the
        // cap -- a budget that fails open is not a budget -- so it counts as a
        // failure and the reminder stays pending.
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

        // The disposition is what stops a second proposal being made out of this
        // row. Both ways of not moving it are failures and are said loudly
        // rather than treated as tidying: the proposal is in the queue either
        // way, and a reminder still pending beside it is one the next run will
        // propose again.
        match reminders
            .set_disposition(
                &reminder.id,
                &crate::user_data::domain::profile::ProfileScope::Household,
                ReminderDisposition::Proposed,
                now,
            )
            .await
        {
            Ok(true) => report.proposed += 1,
            Ok(false) => {
                report.failed += 1;
                // Somebody dismissed it between the list and this write. The
                // proposal that was just saved is now one nothing asked for.
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

/// Count a refusal and say why, once, in one place.
///
/// The content never rises above DEBUG: an unaddressable reminder is logged at
/// INFO because a household running with no profile rows deserves to be able to
/// find out why its dates are not reaching the queue, and the sentence itself is
/// the household's private words.
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

    /// An in-memory proposal store. Only the three methods this module calls do
    /// anything; the rest answer emptily, because a fake that pretended to
    /// implement reads nobody here makes would be a second thing to keep true.
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

    /// The blocker the whole design is shaped around: on a pond with no profile
    /// rows the reminder is not promoted, is not lost, and is still pending.
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

        // The disposition is what stops the second one.
        let second = promote_pending_reminders(&reminders, &proposals, 50, Utc::now())
            .await
            .unwrap();
        assert_eq!(second.considered, 0);
        assert_eq!(second.proposed, 0);
        assert_eq!(proposals.saved.lock().unwrap().len(), 1);
    }

    /// The subject's own words reach the proposal unparsed, and no resolved date
    /// appears anywhere in it.
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

    /// The cap is the household's protection from being pestered, and this
    /// producer is inside it rather than beside it.
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

    /// A store that will not take the write is the one outcome that must not be
    /// reported as an ordinary skip.
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

    /// Every refusal says why in words, so a household with no proposals can
    /// find out which of the three reasons it is.
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
