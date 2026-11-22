use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::json;

use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
use pond_core::shared::domain::agent_note::{AgentNote, NoteKind};
use pond_core::shared::domain::goose_activity::{ActivityKind, GooseActivity};
use pond_core::shared::domain::session_activity::{PresenceTransition, ProfilePresence};
use pond_core::shared::domain::todo::{
    new_todo, HookCatalog, Place, StateFilter, TodoSource, Transition, Tz,
};
use pond_core::shared::mocks::mock_agent_notes::MockAgentNotes;
use pond_core::shared::mocks::mock_long_task::MockLongTaskRepository;
use pond_core::shared::mocks::mock_todo::MockTodoRepository;
use pond_core::shared::ports::agent_notes::AgentNotesRepository;
use pond_core::shared::ports::goose_activity::GooseActivityRepository;
use pond_core::shared::ports::todo::TodoRepository;
use pond_core::shared::services::goose_thread::{thread_id, GooseThread};
use pond_core::user_data::domain::anchor::{
    Anchor, AnchorKind, AnchorOrigin, Deliverable, DeliverableKind, DeliverableState,
};
use pond_core::user_data::domain::device::{DeviceId, DeviceStateChanged, DeviceStateValue};
use pond_core::user_data::domain::engine_job::{JobClass, JobKind, JobState};
use pond_core::user_data::domain::intention::{Condition, IntentAct, IntentionState};
use pond_core::user_data::domain::message_note::SourceKind;
use pond_core::user_data::domain::profile::{CreateProfileRequest, ProfileScope};
use pond_core::user_data::domain::question::{Question, QuestionPurpose};
use pond_core::user_data::domain::reminder::ReminderDisposition;
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::domain::session::IdentificationSource;
use pond_core::user_data::mocks::mock_anchors::InMemoryAnchorRepository;
use pond_core::user_data::mocks::mock_captures::InMemoryCaptureRepository;
use pond_core::user_data::mocks::mock_dont_list::InMemoryDontListRepository;
use pond_core::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;
use pond_core::user_data::mocks::mock_fact_sheet::InMemoryFactSheetRepository;
use pond_core::user_data::mocks::mock_intentions::InMemoryIntentionRepository;
use pond_core::user_data::mocks::mock_interests::InMemoryInterestRepository;
use pond_core::user_data::mocks::mock_member_agents::InMemoryMemberAgentRepository;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_questions::InMemoryQuestionRepository;
use pond_core::user_data::mocks::mock_reminder::MockReminderRepository;
use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::ports::anchors::AnchorRepository;
use pond_core::user_data::ports::engine_jobs::EngineJobRepository;
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::questions::QuestionRepository;
use pond_core::user_data::ports::reminder_repository::ReminderRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::services::fact_sheet::ToolHint;
use pond_core::user_data::services::intentions::{INTENT_CRITIC_SYSTEM, MAX_WAITING};
use pond_core::user_data::services::proactive_review::{
    heartbeat_brief, FeedbackLedger, HeartbeatShown, MAX_ERRANDS_PER_DAY,
};

use super::carry::NO_TIME_SAID;
use super::*;
use crate::engine_runner::{EngineModel, ModelStop};

/// Monday 16 November 2026, 09:00; a household's timezone is UTC until it is set.
fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
}

fn hours(count: i64) -> DateTime<Utc> {
    now() + Duration::hours(count)
}

/// A model that answers from a script, in order, and keeps what it was asked.
#[derive(Default)]
struct Scripted {
    replies: Mutex<VecDeque<Result<String, ModelStop>>>,
    asked: Mutex<Vec<(String, String)>>,
}

impl Scripted {
    fn say(&self, reply: &str) {
        self.replies
            .lock()
            .unwrap()
            .push_back(Ok(reply.to_string()));
    }

    fn stop(&self, stop: ModelStop) {
        self.replies.lock().unwrap().push_back(Err(stop));
    }

    fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl EngineModel for Scripted {
    async fn complete(&self, system: &str, user: String) -> Result<String, ModelStop> {
        self.asked.lock().unwrap().push((system.to_string(), user));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(ModelStop::Failed("the script ran out".to_string())))
    }
}

#[derive(Default)]
struct Activity(Mutex<Vec<GooseActivity>>);

impl Activity {
    fn all(&self) -> Vec<GooseActivity> {
        self.0.lock().unwrap().clone()
    }
}

#[async_trait]
impl GooseActivityRepository for Activity {
    async fn add(&self, item: &GooseActivity) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(item.clone());
        Ok(())
    }

    async fn list_for(&self, member: &str, limit: usize) -> anyhow::Result<Vec<GooseActivity>> {
        let mut items = self.all();
        items.retain(|item| item.profile_id == member);
        items.reverse();
        items.truncate(limit);
        Ok(items)
    }

    async fn get(&self, member: &str, id: &str) -> anyhow::Result<Option<GooseActivity>> {
        Ok(self
            .all()
            .into_iter()
            .find(|item| item.profile_id == member && item.id == id))
    }

    async fn mark_undone(&self, _: &str, _: &str, _: DateTime<Utc>) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count_since(
        &self,
        member: &str,
        kind: ActivityKind,
        since: DateTime<Utc>,
    ) -> anyhow::Result<usize> {
        Ok(self
            .all()
            .iter()
            .filter(|item| item.profile_id == member && item.kind == kind && item.at >= since)
            .count())
    }

    async fn prune(&self, _: DateTime<Utc>) -> anyhow::Result<u64> {
        Ok(0)
    }
}

struct Things(Vec<ThingChoice>);

#[async_trait]
impl ThingSources for Things {
    async fn things(&self) -> Vec<ThingChoice> {
        self.0.clone()
    }
}

#[derive(Default)]
struct Scheduled(Mutex<Vec<String>>);

#[async_trait]
impl ScheduleWords for Scheduled {
    async fn words_for(&self, _: &str) -> anyhow::Result<Vec<String>> {
        Ok(self.0.lock().unwrap().clone())
    }
}

/// An errand runner that answers from a script and keeps what it was asked to find out.
#[derive(Default)]
struct Lookups {
    answers: Mutex<VecDeque<Looked>>,
    asked: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl Errands for Lookups {
    async fn look_into(
        &self,
        profile_id: &str,
        text: &str,
        _: &str,
        _: DateTime<Utc>,
        _: Tz,
    ) -> Looked {
        self.asked
            .lock()
            .unwrap()
            .push((profile_id.to_string(), text.to_string()));
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Looked::Nothing)
    }
}

struct Pond {
    deps: IntentionDeps,
    intentions: Arc<InMemoryIntentionRepository>,
    todos: Arc<MockTodoRepository>,
    reminders: Arc<MockReminderRepository>,
    notes: Arc<MockAgentNotes>,
    activity: Arc<Activity>,
    jobs: Arc<InMemoryEngineJobRepository>,
    anchors: Arc<InMemoryAnchorRepository>,
    questions: Arc<InMemoryQuestionRepository>,
    sessions: Arc<InMemorySessionStorage>,
    notifier: Arc<MockMemberNotifier>,
    model: Arc<Scripted>,
    lookups: Arc<Lookups>,
    scheduled: Arc<Scheduled>,
    member: String,
}

async fn pond() -> Pond {
    let profiles = Arc::new(MockProfileRepository::new());
    let member = profiles
        .create(CreateProfileRequest {
            display_name: "Jerry".to_string(),
            avatar_emoji: "J".to_string(),
        })
        .await
        .unwrap()
        .id;
    let intentions = Arc::new(InMemoryIntentionRepository::new());
    let todos = Arc::new(MockTodoRepository::new());
    let reminders = Arc::new(MockReminderRepository::new());
    let notes = Arc::new(MockAgentNotes::new());
    let activity = Arc::new(Activity::default());
    let jobs = Arc::new(InMemoryEngineJobRepository::new());
    let anchors = Arc::new(InMemoryAnchorRepository::new());
    let questions = Arc::new(InMemoryQuestionRepository::new());
    let sessions = Arc::new(InMemorySessionStorage::new());
    let notifier = Arc::new(MockMemberNotifier::new().with_devices(&member, &["phone-1"]));
    let model = Arc::new(Scripted::default());
    let lookups = Arc::new(Lookups::default());
    let scheduled = Arc::new(Scheduled::default());
    let engine = EngineDeps {
        tasks: Arc::new(MockLongTaskRepository::new()),
        jobs: jobs.clone(),
        anchors: anchors.clone(),
        facts: Arc::new(InMemoryFactSheetRepository::new()),
        dont: Arc::new(InMemoryDontListRepository::new()),
        questions: questions.clone(),
        captures: Arc::new(InMemoryCaptureRepository::new()),
        agents: Arc::new(InMemoryMemberAgentRepository::new()),
        profiles,
        settings: Arc::new(MockSettingsRepository::new()),
        memories: Arc::new(MockMemoryRepository::new()),
        sessions: sessions.clone(),
        interests: Arc::new(InMemoryInterestRepository::new()),
        cards: Arc::new(pond_core::user_data::mocks::mock_cards::InMemoryCardRepository::new()),
        todos: todos.clone(),
        model: model.clone(),
        recall: None,
        similar: None,
        web: None,
        tools: Arc::new(Vec::<ToolHint>::new),
        notifier: notifier.clone(),
        thread: Some(Arc::new(GooseThread::new(sessions.clone()))),
        bell: None,
        pictures: None,
    };
    let deps = IntentionDeps {
        engine,
        intentions: intentions.clone(),
        todos: todos.clone(),
        reminders: reminders.clone(),
        notes: notes.clone(),
        activity: activity.clone(),
        things: Arc::new(Things(vec![
            ThingChoice {
                source_id: "camera:gate".into(),
                label: "Gate".into(),
                events: vec!["person".into(), "motion".into()],
            },
            ThingChoice {
                source_id: "device:lamp".into(),
                label: "Desk lamp".into(),
                events: vec!["on".into(), "off".into()],
            },
        ])),
        schedules: scheduled.clone(),
        errands: lookups.clone(),
        in_hand: InHand::default(),
    };
    Pond {
        deps,
        intentions,
        todos,
        reminders,
        notes,
        activity,
        jobs,
        anchors,
        questions,
        sessions,
        notifier,
        model,
        lookups,
        scheduled,
        member,
    }
}

impl Pond {
    async fn summit(&self) -> Anchor {
        let mut summit = Anchor::new(
            &self.member,
            AnchorKind::Event,
            "AI Summit Nairobi",
            AnchorOrigin::You,
            now(),
        )
        .unwrap();
        summit.starts_at = Some(now() + Duration::days(17));
        assert!(self.anchors.add(&summit).await.unwrap());
        summit
    }

    async fn made(&self, anchor: &Anchor, slot: &str, state: DeliverableState) -> Deliverable {
        let mut made = Deliverable::new(
            &anchor.id,
            DeliverableKind::Dossier,
            slot,
            &format!("The {slot} for {}", anchor.title),
            now(),
        )
        .unwrap();
        made.state = state;
        assert!(self.anchors.add_deliverable(&made).await.unwrap());
        made
    }

    /// A waiting intention of the member's, due no later than three hours from now.
    async fn waiting(&self, act: IntentAct, text: &str, condition: Condition) -> Intention {
        self.waiting_for(&self.member, act, text, condition).await
    }

    async fn waiting_for(
        &self,
        member: &str,
        act: IntentAct,
        text: &str,
        condition: Condition,
    ) -> Intention {
        let job = Intention::new(member, act, text, condition, "then", hours(3), now()).unwrap();
        assert!(self.intentions.add(&job).await.unwrap());
        job
    }

    async fn stored(&self, job: &Intention) -> Intention {
        self.intentions
            .get(&job.profile_id, &job.id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn thread(&self) -> Vec<String> {
        self.sessions
            .get_messages(&thread_id(Some(&self.member)))
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|line| line.message.content)
            .collect()
    }

    async fn heartbeat_jobs(&self) -> Vec<pond_core::user_data::domain::engine_job::EngineJob> {
        self.jobs.recent(&self.member, 20).await.unwrap()
    }

    async fn review(&self, answer: &str) -> Kept {
        let shown = review_context(&self.deps, &self.member, now()).await;
        close_review(
            &self.deps,
            &self.member,
            answer,
            &shown,
            "- camera events from gate (person) at 08:40\n",
            now(),
            now() + Duration::minutes(2),
        )
        .await
    }
}

fn at_time(at: DateTime<Utc>) -> Condition {
    Condition::Time { at }
}

fn on_thing(source_id: &str, event: &str) -> Condition {
    Condition::Thing {
        source_id: source_id.into(),
        event: event.into(),
    }
}

fn seen(source_id: &str, event: &str) -> ConditionEvent {
    ConditionEvent::Thing {
        source_id: source_id.into(),
        event: event.into(),
    }
}

fn turn(member: &str) -> ConditionEvent {
    ConditionEvent::Chat {
        profile_id: member.into(),
    }
}

const LEAVE: &str = "Leave by 7:15, the summit doors open at 8";

const TWO_JOBS: &str = r#"[
    {"do": "intend", "act": "push", "text": "Leave by 7:15, the summit doors open at 8",
     "when": {"time": "3 December at 7am"}, "until": "3 December at 9am",
     "about": "AI Summit", "why": "The summit is on their calendar"},
    {"do": "intend", "act": "push", "text": "Take the washing in", "when": "later"},
    {"do": "suggest", "trigger_kind": "time", "rationale": "It is late",
     "suggestion": "Turn the porch light off at midnight", "confidence": 0.8}
]"#;

#[tokio::test]
async fn the_review_keeps_a_job_with_a_condition_and_a_deadline_and_refuses_one_with_neither() {
    let pond = pond().await;
    let summit = pond.summit().await;
    pond.model.say(r#"{"drop": []}"#);

    let kept = pond.review(TWO_JOBS).await;

    assert_eq!(kept.read, 2);
    assert_eq!(
        kept.not_kept,
        vec!["its `when` is not one of the four conditions"]
    );
    assert_eq!(kept.stored.len(), 1);
    let waiting = pond
        .intentions
        .list(&pond.member, Some(IntentionState::Waiting))
        .await
        .unwrap();
    assert_eq!(waiting, kept.stored);
    let job = &waiting[0];
    assert_eq!(job.act, IntentAct::Push);
    assert_eq!(job.text, LEAVE);
    assert_eq!(
        job.condition,
        at_time(Utc.with_ymd_and_hms(2026, 12, 3, 7, 0, 0).unwrap())
    );
    assert_eq!(
        job.until,
        Utc.with_ymd_and_hms(2026, 12, 3, 9, 0, 0).unwrap()
    );
    assert_eq!(job.anchor_id.as_deref(), Some(summit.id.as_str()));
    assert_eq!(job.why, "The summit is on their calendar");

    let asked = pond.model.asked();
    assert_eq!(asked.len(), 1, "one check for the whole run");
    assert_eq!(asked[0].0, INTENT_CRITIC_SYSTEM);
    assert!(
        asked[0]
            .1
            .starts_with("What has happened:\n- camera events from gate (person) at 08:40\n"),
        "{}",
        asked[0].1
    );
    assert!(asked[0]
        .1
        .contains("The member's plans:\n- AI Summit Nairobi (event), 3 December\n"));
    assert!(asked[0].1.contains(&format!("1. Tell them: {LEAVE}\n")));
    assert!(
        !asked[0].1.contains("washing"),
        "a refused job is not checked"
    );

    let logged = pond.heartbeat_jobs().await;
    assert_eq!(logged.len(), 1);
    let run = &logged[0];
    assert_eq!(run.kind, JobKind::Heartbeat);
    assert_eq!(run.class, JobClass::Upkeep);
    assert_eq!(run.task_id, None);
    assert_eq!(run.state, JobState::Passed);
    assert_eq!(run.rounds, 1);
    assert_eq!(run.started_at, Some(now()));
    assert_eq!(run.finished_at, Some(now() + Duration::minutes(2)));
    assert_eq!(
        run.outcome.as_deref(),
        Some("Kept 1 of 2 jobs. Not kept: its `when` is not one of the four conditions.")
    );
    assert!(
        pond.jobs.open().await.unwrap().is_empty(),
        "the run is logged settled, so the engine's tick opens no task for it"
    );
    assert!(pond
        .todos
        .list(&pond.member, StateFilter::All)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_critic_that_drops_everything_stores_nothing() {
    let pond = pond().await;
    pond.summit().await;
    pond.model
        .say(r#"{"drop": [{"n": 1, "why": "only remarks on what happened"}]}"#);

    let kept = pond.review(TWO_JOBS).await;

    assert!(kept.stored.is_empty());
    assert!(pond
        .intentions
        .list(&pond.member, None)
        .await
        .unwrap()
        .is_empty());
    let run = &pond.heartbeat_jobs().await[0];
    assert_eq!(run.state, JobState::Dropped);
    assert_eq!(run.rounds, 1);
    assert_eq!(
        run.outcome.as_deref(),
        Some(
            "Kept none of 2 jobs. Not kept: its `when` is not one of the four conditions; \
             the check dropped it: only remarks on what happened."
        )
    );
}

#[tokio::test]
async fn an_unreadable_critic_reply_stores_nothing() {
    let pond = pond().await;
    pond.summit().await;
    pond.model.say("Both jobs look useful to me.");

    let kept = pond.review(TWO_JOBS).await;

    assert!(kept.stored.is_empty());
    assert!(pond
        .intentions
        .list(&pond.member, None)
        .await
        .unwrap()
        .is_empty());
    let run = &pond.heartbeat_jobs().await[0];
    assert_eq!(run.state, JobState::Dropped);
    assert_eq!(run.rounds, 1);
    assert!(run
        .outcome
        .as_deref()
        .unwrap()
        .ends_with("the check's reply could not be read, so none was kept."));
}

#[tokio::test]
async fn a_check_a_turn_cut_short_or_that_failed_stores_nothing() {
    for (stop, why) in [
        (
            ModelStop::Cut,
            "someone began talking before the check finished, so none was kept.",
        ),
        (
            ModelStop::Failed("no model is chosen".into()),
            "the check could not be run, so none was kept.",
        ),
    ] {
        let pond = pond().await;
        pond.summit().await;
        pond.model.stop(stop);

        let kept = pond.review(TWO_JOBS).await;

        assert!(kept.stored.is_empty());
        assert!(pond
            .intentions
            .list(&pond.member, None)
            .await
            .unwrap()
            .is_empty());
        let run = &pond.heartbeat_jobs().await[0];
        assert_eq!(run.state, JobState::Dropped);
        assert_eq!(run.rounds, 0, "the critic gave no verdict");
        assert!(run.outcome.as_deref().unwrap().ends_with(why), "{run:?}");
    }
}

#[tokio::test]
async fn an_answer_that_leaves_no_job_asks_no_critic_and_logs_no_run() {
    let pond = pond().await;
    for answer in [
        "[]",
        "Nothing to do.",
        r#"[{"do": "suggest", "trigger_kind": "time", "rationale": "It is late",
             "suggestion": "Turn the porch light off", "confidence": 0.8}]"#,
    ] {
        assert_eq!(pond.review(answer).await, Kept::default(), "{answer}");
    }
    assert!(pond.model.asked().is_empty());
    assert!(pond.heartbeat_jobs().await.is_empty());
}

#[tokio::test]
async fn jobs_that_all_fail_the_code_checks_are_logged_and_never_reach_the_critic() {
    let pond = pond().await;
    let kept = pond
        .review(
            r#"[{"do": "intend", "act": "refresh", "text": "Bring the dossier up to date",
                 "when": {"chat": true}, "until": "Friday", "about": "the moon"},
                {"do": "intend", "act": "remind", "text": "Call the dentist"},
                {"do": "intend", "act": "push", "text": "Say hello",
                 "when": {"thing": "Gate", "event": "package"}, "until": "Friday"}]"#,
        )
        .await;
    assert_eq!(kept.read, 3);
    assert!(kept.stored.is_empty());
    assert!(pond.model.asked().is_empty());
    let run = &pond.heartbeat_jobs().await[0];
    assert_eq!(run.state, JobState::Dropped);
    assert_eq!(run.rounds, 0);
    assert_eq!(
        run.outcome.as_deref(),
        Some(
            "Kept none of 3 jobs. Not kept: it names no act the heartbeat has, or has no words; \
             it names no one plan of the member's to refresh or ask about; \
             its event is not one the source raises."
        )
    );
}

#[tokio::test]
async fn a_job_names_only_a_place_or_a_thing_this_member_and_pond_have() {
    let pond = pond().await;
    let office = Place::new(&pond.member, "Office", -1.29, 36.82, None, now()).unwrap();
    pond.todos.add_place(&office).await.unwrap();
    pond.model.say(r#"{"drop": []}"#);

    let kept = pond
        .review(
            r#"[{"do": "intend", "act": "push", "text": "Ask the front desk about parking",
                 "when": {"place": "office", "on": "arrive"}, "until": "Friday"},
                {"do": "intend", "act": "push", "text": "Someone is at the gate",
                 "when": {"thing": "gate", "event": "person"}, "until": "tomorrow"},
                {"do": "intend", "act": "push", "text": "Stretch at the gym",
                 "when": {"place": "gym", "on": "arrive"}, "until": "Friday"}]"#,
        )
        .await;

    assert_eq!(
        kept.not_kept,
        vec!["its place is not one saved place of the member's"]
    );
    let conditions: Vec<Condition> = kept
        .stored
        .iter()
        .map(|job| job.condition.clone())
        .collect();
    assert_eq!(
        conditions,
        vec![
            Condition::Place {
                place_id: office.id.clone(),
                transition: Transition::Arrive
            },
            on_thing("camera:gate", "person"),
        ]
    );
}

#[tokio::test]
async fn what_is_waiting_on_a_list_or_scheduled_is_not_kept_again() {
    let pond = pond().await;
    pond.waiting(IntentAct::Push, "Take the bins out", Condition::Chat)
        .await;
    let todo = new_todo(
        &pond.member,
        &json!({"text": "Call the garage"}),
        &HookCatalog::default(),
        TodoSource::Chat,
        None,
        now(),
        zone("UTC"),
    )
    .unwrap();
    pond.todos.create(&todo).await.unwrap();
    pond.scheduled
        .0
        .lock()
        .unwrap()
        .push("Water the plants".into());
    pond.model.say(r#"{"drop": []}"#);

    let job = |text: &str| {
        format!(
            r#"{{"do": "intend", "act": "propose_todo", "text": "{text}",
                 "when": {{"chat": true}}, "until": "Friday"}}"#
        )
    };
    let kept = pond
        .review(&format!(
            "[{},{},{}]",
            job("take the BINS out!"),
            job("Call the garage."),
            job("water the plants")
        ))
        .await;
    assert!(kept.stored.is_empty());
    assert_eq!(
        kept.not_kept,
        vec!["a waiting job, an open to-do or a schedule already says it"; 3]
    );
    assert!(pond.model.asked().is_empty());

    let twice = pond
        .review(&format!(
            "[{},{}]",
            job("Renew the logbook"),
            job("renew the logbook")
        ))
        .await;
    assert_eq!(
        twice.stored.len(),
        1,
        "the second wording of one job is refused"
    );
    assert_eq!(twice.not_kept.len(), 1);
}

#[tokio::test]
async fn a_member_with_a_full_list_of_waiting_jobs_is_left_no_more() {
    let pond = pond().await;
    for n in 0..MAX_WAITING {
        pond.waiting(IntentAct::Push, &format!("Job number {n}"), Condition::Chat)
            .await;
    }
    let kept = pond
        .review(
            r#"[{"do": "intend", "act": "push", "text": "One more thing",
                 "when": {"chat": true}, "until": "Friday"}]"#,
        )
        .await;
    assert!(kept.stored.is_empty());
    assert_eq!(
        kept.not_kept,
        vec!["as many jobs as may wait at once are already waiting"]
    );
    assert!(pond.model.asked().is_empty());
}

#[tokio::test]
async fn a_note_left_by_the_chat_model_is_shown_to_one_review_and_then_deleted() {
    let pond = pond().await;
    let note = |id: &str, kind: NoteKind, owner: Option<&str>, text: &str| AgentNote {
        id: id.into(),
        profile_id: owner.map(str::to_string),
        kind,
        text: text.into(),
        created_at: now() - Duration::days(1),
    };
    for left in [
        note(
            "n1",
            NoteKind::Note,
            Some(&pond.member),
            "Ask how the interview went",
        ),
        note("n2", NoteKind::Note, None, "The pump reading looked high"),
        note(
            "n3",
            NoteKind::Lesson,
            Some(&pond.member),
            "Give times in 24-hour form",
        ),
        note(
            "n4",
            NoteKind::Note,
            Some("someone-else"),
            "Not this member's",
        ),
    ] {
        pond.notes.add(left).await.unwrap();
    }

    let shown = review_context(&pond.deps, &pond.member, now()).await;
    let texts: Vec<&str> = shown.notes.iter().map(|n| n.text.as_str()).collect();
    assert_eq!(
        texts,
        vec!["Ask how the interview went", "The pump reading looked high"]
    );
    let brief = heartbeat_brief(
        now(),
        zone("UTC"),
        &[],
        &FeedbackLedger::empty(),
        &HeartbeatShown {
            checklist: "",
            plans: &shown.plans,
            conditions: &shown.conditions,
            waiting: &shown.waiting,
            notes: &shown.notes,
            watches: &[],
        },
    );
    assert!(brief.contains("- Ask how the interview went (written 15 November)\n"));
    assert!(brief.contains("    Gate: person, motion\n"));

    // The review made nothing of them: they go all the same.
    close_review(&pond.deps, &pond.member, "[]", &shown, "", now(), now()).await;

    let left: Vec<String> = pond.notes.all().await.into_iter().map(|n| n.id).collect();
    assert_eq!(
        left,
        vec!["n3", "n4"],
        "a lesson and another member's note stay"
    );
    assert!(review_context(&pond.deps, &pond.member, now())
        .await
        .notes
        .is_empty());
    let owner = ProfileScope::Owner(pond.member.clone());
    assert!(pond
        .notes
        .list(&owner, NoteKind::Note)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_due_time_intention_fires_once_and_is_settled() {
    let pond = pond().await;
    let job = pond
        .waiting(IntentAct::Push, LEAVE, at_time(hours(1)))
        .await;

    assert_eq!(tick(&pond.deps, now()).await, TickReport::default());
    assert!(pond.notifier.sent().is_empty(), "its time has not come");

    let fired = TickReport {
        fired: 1,
        expired: 0,
    };
    assert_eq!(tick(&pond.deps, hours(1)).await, fired);
    assert_eq!(tick(&pond.deps, hours(2)).await, TickReport::default());

    let sent = pond.notifier.sent();
    assert_eq!(sent.len(), 1, "told once");
    let (to, notice) = &sent[0];
    assert_eq!(to, &pond.member);
    assert_eq!(notice.category, "alert");
    assert_eq!(notice.title, "A heads-up");
    assert_eq!(notice.body, LEAVE);
    let data = notice.data.as_ref().unwrap();
    assert_eq!(data["open"], "chat");
    assert_eq!(data["session_id"], thread_id(Some(&pond.member)).as_str());
    assert_eq!(pond.thread().await, vec![format!("A heads-up. {LEAVE}")]);
    let line = &pond
        .sessions
        .get_messages(&thread_id(Some(&pond.member)))
        .await
        .unwrap()[0];
    assert_eq!(
        line.note.as_ref().unwrap().from.as_ref().unwrap().kind,
        SourceKind::Heartbeat
    );

    let settled = pond.stored(&job).await;
    assert_eq!(settled.state, IntentionState::Done);
    assert_eq!(settled.outcome.as_deref(), Some("Sent to your phone."));
    assert_eq!(settled.settled_at, Some(hours(1)));

    let log = pond.activity.all();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].kind, ActivityKind::Intention);
    assert_eq!(log[0].profile_id, pond.member);
    assert_eq!(log[0].ref_id.as_deref(), Some(job.id.as_str()));
    assert_eq!(
        log[0].summary,
        format!("Told you: {LEAVE}. Sent to your phone.")
    );
    assert_eq!(log[0].at, hours(1));
}

#[tokio::test]
async fn an_intention_past_its_deadline_expires_without_firing() {
    let pond = pond().await;
    let timed = pond
        .waiting(IntentAct::Push, LEAVE, at_time(hours(1)))
        .await;
    let on_chat = pond
        .waiting(IntentAct::LookInto, "When the pool opens", Condition::Chat)
        .await;
    let late = hours(3) + Duration::seconds(1);

    let report = tick(&pond.deps, late).await;

    assert_eq!(
        report,
        TickReport {
            expired: 2,
            fired: 0
        }
    );
    assert!(pond.notifier.sent().is_empty());
    assert!(pond.thread().await.is_empty());
    assert!(pond.lookups.asked.lock().unwrap().is_empty());
    for job in [&timed, &on_chat] {
        let settled = pond.stored(job).await;
        assert_eq!(settled.state, IntentionState::Expired);
        assert_eq!(
            settled.outcome.as_deref(),
            Some("Its time ran out before what it waited for came.")
        );
        assert_eq!(settled.settled_at, Some(late));
    }
    let lines: Vec<String> = pond.activity.all().into_iter().map(|a| a.summary).collect();
    assert!(lines.contains(&format!("Let go, its time ran out: {LEAVE}")));
    assert!(lines.contains(&"Let go, its time ran out: When the pool opens".to_string()));

    assert_eq!(on_event(&pond.deps, &turn(&pond.member), late).await, 0);
    assert_eq!(tick(&pond.deps, late).await, TickReport::default());
}

#[tokio::test]
async fn an_event_after_the_deadline_fires_nothing() {
    let pond = pond().await;
    let job = pond
        .waiting(IntentAct::Push, "Say hello", Condition::Chat)
        .await;
    let late = hours(4);
    assert_eq!(on_event(&pond.deps, &turn(&pond.member), late).await, 0);
    assert_eq!(pond.stored(&job).await.state, IntentionState::Waiting);
    assert!(pond.notifier.sent().is_empty());
}

#[tokio::test]
async fn a_thing_event_fires_only_the_intention_waiting_on_that_source_and_event() {
    let pond = pond().await;
    let person = pond
        .waiting(
            IntentAct::Push,
            "Someone is at the gate",
            on_thing("camera:gate", "person"),
        )
        .await;
    let motion = pond
        .waiting(
            IntentAct::Push,
            "Something moved at the gate",
            on_thing("camera:gate", "motion"),
        )
        .await;
    let lamp = pond
        .waiting(
            IntentAct::Push,
            "The desk lamp is on",
            on_thing("device:lamp", "on"),
        )
        .await;
    let timed = pond
        .waiting(IntentAct::Push, LEAVE, at_time(hours(1)))
        .await;

    assert_eq!(
        on_event(&pond.deps, &seen("camera:gate", "person"), now()).await,
        1
    );

    assert_eq!(pond.stored(&person).await.state, IntentionState::Done);
    for other in [&motion, &lamp, &timed] {
        assert_eq!(pond.stored(other).await.state, IntentionState::Waiting);
    }
    let sent = pond.notifier.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.body, "Someone is at the gate");

    assert_eq!(
        on_event(&pond.deps, &seen("camera:gate", "person"), now()).await,
        0,
        "a settled intention does not fire again"
    );
    assert_eq!(
        on_event(&pond.deps, &seen("camera:porch", "person"), now()).await,
        0
    );
    assert_eq!(pond.notifier.sent().len(), 1);
}

#[test]
fn a_camera_or_device_event_on_the_bus_names_its_source_as_the_catalogue_does() {
    let camera = BusEvent::Camera(CameraEvent {
        id: None,
        camera_id: "gate".into(),
        event_type: "Person".into(),
        confidence: Some(0.9),
        snapshot_path: None,
        metadata: None,
        acknowledged: false,
        created_at: now(),
    });
    assert_eq!(
        thing_event(&camera, now()),
        Some(seen("camera:gate", "person"))
    );
    let lamp = BusEvent::Device(DeviceStateChanged {
        device_id: DeviceId::new("lamp"),
        key: "power".into(),
        value: DeviceStateValue::Bool(true),
        changed_at: now(),
    });
    assert_eq!(thing_event(&lamp, now()), Some(seen("device:lamp", "on")));

    let home = BusEvent::Presence(ProfilePresence {
        profile_id: "jerry".into(),
        transition: PresenceTransition::Arrived,
        source: IdentificationSource::PairedDevice,
        confidence: None,
        session_id: "s1".into(),
        at: now(),
    });
    assert_eq!(
        thing_event(&home, now()),
        None,
        "nobody's coming home fires a job"
    );
}

#[tokio::test]
async fn a_place_crossing_and_a_turn_fire_only_what_waits_on_them() {
    let pond = pond().await;
    let at_office = pond
        .waiting(
            IntentAct::Push,
            "Ask the front desk about parking",
            Condition::Place {
                place_id: "place-1".into(),
                transition: Transition::Arrive,
            },
        )
        .await;
    let on_chat = pond
        .waiting(IntentAct::Push, "The plumber rang back", Condition::Chat)
        .await;
    let theirs = pond
        .waiting_for("amina", IntentAct::Push, "Amina's own", Condition::Chat)
        .await;

    let crossing = |place: &str, transition: Transition| ConditionEvent::Place {
        place_id: place.into(),
        transition,
    };
    assert_eq!(
        on_event(&pond.deps, &crossing("place-1", Transition::Leave), now()).await,
        0
    );
    assert_eq!(
        on_event(&pond.deps, &crossing("place-2", Transition::Arrive), now()).await,
        0
    );
    assert_eq!(
        on_event(&pond.deps, &crossing("place-1", Transition::Arrive), now()).await,
        1
    );
    assert_eq!(pond.stored(&at_office).await.state, IntentionState::Done);
    assert_eq!(pond.stored(&on_chat).await.state, IntentionState::Waiting);

    assert_eq!(on_event(&pond.deps, &turn(&pond.member), now()).await, 1);
    assert_eq!(pond.stored(&on_chat).await.state, IntentionState::Done);
    assert_eq!(pond.stored(&theirs).await.state, IntentionState::Waiting);
    let told: Vec<String> = pond
        .notifier
        .sent()
        .into_iter()
        .map(|(to, notice)| format!("{to}: {}", notice.body))
        .collect();
    assert_eq!(
        told,
        vec![
            format!("{}: Ask the front desk about parking", pond.member),
            format!("{}: The plumber rang back", pond.member),
        ]
    );
}

#[tokio::test]
async fn a_push_about_a_plan_is_titled_with_it_and_opens_it() {
    let pond = pond().await;
    let summit = pond.summit().await;
    let mut job = Intention::new(
        &pond.member,
        IntentAct::Push,
        LEAVE,
        at_time(hours(1)),
        "On Thursday 3 December at 07:00",
        hours(3),
        now(),
    )
    .unwrap();
    job.anchor_id = Some(summit.id.clone());
    pond.intentions.add(&job).await.unwrap();

    tick(&pond.deps, hours(1)).await;

    let (_, notice) = &pond.notifier.sent()[0];
    assert_eq!(notice.title, "AI Summit Nairobi");
    let data = notice.data.as_ref().unwrap();
    assert_eq!(data["open"], "anchor");
    assert_eq!(data["anchor_id"], summit.id.as_str());
    assert_eq!(
        pond.thread().await,
        vec![format!("AI Summit Nairobi. {LEAVE}")]
    );
}

#[tokio::test]
async fn a_member_with_no_phone_still_gets_the_thread_line_and_is_told_so() {
    let pond = pond().await;
    let job = pond
        .waiting_for(
            "amina",
            IntentAct::Push,
            "The plumber rang back",
            Condition::Chat,
        )
        .await;
    assert_eq!(on_event(&pond.deps, &turn("amina"), now()).await, 1);
    assert_eq!(
        pond.stored(&job).await.outcome.as_deref(),
        Some("You have no phone paired, so it is only in your Goose thread.")
    );
    let line = pond
        .sessions
        .get_messages(&thread_id(Some("amina")))
        .await
        .unwrap();
    assert_eq!(line.len(), 1);
}

#[tokio::test]
async fn a_refresh_queues_what_is_shown_of_the_plan_for_its_deadline_and_nothing_twice() {
    let pond = pond().await;
    let summit = pond.summit().await;
    let ready = pond.made(&summit, "dossier", DeliverableState::Ready).await;
    let unsure = pond.made(&summit, "people", DeliverableState::Unsure).await;
    pond.made(&summit, "packing", DeliverableState::Queued)
        .await;
    pond.made(&summit, "route", DeliverableState::Making).await;
    let mut job = Intention::new(
        &pond.member,
        IntentAct::Refresh,
        "Bring the dossier up to date with the final agenda",
        Condition::Chat,
        "then",
        hours(3),
        now(),
    )
    .unwrap();
    job.anchor_id = Some(summit.id.clone());
    pond.intentions.add(&job).await.unwrap();

    assert_eq!(on_event(&pond.deps, &turn(&pond.member), now()).await, 1);

    let queued = pond.jobs.open().await.unwrap();
    let mut refreshed: Vec<&str> = queued
        .iter()
        .map(|job| job.deliverable_id.as_deref().unwrap())
        .collect();
    refreshed.sort_unstable();
    let mut shown = vec![ready.id.as_str(), unsure.id.as_str()];
    shown.sort_unstable();
    assert_eq!(refreshed, shown);
    for job in &queued {
        assert_eq!(job.kind, JobKind::Deliverable);
        assert_eq!(job.class, JobClass::Deadline);
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.task_id, None);
        assert_eq!(job.due_at, summit.starts_at);
        assert_eq!(job.profile_id.as_deref(), Some(pond.member.as_str()));
    }
    assert_eq!(
        pond.stored(&job).await.outcome.as_deref(),
        Some("Making again: The dossier for AI Summit Nairobi, The people for AI Summit Nairobi.")
    );
    assert!(
        pond.notifier.sent().is_empty(),
        "the refresh tells them when it is ready"
    );

    let mut again = Intention::new(
        &pond.member,
        IntentAct::Refresh,
        "Check the dossier once more",
        Condition::Chat,
        "then",
        hours(3),
        now(),
    )
    .unwrap();
    again.anchor_id = Some(summit.id.clone());
    pond.intentions.add(&again).await.unwrap();
    on_event(&pond.deps, &turn(&pond.member), now()).await;
    assert_eq!(
        pond.jobs.open().await.unwrap().len(),
        2,
        "a deliverable never has two open jobs"
    );
}

#[tokio::test]
async fn a_refresh_with_nothing_shown_is_settled_with_that_reason() {
    let pond = pond().await;
    let summit = pond.summit().await;
    pond.made(&summit, "dossier", DeliverableState::Queued)
        .await;
    let mut job = Intention::new(
        &pond.member,
        IntentAct::Refresh,
        "Bring the dossier up to date",
        Condition::Chat,
        "then",
        hours(3),
        now(),
    )
    .unwrap();
    job.anchor_id = Some(summit.id.clone());
    pond.intentions.add(&job).await.unwrap();

    on_event(&pond.deps, &turn(&pond.member), now()).await;

    assert!(pond.jobs.open().await.unwrap().is_empty());
    let settled = pond.stored(&job).await;
    assert_eq!(settled.state, IntentionState::Done);
    assert_eq!(
        settled.outcome.as_deref(),
        Some("Nothing made for AI Summit Nairobi was ready to make again.")
    );
}

#[tokio::test]
async fn asking_pushes_the_plans_open_questions_once_and_is_settled_when_there_are_none() {
    let pond = pond().await;
    let summit = pond.summit().await;
    let ask = |text: &'static str| {
        let mut job = Intention::new(
            &pond.member,
            IntentAct::Ask,
            text,
            Condition::Chat,
            "then",
            hours(3),
            now(),
        )
        .unwrap();
        job.anchor_id = Some(summit.id.clone());
        job
    };

    let early = ask("Ask what they want from the summit");
    pond.intentions.add(&early).await.unwrap();
    on_event(&pond.deps, &turn(&pond.member), now()).await;
    assert!(pond.notifier.sent().is_empty());
    assert_eq!(
        pond.stored(&early).await.outcome.as_deref(),
        Some("There was no open question about AI Summit Nairobi.")
    );

    let options = ["Talks".to_string(), "People".to_string()];
    let batch: Vec<Question> = ["Which sessions matter most?", "Who do you want to meet?"]
        .into_iter()
        .map(|text| {
            let mut question = Question::new(
                &pond.member,
                "batch-1",
                QuestionPurpose::Anchor,
                text,
                &options,
                false,
                now(),
            )
            .unwrap();
            question.anchor_id = Some(summit.id.clone());
            question
        })
        .collect();
    assert_eq!(pond.questions.add_batch(&batch).await.unwrap().len(), 2);

    let later = ask("Ask which sessions they want");
    pond.intentions.add(&later).await.unwrap();
    assert_eq!(on_event(&pond.deps, &turn(&pond.member), hours(1)).await, 1);

    let sent = pond.notifier.sent();
    assert_eq!(sent.len(), 1, "one notice for the whole batch");
    let (to, notice) = &sent[0];
    assert_eq!(to, &pond.member);
    assert_eq!(notice.category, "action_required");
    assert_eq!(notice.title, "Questions about AI Summit Nairobi");
    assert_eq!(notice.body, "Which sessions matter most? And one more.");
    let data = notice.data.as_ref().unwrap();
    assert_eq!(data["open"], "questions");
    assert_eq!(data["anchor_id"], summit.id.as_str());
    assert_eq!(data["batch_id"], "batch-1");
    assert_eq!(
        pond.stored(&later).await.outcome.as_deref(),
        Some("2 questions about AI Summit Nairobi are waiting for your answer.")
    );
    let open = pond.questions.open(&pond.member).await.unwrap();
    assert!(open.iter().all(|q| q.asked_at == Some(hours(1))));
}

#[tokio::test]
async fn propose_todo_offers_and_never_adds() {
    let pond = pond().await;
    let mut job = Intention::new(
        &pond.member,
        IntentAct::ProposeTodo,
        "Renew the car insurance",
        Condition::Chat,
        "then",
        hours(3),
        now(),
    )
    .unwrap();
    job.why = "It lapses this month".into();
    pond.intentions.add(&job).await.unwrap();

    assert_eq!(on_event(&pond.deps, &turn(&pond.member), now()).await, 1);

    assert!(
        pond.todos
            .list(&pond.member, StateFilter::All)
            .await
            .unwrap()
            .is_empty(),
        "the heartbeat never writes a to-do"
    );
    let offers = pond.reminders.rows();
    assert_eq!(offers.len(), 1);
    let offer = &offers[0];
    assert_eq!(offer.about, "Renew the car insurance");
    assert_eq!(offer.when_said, NO_TIME_SAID);
    assert_eq!(offer.profile_id.as_deref(), Some(pond.member.as_str()));
    assert_eq!(offer.subject, "Jerry");
    assert_eq!(offer.disposition, ReminderDisposition::Offered);
    assert_eq!(
        pond.reminders.open_to(&pond.member).await.unwrap().len(),
        1,
        "it waits where the member answers offers"
    );

    let sent = pond.notifier.sent();
    assert_eq!(sent.len(), 1);
    let (to, notice) = &sent[0];
    assert_eq!(to, &pond.member);
    assert_eq!(notice.category, "action_required");
    assert_eq!(
        notice.title,
        "Add 'Renew the car insurance' to your to-dos?"
    );
    assert_eq!(
        notice.body,
        "It lapses this month. Add it and pick when I should remind you."
    );
    assert_eq!(
        notice.data,
        Some(json!({
            "action": "todo_offer",
            "reminder_id": offer.id,
            "text": "Renew the car insurance",
            "when_said": NO_TIME_SAID,
            "due_at": null,
            "label": "Add",
            "open": "todos",
        }))
    );
    let settled = pond.stored(&job).await;
    assert_eq!(settled.state, IntentionState::Done);
    assert_eq!(
        settled.outcome.as_deref(),
        Some("Asked whether to add it to your to-dos.")
    );
    assert_eq!(
        pond.activity.all()[0].summary,
        "Offered you a to-do: Renew the car insurance. Asked whether to add it to your to-dos."
    );
}

#[tokio::test]
async fn a_todo_already_listed_scheduled_or_on_offer_is_not_offered_again() {
    let pond = pond().await;
    let todo = new_todo(
        &pond.member,
        &json!({"text": "Call the garage"}),
        &HookCatalog::default(),
        TodoSource::Chat,
        None,
        now(),
        zone("UTC"),
    )
    .unwrap();
    pond.todos.create(&todo).await.unwrap();
    pond.scheduled
        .0
        .lock()
        .unwrap()
        .push("Water the plants".into());

    let propose = |text: &'static str| {
        Intention::new(
            &pond.member,
            IntentAct::ProposeTodo,
            text,
            Condition::Chat,
            "then",
            hours(3),
            now(),
        )
        .unwrap()
    };
    let listed = propose("call the GARAGE");
    let scheduled = propose("Water the plants!");
    let fresh = propose("Renew the logbook");
    for job in [&listed, &scheduled, &fresh] {
        pond.intentions.add(job).await.unwrap();
    }
    assert_eq!(on_event(&pond.deps, &turn(&pond.member), now()).await, 3);

    assert_eq!(
        pond.stored(&listed).await.outcome.as_deref(),
        Some("It is already on your to-dos, so it was not offered.")
    );
    assert_eq!(
        pond.stored(&scheduled).await.outcome.as_deref(),
        Some("A schedule already does it, so it was not offered.")
    );
    assert_eq!(
        pond.reminders.rows().len(),
        1,
        "only the new one is offered"
    );
    assert_eq!(pond.notifier.sent().len(), 1);

    let again = propose("renew the logbook.");
    // The first offer's intention is settled, so the same words may wait again.
    pond.intentions.add(&again).await.unwrap();
    on_event(&pond.deps, &turn(&pond.member), hours(1)).await;
    assert_eq!(
        pond.stored(&again).await.outcome.as_deref(),
        Some("That offer was already waiting for your answer.")
    );
    assert_eq!(pond.reminders.rows().len(), 1);
    assert_eq!(pond.notifier.sent().len(), 1);
    assert_eq!(
        pond.todos
            .list(&pond.member, StateFilter::All)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn looking_into_something_tells_the_member_what_was_found_as_an_errand_always_did() {
    let pond = pond().await;
    let job = pond
        .waiting(
            IntentAct::LookInto,
            "When the pool opens on Friday",
            Condition::Chat,
        )
        .await;
    pond.lookups
        .answers
        .lock()
        .unwrap()
        .push_back(Looked::Found("It opens at 6am and closes at 8pm.".into()));

    assert_eq!(on_event(&pond.deps, &turn(&pond.member), now()).await, 1);

    assert_eq!(
        pond.lookups.asked.lock().unwrap().clone(),
        vec![(
            pond.member.clone(),
            "When the pool opens on Friday".to_string()
        )]
    );
    assert_eq!(
        pond.thread().await,
        vec!["It opens at 6am and closes at 8pm."]
    );
    let line = &pond
        .sessions
        .get_messages(&thread_id(Some(&pond.member)))
        .await
        .unwrap()[0];
    let from = line.note.as_ref().unwrap().from.as_ref().unwrap();
    assert_eq!(from.kind, SourceKind::Lookup);
    assert_eq!(from.label, "When the pool opens on Friday");

    let sent = pond.notifier.sent();
    assert_eq!(sent.len(), 1);
    let notice = &sent[0].1;
    assert_eq!(notice.category, "update");
    assert_eq!(notice.title, "Looked into: When the pool opens on Friday");
    assert_eq!(notice.body, "It opens at 6am and closes at 8pm.");
    assert_eq!(notice.data.as_ref().unwrap()["open"], "goose_activity");

    let settled = pond.stored(&job).await;
    assert_eq!(settled.state, IntentionState::Done);
    assert_eq!(
        settled.outcome.as_deref(),
        Some("The answer is in your Goose thread.")
    );
    let log = pond.activity.all();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].kind, ActivityKind::Errand);
    assert_eq!(
        log[0].summary,
        "Looked into: When the pool opens on Friday. The answer is in your Goose thread."
    );
    assert_eq!(
        log[0].detail.as_deref(),
        Some("It opens at 6am and closes at 8pm.")
    );
    assert_eq!(log[0].ref_id.as_deref(), Some(job.id.as_str()));
}

#[tokio::test]
async fn a_look_up_that_finds_nothing_or_cannot_run_is_settled_with_why_and_tells_nobody() {
    let pond = pond().await;
    let nothing = pond
        .waiting(IntentAct::LookInto, "What the fare is now", Condition::Chat)
        .await;
    let off = pond
        .waiting(
            IntentAct::LookInto,
            "Whether the road is open",
            at_time(hours(1)),
        )
        .await;
    {
        let mut answers = pond.lookups.answers.lock().unwrap();
        answers.push_back(Looked::Nothing);
        answers.push_back(Looked::NotRun(
            "Not looked up: this pond has helpers turned off.".into(),
        ));
    }

    on_event(&pond.deps, &turn(&pond.member), now()).await;
    tick(&pond.deps, hours(1)).await;

    assert!(pond.notifier.sent().is_empty());
    assert!(pond.thread().await.is_empty());
    assert_eq!(
        pond.stored(&nothing).await.outcome.as_deref(),
        Some("It could not be found out.")
    );
    assert_eq!(
        pond.stored(&off).await.outcome.as_deref(),
        Some("Not looked up: this pond has helpers turned off.")
    );
    let kinds: Vec<ActivityKind> = pond.activity.all().into_iter().map(|a| a.kind).collect();
    assert_eq!(
        kinds,
        vec![ActivityKind::Errand, ActivityKind::Intention],
        "a look-up that ran counts towards the day's; one that did not run does not"
    );
}

#[tokio::test]
async fn a_member_gets_only_so_many_look_ups_in_a_day() {
    let pond = pond().await;
    for n in 0..MAX_ERRANDS_PER_DAY {
        pond.waiting(
            IntentAct::LookInto,
            &format!("Question number {n}"),
            Condition::Chat,
        )
        .await;
        pond.lookups
            .answers
            .lock()
            .unwrap()
            .push_back(Looked::Found(format!("Answer number {n}")));
    }
    assert_eq!(
        on_event(&pond.deps, &turn(&pond.member), now()).await,
        MAX_ERRANDS_PER_DAY
    );

    let extra = pond
        .waiting(IntentAct::LookInto, "One question more", Condition::Chat)
        .await;
    assert_eq!(on_event(&pond.deps, &turn(&pond.member), hours(1)).await, 1);
    assert_eq!(
        pond.lookups.asked.lock().unwrap().len(),
        MAX_ERRANDS_PER_DAY,
        "the extra one was never looked up"
    );
    assert_eq!(
        pond.stored(&extra).await.outcome.as_deref(),
        Some("Not looked up: enough had been looked up for you in one day.")
    );

    // A day on, the member has room again.
    let mut tomorrow = Intention::new(
        &pond.member,
        IntentAct::LookInto,
        "Tomorrow's question",
        Condition::Chat,
        "then",
        now() + Duration::days(3),
        now(),
    )
    .unwrap();
    tomorrow.why = "They asked".into();
    pond.intentions.add(&tomorrow).await.unwrap();
    pond.lookups
        .answers
        .lock()
        .unwrap()
        .push_back(Looked::Found("Tomorrow's answer".into()));
    on_event(
        &pond.deps,
        &turn(&pond.member),
        now() + Duration::days(1) + Duration::minutes(1),
    )
    .await;
    assert_eq!(
        pond.stored(&tomorrow).await.outcome.as_deref(),
        Some("The answer is in your Goose thread.")
    );
}

#[tokio::test]
async fn an_intention_already_in_hand_is_not_settled_by_a_second_tick() {
    let pond = pond().await;
    let job = pond
        .waiting(IntentAct::Push, LEAVE, at_time(hours(1)))
        .await;
    let held = pond
        .deps
        .in_hand
        .take(&job.id)
        .expect("nothing holds it yet");

    assert_eq!(tick(&pond.deps, hours(1)).await, TickReport::default());
    assert_eq!(tick(&pond.deps, hours(4)).await, TickReport::default());
    assert_eq!(pond.stored(&job).await.state, IntentionState::Waiting);

    drop(held);
    assert_eq!(
        tick(&pond.deps, hours(1)).await,
        TickReport {
            fired: 1,
            expired: 0
        }
    );
}
