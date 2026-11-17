//! The order the one long-task slot takes open tasks in, once engine jobs wait in the same line.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::shared::domain::long_task::LongTask;
use crate::shared::services::agent_activations::is_activation_task;
use crate::shared::services::long_task_queue;
use crate::user_data::domain::engine_job::{is_engine_task, EngineJob, JobClass};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Place {
    Asked,
    Activation,
    Deadline,
    Upkeep,
    Open,
    /// An engine task whose job row is gone: nothing says what it serves.
    Unclaimed,
}

fn place_of(task: &LongTask, job: Option<&EngineJob>) -> Place {
    if is_activation_task(task) {
        return Place::Activation;
    }
    if !is_engine_task(task.id()) {
        return Place::Asked;
    }
    match job.map(|job| job.class) {
        Some(JobClass::Asked) => Place::Asked,
        Some(JobClass::Deadline) => Place::Deadline,
        Some(JobClass::Upkeep) => Place::Upkeep,
        Some(JobClass::Open) => Place::Open,
        None => Place::Unclaimed,
    }
}

/// Open tasks in running order: what a member asked for, activations, then engine jobs by
/// deadline, upkeep and open; oldest first within each but a deadline, which runs nearest first.
pub fn run_order<'a>(tasks: &'a [LongTask], jobs: &[EngineJob]) -> Vec<&'a LongTask> {
    let by_task: HashMap<&str, &EngineJob> = jobs
        .iter()
        .filter_map(|job| Some((job.task_id.as_deref()?, job)))
        .collect();
    let mut order = long_task_queue::run_order(tasks);
    // Stable, so each place keeps the oldest-first order it arrived in.
    order.sort_by(|a, b| {
        let (job_a, job_b) = (by_task.get(a.id()), by_task.get(b.id()));
        let place = place_of(a, job_a.copied());
        place
            .cmp(&place_of(b, job_b.copied()))
            .then_with(|| match place {
                Place::Deadline => {
                    let due = |job: Option<&&EngineJob>| job.and_then(|job| job.due_at);
                    match (due(job_a), due(job_b)) {
                        (Some(x), Some(y)) => x.cmp(&y),
                        (Some(_), None) => Ordering::Less,
                        (None, Some(_)) => Ordering::Greater,
                        (None, None) => Ordering::Equal,
                    }
                }
                _ => Ordering::Equal,
            })
    });
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::{
        AnswerShape, LongTaskKind, LongTaskParts, LongTaskState,
    };
    use crate::user_data::domain::engine_job::JobKind;
    use crate::user_data::domain::profile::ProfileScope;
    use chrono::{DateTime, Duration, Utc};

    fn at(minutes: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + minutes * 60, 0).unwrap()
    }

    fn task(id: &str, asked_at: i64) -> LongTask {
        LongTask::from_parts(LongTaskParts {
            id: id.to_string(),
            owner_profile_id: None,
            scope: ProfileScope::Owner("jerry".into()),
            goal: format!("goal of {id}"),
            origin_session_id: None,
            state: LongTaskState::Running,
            max_steps: 6,
            deadline: None,
            kind: LongTaskKind::General,
            answer: AnswerShape::Auto,
            created_at: at(asked_at),
            updated_at: at(asked_at),
            finished_at: None,
        })
        .unwrap()
    }

    fn job(task_id: &str, class: JobClass, due_in_days: Option<i64>) -> EngineJob {
        let mut job = EngineJob::new(JobKind::Deliverable, class, Some("jerry"), "a job", at(0));
        job.task_id = Some(task_id.to_string());
        job.due_at = due_in_days.map(|days| at(0) + Duration::days(days));
        job
    }

    fn ids(order: &[&LongTask]) -> Vec<String> {
        order.iter().map(|t| t.id().to_string()).collect()
    }

    #[test]
    fn one_task_of_every_class_runs_in_the_order_the_line_is_read() {
        let tasks = [
            task("engine-open", 1),
            task("engine-upkeep", 2),
            task("engine-far", 3),
            task("engine-near", 4),
            task("agent-morning", 5),
            task("engine-asked", 6),
            task("a-members-task", 7),
            task("engine-orphan", 0),
        ];
        let jobs = [
            job("engine-open", JobClass::Open, None),
            job("engine-upkeep", JobClass::Upkeep, None),
            job("engine-far", JobClass::Deadline, Some(9)),
            job("engine-near", JobClass::Deadline, Some(2)),
            job("engine-asked", JobClass::Asked, None),
        ];
        assert_eq!(
            ids(&run_order(&tasks, &jobs)),
            [
                "engine-asked",
                "a-members-task",
                "agent-morning",
                "engine-near",
                "engine-far",
                "engine-upkeep",
                "engine-open",
                "engine-orphan",
            ]
        );
    }

    #[test]
    fn what_a_member_asked_for_runs_oldest_first_whether_or_not_it_is_an_engine_job() {
        let tasks = [
            task("engine-refresh", 10),
            task("plan-the-trip", 5),
            task("follow-up-1", 20),
        ];
        let jobs = [job("engine-refresh", JobClass::Asked, None)];
        assert_eq!(
            ids(&run_order(&tasks, &jobs)),
            ["plan-the-trip", "engine-refresh", "follow-up-1"]
        );
    }

    #[test]
    fn a_deadline_with_no_date_waits_behind_those_with_one_and_ties_run_oldest_first() {
        let tasks = [
            task("engine-undated", 1),
            task("engine-later", 2),
            task("engine-same-day-young", 4),
            task("engine-same-day-old", 3),
        ];
        let jobs = [
            job("engine-undated", JobClass::Deadline, None),
            job("engine-later", JobClass::Deadline, Some(5)),
            job("engine-same-day-young", JobClass::Deadline, Some(1)),
            job("engine-same-day-old", JobClass::Deadline, Some(1)),
        ];
        assert_eq!(
            ids(&run_order(&tasks, &jobs)),
            [
                "engine-same-day-old",
                "engine-same-day-young",
                "engine-later",
                "engine-undated",
            ]
        );
    }

    #[test]
    fn only_open_tasks_wait_in_the_line() {
        let mut paused = task("engine-paused", 1);
        paused.transition_to(LongTaskState::Paused, at(2)).unwrap();
        let mut done = task("engine-done", 1);
        done.transition_to(LongTaskState::Completed, at(2)).unwrap();
        let tasks = [paused, done, task("engine-live", 3)];
        let jobs = [
            job("engine-paused", JobClass::Asked, None),
            job("engine-done", JobClass::Asked, None),
            job("engine-live", JobClass::Open, None),
        ];
        assert_eq!(ids(&run_order(&tasks, &jobs)), ["engine-live"]);
    }
}
