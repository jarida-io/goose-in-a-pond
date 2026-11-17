use chrono::NaiveDate;

use pond_core::mcp::ports::notification::Notification;

use pond_core::user_data::domain::anchor::title_key;
use pond_core::user_data::domain::capture::{
    Capture, CaptureDetails, CaptureKind, CaptureState, Sensitivity, PICTURE_KEPT_FOR,
};
use pond_core::user_data::domain::specialist::{AgentSwitch, SpecialistId};
use pond_core::user_data::ports::capture_pictures::CapturePictures;
use pond_core::user_data::ports::captures::CaptureRepository;
use pond_core::user_data::ports::ledger::LedgerRepository;
use pond_core::user_data::ports::member_agents::MemberAgentRepository;
use pond_core::user_data::services::capture_reading::{READ_CRITIC_SYSTEM, READ_SYSTEM};
use pond_core::user_data::services::ledger_reading::{LEDGER_CRITIC_SYSTEM, LEDGER_SYSTEM};

use super::*;

const FLIGHT: &str = r#"{"sensitive": "none",
    "description": "A Jambojet flight from Nairobi to Kisumu on 12 December",
    "intent": "to book the flight",
    "details": {"names": ["Jambojet"], "dates": ["12 December"], "places": ["Nairobi", "Kisumu"],
                "prices": ["KSh 8,400"], "links": []},
    "plan": {"kind": "trip", "title": "Kisumu in December", "when": "12 December",
             "where": "Kisumu", "details": "Jambojet, KSh 8,400"}}"#;
const RECEIPT: &str = r#"{"sensitive": "money", "description": "", "intent": "",
    "details": {}, "plan": null}"#;
const AGREE: &str = r#"{"verdict": "agree", "why": ""}"#;
const DISAGREE: &str = r#"{"verdict": "disagree", "why": "the price is KSh 9,400"}"#;
const PAYMENTS: &str = r#"{"entries": [
    {"date": "2025-11-14", "payee": "Naivas", "amount": "KSh 4,200.50", "currency": "KES", "category": "groceries"},
    {"date": "2025-11-14", "payee": "Naivas", "amount": "KSh 4,200.50", "currency": "KES", "category": "groceries"}]}"#;

impl Pond {
    /// A picture the member's phone sent, waiting to be read.
    async fn send(&self, now: DateTime<Utc>) -> Capture {
        let mut capture = Capture::new(&self.member, CaptureKind::Screenshot, now);
        let file = self
            .pictures
            .save(&capture.id, "image/png", b"png")
            .await
            .unwrap();
        capture.picture_file = Some(file);
        capture.content_hash = Some(capture.id.clone());
        assert!(self.captures.add(&capture).await.unwrap());
        capture
    }

    async fn capture(&self, id: &str) -> Option<Capture> {
        self.captures.find(id).await.unwrap()
    }

    /// Ticks, then drives the one open job of `kind` reading `capture` to its end.
    async fn read(&self, kind: JobKind, capture: &Capture, now: DateTime<Utc>) -> EngineJob {
        tick(&self.deps, now).await;
        let job = self
            .jobs
            .open()
            .await
            .unwrap()
            .into_iter()
            .find(|job| job.kind == kind && job.subject == capture.id)
            .unwrap_or_else(|| panic!("no open {kind:?} job for the picture"));
        let task_id = job.task_id.clone().expect("the tick opened its task");
        self.drive(&task_id, now).await;
        assert!(
            self.tasks.get_task(&task_id).await.unwrap().is_none(),
            "nothing the model said about a picture is kept with its task"
        );
        self.job(&job.id).await
    }

    async fn budget(&self) -> AgentSwitch {
        self.agents
            .switch(&self.member, SpecialistId::Budget)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn a_picture_both_agree_on_is_kept_with_its_details_and_its_plan_is_offered() {
    let pond = pond().await;
    let now = Utc::now();
    let sent = pond.send(now).await;
    pond.model.say(FLIGHT);
    pond.model.say(AGREE);

    let job = pond.read(JobKind::Capture, &sent, now).await;

    let asked = pond.model.asked();
    let systems: Vec<&str> = asked.iter().map(|(system, _)| system.as_str()).collect();
    assert_eq!(systems, [READ_SYSTEM, READ_CRITIC_SYSTEM]);
    assert!(
        asked[1].1.contains("Shows: A Jambojet flight"),
        "{}",
        asked[1].1
    );
    assert_eq!(pond.model.seen(), ["image/png", "image/png"], "both saw it");

    let kept = pond.capture(&sent.id).await.expect("the row stays");
    assert_eq!(kept.state, CaptureState::Read);
    assert_eq!(
        kept.description.as_deref(),
        Some("A Jambojet flight from Nairobi to Kisumu on 12 December")
    );
    assert_eq!(kept.intent.as_deref(), Some("to book the flight"));
    assert_eq!(kept.details.prices, ["KSh 8,400"]);
    assert!(kept.is_searchable());
    assert_eq!(kept.picture_until, Some(now + PICTURE_KEPT_FOR));
    assert_eq!(
        pond.pictures.count(),
        1,
        "the picture is kept for its 30 days"
    );
    tokio::time::timeout(
        std::time::Duration::from_millis(50),
        pond.indexed.notified(),
    )
    .await
    .expect("the embedder was told");

    let trip = pond
        .anchors
        .by_title(&pond.member, &title_key("Kisumu in December"))
        .await
        .unwrap()
        .expect("the plan became a candidate anchor");
    assert_eq!(trip.kind, AnchorKind::Trip);
    assert_eq!(trip.state, AnchorState::Candidate);
    assert_eq!(trip.origin, AnchorOrigin::Capture);
    assert_eq!(trip.origin_ref.as_deref(), Some(sent.id.as_str()));
    assert_eq!(trip.place.as_deref(), Some("Kisumu"));

    assert_eq!(job.state, JobState::Passed);
    assert_eq!(job.rounds, 1);
    assert_eq!(
        job.outcome.as_deref(),
        Some("Passed its first check. Kept what it shows, and offered the plan in it.")
    );

    tick(&pond.deps, now + Duration::minutes(1)).await;
    let reads = pond
        .jobs
        .recent(&pond.member, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == JobKind::Capture)
        .count();
    assert_eq!(reads, 1, "a read picture is not read again");
}

#[tokio::test]
async fn a_picture_either_calls_private_leaves_no_row_and_no_file() {
    let pond = pond().await;
    let now = Utc::now();

    let login = pond.send(now).await;
    pond.model
        .say(r#"{"sensitive": "private", "description": "A bank login"}"#);
    let job = pond.read(JobKind::Capture, &login, now).await;
    assert_eq!(
        pond.model.asked().len(),
        1,
        "a private reading needs no check"
    );
    assert!(pond.capture(&login.id).await.is_none());
    assert_eq!(pond.pictures.count(), 0);
    assert_eq!(job.state, JobState::Dropped);
    assert_eq!(
        job.outcome.as_deref(),
        Some("Dropped: the picture was private, so nothing of it was kept.")
    );

    let chat = pond.send(now).await;
    pond.model.say(FLIGHT);
    pond.model.say(r#"{"verdict": "private", "why": "a chat"}"#);
    pond.read(JobKind::Capture, &chat, now).await;
    assert!(pond.capture(&chat.id).await.is_none());
    assert_eq!(pond.pictures.count(), 0);
    assert!(
        pond.anchors
            .by_title(&pond.member, &title_key("Kisumu in December"))
            .await
            .unwrap()
            .is_none(),
        "nothing it showed is offered"
    );
}

#[tokio::test]
async fn a_disagreement_is_read_once_more_and_then_the_picture_is_forgotten() {
    let pond = pond().await;
    let now = Utc::now();
    let sent = pond.send(now).await;

    pond.model.say(FLIGHT);
    pond.model.say(DISAGREE);
    let first = pond.read(JobKind::Capture, &sent, now).await;
    let waiting = pond.capture(&sent.id).await.expect("kept for another read");
    assert_eq!(waiting.state, CaptureState::Pending);
    assert_eq!(waiting.attempts, 1);
    assert_eq!(pond.pictures.count(), 1);
    assert_eq!(first.state, JobState::Dropped);
    assert_eq!(
        first.outcome.as_deref(),
        Some("Dropped: the check disagreed with the reading. It will be read once more.")
    );

    pond.model.say(FLIGHT);
    pond.model.say(DISAGREE);
    let second = pond.read(JobKind::Capture, &sent, now).await;
    assert!(pond.capture(&sent.id).await.is_none());
    assert_eq!(pond.pictures.count(), 0);
    assert!(second
        .outcome
        .as_deref()
        .unwrap()
        .ends_with("It was tried twice, so the picture was deleted."));
    assert!(
        !second.outcome.unwrap().contains("9,400"),
        "the check's own words are not logged"
    );

    let report = tick(&pond.deps, now + Duration::minutes(1)).await;
    assert_eq!(pond.model.asked().len(), 4);
    assert!(pond
        .jobs
        .open()
        .await
        .unwrap()
        .iter()
        .all(|job| job.kind != JobKind::Capture));
    assert_eq!(report.pictures_deleted, 0);
}

#[tokio::test]
async fn a_money_picture_with_budget_on_becomes_ledger_entries_and_leaves_nothing() {
    let pond = pond().await;
    let now = Utc::now();
    pond.agents
        .set(&pond.member, SpecialistId::Budget, AgentSwitch::On, now)
        .await
        .unwrap();
    let receipt = pond.send(now).await;

    pond.model.say(RECEIPT);
    pond.model.say(AGREE);
    let read = pond.read(JobKind::Capture, &receipt, now).await;
    assert_eq!(read.state, JobState::Passed);
    let held = pond.capture(&receipt.id).await.expect("held for Budget");
    assert!(held.hot);
    assert_eq!(held.sensitive, Sensitivity::Money);
    assert_eq!(held.description, None, "nothing about it is indexed");
    assert!(!held.is_searchable());
    let woken = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        pond.indexed.notified(),
    )
    .await;
    assert!(woken.is_err(), "the embedder has nothing to index");

    pond.model.say(PAYMENTS);
    pond.model.say(r#"{"drop": [2]}"#);
    let ledger_job = pond.read(JobKind::Ledger, &receipt, now).await;

    let systems: Vec<String> = pond
        .model
        .asked()
        .into_iter()
        .map(|(system, _)| system)
        .collect();
    assert_eq!(
        systems,
        [
            READ_SYSTEM,
            READ_CRITIC_SYSTEM,
            LEDGER_SYSTEM,
            LEDGER_CRITIC_SYSTEM
        ]
    );
    assert_eq!(
        pond.model.seen().len(),
        4,
        "every call was shown the picture"
    );
    assert!(pond.model.asked()[3]
        .1
        .contains("1. 2025-11-14 | Naivas | 4200.50 KES"));

    let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).unwrap();
    let entries = pond
        .ledger
        .list(&pond.member, day(2000, 1, 1), day(2100, 1, 1))
        .await
        .unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].payee, "Naivas");
    assert_eq!(entries[0].amount_minor, 420_050);
    assert_eq!(entries[0].currency, "KES");
    assert_eq!(entries[0].category, "groceries");

    assert!(pond.capture(&receipt.id).await.is_none(), "deleted whole");
    assert_eq!(pond.pictures.count(), 0);
    assert_eq!(ledger_job.state, JobState::Passed);
    assert_eq!(
        ledger_job.outcome.as_deref(),
        Some("Passed its first check. Saved one payment. The picture was deleted.")
    );
}

#[tokio::test]
async fn with_budget_off_three_are_held_the_offer_is_made_once_and_a_fourth_is_deleted() {
    let pond = pond().await;
    let now = Utc::now();
    let mut sent = Vec::new();
    for _ in 0..3 {
        let receipt = pond.send(now).await;
        pond.model.say(RECEIPT);
        pond.model.say(AGREE);
        pond.read(JobKind::Capture, &receipt, now).await;
        sent.push(receipt);
    }
    for receipt in &sent {
        assert!(pond.capture(&receipt.id).await.unwrap().hot);
    }
    assert_eq!(pond.pictures.count(), 3);
    assert_eq!(pond.budget().await, AgentSwitch::Offered);
    let offers: Vec<Notification> = pond
        .notifier
        .sent()
        .into_iter()
        .map(|(_, notice)| notice)
        .filter(|notice| notice.id.starts_with("agent-offer-budget"))
        .collect();
    assert_eq!(offers.len(), 1);
    let offer = &offers[0];
    assert_eq!(offer.category, "action_required");
    assert_eq!(offer.title, "Keep a record of what you spend?");
    assert!(
        offer
            .body
            .starts_with("You have sent three pictures of receipts or payments."),
        "{}",
        offer.body
    );
    let data = offer.data.as_ref().unwrap();
    assert_eq!(data["action"], "agent_offer");
    assert_eq!(data["agent"], "budget");

    let fourth = pond.send(now).await;
    pond.model.say(RECEIPT);
    pond.model.say(AGREE);
    let job = pond.read(JobKind::Capture, &fourth, now).await;
    assert!(pond.capture(&fourth.id).await.is_none());
    assert_eq!(pond.pictures.count(), 3, "only the fourth went");
    assert_eq!(job.state, JobState::Dropped);
    let offers = pond
        .notifier
        .sent()
        .iter()
        .filter(|(_, notice)| notice.id.starts_with("agent-offer-budget"))
        .count();
    assert_eq!(offers, 1, "the offer is made once");
    assert!(
        pond.jobs
            .open()
            .await
            .unwrap()
            .iter()
            .all(|job| job.kind != JobKind::Ledger),
        "nothing reads them while Budget is not on"
    );

    pond.agents
        .set(&pond.member, SpecialistId::Budget, AgentSwitch::On, now)
        .await
        .unwrap();
    let report = tick(&pond.deps, now + Duration::minutes(1)).await;
    let reading: Vec<String> = pond
        .jobs
        .open()
        .await
        .unwrap()
        .into_iter()
        .filter(|job| job.kind == JobKind::Ledger)
        .map(|job| job.subject)
        .collect();
    assert_eq!(reading.len(), 3, "once on, each held picture is read");
    assert!(report.queued >= 3);
}

#[tokio::test]
async fn a_picture_past_its_time_loses_its_file_and_keeps_its_row() {
    let pond = pond().await;
    let now = Utc::now();
    let mut read = pond.send(now).await;
    read.read(
        "A poster for the AI Summit",
        Some("to go"),
        CaptureDetails::default(),
        now,
    );
    assert!(pond.captures.save(&read).await.unwrap());

    let early = tick(&pond.deps, now + Duration::days(29)).await;
    assert_eq!(early.pictures_deleted, 0);
    assert_eq!(pond.pictures.count(), 1);

    let report = tick(&pond.deps, now + Duration::days(31)).await;
    assert_eq!(report.pictures_deleted, 1);
    assert_eq!(pond.pictures.count(), 0);
    let kept = pond.capture(&read.id).await.expect("the row stays");
    assert_eq!(kept.picture_file, None);
    assert_eq!(kept.picture_until, None);
    assert_eq!(
        kept.description.as_deref(),
        Some("A poster for the AI Summit")
    );
}

#[tokio::test]
async fn a_file_no_capture_names_is_deleted_once_it_is_a_day_old() {
    let pond = pond().await;
    let now = Utc::now();
    let kept = pond.send(now).await;
    let orphan = pond
        .pictures
        .save("gone-member-1", "image/jpeg", b"jpg")
        .await
        .unwrap();
    let fresh = pond
        .pictures
        .save("being-sent-1", "image/jpeg", b"jpg")
        .await
        .unwrap();
    let long_ago = now - Duration::days(2);
    assert!(pond.pictures.backdate(&orphan, long_ago));
    assert!(pond
        .pictures
        .backdate(kept.picture_file.as_deref().unwrap(), long_ago));

    let report = tick(&pond.deps, now).await;
    assert_eq!(report.pictures_deleted, 1);
    let left: Vec<String> = pond
        .pictures
        .kept()
        .await
        .unwrap()
        .into_iter()
        .map(|picture| picture.file)
        .collect();
    assert!(!left.contains(&orphan));
    assert!(
        left.contains(&fresh),
        "a file younger than a day may be mid-save"
    );
    assert!(left.contains(kept.picture_file.as_ref().unwrap()));
}

#[tokio::test]
async fn a_pond_whose_model_reads_no_pictures_leaves_them_waiting_and_makes_no_job() {
    let pond = pond().await;
    let now = Utc::now();
    pond.model.blind.store(true, Ordering::Relaxed);
    let sent = pond.send(now).await;

    tick(&pond.deps, now).await;
    assert!(pond
        .jobs
        .recent(&pond.member, 50)
        .await
        .unwrap()
        .iter()
        .all(|job| job.kind != JobKind::Capture));
    let waiting = pond.capture(&sent.id).await.unwrap();
    assert_eq!(waiting.state, CaptureState::Pending);
    assert_eq!(waiting.attempts, 0);
    assert_eq!(pond.pictures.count(), 1);
}
