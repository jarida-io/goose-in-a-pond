//! What the engine queues on its minute tick, decided from one member's stored state with no
//! model call.

use chrono::{DateTime, Duration, Utc};

use super::recipes::{next_slot, slots_of, Slot};
use crate::user_data::domain::anchor::{
    Anchor, AnchorKind, AnchorState, Deliverable, DeliverableState,
};
use crate::user_data::domain::capture::{Capture, CaptureState};
use crate::user_data::domain::engine_job::{
    EngineJob, JobClass, JobKind, OPEN_CLASS_JOBS_PER_MEMBER,
};

/// How long a deliverable whose job died waits before another is queued for it.
pub const RETRY_AFTER: Duration = Duration::hours(6);

/// One member's state, as the tick reads it.
#[derive(Debug, Clone, Copy)]
pub struct TickSnapshot<'a> {
    pub profile_id: &'a str,
    /// The member's candidate and active anchors; one only shared with them is passed over.
    pub anchors: &'a [Anchor],
    /// The deliverables of those anchors.
    pub deliverables: &'a [Deliverable],
    /// The member's queued and running jobs.
    pub open_jobs: &'a [EngineJob],
    pub fact_sheet_due: bool,
    pub pending_captures: &'a [Capture],
}

/// One thing the tick asks the engine to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickAction {
    QueueQuestions {
        anchor_id: String,
        class: JobClass,
        due_at: Option<DateTime<Utc>>,
    },
    /// Make the slot's first version; its deliverable row may already exist, still queued.
    QueueSlot {
        anchor_id: String,
        slot: &'static Slot,
        class: JobClass,
        due_at: Option<DateTime<Utc>>,
    },
    QueueRefresh {
        anchor_id: String,
        deliverable_id: String,
        class: JobClass,
        due_at: Option<DateTime<Utc>>,
    },
    QueueFactSheet,
    QueueCapture {
        capture_id: String,
    },
    FadeAnchor {
        anchor_id: String,
    },
    EndAnchor {
        anchor_id: String,
    },
}

/// The class a job serving `anchor` runs in, and the date it is due by: an anchor with a date
/// is a deadline, one without is open.
pub fn class_of(anchor: &Anchor) -> (JobClass, Option<DateTime<Utc>>) {
    match anchor.starts_at.or(anchor.ends_at) {
        Some(date) => (JobClass::Deadline, Some(date)),
        None => (JobClass::Open, None),
    }
}

/// The places left for jobs of class `open`; a job of any other class always has one.
struct OpenRoom(usize);

impl OpenRoom {
    fn has(&self, class: JobClass, jobs: usize) -> bool {
        class != JobClass::Open || self.0 >= jobs
    }

    fn take(&mut self, class: JobClass, jobs: usize) {
        if class == JobClass::Open {
            self.0 = self.0.saturating_sub(jobs);
        }
    }
}

/// The slot whose queued deliverable lost its job long enough ago to be tried again.
fn stalled_slot(
    anchor: &Anchor,
    deliverables: &[Deliverable],
    open: &[&EngineJob],
    now: DateTime<Utc>,
) -> Option<&'static Slot> {
    let slots = slots_of(anchor);
    let of = |slot: &Slot| {
        deliverables
            .iter()
            .find(|d| d.anchor_id == anchor.id && d.slot == slot.slot)
    };
    let slot = slots
        .iter()
        .find(|slot| of(slot).is_none_or(|d| d.state != DeliverableState::Ready))?;
    let waiting = of(slot).filter(|d| d.state == DeliverableState::Queued)?;
    let being_made = open
        .iter()
        .any(|job| job.deliverable_id.as_deref() == Some(waiting.id.as_str()));
    (!being_made && now - waiting.updated_at >= RETRY_AFTER).then_some(slot)
}

/// What to queue, fade and end for one member now. One open job per deliverable, per fact
/// sheet and per capture; two of class `open` at most; nothing for an anchor that is not
/// active, and no refresh of a deliverable the member rejected.
pub fn plan_tick(snapshot: &TickSnapshot<'_>, now: DateTime<Utc>) -> Vec<TickAction> {
    let mine = |owner: &str| owner == snapshot.profile_id;
    let open: Vec<&EngineJob> = snapshot
        .open_jobs
        .iter()
        .filter(|job| job.state.is_open() && job.profile_id.as_deref().is_some_and(mine))
        .collect();
    let mut actions = Vec::new();

    for anchor in snapshot.anchors.iter().filter(|a| mine(&a.profile_id)) {
        if anchor.should_fade(now) {
            actions.push(TickAction::FadeAnchor {
                anchor_id: anchor.id.clone(),
            });
        } else if anchor.should_end(now) {
            actions.push(TickAction::EndAnchor {
                anchor_id: anchor.id.clone(),
            });
        }
    }

    if snapshot.fact_sheet_due && !open.iter().any(|job| job.kind == JobKind::FactSheet) {
        actions.push(TickAction::QueueFactSheet);
    }

    for capture in snapshot.pending_captures {
        let waits = mine(&capture.profile_id)
            && capture.state == CaptureState::Pending
            && !capture.is_unreadable();
        let being_read = open.iter().any(|job| {
            matches!(job.kind, JobKind::Capture | JobKind::Ledger) && job.subject == capture.id
        });
        if waits && !being_read {
            actions.push(TickAction::QueueCapture {
                capture_id: capture.id.clone(),
            });
        }
    }

    let mut live: Vec<&Anchor> = snapshot
        .anchors
        .iter()
        .filter(|a| mine(&a.profile_id) && a.state == AnchorState::Active && !a.should_end(now))
        .collect();
    live.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let made = |anchor: &Anchor| {
        snapshot
            .deliverables
            .iter()
            .any(|d| d.anchor_id == anchor.id)
    };
    let mut room = OpenRoom(
        OPEN_CLASS_JOBS_PER_MEMBER
            .saturating_sub(open.iter().filter(|j| j.class == JobClass::Open).count()),
    );

    for anchor in live.iter().filter(|anchor| !made(anchor)) {
        let Some(slot) = next_slot(anchor, snapshot.deliverables) else {
            continue;
        };
        let (class, due_at) = class_of(anchor);
        let asks = anchor.kind != AnchorKind::Standing
            && !open.iter().any(|job| {
                job.kind == JobKind::Questions
                    && job.anchor_id.as_deref() == Some(anchor.id.as_str())
            });
        let jobs = 1 + usize::from(asks);
        if !room.has(class, jobs) {
            // Held for it: a later slot or a refresh taking a place would keep it waiting.
            room.0 = 0;
            continue;
        }
        room.take(class, jobs);
        // Before the slot: once the slot's deliverable exists the questions are not asked.
        if asks {
            actions.push(TickAction::QueueQuestions {
                anchor_id: anchor.id.clone(),
                class,
                due_at,
            });
        }
        actions.push(TickAction::QueueSlot {
            anchor_id: anchor.id.clone(),
            slot,
            class,
            due_at,
        });
    }

    for anchor in live.iter().filter(|anchor| made(anchor)) {
        let slot = next_slot(anchor, snapshot.deliverables)
            .or_else(|| stalled_slot(anchor, snapshot.deliverables, &open, now));
        let (class, due_at) = class_of(anchor);
        if let Some(slot) = slot.filter(|_| room.has(class, 1)) {
            room.take(class, 1);
            actions.push(TickAction::QueueSlot {
                anchor_id: anchor.id.clone(),
                slot,
                class,
                due_at,
            });
        }
    }

    let mut due: Vec<(&Deliverable, &Anchor)> = snapshot
        .deliverables
        .iter()
        .filter(|d| d.refresh_is_due(now))
        .filter(|d| {
            !open
                .iter()
                .any(|job| job.deliverable_id.as_deref() == Some(d.id.as_str()))
        })
        .filter_map(|d| Some((d, *live.iter().find(|a| a.id == d.anchor_id)?)))
        .collect();
    due.sort_by(|(a, _), (b, _)| a.refresh_at.cmp(&b.refresh_at).then(a.id.cmp(&b.id)));
    for (deliverable, anchor) in due {
        let (class, due_at) = class_of(anchor);
        if room.has(class, 1) {
            room.take(class, 1);
            actions.push(TickAction::QueueRefresh {
                anchor_id: anchor.id.clone(),
                deliverable_id: deliverable.id.clone(),
                class,
                due_at,
            });
        }
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::AnchorOrigin;
    use crate::user_data::domain::capture::{CaptureKind, MAX_ATTEMPTS};
    use crate::user_data::domain::specialist::SpecialistId;
    use crate::user_data::services::engine::recipes::{deliverable_title, slot_named};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn anchor(kind: AnchorKind, title: &str, days_until: Option<i64>) -> Anchor {
        let mut anchor = Anchor::new(
            "jerry",
            kind,
            title,
            AnchorOrigin::You,
            now() - Duration::hours(1),
        )
        .unwrap();
        anchor.starts_at = days_until.map(|days| now() + Duration::days(days));
        anchor
    }

    fn deliverable(anchor: &Anchor, slot: &str, state: DeliverableState) -> Deliverable {
        let recipe = slot_named(anchor, slot).unwrap();
        let mut made = Deliverable::new(
            &anchor.id,
            recipe.kind,
            recipe.slot,
            &deliverable_title(anchor, recipe),
            now() - Duration::minutes(30),
        )
        .unwrap();
        made.state = state;
        if state.is_shown() {
            made.version = 1;
        }
        made
    }

    fn making(anchor: &Anchor, deliverable: &Deliverable) -> EngineJob {
        let (class, due_at) = class_of(anchor);
        EngineJob::for_deliverable(
            "jerry",
            &anchor.id,
            &deliverable.id,
            &deliverable.title,
            class,
            due_at,
            now(),
        )
    }

    #[derive(Default)]
    struct State {
        anchors: Vec<Anchor>,
        deliverables: Vec<Deliverable>,
        jobs: Vec<EngineJob>,
        captures: Vec<Capture>,
        fact_sheet_due: bool,
    }

    impl State {
        fn tick(&self) -> Vec<TickAction> {
            self.tick_at(now())
        }

        fn tick_at(&self, at: DateTime<Utc>) -> Vec<TickAction> {
            plan_tick(
                &TickSnapshot {
                    profile_id: "jerry",
                    anchors: &self.anchors,
                    deliverables: &self.deliverables,
                    open_jobs: &self.jobs,
                    fact_sheet_due: self.fact_sheet_due,
                    pending_captures: &self.captures,
                },
                at,
            )
        }
    }

    fn nothing() -> Vec<TickAction> {
        Vec::new()
    }

    fn slot_of(action: &TickAction) -> Option<&'static str> {
        match action {
            TickAction::QueueSlot { slot, .. } => Some(slot.slot),
            _ => None,
        }
    }

    #[test]
    fn a_newly_active_anchor_gets_its_questions_and_its_first_slot() {
        let summit = anchor(AnchorKind::Event, "AI Summit Nairobi", Some(17));
        let state = State {
            anchors: vec![summit.clone()],
            ..State::default()
        };
        let due_at = summit.starts_at;
        assert_eq!(
            state.tick(),
            [
                TickAction::QueueQuestions {
                    anchor_id: summit.id.clone(),
                    class: JobClass::Deadline,
                    due_at,
                },
                TickAction::QueueSlot {
                    anchor_id: summit.id.clone(),
                    slot: slot_named(&summit, "dossier").unwrap(),
                    class: JobClass::Deadline,
                    due_at,
                },
            ]
        );
    }

    #[test]
    fn questions_are_asked_once_and_a_standing_anchor_is_asked_none() {
        let summit = anchor(AnchorKind::Event, "AI Summit Nairobi", Some(17));
        let mut asking = EngineJob::new(
            JobKind::Questions,
            JobClass::Deadline,
            Some("jerry"),
            "AI Summit Nairobi",
            now(),
        );
        asking.anchor_id = Some(summit.id.clone());
        let mut state = State {
            anchors: vec![summit.clone()],
            jobs: vec![asking],
            ..State::default()
        };
        let actions = state.tick();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(slot_of(&actions[0]), Some("dossier"));

        let dossier = deliverable(&summit, "dossier", DeliverableState::Queued);
        state.jobs = vec![making(&summit, &dossier)];
        state.deliverables = vec![dossier];
        assert_eq!(
            state.tick(),
            nothing(),
            "its slot is queued: nothing is asked again"
        );

        let everyday = anchor(AnchorKind::Standing, "Everyday", None);
        let standing = State {
            anchors: vec![everyday],
            ..State::default()
        };
        let actions = standing.tick();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(slot_of(&actions[0]), Some("out"));
    }

    #[test]
    fn a_candidate_gets_nothing_until_it_fades_and_an_anchor_that_is_over_ends() {
        let mut offered = Anchor::new(
            "jerry",
            AnchorKind::Trip,
            "Kisumu in December",
            AnchorOrigin::Chat,
            now() - Duration::days(6),
        )
        .unwrap();
        let mut state = State {
            anchors: vec![offered.clone()],
            ..State::default()
        };
        assert_eq!(state.tick(), nothing());
        offered.created_at = now() - Duration::days(7);
        state.anchors = vec![offered.clone()];
        assert_eq!(
            state.tick(),
            [TickAction::FadeAnchor {
                anchor_id: offered.id.clone()
            }]
        );

        let over = anchor(AnchorKind::Event, "Last week's meetup", Some(-2));
        let mut stale = deliverable(&over, "dossier", DeliverableState::Ready);
        stale.refresh_at = Some(now() - Duration::days(1));
        let state = State {
            anchors: vec![over.clone()],
            deliverables: vec![stale],
            ..State::default()
        };
        assert_eq!(
            state.tick(),
            [TickAction::EndAnchor {
                anchor_id: over.id.clone()
            }],
            "nothing is refreshed for an anchor that is ending"
        );

        for gone in [AnchorState::Ended, AnchorState::Stopped, AnchorState::Faded] {
            let mut closed = anchor(AnchorKind::Trip, "Diani in April", Some(40));
            closed.state = gone;
            let state = State {
                anchors: vec![closed],
                ..State::default()
            };
            assert_eq!(state.tick(), nothing(), "{gone:?}");
        }
    }

    #[test]
    fn a_slot_that_becomes_ready_queues_the_next_one() {
        let trip = anchor(AnchorKind::Trip, "Kisumu in December", Some(30));
        let mut state = State {
            anchors: vec![trip.clone()],
            deliverables: vec![deliverable(&trip, "plan", DeliverableState::Making)],
            ..State::default()
        };
        state.jobs = vec![making(&trip, &state.deliverables[0])];
        assert_eq!(state.tick(), nothing());

        state.deliverables[0].state = DeliverableState::Ready;
        state.jobs.clear();
        let actions = state.tick();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(slot_of(&actions[0]), Some("stays"));

        state.deliverables[0].state = DeliverableState::Unsure;
        assert_eq!(state.tick(), nothing(), "an unsure slot does not chain");
    }

    #[test]
    fn a_due_refresh_is_a_deadline_for_a_dated_anchor_and_open_for_an_undated_one() {
        let summit = anchor(AnchorKind::Event, "AI Summit Nairobi", Some(17));
        let racing = anchor(AnchorKind::Interest, "Formula 1", None);
        let mut dossier = deliverable(&summit, "dossier", DeliverableState::Ready);
        dossier.refresh_at = Some(now() - Duration::hours(2));
        let mut digest = deliverable(&racing, "digest", DeliverableState::Unsure);
        digest.refresh_at = Some(now() - Duration::hours(5));
        let mut state = State {
            anchors: vec![summit.clone(), racing.clone()],
            deliverables: vec![dossier.clone(), digest.clone()],
            ..State::default()
        };
        assert_eq!(
            state.tick(),
            [
                TickAction::QueueRefresh {
                    anchor_id: racing.id.clone(),
                    deliverable_id: digest.id.clone(),
                    class: JobClass::Open,
                    due_at: None,
                },
                TickAction::QueueRefresh {
                    anchor_id: summit.id.clone(),
                    deliverable_id: dossier.id.clone(),
                    class: JobClass::Deadline,
                    due_at: summit.starts_at,
                },
            ],
            "the longest due first"
        );

        state.jobs = vec![making(&summit, &dossier), making(&racing, &digest)];
        assert_eq!(state.tick(), nothing(), "one open job to a deliverable");

        state.jobs.clear();
        state.deliverables[0].refresh_at = Some(now() + Duration::minutes(1));
        state.deliverables[1].refresh_at = None;
        assert_eq!(state.tick(), nothing(), "nothing is due");
    }

    #[test]
    fn a_rejected_or_archived_deliverable_is_never_refreshed() {
        let summit = anchor(AnchorKind::Event, "AI Summit Nairobi", Some(17));
        for state_of in [
            DeliverableState::Rejected,
            DeliverableState::Archived,
            DeliverableState::Making,
        ] {
            let mut dossier = deliverable(&summit, "dossier", state_of);
            dossier.refresh_at = Some(now() - Duration::days(3));
            let state = State {
                anchors: vec![summit.clone()],
                deliverables: vec![dossier],
                ..State::default()
            };
            assert_eq!(state.tick(), nothing(), "{state_of:?}");
        }
    }

    #[test]
    fn a_due_fact_sheet_has_one_job_at_a_time() {
        let mut state = State {
            fact_sheet_due: true,
            ..State::default()
        };
        assert_eq!(state.tick(), [TickAction::QueueFactSheet]);
        state.jobs = vec![EngineJob::new(
            JobKind::FactSheet,
            JobClass::Upkeep,
            Some("jerry"),
            "fact sheet",
            now(),
        )];
        assert_eq!(state.tick(), nothing());

        state.jobs[0].profile_id = Some("liz".into());
        assert_eq!(
            state.tick(),
            [TickAction::QueueFactSheet],
            "another member's job is not this member's"
        );
        state.fact_sheet_due = false;
        assert_eq!(state.tick(), nothing());
    }

    #[test]
    fn a_pending_capture_has_one_job_and_one_past_its_tries_has_none() {
        let first = Capture::new("jerry", CaptureKind::Screenshot, now());
        let second = Capture::new("jerry", CaptureKind::Photo, now());
        let mut spent = Capture::new("jerry", CaptureKind::Photo, now());
        for _ in 0..MAX_ATTEMPTS {
            spent.count_attempt(now());
        }
        let mut read = Capture::new("jerry", CaptureKind::Photo, now());
        read.state = CaptureState::Read;
        let state = State {
            captures: vec![first.clone(), second.clone(), spent, read],
            jobs: vec![EngineJob::for_capture(
                "jerry",
                &first.id,
                JobKind::Capture,
                now(),
            )],
            ..State::default()
        };
        assert_eq!(
            state.tick(),
            [TickAction::QueueCapture {
                capture_id: second.id.clone()
            }]
        );
    }

    #[test]
    fn a_member_has_two_open_class_jobs_at_most() {
        let interests: Vec<Anchor> = ["Formula 1", "Sourdough", "Jazz"]
            .iter()
            .map(|title| anchor(AnchorKind::Interest, title, None))
            .collect();
        let mut digests: Vec<Deliverable> = interests
            .iter()
            .map(|a| deliverable(a, "digest", DeliverableState::Ready))
            .collect();
        for (hours, digest) in digests.iter_mut().enumerate() {
            digest.refresh_at = Some(now() - Duration::hours(1 + hours as i64));
        }
        let mut state = State {
            anchors: interests.clone(),
            deliverables: digests.clone(),
            ..State::default()
        };
        let queued: Vec<String> = state
            .tick()
            .into_iter()
            .map(|action| match action {
                TickAction::QueueRefresh { deliverable_id, .. } => deliverable_id,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(queued, [digests[2].id.clone(), digests[1].id.clone()]);

        state.jobs = vec![making(&interests[2], &digests[2])];
        assert_eq!(state.tick().len(), 1, "one place is taken");
        state.jobs.push(making(&interests[1], &digests[1]));
        assert_eq!(state.tick(), nothing(), "both are");

        let summit = anchor(AnchorKind::Event, "AI Summit Nairobi", Some(17));
        state.anchors.push(summit);
        let actions = state.tick();
        assert_eq!(actions.len(), 2, "a deadline is not held back: {actions:?}");
    }

    #[test]
    fn a_new_undated_anchor_waits_for_both_its_places_and_holds_them() {
        let old = anchor(AnchorKind::Interest, "Formula 1", None);
        let mut other = anchor(AnchorKind::Interest, "Jazz", None);
        other.created_at = now() - Duration::hours(3);
        let mut new = anchor(AnchorKind::Project, "Pond launch", None);
        new.created_at = now() - Duration::minutes(5);
        let running = deliverable(&old, "digest", DeliverableState::Making);
        let mut due = deliverable(&other, "digest", DeliverableState::Ready);
        due.refresh_at = Some(now() - Duration::hours(1));
        let mut state = State {
            anchors: vec![old.clone(), other, new.clone()],
            jobs: vec![making(&old, &running)],
            deliverables: vec![running, due],
            ..State::default()
        };
        assert_eq!(
            state.tick(),
            nothing(),
            "one place is free: the new anchor needs two, and the refresh must not take it"
        );

        state.jobs.clear();
        state.deliverables[0].state = DeliverableState::Ready;
        let actions = state.tick();
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert!(matches!(
            &actions[0],
            TickAction::QueueQuestions { anchor_id, class: JobClass::Open, due_at: None }
                if *anchor_id == new.id
        ));
        assert_eq!(slot_of(&actions[1]), Some("digest"));
    }

    #[test]
    fn a_queued_deliverable_whose_job_died_is_tried_again_after_a_rest() {
        let trip = anchor(AnchorKind::Trip, "Kisumu in December", Some(30));
        let mut plan = deliverable(&trip, "plan", DeliverableState::Queued);
        plan.updated_at = now() - Duration::hours(5);
        let mut state = State {
            anchors: vec![trip.clone()],
            deliverables: vec![plan.clone()],
            ..State::default()
        };
        assert_eq!(state.tick(), nothing(), "not yet");
        let actions = state.tick_at(now() + Duration::hours(1));
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(slot_of(&actions[0]), Some("plan"));

        state.jobs = vec![making(&trip, &plan)];
        assert_eq!(state.tick_at(now() + Duration::hours(1)), nothing());
    }

    #[test]
    fn an_anchor_only_shared_with_the_member_is_its_owners_to_serve() {
        let mut theirs = anchor(AnchorKind::Trip, "Diani in April", Some(40));
        theirs.profile_id = "liz".into();
        theirs.shared_with = vec!["jerry".into()];
        let mut old = theirs.clone();
        old.id = "another".into();
        old.starts_at = Some(now() - Duration::days(9));
        let state = State {
            anchors: vec![theirs, old],
            ..State::default()
        };
        assert_eq!(state.tick(), nothing());
    }

    #[test]
    fn the_budget_anchor_is_served_from_its_own_recipe() {
        let mut budget = anchor(AnchorKind::Standing, "Budget", None);
        budget.agent = Some(SpecialistId::Budget);
        let state = State {
            anchors: vec![budget],
            ..State::default()
        };
        let actions = state.tick();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(slot_of(&actions[0]), Some("month"));
    }
}
