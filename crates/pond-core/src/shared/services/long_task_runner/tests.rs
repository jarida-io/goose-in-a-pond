use super::*;
use crate::shared::domain::long_task::{ArtifactKind, LongTaskBudget, PlanStepParts, StepOutcome};
use crate::user_data::domain::profile::ProfileScope;

fn t(secs: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
}

fn task_in(state: LongTaskState, deadline: Option<DateTime<Utc>>) -> LongTask {
    let mut task = LongTask::start(
        ProfileScope::Owner("member-1".to_string()),
        None,
        "plan the school trip",
        Some("chat-1".to_string()),
        LongTaskBudget::new(12, deadline).unwrap(),
        t(0),
    )
    .unwrap();
    if state != LongTaskState::Planning {
        task.transition_to(LongTaskState::Running, t(1)).unwrap();
    }
    if !matches!(state, LongTaskState::Planning | LongTaskState::Running) {
        task.transition_to(state, t(1)).unwrap();
    }
    task
}

fn step(key: &str, ordinal: u32, deps: &[&str], state: StepState, attempts: u32) -> PlanStep {
    PlanStep::from_parts(PlanStepParts {
        task_id: "task-1".to_string(),
        key: key.to_string(),
        ordinal,
        role: "long-task-worker".to_string(),
        instructions: format!("do {key}"),
        inputs: Vec::new(),
        depends_on: deps.iter().map(|d| d.to_string()).collect(),
        acceptance: None,
        state,
        attempts,
        summary: (state == StepState::Completed).then(|| format!("{key} done")),
        failure: (state == StepState::Failed).then(|| "the page would not load".to_string()),
        started_at: None,
        finished_at: None,
        agent: None,
    })
    .unwrap()
}

fn key_of(run: &NextRun<'_>) -> Option<String> {
    match run {
        NextRun::Do(step) => Some(step.key().to_string()),
        _ => None,
    }
}

#[test]
fn a_new_task_is_planned_first() {
    let task = task_in(LongTaskState::Planning, None);
    assert_eq!(
        next_run(&task, &[], 0, t(2)),
        NextRun::Plan(PlanReason::Fresh)
    );
}

#[test]
fn steps_run_in_dependency_order() {
    let task = task_in(LongTaskState::Running, None);
    let steps = [
        step("a", 0, &[], StepState::Completed, 1),
        step("b", 1, &["a"], StepState::Pending, 0),
        step("c", 2, &["b"], StepState::Pending, 0),
    ];
    assert_eq!(
        key_of(&next_run(&task, &steps, 0, t(2))).as_deref(),
        Some("b")
    );
}

#[test]
fn a_failed_step_is_retried_before_its_dependents_count_as_blocked() {
    let task = task_in(LongTaskState::Running, None);
    let steps = [
        step("a", 0, &[], StepState::Failed, 1),
        step("b", 1, &["a"], StepState::Pending, 0),
    ];
    assert_eq!(
        key_of(&next_run(&task, &steps, 0, t(2))).as_deref(),
        Some("a"),
        "one failure earns a retry; going straight to the planner throws the step away"
    );

    let out_of_tries = [
        step("a", 0, &[], StepState::Failed, MAX_STEP_ATTEMPTS),
        step("b", 1, &["a"], StepState::Pending, 0),
    ];
    assert_eq!(
        next_run(&task, &out_of_tries, 0, t(2)),
        NextRun::Plan(PlanReason::Blocked(vec![StepKey::parse("b").unwrap()])),
        "out of tries, the planner must plan around it"
    );
}

#[test]
fn a_settled_plan_finishes_and_a_waiting_task_runs_nothing() {
    let task = task_in(LongTaskState::Running, None);
    let steps = [
        step("a", 0, &[], StepState::Completed, 1),
        step("b", 1, &[], StepState::Skipped, 0),
    ];
    assert_eq!(next_run(&task, &steps, 0, t(2)), NextRun::Finish);

    for state in [
        LongTaskState::Paused,
        LongTaskState::AwaitingApproval,
        LongTaskState::Cancelled,
    ] {
        let task = task_in(state, None);
        let pending = [step("a", 0, &[], StepState::Pending, 0)];
        assert_eq!(
            next_run(&task, &pending, 0, t(2)),
            NextRun::Wait,
            "a {state} task must not run a step"
        );
    }
}

#[test]
fn a_task_gives_up_past_its_deadline_or_after_fruitless_plans() {
    let task = task_in(LongTaskState::Running, Some(t(100)));
    let pending = [step("a", 0, &[], StepState::Pending, 0)];
    assert!(matches!(
        next_run(&task, &pending, 0, t(101)),
        NextRun::GiveUp(_)
    ));
    assert!(
        matches!(next_run(&task, &pending, 0, t(99)), NextRun::Do(_)),
        "before the deadline the step runs, or the check above proves nothing"
    );
    assert!(matches!(
        next_run(&task, &pending, MAX_FRUITLESS_PLANS, t(2)),
        NextRun::GiveUp(_)
    ));
}

fn summary(id: &str, step_key: Option<&str>, title: &str) -> ArtifactSummary {
    ArtifactSummary {
        id: id.to_string(),
        task_id: "task-1".to_string(),
        step_key: step_key.map(|k| StepKey::parse(k).unwrap()),
        kind: ArtifactKind::Findings,
        title: title.to_string(),
        summary: format!("about {title}"),
        tokens: 120,
        created_at: t(5),
    }
}

#[test]
fn a_worker_sees_its_inputs_and_its_dependencies_output_and_nothing_else() {
    let task = task_in(LongTaskState::Running, None);
    let b = PlanStep::from_parts(PlanStepParts {
        task_id: "task-1".to_string(),
        key: "b".to_string(),
        ordinal: 1,
        role: "long-task-worker".to_string(),
        instructions: "compare the two quotes".to_string(),
        inputs: vec!["given".to_string()],
        depends_on: vec!["a".to_string()],
        acceptance: Some("names the cheaper bus company".to_string()),
        state: StepState::Pending,
        attempts: 0,
        summary: None,
        failure: None,
        started_at: None,
        finished_at: None,
        agent: None,
    })
    .unwrap();
    let artifacts = [
        summary("given", None, "Brief"),
        summary("from-a", Some("a"), "Quotes"),
        summary("from-z", Some("z"), "Unrelated"),
    ];
    let brief = worker_brief(&task, &b, &artifacts, &HashMap::new(), t(10));
    assert!(brief.contains("compare the two quotes"));
    assert!(brief.contains("names the cheaper bus company"));
    assert!(brief.contains("given") && brief.contains("from-a"));
    assert!(
        !brief.contains("from-z"),
        "a step is shown only what it was given, or its window fills with other steps' work"
    );
}

#[test]
fn the_planner_is_told_what_failed_and_why() {
    let task = task_in(LongTaskState::Running, None);
    let steps = [
        step("a", 0, &[], StepState::Completed, 1),
        step("b", 1, &["a"], StepState::Failed, MAX_STEP_ATTEMPTS),
    ];
    let brief = planner_brief(
        &task,
        &steps,
        &[],
        &[],
        &PlanReason::Blocked(Vec::new()),
        None,
        t(10),
    );
    assert!(brief.contains("a done"));
    assert!(brief.contains("the page would not load"));
}

#[test]
fn only_a_research_task_is_briefed_to_search_and_cite() {
    let general = task_in(LongTaskState::Running, None);
    let research = task_in(LongTaskState::Running, None).with_kind(LongTaskKind::Research);
    let step_b = step("b", 0, &[], StepState::Pending, 0);
    let plan = |task: &LongTask| {
        planner_brief(
            task,
            &[],
            &[],
            &[],
            &PlanReason::Blocked(Vec::new()),
            None,
            t(10),
        )
    };
    let work = |task: &LongTask| worker_brief(task, &step_b, &[], &HashMap::new(), t(10));

    assert!(plan(&research).contains("This is research"));
    assert!(
        plan(&research).contains(RESULT_TITLE),
        "the plan ends in the report the card opens"
    );
    assert!(
        work(&research).contains("Markdown link"),
        "sources are cited as links"
    );
    assert!(
        plan(&research).contains("Do not plan a step that combines"),
        "a compile step would repeat every section in the joined report"
    );
    assert!(!plan(&general).contains("This is research"));
    assert!(!work(&general).contains("This is research"));
}

#[test]
fn the_brief_sizes_the_answer_by_the_members_choice_or_asks_the_planner_to() {
    let research = |answer| {
        task_in(LongTaskState::Running, None)
            .with_kind(LongTaskKind::Research)
            .with_answer(answer)
    };
    let step_b = step("b", 0, &[], StepState::Pending, 0);
    let plan =
        |task: &LongTask| planner_brief(task, &[], &[], &[], &PlanReason::Fresh, None, t(10));
    let work = |task: &LongTask| worker_brief(task, &step_b, &[], &HashMap::new(), t(10));

    let short = research(AnswerShape::Short);
    assert!(plan(&short).contains("wants a short answer"));
    assert!(
        !plan(&short).contains("three parts"),
        "no sections for a short answer"
    );
    assert!(
        !plan(&short).contains("plan_write's answer"),
        "the member already chose"
    );
    assert!(work(&short).contains("150 words"));
    assert!(!work(&short).contains("400 to 900"));

    let report = research(AnswerShape::Report);
    assert!(plan(&report).contains("three parts"));
    assert!(!plan(&report).contains("wants a short answer"));
    assert!(work(&report).contains("400 to 900"));

    let auto = research(AnswerShape::Auto);
    assert!(plan(&auto).contains("plan_write's answer"));
    assert!(plan(&auto).contains("wants a short answer") && plan(&auto).contains("three parts"));

    let general = task_in(LongTaskState::Running, None).with_answer(AnswerShape::Short);
    assert!(plan(&general).contains("wants a short answer"));
    assert!(work(&general).contains("150 words"));
}

fn run(status: TaskStatus, result: Option<&str>) -> TaskRun {
    TaskRun {
        id: "run-1".to_string(),
        role: "long-task-worker".to_string(),
        parent_session_id: long_task_session_id("task-1", "run-1"),
        status,
        result: result.map(str::to_string),
        error: None,
        started_at: t(3),
        finished_at: Some(t(4)),
    }
}

#[test]
fn a_run_settles_to_exactly_one_hand_back() {
    let reported = StepOutcome::new(OutcomeKind::Completed, "found two quotes", Vec::new(), None);
    let done = |text: &str| run(TaskStatus::Completed, Some(text));
    assert_eq!(
        settle_run(
            Some(reported.clone()),
            &["saved".to_string()],
            &done("prose"),
            false
        ),
        RunEnd::Reported(reported),
        "a reported outcome wins over saved work and the helper's last words"
    );
    match settle_run(
        None,
        &["day-1".to_string()],
        &done("Here is day one."),
        false,
    ) {
        RunEnd::Reported(o) => {
            assert_eq!(
                o.outcome(),
                OutcomeKind::Completed,
                "saved work counts as done"
            );
            assert_eq!(o.artifacts(), ["day-1".to_string()]);
            assert_eq!(o.summary(), "Here is day one.");
        }
        other => panic!("expected the saved work, got {other:?}"),
    }
    match settle_run(
        None,
        &[],
        &done(r#"{"outcome": "completed", "summary": "done from text"}"#),
        false,
    ) {
        RunEnd::Reported(o) => assert_eq!(o.summary(), "done from text"),
        other => panic!("expected a parsed report, got {other:?}"),
    }
    match settle_run(None, &[], &done("I think it went fine"), false) {
        RunEnd::Reported(o) => assert_eq!(
            o.outcome(),
            OutcomeKind::Failed,
            "prose with no report and nothing saved must never count as done"
        ),
        other => panic!("expected a failure, got {other:?}"),
    }
    assert_eq!(
        settle_run(
            None,
            &["saved".to_string()],
            &run(TaskStatus::Cancelled, None),
            false
        ),
        RunEnd::Preempted,
        "a stopped run reruns, even if it saved something"
    );
    match settle_run(
        None,
        &[],
        &run(TaskStatus::TurnBudgetExhausted, None),
        false,
    ) {
        RunEnd::Reported(o) => assert_eq!(o.outcome(), OutcomeKind::Failed),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_step_session_reads_as_the_ponds_own() {
    assert!(long_task_session_id("t", "r").starts_with("sched-"));
}

fn retry_of(key: &str, failure: &str) -> PlanStep {
    PlanStep::from_parts(PlanStepParts {
        task_id: "task-1".to_string(),
        key: key.to_string(),
        ordinal: 1,
        role: "long-task-worker".to_string(),
        instructions: format!("do {key}"),
        inputs: Vec::new(),
        depends_on: vec!["a".to_string()],
        acceptance: None,
        state: StepState::Failed,
        attempts: 1,
        summary: None,
        failure: Some(failure.to_string()),
        started_at: None,
        finished_at: None,
        agent: None,
    })
    .unwrap()
}

#[test]
fn small_inputs_are_put_in_the_brief_and_large_ones_stay_pull_only() {
    let task = task_in(LongTaskState::Running, None);
    let b = step("b", 1, &["a"], StepState::Pending, 0);
    let mut big = summary("from-a-big", Some("a"), "All the hotel pages");
    big.tokens = INLINE_ARTIFACT_TOKENS + 1;
    let artifacts = [summary("from-a", Some("a"), "Quotes"), big];
    let ids = inline_ids(&b, &artifacts);
    assert_eq!(ids, vec!["from-a".to_string()]);

    let bodies = HashMap::from([(
        "from-a".to_string(),
        "Bus A: 1,200 KES. Bus B: 950 KES.".to_string(),
    )]);
    let brief = worker_brief(&task, &b, &artifacts, &bodies, t(10));
    assert!(brief.contains("already read for you"), "{brief}");
    assert!(brief.contains("Bus B: 950 KES."), "{brief}");
    let listed = brief.split("Other artifacts you may read:").nth(1).unwrap();
    assert!(
        listed.contains("from-a-big") && !listed.contains("from-a ["),
        "{brief}"
    );
}

#[test]
fn inlining_stops_at_the_total_budget() {
    let b = step("b", 1, &["a"], StepState::Pending, 0);
    let mut artifacts = Vec::new();
    for i in 0..5 {
        let mut a = summary(&format!("part-{i}"), Some("a"), "Part");
        a.tokens = 1_000;
        artifacts.push(a);
    }
    assert_eq!(
        inline_ids(&b, &artifacts).len(),
        (INLINE_TOTAL_TOKENS / 1_000) as usize
    );
}

#[test]
fn a_retry_sees_what_its_last_try_saved_and_gets_more_turns_after_running_out() {
    let task = task_in(LongTaskState::Running, None);
    let artifacts = [summary("from-b-try-1", Some("b"), "Half the list")];
    let retry = retry_of("b", OUT_OF_TURNS);
    assert_eq!(
        inline_ids(&retry, &artifacts),
        vec!["from-b-try-1".to_string()]
    );
    assert!(
        worker_brief(&task, &retry, &artifacts, &HashMap::new(), t(10)).contains("from-b-try-1")
    );
    assert!(
        !worker_brief(
            &task,
            &step("b", 1, &["a"], StepState::Pending, 0),
            &artifacts,
            &HashMap::new(),
            t(10)
        )
        .contains("from-b-try-1"),
        "a first try is not shown work it has not done"
    );
    assert_eq!(turns_for(6, &retry), 9);
    assert_eq!(turns_for(6, &retry_of("b", "the page would not load")), 6);
    assert_eq!(turns_for(6, &step("b", 1, &[], StepState::Pending, 0)), 6);
}

#[test]
fn an_empty_result_or_a_run_out_of_turns_fails_so_the_step_retries() {
    let done = run(TaskStatus::Completed, Some("ok"));
    let empty = StepOutcome::new(OutcomeKind::Completed, "done", Vec::new(), None);
    match settle_run(Some(empty), &[], &done, true) {
        RunEnd::Reported(o) => assert_eq!(o.failure(), Some(CAME_BACK_EMPTY)),
        other => panic!("expected a failure, got {other:?}"),
    }
    let real = StepOutcome::new(
        OutcomeKind::Completed,
        "Bus B is cheaper by 250 KES a seat.",
        Vec::new(),
        None,
    );
    assert_eq!(
        settle_run(Some(real.clone()), &[], &done, true),
        RunEnd::Reported(real),
        "a result in the summary is a result"
    );
    let out = run(TaskStatus::TurnBudgetExhausted, None);
    match settle_run(None, &["half".to_string()], &out, true) {
        RunEnd::Reported(o) => {
            assert_eq!(o.failure(), Some(OUT_OF_TURNS), "half-done work is retried")
        }
        other => panic!("expected a failure, got {other:?}"),
    }
    match settle_run(None, &["half".to_string()], &out, false) {
        RunEnd::Reported(o) => assert_eq!(
            o.outcome(),
            OutcomeKind::Completed,
            "on the last try, saved work stands"
        ),
        other => panic!("expected the saved work, got {other:?}"),
    }
}

#[test]
fn a_result_saved_under_a_longer_title_is_still_the_result() {
    let titled = |id: &str, title: &str| summary(id, Some("s"), title);
    let exact = [
        titled("a", "Final Trip Plan Result"),
        titled("b", "Result"),
        titled("c", "Notes"),
    ];
    assert_eq!(
        result_artifact(&exact).map(|a| a.id.as_str()),
        Some("b"),
        "an exact Result wins"
    );
    let drifted = [
        titled("a", "Search results for hotels"),
        titled("b", "Arusha Trip Result"),
        titled("c", "Calendar"),
    ];
    assert_eq!(result_artifact(&drifted).map(|a| a.id.as_str()), Some("b"));
    let none = [
        titled("a", "Search results"),
        titled("b", "Resulting costs"),
    ];
    assert_eq!(
        result_artifact(&none),
        None,
        "results and resulting are not the result"
    );
}

#[test]
fn a_run_stopped_after_it_reported_keeps_its_report() {
    let reported = StepOutcome::new(
        OutcomeKind::Completed,
        "Bus B is cheaper by 250 KES a seat.",
        Vec::new(),
        None,
    );
    assert_eq!(
        settle_run(
            Some(reported.clone()),
            &[],
            &run(TaskStatus::Cancelled, None),
            true
        ),
        RunEnd::Reported(reported),
        "the runner stops a run once it reports; that must not throw the report away"
    );
    assert_eq!(
        settle_run(None, &[], &run(TaskStatus::Cancelled, None), true),
        RunEnd::Preempted,
        "stopped before reporting, it reruns"
    );
}

#[test]
fn the_brief_carries_what_was_said_before_the_ask_to_the_planner_and_every_step() {
    use crate::models::domain::message::Role;
    let earlier = [
        (Role::User, "We're four, and I'm lactose intolerant."),
        (Role::Tool, "{\"tool\": \"weather\"}"),
        (Role::Assistant, "Noted. When are you going?"),
        (Role::User, "  "),
    ];
    let text = conversation_brief(&earlier).unwrap();
    assert_eq!(
        text,
        "Earlier in the conversation:\nThe person: We're four, and I'm lactose intolerant.\nThe assistant: Noted. When are you going?"
    );
    assert_eq!(conversation_brief(&[]), None);
    assert_eq!(conversation_brief(&[(Role::User, " ")]), None);

    let long = "x".repeat(BRIEF_CHARS);
    let many = [(Role::User, "first, dropped"), (Role::User, long.as_str())];
    let kept = conversation_brief(&many).unwrap();
    assert!(
        !kept.contains("first, dropped"),
        "the end of the conversation is kept"
    );
    assert!(kept.chars().count() <= BRIEF_CHARS + 40, "{}", kept.len());

    let task = task_in(LongTaskState::Running, None);
    let plan = planner_brief(&task, &[], &[], &[], &PlanReason::Fresh, Some(&text), t(10));
    assert!(plan.contains("lactose intolerant"), "{plan}");

    let mut brief = summary("brief-1", None, BRIEF_TITLE);
    brief.tokens = 40;
    let artifacts = [brief, summary("from-z", Some("z"), "Unrelated")];
    let b = step("b", 1, &["a"], StepState::Pending, 0);
    assert_eq!(
        brief_artifact(&artifacts).map(|a| a.id.as_str()),
        Some("brief-1")
    );
    assert_eq!(
        inline_ids(&b, &artifacts),
        vec!["brief-1".to_string()],
        "every step is given the brief"
    );
    let bodies = HashMap::from([("brief-1".to_string(), text.clone())]);
    assert!(worker_brief(&task, &b, &artifacts, &bodies, t(10)).contains("lactose intolerant"));

    assert_eq!(clip_brief("  "), None);
    assert_eq!(
        clip_brief(&"y".repeat(BRIEF_CHARS + 10)).unwrap().len(),
        BRIEF_CHARS
    );
}

#[test]
fn a_made_agent_gets_only_the_tasks_tools_and_the_workers_budget() {
    use crate::shared::domain::long_task::StepAgent;
    let worker = crate::shared::services::long_task_roles::worker_role().unwrap();
    let agent = StepAgent {
        name: "Fare Checker".into(),
        instructions: "Compare matatu and SGR fares.".into(),
        tools: vec![
            "giap-knowledge".into(),
            "giap-device-control".into(),
            "giap-made-up".into(),
        ],
    };
    let mut step = step("fares", 0, &[], StepState::Pending, 0);
    step = PlanStep::from_parts(PlanStepParts {
        role: "new:Fare Checker".into(),
        agent: Some(agent.clone()),
        ..step_parts(&step)
    })
    .unwrap();
    let role = role_for_agent(&step, &agent, &worker).unwrap();
    assert_eq!(role.name(), "new:Fare Checker");
    let tools: Vec<&str> = role
        .requested_tool_groups()
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(
        tools,
        vec!["giap-knowledge", TASK_WORK_EXTENSION],
        "no device control, nothing made up"
    );
    assert!(role
        .instructions()
        .contains("Compare matatu and SGR fares."));
    assert!(role.instructions().contains("Never put a person's name"));
    assert_eq!(role.max_turns(), worker.max_turns());
}

fn step_parts(step: &PlanStep) -> PlanStepParts {
    PlanStepParts {
        task_id: step.task_id().to_string(),
        key: step.key().as_str().to_string(),
        ordinal: step.ordinal(),
        role: step.role().to_string(),
        instructions: step.instructions().to_string(),
        inputs: step.inputs().to_vec(),
        depends_on: step
            .depends_on()
            .iter()
            .map(|k| k.as_str().to_string())
            .collect(),
        acceptance: step.acceptance().map(str::to_string),
        state: step.state(),
        attempts: step.attempts(),
        summary: step.summary().map(str::to_string),
        failure: step.failure().map(str::to_string),
        started_at: step.started_at(),
        finished_at: step.finished_at(),
        agent: step.agent().cloned(),
    }
}

#[test]
fn a_short_answers_notification_reads_its_links_as_words_and_stops_at_a_word() {
    assert_eq!(
        notification_text("Jerry was seen at 16:10 ([camera events](https://pond.local/e))."),
        "Jerry was seen at 16:10 (camera events)."
    );
    assert_eq!(notification_text("## Answer\nYes."), "Answer\nYes.");
    let long = "word ".repeat(400);
    let cut = notification_text(&long);
    assert!(cut.ends_with("word..."), "{cut}");
    assert!(cut.chars().count() <= NOTIFICATION_CHARS + 3);
}

#[test]
fn a_short_answer_without_a_result_title_still_has_an_answer_and_a_report_does_not_guess() {
    let found = ArtifactSummary {
        id: "f1".into(),
        task_id: "task-1".into(),
        step_key: Some(StepKey::parse("look").unwrap()),
        kind: ArtifactKind::Findings,
        title: "Current Presence and Camera Status".into(),
        summary: "Nobody is in frame.".into(),
        tokens: 40,
        created_at: t(5),
    };
    let short = task_in(LongTaskState::Running, None).with_answer(AnswerShape::Short);
    let report = task_in(LongTaskState::Running, None).with_answer(AnswerShape::Report);
    assert_eq!(
        answer_artifact(&short, std::slice::from_ref(&found)).map(|a| a.id.as_str()),
        Some("f1")
    );
    assert!(answer_artifact(&report, std::slice::from_ref(&found)).is_none());
}

#[test]
fn only_the_plans_last_open_step_is_told_to_save_the_result() {
    let a = step("a", 0, &[], StepState::Completed, 1);
    let b = step("b", 1, &[], StepState::Pending, 0);
    let c = step("c", 2, &[], StepState::Pending, 0);
    let steps = [a, b.clone(), c.clone()];
    assert!(last_step_line(&steps, &b).is_none());
    assert!(last_step_line(&steps, &c).unwrap().contains("\"Result\""));
}

#[test]
fn a_short_answer_left_only_in_a_summary_becomes_its_result() {
    let mut done = step("look", 0, &[], StepState::Running, 1);
    done.record(
        &StepOutcome::new(
            OutcomeKind::Completed,
            "Flying is quickest: about an hour in the air.",
            Vec::new(),
            None,
        ),
        t(9),
    )
    .unwrap();
    let short = task_in(LongTaskState::Running, None).with_answer(AnswerShape::Short);
    let result = summary_as_result(&short, std::slice::from_ref(&done), &[], t(10))
        .expect("the summary is the answer");
    assert_eq!(result.title(), RESULT_TITLE);
    assert!(result.body().contains("about an hour"));
    let report = task_in(LongTaskState::Running, None).with_answer(AnswerShape::Report);
    assert!(summary_as_result(&report, std::slice::from_ref(&done), &[], t(10)).is_none());
}

#[test]
fn a_reply_holding_work_is_kept_as_the_steps_draft() {
    let task = task_in(LongTaskState::Running, None);
    let plan = "## Saturday\n".to_string()
        + &"- 09:00 Breakfast by the lake, then a boat ride to Crescent Island. ".repeat(12);
    let key = StepKey::parse("draft-itinerary").unwrap();
    let middle = ReplyDraft {
        key: key.clone(),
        last: false,
    };
    let kept = reply_as_draft(
        &task,
        &middle,
        &run(TaskStatus::Completed, Some(&plan)),
        t(5),
    )
    .expect("a plan written as a reply is kept");
    assert_eq!(kept.title(), "Saturday");
    assert_eq!(kept.body(), plan.trim());
    assert_eq!(
        kept.step_key().map(|k| k.to_string()).as_deref(),
        Some("draft-itinerary")
    );

    let last = ReplyDraft { key, last: true };
    let result = reply_as_draft(&task, &last, &run(TaskStatus::Completed, Some(&plan)), t(5));
    assert_eq!(
        result.map(|a| a.title().to_string()).as_deref(),
        Some(RESULT_TITLE)
    );

    assert!(
        reply_as_draft(
            &task,
            &middle,
            &run(TaskStatus::Completed, Some("I could not save it.")),
            t(5)
        )
        .is_none(),
        "a sign-off is not work"
    );
    assert!(
        reply_as_draft(&task, &middle, &run(TaskStatus::Failed, Some(&plan)), t(5)).is_none(),
        "a failed run's text is not handed on"
    );
}

#[test]
fn a_draft_without_a_heading_is_named_from_its_key() {
    let task = task_in(LongTaskState::Running, None);
    let text = "The train is faster and costs less than most buses. ".repeat(10);
    let draft = ReplyDraft {
        key: StepKey::parse("draft-comfort").unwrap(),
        last: false,
    };
    let kept = reply_as_draft(
        &task,
        &draft,
        &run(TaskStatus::Completed, Some(&text)),
        t(5),
    )
    .unwrap();
    assert_eq!(kept.title(), "Comfort");
}
