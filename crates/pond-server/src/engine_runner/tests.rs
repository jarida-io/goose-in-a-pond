mod cards;
mod pictures;
mod rules_and_interests;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};

use pond_core::mcp::mocks::mock_member_notifier::MockMemberNotifier;
use pond_core::models::domain::message::ImageAttachment;
use pond_core::shared::domain::long_task::{
    validate_plan, Artifact, ArtifactKind, LongTask, LongTaskBudget, LongTaskState, OutcomeKind,
    StepDraft, StepOutcome, StepState,
};
use pond_core::shared::mocks::mock_long_task::MockLongTaskRepository;
use pond_core::shared::mocks::mock_todo::MockTodoRepository;
use pond_core::shared::ports::long_task::LongTaskOwnerFilter;
use pond_core::shared::services::goose_thread::{thread_id, GooseThread};
use pond_core::shared::services::long_task_roles::{COMPOSER_ROLE, WORKER_ROLE};
use pond_core::shared::services::long_task_runner::{brief_artifact, next_run, NextRun};
use pond_core::user_data::domain::anchor::{
    Anchor, AnchorFact, AnchorKind, AnchorOrigin, AnchorState, Deliverable, DeliverableDraft,
    DeliverableKind, DeliverableState,
};
use pond_core::user_data::domain::dont_list::{DontRule, RuleOrigin, Strength};
use pond_core::user_data::domain::engine_job::JobClass;
use pond_core::user_data::domain::fact_sheet::{
    Confidence, FactCategory, FactLine, FactSource, FactState,
};
use pond_core::user_data::domain::memory::{MemoryFragment, MemorySegment};
use pond_core::user_data::domain::profile::{CreateProfileRequest, ProfileScope};
use pond_core::user_data::domain::question::QuestionPurpose;
use pond_core::user_data::mocks::mock_anchors::InMemoryAnchorRepository;
use pond_core::user_data::mocks::mock_capture_pictures::InMemoryCapturePictures;
use pond_core::user_data::mocks::mock_captures::InMemoryCaptureRepository;
use pond_core::user_data::mocks::mock_cards::InMemoryCardRepository;
use pond_core::user_data::mocks::mock_dont_list::InMemoryDontListRepository;
use pond_core::user_data::mocks::mock_engine_jobs::InMemoryEngineJobRepository;
use pond_core::user_data::mocks::mock_fact_sheet::InMemoryFactSheetRepository;
use pond_core::user_data::mocks::mock_interests::InMemoryInterestRepository;
use pond_core::user_data::mocks::mock_ledger::InMemoryLedgerRepository;
use pond_core::user_data::mocks::mock_member_agents::InMemoryMemberAgentRepository;
use pond_core::user_data::mocks::mock_memory::MockMemoryRepository;
use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
use pond_core::user_data::mocks::mock_questions::InMemoryQuestionRepository;
use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
use pond_core::user_data::services::engine::critic::CRITIC_SYSTEM;
use pond_core::user_data::services::engine::questions::QUESTIONS_SYSTEM;
use pond_core::user_data::services::engine::revise::REVISE_SYSTEM;
use pond_core::user_data::services::engine::{ROLE_CRITIC, ROLE_MAKE, ROLE_REVISE};
use pond_core::user_data::services::fact_sheet::{
    request_rebuild, CRITIC_SYSTEM as SHEET_CRITIC_SYSTEM, SYSTEM as SHEET_SYSTEM,
};
use pond_core::user_data::services::profile_gaps::PROFILE_QUESTION_SYSTEM;

use super::*;

/// A model that answers from a script, in order, and keeps what it was asked.
struct Scripted {
    replies: Mutex<VecDeque<Result<String, ModelStop>>>,
    asked: Mutex<Vec<(String, String)>>,
    /// The type of each picture it was shown, in order.
    seen: Mutex<Vec<String>>,
    blind: AtomicBool,
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

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl EngineModel for Scripted {
    async fn complete_seeing(
        &self,
        system: &str,
        user: String,
        picture: ImageAttachment,
    ) -> Result<String, ModelStop> {
        self.seen.lock().unwrap().push(picture.mime_type);
        self.complete(system, user).await
    }

    async fn reads_pictures(&self) -> bool {
        !self.blind.load(Ordering::Relaxed)
    }

    async fn complete(&self, system: &str, user: String) -> Result<String, ModelStop> {
        self.asked.lock().unwrap().push((system.to_string(), user));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(ModelStop::Failed("the script ran out".to_string())))
    }
}

/// A web search that answers every query with the same passages and keeps what it was asked.
#[derive(Default)]
struct ScriptedWeb {
    passages: Mutex<Vec<(String, String, String)>>,
    asked: Mutex<Vec<String>>,
}

#[async_trait]
impl WebFinder for ScriptedWeb {
    async fn find(
        &self,
        query: &str,
    ) -> Result<Vec<pond_core::user_data::services::cards::Passage>, String> {
        self.asked.lock().unwrap().push(query.to_string());
        Ok(self
            .passages
            .lock()
            .unwrap()
            .iter()
            .map(
                |(title, url, text)| pond_core::user_data::services::cards::Passage {
                    title: title.clone(),
                    url: url.clone(),
                    text: text.clone(),
                },
            )
            .collect())
    }
}

struct Pond {
    deps: EngineDeps,
    tasks: Arc<MockLongTaskRepository>,
    jobs: Arc<InMemoryEngineJobRepository>,
    anchors: Arc<InMemoryAnchorRepository>,
    facts: Arc<InMemoryFactSheetRepository>,
    dont: Arc<InMemoryDontListRepository>,
    questions: Arc<InMemoryQuestionRepository>,
    captures: Arc<InMemoryCaptureRepository>,
    pictures: Arc<InMemoryCapturePictures>,
    ledger: Arc<InMemoryLedgerRepository>,
    agents: Arc<InMemoryMemberAgentRepository>,
    interests: Arc<InMemoryInterestRepository>,
    cards: Arc<InMemoryCardRepository>,
    web: Arc<ScriptedWeb>,
    indexed: Arc<tokio::sync::Notify>,
    memories: Arc<MockMemoryRepository>,
    sessions: Arc<InMemorySessionStorage>,
    notifier: Arc<MockMemberNotifier>,
    model: Arc<Scripted>,
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
    let tasks = Arc::new(MockLongTaskRepository::new());
    let jobs = Arc::new(InMemoryEngineJobRepository::new());
    let anchors = Arc::new(InMemoryAnchorRepository::new());
    let facts = Arc::new(InMemoryFactSheetRepository::new());
    let dont = Arc::new(InMemoryDontListRepository::new());
    let questions = Arc::new(InMemoryQuestionRepository::new());
    let memories = Arc::new(MockMemoryRepository::new());
    let sessions = Arc::new(InMemorySessionStorage::new());
    let notifier = Arc::new(MockMemberNotifier::new().with_devices(&member, &["phone-1"]));
    let captures = Arc::new(InMemoryCaptureRepository::new());
    let pictures = Arc::new(InMemoryCapturePictures::new());
    let ledger = Arc::new(InMemoryLedgerRepository::new());
    let agents = Arc::new(InMemoryMemberAgentRepository::new());
    let interests = Arc::new(InMemoryInterestRepository::new());
    let cards = Arc::new(InMemoryCardRepository::new());
    let todos = Arc::new(MockTodoRepository::new());
    let web = Arc::new(ScriptedWeb::default());
    let indexed = Arc::new(tokio::sync::Notify::new());
    let model = Arc::new(Scripted {
        replies: Mutex::new(VecDeque::new()),
        asked: Mutex::new(Vec::new()),
        seen: Mutex::new(Vec::new()),
        blind: AtomicBool::new(false),
    });
    let deps = EngineDeps {
        tasks: tasks.clone(),
        jobs: jobs.clone(),
        anchors: anchors.clone(),
        facts: facts.clone(),
        dont: dont.clone(),
        questions: questions.clone(),
        captures: captures.clone(),
        agents: agents.clone(),
        profiles,
        settings: Arc::new(MockSettingsRepository::new()),
        memories: memories.clone(),
        sessions: sessions.clone(),
        interests: interests.clone(),
        cards: cards.clone(),
        todos: todos.clone(),
        model: model.clone(),
        recall: None,
        similar: None,
        web: Some(web.clone()),
        tools: Arc::new(|| vec![ToolHint::new("get_weather", "The forecast for a place")]),
        notifier: notifier.clone(),
        thread: Some(Arc::new(GooseThread::new(sessions.clone()))),
        bell: None,
        pictures: Some(PictureDeps {
            store: pictures.clone(),
            ledger: ledger.clone(),
            indexed: Some(indexed.clone()),
        }),
    };
    Pond {
        deps,
        tasks,
        jobs,
        anchors,
        facts,
        dont,
        questions,
        captures,
        pictures,
        ledger,
        agents,
        interests,
        cards,
        web,
        indexed,
        memories,
        sessions,
        notifier,
        model,
        member,
    }
}

impl Pond {
    async fn remember(&self, id: &str, content: &str, importance: f32) {
        let mut memory = MemoryFragment::from_extraction(
            id.to_string(),
            None,
            content.to_string(),
            MemorySegment::Identity,
            importance,
            None,
        );
        memory.profile_id = Some(self.member.clone());
        self.memories.add(memory).await.unwrap();
    }

    async fn summit(&self, now: DateTime<Utc>) -> Anchor {
        let mut summit = Anchor::new(
            &self.member,
            AnchorKind::Event,
            "AI Summit Nairobi",
            AnchorOrigin::You,
            now,
        )
        .unwrap();
        summit.starts_at = Some(now + Duration::days(17));
        assert!(self.anchors.add(&summit).await.unwrap());
        summit
    }

    async fn job_of(&self, kind: JobKind) -> EngineJob {
        self.jobs
            .recent(&self.member, 50)
            .await
            .unwrap()
            .into_iter()
            .find(|job| job.kind == kind)
            .unwrap_or_else(|| panic!("no {kind:?} job"))
    }

    /// The job that makes the summit's dossier, not the everyday anchor's shortlist.
    async fn dossier_job(&self, summit: &Anchor) -> EngineJob {
        self.jobs
            .recent(&self.member, 50)
            .await
            .unwrap()
            .into_iter()
            .find(|job| {
                job.kind == JobKind::Deliverable
                    && job.anchor_id.as_deref() == Some(summit.id.as_str())
            })
            .expect("the summit's dossier has a job")
    }

    /// A set made already today, so the tick queues no morning set whatever the hour.
    async fn made_todays_cards(&self, now: DateTime<Utc>) {
        let mut made = EngineJob::new(
            JobKind::Cards,
            JobClass::Upkeep,
            Some(&self.member),
            "",
            now,
        );
        made.start(None, now).unwrap();
        made.settle(JobState::Passed, "made", now).unwrap();
        self.jobs.add(&made).await.unwrap();
    }

    async fn job(&self, id: &str) -> EngineJob {
        self.jobs.get(id).await.unwrap().unwrap()
    }

    async fn task(&self, id: &str) -> LongTask {
        self.tasks.get_task(id).await.unwrap().unwrap()
    }

    async fn all_tasks(&self) -> Vec<LongTask> {
        self.tasks
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await
            .unwrap()
    }

    async fn deliverable(&self, id: &str) -> Deliverable {
        self.anchors.find_deliverable(id).await.unwrap().unwrap()
    }

    /// Stands in for the planner and the helpers of a research task: one step that gathers
    /// and one that writes, each finished with what it saved.
    async fn research(&self, task_id: &str, findings: &str, result: &str, now: DateTime<Utc>) {
        let task = self.task(task_id).await;
        let plan = validate_plan(
            &task,
            vec![
                StepDraft {
                    key: "gather".to_string(),
                    role: WORKER_ROLE.to_string(),
                    instructions: "Read the programme and the venue's page.".to_string(),
                    ..StepDraft::default()
                },
                StepDraft {
                    key: "write".to_string(),
                    role: COMPOSER_ROLE.to_string(),
                    instructions: "Write the dossier from the findings.".to_string(),
                    depends_on: vec!["gather".to_string()],
                    ..StepDraft::default()
                },
            ],
            &[],
        )
        .unwrap();
        self.tasks.replace_open_steps(&plan).await.unwrap();
        self.tasks
            .set_task_state(task_id, LongTaskState::Running, now)
            .await
            .unwrap();
        for (key, kind, title, body) in [
            ("gather", ArtifactKind::Findings, "Programme", findings),
            ("write", ArtifactKind::Draft, "Result", result),
        ] {
            let mut step = self
                .tasks
                .list_steps(task_id)
                .await
                .unwrap()
                .into_iter()
                .find(|step| step.key().as_str() == key)
                .unwrap();
            step.start(now).unwrap();
            self.tasks.update_step(&step).await.unwrap();
            let saved = Artifact::new(
                task_id,
                Some(step.key().clone()),
                kind,
                title,
                "what the step saved",
                body.to_string(),
                Utc::now(),
            )
            .unwrap();
            self.tasks.insert_artifact(&saved).await.unwrap();
            let done = StepOutcome::new(
                OutcomeKind::Completed,
                "done",
                vec![saved.id().to_string()],
                None,
            );
            step.record(&done, now).unwrap();
            self.tasks.update_step(&step).await.unwrap();
        }
    }

    /// Advances one task the way the runner does on a pond with helpers off, until it ends
    /// or its next run needs a helper.
    async fn drive(&self, task_id: &str, now: DateTime<Utc>) {
        for _ in 0..40 {
            // A picture job's settle deletes its task.
            let Some(task) = self.tasks.get_task(task_id).await.unwrap() else {
                return;
            };
            if task.state().is_terminal() {
                return;
            }
            let task = begin(&self.deps, task, now).await;
            let steps = self.tasks.list_steps(task_id).await.unwrap();
            match next_run(&task, &steps, 0, now) {
                NextRun::Do(step) if is_inline_role(step.role()) => {
                    run_inline_step(&self.deps, &task, step, now).await.unwrap();
                }
                NextRun::Finish => {
                    at_finish(&self.deps, &task, now).await.unwrap();
                }
                NextRun::Plan(_) => {
                    let instead = instead_of_planning(&self.deps, &task, &steps, now)
                        .await
                        .unwrap();
                    if instead.is_none() {
                        return;
                    }
                }
                NextRun::GiveUp(reason) => {
                    give_up(&self.deps, &task, &reason, now).await.unwrap();
                }
                _ => return,
            }
        }
        panic!("the task never came to an end");
    }

    async fn roles_run(&self, task_id: &str) -> Vec<String> {
        self.tasks
            .list_steps(task_id)
            .await
            .unwrap()
            .iter()
            .map(|step| format!("{}:{}", step.role(), step.state().as_str()))
            .collect()
    }
}

const FINDINGS: &str = "Keynote 9am in the Tsavo ballroom. Workshops from 11am. \
                        Doors open at 8am at KICC, Harambee Avenue.";
const DRAFT: &str = "# Dossier for AI Summit Nairobi\n\n\
                     The keynote is at 10am in the Tsavo ballroom, and three sessions fit you.\n\n\
                     ## Getting there\n\nDoors open at 8am at KICC, Harambee Avenue.";
const REVISED: &str = "# Dossier for AI Summit Nairobi\n\n\
                       The keynote is at 9am in the Tsavo ballroom, and three sessions fit you.\n\n\
                       ## Getting there\n\nDoors open at 8am at KICC, Harambee Avenue.";
const FAIL: &str = r#"{"verdict": "fail", "objections": [{"kind": "grounding", "about": "the keynote at 10am", "fix": "the programme says 9am"}]}"#;
const PASS: &str = r#"{"verdict": "pass", "objections": []}"#;

#[tokio::test]
async fn a_queued_fact_sheet_job_runs_its_maker_and_critic_and_replaces_the_sheet() {
    let pond = pond().await;
    let now = Utc::now();
    pond.remember("m1", "Lives in Nairobi with two children", 0.9)
        .await;
    pond.remember("m2", "Works on assistants that run on small devices", 0.6)
        .await;
    let stale = FactLine::new(
        &pond.member,
        FactCategory::Home,
        "Lives in Kisumu",
        Confidence::Likely,
        FactSource::new(
            pond_core::user_data::domain::fact_sheet::FactSourceKind::Memory,
            Some("gone"),
        ),
        now,
    )
    .unwrap();
    pond.facts.write(&stale).await.unwrap();
    request_rebuild(pond.jobs.as_ref(), &pond.member, JobClass::Upkeep, now)
        .await
        .unwrap();

    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::FactSheet).await;
    let task_id = job.task_id.clone().expect("the tick opened its task");
    assert!(is_engine_task(&task_id));
    assert_eq!(pond.task(&task_id).await.state(), LongTaskState::Running);

    pond.model.say(
        r#"{"lines": [
            {"category": "home", "fact": "Lives in Nairobi with two children", "confidence": "sure", "since": "", "sources": [1], "tools": ["get_weather", "book_flight"]},
            {"category": "work", "fact": "Is curious about the pond's own model", "confidence": "said", "since": "", "sources": [2], "tools": []}]}"#,
    );
    pond.model
        .say(r#"{"drop": [{"line": 2, "why": "about the pond, not about them"}]}"#);
    pond.model.say(
        r#"{"header": "Work", "text": "What fills most of your weekdays?", "options": [{"label": "Office work", "description": "Goose plans around office hours"}, {"label": "Studying", "description": "Goose keeps term dates in mind"}]}"#,
    );
    pond.drive(&task_id, now).await;

    let asked = pond.model.asked();
    assert_eq!(
        asked.len(),
        3,
        "one call to make it, one to check it, and one to word the gap question"
    );
    assert_eq!(asked[2].0, PROFILE_QUESTION_SYSTEM);
    assert!(
        asked[2]
            .1
            .starts_with("Detail to ask about: what fills their weekdays"),
        "the rebuilt sheet holds a home line, so the gap is work: {}",
        asked[2].1
    );
    let gap = pond.questions.open(&pond.member).await.unwrap();
    assert_eq!(gap.len(), 1, "{gap:?}");
    assert_eq!(gap[0].purpose, QuestionPurpose::Profile);
    assert_eq!(gap[0].gap.as_deref(), Some("work"));
    assert_eq!(asked[0].0, SHEET_SYSTEM);
    assert!(
        asked[0].1.contains("1. (memory) Lives in Nairobi"),
        "{}",
        asked[0].1
    );
    assert!(asked[0].1.contains("- get_weather: The forecast"));
    assert_eq!(asked[1].0, SHEET_CRITIC_SYSTEM);
    assert!(
        asked[1].1.contains("2. [work] Is curious"),
        "{}",
        asked[1].1
    );

    let sheet = pond
        .facts
        .list(&pond.member, FactState::Active)
        .await
        .unwrap();
    assert_eq!(sheet.len(), 1, "{sheet:?}");
    assert_eq!(sheet[0].fact, "Lives in Nairobi with two children");
    assert_eq!(
        sheet[0].tools,
        ["get_weather"],
        "a tool the pond lacks is taken off"
    );
    let retired = pond
        .facts
        .list(&pond.member, FactState::Retired)
        .await
        .unwrap();
    assert_eq!(retired.len(), 1, "the line no source supports is retired");

    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Passed);
    assert_eq!(job.rounds, 1);
    let outcome = job.outcome.unwrap();
    assert!(
        outcome.contains("holds one line; the check dropped one line"),
        "{outcome}"
    );
    assert_eq!(pond.task(&task_id).await.state(), LongTaskState::Completed);
    assert_eq!(
        pond.roles_run(&task_id).await,
        [
            format!("{ROLE_MAKE}:completed"),
            format!("{ROLE_CRITIC}:completed")
        ]
    );
    assert!(
        pond.notifier.sent().is_empty(),
        "a rebuilt sheet tells nobody, and its gap question is not pushed"
    );

    request_rebuild(pond.jobs.as_ref(), &pond.member, JobClass::Upkeep, now)
        .await
        .unwrap();
    tick(&pond.deps, now).await;
    let again = pond
        .jobs
        .recent(&pond.member, 50)
        .await
        .unwrap()
        .into_iter()
        .find(|j| j.kind == JobKind::FactSheet && j.id != job.id)
        .unwrap();
    pond.model.say(
        r#"{"lines": [{"category": "home", "fact": "Lives in Nairobi with two children", "confidence": "sure", "since": "", "sources": [1], "tools": []}]}"#,
    );
    pond.model.say(r#"{"drop": []}"#);
    pond.drive(again.task_id.as_deref().unwrap(), now).await;
    assert_eq!(
        pond.model.asked().len(),
        5,
        "one gap question a day: the second rebuild words none"
    );
}

#[tokio::test]
async fn a_fact_sheet_check_that_cannot_be_read_leaves_the_sheet_as_it_was() {
    let pond = pond().await;
    let now = Utc::now();
    pond.remember("m1", "Lives in Nairobi with two children", 0.9)
        .await;
    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::FactSheet).await;
    let task_id = job.task_id.clone().unwrap();

    pond.model.say(
        r#"{"lines": [{"category": "home", "fact": "Lives in Nairobi", "confidence": "sure", "sources": [1], "tools": []}]}"#,
    );
    pond.model.say("I think the sheet looks fine.");
    pond.drive(&task_id, now).await;

    assert!(pond
        .facts
        .list(&pond.member, FactState::Active)
        .await
        .unwrap()
        .is_empty());
    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Dropped);
    assert!(
        job.outcome
            .as_deref()
            .unwrap()
            .contains("could not be read"),
        "{:?}",
        job.outcome
    );
}

#[tokio::test]
async fn a_tick_opens_one_task_for_each_queued_job_and_never_a_second_for_a_deliverable() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    pond.made_todays_cards(now).await;

    let first = tick(&pond.deps, now).await;
    assert_eq!(
        (first.queued, first.opened),
        (4, 4),
        "the fact sheet, the summit's questions and dossier, and the everyday shortlist"
    );
    let everyday = pond
        .anchors
        .list(&pond.member, &[AnchorState::Active])
        .await
        .unwrap()
        .into_iter()
        .find(|anchor| anchor.kind == AnchorKind::Standing)
        .expect("the member has an everyday anchor");
    assert_eq!(everyday.title, EVERYDAY_TITLE);
    assert_eq!(everyday.origin, AnchorOrigin::Goose);

    let open = pond.jobs.open().await.unwrap();
    assert_eq!(open.len(), 4);
    let mut task_ids: Vec<String> = open
        .iter()
        .map(|job| job.task_id.clone().expect("every queued job got a task"))
        .collect();
    task_ids.sort();
    task_ids.dedup();
    assert_eq!(task_ids.len(), 4, "each job has a task of its own");
    assert!(task_ids.iter().all(|id| is_engine_task(id)));

    let dossier = pond.dossier_job(&summit).await;
    let made = pond
        .deliverable(dossier.deliverable_id.as_deref().unwrap())
        .await;
    assert_eq!(made.state, DeliverableState::Making);
    assert_eq!(made.task_id, dossier.task_id);
    assert_eq!(made.title, "Dossier for AI Summit Nairobi");
    assert_eq!(dossier.class, JobClass::Deadline);
    let task = pond.task(dossier.task_id.as_deref().unwrap()).await;
    assert_eq!(task.state(), LongTaskState::Planning);
    let artifacts = pond.tasks.list_artifacts(task.id()).await.unwrap();
    let brief = pond
        .tasks
        .get_artifact(task.id(), &brief_artifact(&artifacts).unwrap().id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        brief.body().contains("Title: AI Summit Nairobi"),
        "{}",
        brief.body()
    );
    assert!(brief.body().contains("How to do it well:"), "the playbook");

    for _ in 0..3 {
        let again = tick(&pond.deps, now + Duration::minutes(1)).await;
        assert_eq!(again, TickReport::default(), "nothing is queued twice");
    }
    assert_eq!(pond.all_tasks().await.len(), 4);
    assert_eq!(pond.jobs.open().await.unwrap().len(), 4);
    assert_eq!(
        pond.jobs
            .open_for_deliverable(&made.id)
            .await
            .unwrap()
            .unwrap()
            .id,
        dossier.id
    );
    assert_eq!(
        pond.anchors
            .deliverables_of(&summit.id)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_draft_that_fails_once_is_corrected_and_then_passes() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    let rule = DontRule::new(
        &pond.member,
        "recipes built on a colour preference",
        Strength::Never,
        RuleOrigin::You,
        now,
    )
    .unwrap();
    pond.dont.add(&rule).await.unwrap();
    let line =
        FactLine::written_by_member(&pond.member, FactCategory::Home, "Lives in Nairobi", now)
            .unwrap();
    pond.facts.write(&line).await.unwrap();
    tick(&pond.deps, now).await;
    let job = pond.dossier_job(&summit).await;
    let task_id = job.task_id.clone().unwrap();
    let deliverable_id = job.deliverable_id.clone().unwrap();

    pond.drive(&task_id, now).await;
    assert_eq!(
        pond.task(&task_id).await.state(),
        LongTaskState::Planning,
        "its research is the planner's and the helpers'"
    );
    assert!(pond.model.asked().is_empty());

    pond.research(&task_id, FINDINGS, DRAFT, now).await;
    pond.model.say(FAIL);
    pond.model
        .say(&format!("Here is the corrected draft:\n{REVISED}"));
    pond.model.say(PASS);
    pond.drive(&task_id, now).await;

    let asked = pond.model.asked();
    let systems: Vec<&str> = asked.iter().map(|(system, _)| system.as_str()).collect();
    assert_eq!(systems, [CRITIC_SYSTEM, REVISE_SYSTEM, CRITIC_SYSTEM]);
    let first_check = &asked[0].1;
    assert!(
        first_check.contains("Title: AI Summit Nairobi"),
        "the brief: {first_check}"
    );
    assert!(
        first_check.contains("Keynote 9am in the Tsavo ballroom"),
        "the notes"
    );
    assert!(first_check.contains("Home: Lives in Nairobi"), "the sheet");
    assert!(
        first_check.contains("Never: recipes built on a colour preference"),
        "the don't-list"
    );
    assert!(
        first_check.contains("The keynote is at 10am"),
        "the draft: {first_check}"
    );
    assert!(
        asked[1].1.contains("the programme says 9am"),
        "the objection"
    );
    assert!(
        asked[2].1.contains("The keynote is at 9am") && !asked[2].1.contains("at 10am"),
        "the second check reads the corrected draft: {}",
        asked[2].1
    );

    let made = pond.deliverable(&deliverable_id).await;
    assert_eq!(made.state, DeliverableState::Ready);
    assert_eq!(made.version, 1);
    assert_eq!(made.body, REVISED);
    assert!(made.objections.is_empty());
    assert!(
        made.hook.starts_with("The keynote is at 9am"),
        "{}",
        made.hook
    );
    assert!(made.refresh_at.is_some_and(|at| at > now));

    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Passed);
    assert_eq!(job.rounds, 2);
    assert_eq!(job.outcome.as_deref(), Some("Passed after 2 checks."));
    assert_eq!(pond.task(&task_id).await.state(), LongTaskState::Completed);
    let roles = pond.roles_run(&task_id).await;
    assert_eq!(
        &roles[2..],
        [
            format!("{ROLE_CRITIC}:completed"),
            format!("{ROLE_REVISE}:completed"),
            format!("{ROLE_CRITIC}:completed")
        ]
    );

    let sent = pond.notifier.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (to, notice) = &sent[0];
    assert_eq!(to, &pond.member);
    assert_eq!(notice.category, "info");
    assert_eq!(notice.title, "Ready: Dossier for AI Summit Nairobi");
    let data = notice.data.as_ref().unwrap();
    assert_eq!(data["open"], "deliverable");
    assert_eq!(data["deliverable_id"], deliverable_id.as_str());
    assert_eq!(data["speak_if_unfetched"], true);
    let thread = pond
        .sessions
        .get_messages(&thread_id(Some(&pond.member)))
        .await
        .unwrap();
    assert_eq!(thread.len(), 1);
    assert!(
        thread[0]
            .message
            .content
            .starts_with("Ready: Dossier for AI Summit Nairobi. The keynote is at 9am"),
        "{}",
        thread[0].message.content
    );
}

#[tokio::test]
async fn a_draft_that_keeps_failing_is_shown_unsure_with_what_the_check_still_held() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    tick(&pond.deps, now).await;
    let job = pond.dossier_job(&summit).await;
    let task_id = job.task_id.clone().unwrap();
    pond.research(&task_id, FINDINGS, DRAFT, now).await;
    for reply in [FAIL, DRAFT, FAIL, DRAFT, FAIL] {
        pond.model.say(reply);
    }
    pond.drive(&task_id, now).await;

    assert_eq!(
        pond.model.asked().len(),
        5,
        "three checks and the two corrections between them"
    );
    let made = pond
        .deliverable(job.deliverable_id.as_deref().unwrap())
        .await;
    assert_eq!(made.state, DeliverableState::Unsure);
    assert_eq!(made.version, 1);
    assert!(made.body.contains("The keynote is at 10am"));
    assert_eq!(made.objections.len(), 1);
    assert_eq!(made.objections[0].about, "the keynote at 10am");

    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Unsure);
    assert_eq!(job.rounds, 3);
    assert_eq!(job.objections.len(), 1);
    let outcome = job.outcome.unwrap();
    assert!(
        outcome.starts_with("Shown unsure after 3 checks; still held: grounding"),
        "{outcome}"
    );
    assert_eq!(pond.task(&task_id).await.state(), LongTaskState::Completed);

    let sent = pond.notifier.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].1.title,
        "Ready, not fully checked: Dossier for AI Summit Nairobi"
    );
    assert!(
        sent[0]
            .1
            .body
            .ends_with("One point in it is not confirmed."),
        "{}",
        sent[0].1.body
    );
}

#[tokio::test]
async fn a_call_a_persons_turn_cut_is_asked_again_without_spending_a_try() {
    let pond = pond().await;
    let now = Utc::now();
    pond.remember("m1", "Lives in Nairobi with two children", 0.9)
        .await;
    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::FactSheet).await;
    let task_id = job.task_id.clone().unwrap();

    pond.model.stop(ModelStop::Cut);
    let task = begin(&pond.deps, pond.task(&task_id).await, now).await;
    let steps = pond.tasks.list_steps(&task_id).await.unwrap();
    assert!(run_inline_step(&pond.deps, &task, &steps[0], now)
        .await
        .unwrap());
    let step = &pond.tasks.list_steps(&task_id).await.unwrap()[0];
    assert_eq!(step.state(), StepState::Pending);
    assert_eq!(step.attempts(), 0);
    assert!(pond
        .tasks
        .list_artifacts(&task_id)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(pond.job(&job.id).await.state, JobState::Running);

    pond.model
        .stop(ModelStop::Failed("the model ran out of room".into()));
    run_inline_step(&pond.deps, &task, step, now).await.unwrap();
    let step = &pond.tasks.list_steps(&task_id).await.unwrap()[0];
    assert_eq!(step.state(), StepState::Failed);
    assert_eq!(step.attempts(), 1);
    assert_eq!(step.failure(), Some("the model ran out of room"));
}

#[tokio::test]
async fn a_job_whose_maker_cannot_run_settles_failed_and_is_not_handed_to_the_planner() {
    let pond = pond().await;
    let now = Utc::now();
    let review = EngineJob::new(
        JobKind::Heartbeat,
        JobClass::Asked,
        Some(&pond.member),
        "a look at the day",
        now,
    );
    pond.jobs.add(&review).await.unwrap();
    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::Heartbeat).await;
    let task_id = job.task_id.clone().expect("it is opened like any job");
    pond.drive(&task_id, now).await;

    assert!(pond.model.asked().is_empty(), "no case, no model call");
    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(
        job.outcome.as_deref(),
        Some("Failed: this pond cannot run this job yet.")
    );
    assert_eq!(pond.task(&task_id).await.state(), LongTaskState::Failed);
    assert_eq!(
        pond.roles_run(&task_id).await,
        [format!("{ROLE_MAKE}:skipped")]
    );
    assert!(!runs(JobKind::Heartbeat) && runs(JobKind::Rule));
}

#[tokio::test]
async fn the_questions_that_survive_their_check_are_stored_and_pushed_once() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::Questions).await;
    assert_eq!(job.anchor_id.as_deref(), Some(summit.id.as_str()));
    let task_id = job.task_id.clone().unwrap();

    pond.model.say(
        r#"{"questions": [
            {"gap": "who", "header": "Who", "text": "Who is coming with you?", "options": [{"label": "Just me", "description": "One ticket"}, {"label": "A colleague", "description": "Two tickets"}], "recommended": "Just me"},
            {"text": "Do you like advanced physics?", "header": "Physics", "options": ["Yes", "No"]},
            {"gap": "place", "header": "Place", "text": "Where is the summit?", "options": ["KICC", "Elsewhere"]},
            {"gap": "budget", "header": "Budget", "text": "How much will you spend?", "options": ["Under KSh 5,000", "More"]}]}"#,
    );
    pond.model.say(r#"{"drop": [2]}"#);
    pond.drive(&task_id, now).await;

    let asked = pond.model.asked();
    assert_eq!(asked[0].0, QUESTIONS_SYSTEM);
    assert!(
        asked[0]
            .1
            .contains("Details to ask about:\n- who: who is coming\n- budget"),
        "code picked the gaps; the summit's date settles `when`: {}",
        asked[0].1
    );
    let open = pond.questions.open(&pond.member).await.unwrap();
    let texts: Vec<&str> = open.iter().map(|q| q.text.as_str()).collect();
    assert_eq!(
        texts,
        ["Who is coming with you?", "How much will you spend?"],
        "the question with no gap is ignored and the critic dropped the place"
    );
    assert_eq!(open[0].header, "Who");
    assert_eq!(open[0].gap.as_deref(), Some("who"));
    assert_eq!(open[0].recommended, Some(0));
    assert_eq!(open[0].round, 1);
    assert_eq!(open[0].anchor_id.as_deref(), Some(summit.id.as_str()));
    assert_eq!(open[0].asked_at, Some(now));

    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Passed);
    assert_eq!(
        job.outcome.as_deref(),
        Some("Passed its first check. Asked 2 questions.")
    );
    assert!(pond.notifier.sent().is_empty(), "questions are not pushed");
}

#[tokio::test]
async fn a_critic_that_cannot_be_read_keeps_the_batch_and_a_plan_with_no_gap_asks_nothing() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    tick(&pond.deps, now).await;
    let job = pond.job_of(JobKind::Questions).await;
    pond.model.say(
        r#"{"questions": [{"gap": "who", "header": "Who", "text": "Who is coming with you?", "options": ["Just me", "A colleague"]}]}"#,
    );
    pond.model.say("They all look fine to me.");
    pond.drive(&job.task_id.clone().unwrap(), now).await;
    let open = pond.questions.open(&pond.member).await.unwrap();
    assert_eq!(open.len(), 1, "an unreadable check keeps the batch");
    assert_eq!(pond.job(&job.id).await.state, JobState::Passed);

    let mut settled = summit.clone();
    settled.place = Some("KICC".into());
    for (label, value) in [("Who", "Just me"), ("Budget", "KSh 5,000")] {
        settled.set_fact(AnchorFact::new(label, value, "you"), now);
    }
    pond.anchors.save(&settled).await.unwrap();
    let mut again = EngineJob::new(
        JobKind::Questions,
        JobClass::Asked,
        Some(&pond.member),
        &settled.title,
        now,
    );
    again.anchor_id = Some(settled.id.clone());
    pond.jobs.add(&again).await.unwrap();
    let before = pond.model.asked().len();
    tick(&pond.deps, now).await;
    let again = pond.job(&again.id).await;
    pond.drive(&again.task_id.clone().unwrap(), now).await;
    assert_eq!(
        pond.model.asked().len(),
        before,
        "no gap is open: no model call"
    );
    assert_eq!(pond.job(&again.id).await.state, JobState::Dropped);
}

#[tokio::test]
async fn an_anchor_that_is_over_keeps_its_deliverables_and_loses_its_tasks() {
    let pond = pond().await;
    let now = Utc::now();
    let mut meetup = Anchor::new(
        &pond.member,
        AnchorKind::Event,
        "Rust meetup",
        AnchorOrigin::You,
        now - Duration::days(9),
    )
    .unwrap();
    meetup.starts_at = Some(now - Duration::days(2));
    pond.anchors.add(&meetup).await.unwrap();

    let made_by = EngineJob::for_deliverable(
        &pond.member,
        &meetup.id,
        "unset",
        "Dossier for Rust meetup",
        JobClass::Deadline,
        meetup.starts_at,
        now - Duration::days(8),
    );
    let (task, _) = pond_core::user_data::services::engine::task::task_for_job(
        &made_by,
        "Write a dossier for the meetup.",
        &pond.member,
        now - Duration::days(8),
    )
    .unwrap();
    pond.tasks.create_task(&task).await.unwrap();
    let mut dossier = Deliverable::new(
        &meetup.id,
        DeliverableKind::Dossier,
        "dossier",
        "Dossier for Rust meetup",
        now - Duration::days(8),
    )
    .unwrap();
    dossier
        .start_making(task.id(), now - Duration::days(8))
        .unwrap();
    dossier
        .settle_ready(
            DeliverableDraft {
                hook: "Two talks fit you".into(),
                body: "# Dossier\n\nTwo talks fit you.".into(),
                ..DeliverableDraft::default()
            },
            "",
            Some(now - Duration::days(1)),
            now - Duration::days(8),
        )
        .unwrap();
    pond.anchors.add_deliverable(&dossier).await.unwrap();
    let a_members_own = LongTask::start(
        ProfileScope::Owner(pond.member.clone()),
        None,
        "plan the trip",
        None,
        LongTaskBudget::new(6, None).unwrap(),
        now,
    )
    .unwrap();
    pond.tasks.create_task(&a_members_own).await.unwrap();

    let report = tick(&pond.deps, now).await;
    assert_eq!(report.ended, 1);
    let ended = pond.anchors.find(&meetup.id).await.unwrap().unwrap();
    assert_eq!(ended.state, AnchorState::Ended);
    let kept = pond.deliverable(&dossier.id).await;
    assert_eq!(kept.state, DeliverableState::Archived);
    assert_eq!(kept.body, "# Dossier\n\nTwo talks fit you.");
    assert_eq!(kept.task_id, None);
    assert!(pond.tasks.get_task(task.id()).await.unwrap().is_none());
    assert!(pond
        .tasks
        .get_task(a_members_own.id())
        .await
        .unwrap()
        .is_some());
    assert!(
        pond.jobs
            .open()
            .await
            .unwrap()
            .iter()
            .all(|job| job.anchor_id.as_deref() != Some(meetup.id.as_str())),
        "nothing is refreshed for an anchor that ended"
    );
}

#[tokio::test]
async fn a_job_whose_task_was_stopped_is_closed_and_its_deliverable_released() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    tick(&pond.deps, now).await;
    let job = pond.dossier_job(&summit).await;
    let task_id = job.task_id.clone().unwrap();
    pond.tasks
        .set_task_state(&task_id, LongTaskState::Cancelled, now)
        .await
        .unwrap();

    let report = tick(&pond.deps, now + Duration::minutes(1)).await;
    assert_eq!(report.closed, 1);
    assert_eq!(report.queued, 0, "it rests before it is tried again");
    let job = pond.job(&job.id).await;
    assert_eq!(job.state, JobState::Cancelled);
    let released = pond
        .deliverable(job.deliverable_id.as_deref().unwrap())
        .await;
    assert_eq!(released.state, DeliverableState::Queued);

    let later = tick(&pond.deps, now + Duration::hours(7)).await;
    assert_eq!((later.queued, later.opened), (1, 1), "{later:?}");
}

#[tokio::test]
async fn a_deliverable_no_job_is_making_goes_back_to_what_the_member_last_had() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    tick(&pond.deps, now).await;
    let mut job = pond.dossier_job(&summit).await;
    let deliverable_id = job.deliverable_id.clone().unwrap();
    job.settle(JobState::Cancelled, "Stopped by the member.", now)
        .unwrap();
    assert!(pond.jobs.save(&job).await.unwrap());
    assert_eq!(
        pond.deliverable(&deliverable_id).await.state,
        DeliverableState::Making
    );

    tick(&pond.deps, now + Duration::minutes(1)).await;
    assert_eq!(
        pond.deliverable(&deliverable_id).await.state,
        DeliverableState::Queued
    );
}

#[tokio::test]
async fn with_helpers_off_the_slot_still_takes_an_engine_task_whose_next_run_needs_none() {
    let pond = pond().await;
    let now = Utc::now();
    let summit = pond.summit(now).await;
    let asked = LongTask::start(
        ProfileScope::Owner(pond.member.clone()),
        None,
        "plan the trip",
        None,
        LongTaskBudget::new(6, None).unwrap(),
        now - Duration::minutes(5),
    )
    .unwrap();
    pond.tasks.create_task(&asked).await.unwrap();
    tick(&pond.deps, now).await;
    let open = pond
        .tasks
        .list_tasks(LongTaskOwnerFilter::Everyone, false)
        .await
        .unwrap();

    let (head, inline) = next_task(&pond.deps, &open, true, now).await;
    assert_eq!(
        head.unwrap().id(),
        asked.id(),
        "what a member asked for is first"
    );
    assert!(!inline);

    let (taken, inline) = next_task(&pond.deps, &open, false, now).await;
    let taken = taken.expect("an inline job can go on");
    assert!(inline);
    let job = { pond.jobs.by_task(taken.id()).await.unwrap().unwrap() };
    assert_eq!(
        job.kind,
        JobKind::Questions,
        "the deadline's inline job, ahead of upkeep; its dossier waits for helpers"
    );
    assert_ne!(
        Some(taken.id()),
        pond.dossier_job(&summit).await.task_id.as_deref()
    );

    let later = now + Duration::hours(3);
    let begun = begin(&pond.deps, taken.clone(), later).await;
    assert_eq!(
        begun.budget().deadline(),
        Some(later + Duration::minutes(15)),
        "its time runs from when the runner took it"
    );
    assert_eq!(pond.job(&job.id).await.state, JobState::Running);
    assert_eq!(pond.job(&job.id).await.started_at, Some(later));
}
