use chrono::TimeZone;

use pond_core::mcp::ports::notification::Notification;

use pond_core::user_data::domain::card::{ActionKind, CardKind, CardState};
use pond_core::user_data::ports::cards::CardRepository;
use pond_core::user_data::services::cards::{
    CRITIC_SYSTEM as CARDS_CRITIC, MAKER_SYSTEM as CARDS_MAKER, QUERIES_SYSTEM as CARDS_QUERIES,
};

use super::*;

/// Tuesday 17 November 2026, an hour past the morning set on the pond's UTC clock.
fn morning() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 7, 0, 0).unwrap()
}

const MAKER_REPLY: &str = r#"{"cards": [
  {"kind": "do", "title": "Run the Karura loop this evening", "why": "You run in Karura on weekends",
   "why_source": 2, "when": "today 6 pm", "where": "Karura Forest", "cost": "KSh 600",
   "action": {"kind": "todo", "label": "Add to-do"}, "sources": [2, 3],
   "found": "Entry is KSh 600", "next": "Check the gate times", "caveats": "Rain is possible"},
  {"kind": "next_step", "title": "Pick the summit sessions you want", "why": "You are going to the AI Summit",
   "sources": [1], "when": "Saturday", "action": {"kind": "go_deeper"}},
  {"kind": "ask", "title": "What should I bring to the summit?", "why": "You are going to the AI Summit",
   "sources": [1], "action": {"kind": "chat", "prompt": "What should I bring to the AI Summit?"}},
  {"kind": "do", "title": "Something made up", "why": "You like things", "sources": [9], "effort": "1 hour"},
  {"kind": "do", "title": "Do something fun", "why": "You like fun", "sources": [2], "effort": "1 hour"}
]}"#;

#[tokio::test]
async fn the_morning_set_is_searched_made_checked_stored_and_its_timely_card_pushed_once() {
    let pond = pond().await;
    let now = morning();
    pond.summit(now).await;
    pond.remember("m1", "Runs in Karura Forest on weekends", 0.8)
        .await;
    pond.web.passages.lock().unwrap().push((
        "Karura Forest".to_string(),
        "https://karura.example/".to_string(),
        "Open 6 am to 6 pm. Entry KSh 600.".to_string(),
    ));

    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::Cards).await;
    assert_eq!(job.class, JobClass::Upkeep);
    tick(&pond.deps, now).await;
    assert_eq!(
        pond.jobs
            .open_of_kind(&pond.member, JobKind::Cards)
            .await
            .unwrap()
            .len(),
        1,
        "never two at once"
    );

    pond.model
        .say(r#"{"queries": ["Karura Forest opening hours"]}"#);
    pond.model.say(MAKER_REPLY);
    pond.model.say(r#"{"drop": [4]}"#);
    pond.drive(job.task_id.as_deref().unwrap(), now).await;

    let asked = pond.model.asked();
    let systems: Vec<&str> = asked.iter().map(|(system, _)| system.as_str()).collect();
    assert_eq!(systems, [CARDS_QUERIES, CARDS_MAKER, CARDS_CRITIC]);
    assert!(
        asked[1].1.contains("3. (web) Karura Forest"),
        "{}",
        asked[1].1
    );
    assert_eq!(
        *pond.web.asked.lock().unwrap(),
        ["Karura Forest opening hours"]
    );

    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Passed, "{:?}", job.outcome);
    let shown = pond.cards.shown(&pond.member, now).await.unwrap();
    let titles: Vec<&str> = shown.iter().map(|card| card.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Run the Karura loop this evening",
            "Pick the summit sessions you want",
            "What should I bring to the summit?",
        ]
    );
    let run = &shown[0];
    assert_eq!(run.batch_id, job.id);
    assert_eq!(run.state, CardState::New);
    assert_eq!(run.action.as_ref().unwrap().kind, ActionKind::Todo);
    assert_eq!(
        run.sources[1].url.as_deref(),
        Some("https://karura.example/")
    );
    assert_eq!(shown[1].kind, CardKind::NextStep);
    assert!(shown[1].anchor.is_some(), "a next step names its plan");

    let pushed: Vec<Notification> = pond
        .notifier
        .sent()
        .into_iter()
        .map(|(_, notice)| notice)
        .filter(|notice| notice.id.starts_with("card-"))
        .collect();
    assert_eq!(pushed.len(), 1, "only the card that falls today");
    assert_eq!(pushed[0].category, "info");
    assert_eq!(pushed[0].title, "Run the Karura loop this evening");
    assert_eq!(pushed[0].data.as_ref().unwrap()["open"], "idea_cards");

    tick(&pond.deps, now + Duration::hours(3)).await;
    assert!(
        pond.jobs
            .open_of_kind(&pond.member, JobKind::Cards)
            .await
            .unwrap()
            .is_empty(),
        "once a day"
    );
}

#[tokio::test]
async fn a_draft_with_no_card_its_sources_support_is_dropped_without_a_check() {
    let pond = pond().await;
    let now = morning();
    pond.remember("m1", "Runs in Karura Forest on weekends", 0.8)
        .await;
    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::Cards).await;
    pond.model.say(r#"{"queries": []}"#);
    pond.model
        .say(r#"{"cards": [{"kind": "do", "title": "Fly to Mars", "why": "You", "sources": [7], "effort": "a year"}]}"#);
    pond.drive(job.task_id.as_deref().unwrap(), now).await;

    assert_eq!(pond.model.asked().len(), 2);
    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Dropped);
    assert!(pond.cards.all().is_empty());
}
