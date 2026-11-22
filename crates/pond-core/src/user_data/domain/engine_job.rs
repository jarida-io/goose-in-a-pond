//! One maker-critic run of the idea engine. Most jobs are also a long task whose id starts with
//! [`ENGINE_TASK_PREFIX`]; memory extraction and consolidation log a job with no task.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use super::anchor::{IllegalMove, Objection};

/// Critic rounds a job gets before it settles without a pass.
pub const MAX_ROUNDS: u32 = 3;
/// What the id of a job's long task starts with; member task lists leave these out.
pub const ENGINE_TASK_PREFIX: &str = "engine-";
/// Open jobs of class `open` one member may have.
pub const OPEN_CLASS_JOBS_PER_MEMBER: usize = 2;

pub fn is_engine_task(id: &str) -> bool {
    id.starts_with(ENGINE_TASK_PREFIX)
}

/// A fresh long-task id for an engine job.
pub fn new_engine_task_id() -> String {
    format!("{ENGINE_TASK_PREFIX}{}", uuid::Uuid::new_v4())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Deliverable,
    FactSheet,
    Memory,
    Consolidation,
    Heartbeat,
    Capture,
    Ledger,
    Rule,
    Questions,
    Cards,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deliverable => "deliverable",
            Self::FactSheet => "fact_sheet",
            Self::Memory => "memory",
            Self::Consolidation => "consolidation",
            Self::Heartbeat => "heartbeat",
            Self::Capture => "capture",
            Self::Ledger => "ledger",
            Self::Rule => "rule",
            Self::Questions => "questions",
            Self::Cards => "cards",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "deliverable" => Some(Self::Deliverable),
            "fact_sheet" => Some(Self::FactSheet),
            "memory" => Some(Self::Memory),
            "consolidation" => Some(Self::Consolidation),
            "heartbeat" => Some(Self::Heartbeat),
            "capture" => Some(Self::Capture),
            "ledger" => Some(Self::Ledger),
            "rule" => Some(Self::Rule),
            "questions" => Some(Self::Questions),
            "cards" => Some(Self::Cards),
            _ => None,
        }
    }

    /// How long the job may run before it settles without a pass.
    pub fn time_cap(self) -> Duration {
        match self {
            Self::Deliverable => Duration::minutes(60),
            _ => Duration::minutes(15),
        }
    }

    /// What a job that ran out of time or rounds settles as: a deliverable is still shown,
    /// anything Goose would keep about the member is dropped.
    pub fn settles_without_a_pass_as(self) -> JobState {
        match self {
            Self::Deliverable => JobState::Unsure,
            _ => JobState::Dropped,
        }
    }
}

/// Where a job stands in the queue; the variants are in running order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobClass {
    /// The member asked for it; runs with the tasks they started.
    Asked,
    /// Serves an anchor with a date; nearest `due_at` first.
    Deadline,
    Upkeep,
    Open,
}

impl JobClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Asked => "asked",
            Self::Deadline => "deadline",
            Self::Upkeep => "upkeep",
            Self::Open => "open",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "asked" => Some(Self::Asked),
            "deadline" => Some(Self::Deadline),
            "upkeep" => Some(Self::Upkeep),
            "open" => Some(Self::Open),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Passed,
    Unsure,
    Dropped,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Unsure => "unsure",
            Self::Dropped => "dropped",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "passed" => Some(Self::Passed),
            "unsure" => Some(Self::Unsure),
            "dropped" => Some(Self::Dropped),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::Running)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineJob {
    pub id: String,
    /// The long task that runs it; `None` for a pass that keeps its own lane slot.
    #[serde(default)]
    pub task_id: Option<String>,
    /// `None` for a household-wide pass.
    #[serde(skip)]
    pub profile_id: Option<String>,
    pub kind: JobKind,
    pub class: JobClass,
    #[serde(default)]
    pub anchor_id: Option<String>,
    #[serde(default)]
    pub deliverable_id: Option<String>,
    /// What the job is about, as the log shows it. A capture or ledger job holds its capture's
    /// id here: that is how the one open job per capture is found.
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub due_at: Option<DateTime<Utc>>,
    pub state: JobState,
    pub rounds: u32,
    #[serde(default)]
    pub objections: Vec<Objection>,
    #[serde(default)]
    pub outcome: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
}

impl EngineJob {
    pub fn new(
        kind: JobKind,
        class: JobClass,
        profile_id: Option<&str>,
        subject: &str,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id: None,
            profile_id: profile_id.map(str::to_string),
            kind,
            class,
            anchor_id: None,
            deliverable_id: None,
            subject: subject.trim().to_string(),
            due_at: None,
            state: JobState::Queued,
            rounds: 0,
            objections: Vec::new(),
            outcome: None,
            created_at: now,
            started_at: None,
            finished_at: None,
        }
    }

    /// A job that makes or refreshes one deliverable of an anchor.
    pub fn for_deliverable(
        profile_id: &str,
        anchor_id: &str,
        deliverable_id: &str,
        title: &str,
        class: JobClass,
        due_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            anchor_id: Some(anchor_id.to_string()),
            deliverable_id: Some(deliverable_id.to_string()),
            due_at,
            ..Self::new(JobKind::Deliverable, class, Some(profile_id), title, now)
        }
    }

    /// A job that reads one capture; `kind` is `Capture` or `Ledger`.
    pub fn for_capture(
        profile_id: &str,
        capture_id: &str,
        kind: JobKind,
        now: DateTime<Utc>,
    ) -> Self {
        Self::new(kind, JobClass::Upkeep, Some(profile_id), capture_id, now)
    }

    fn illegal(&self, to: JobState) -> IllegalMove {
        IllegalMove {
            what: "an engine job",
            from: self.state.as_str(),
            to: to.as_str(),
        }
    }

    /// The runner takes a queued job.
    pub fn start(&mut self, task_id: Option<&str>, now: DateTime<Utc>) -> Result<(), IllegalMove> {
        if self.state != JobState::Queued {
            return Err(self.illegal(JobState::Running));
        }
        self.state = JobState::Running;
        if let Some(task_id) = task_id {
            self.task_id = Some(task_id.to_string());
        }
        self.started_at = Some(now);
        Ok(())
    }

    /// A critic gave its verdict on one more draft.
    pub fn count_round(&mut self, objections: Vec<Objection>) {
        self.rounds += 1;
        self.objections = objections;
    }

    /// Whether a failed verdict may be answered with a revision.
    pub fn may_revise(&self, now: DateTime<Utc>) -> bool {
        self.rounds < MAX_ROUNDS && !self.is_out_of_time(now)
    }

    pub fn is_out_of_time(&self, now: DateTime<Utc>) -> bool {
        self.started_at
            .is_some_and(|started| now - started >= self.kind.time_cap())
    }

    /// Closes an open job as `state` with its one-line outcome; a closed job is refused.
    pub fn settle(
        &mut self,
        state: JobState,
        outcome: &str,
        now: DateTime<Utc>,
    ) -> Result<(), IllegalMove> {
        if !self.state.is_open() || state.is_open() {
            return Err(self.illegal(state));
        }
        self.state = state;
        self.outcome = Some(outcome.split_whitespace().collect::<Vec<_>>().join(" "));
        self.finished_at = Some(now);
        Ok(())
    }
}

/// Open jobs in the order they run after the member's own tasks: by class, a deadline by its
/// nearest `due_at`, the rest oldest first.
pub fn sort_for_running(jobs: &mut [EngineJob]) {
    jobs.sort_by(|a, b| {
        a.class
            .cmp(&b.class)
            .then_with(|| match a.class {
                JobClass::Deadline => match (a.due_at, b.due_at) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
                _ => std::cmp::Ordering::Equal,
            })
            .then(a.created_at.cmp(&b.created_at))
            .then(a.id.cmp(&b.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::anchor::ObjectionKind;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn job(kind: JobKind, class: JobClass, minutes_old: i64) -> EngineJob {
        EngineJob::new(
            kind,
            class,
            Some("jerry"),
            "a job",
            now() - Duration::minutes(minutes_old),
        )
    }

    #[test]
    fn an_engine_task_is_known_by_its_id() {
        assert!(is_engine_task(&new_engine_task_id()));
        assert!(is_engine_task("engine-7f3a"));
        assert!(!is_engine_task("agent-engine-1"));
        assert!(!is_engine_task("follow-up-2"));
    }

    #[test]
    fn a_job_runs_once_and_settles_once() {
        let mut job = job(JobKind::FactSheet, JobClass::Upkeep, 0);
        job.start(Some("engine-1"), now()).unwrap();
        assert!(job.start(None, now()).is_err());
        assert!(
            job.settle(JobState::Running, "", now()).is_err(),
            "not an end"
        );
        job.settle(JobState::Passed, "  41 lines\n kept ", now())
            .unwrap();
        assert_eq!(job.outcome.as_deref(), Some("41 lines kept"));
        assert_eq!(job.finished_at, Some(now()));
        assert!(job.settle(JobState::Failed, "late", now()).is_err());
    }

    #[test]
    fn a_failed_verdict_is_revised_until_the_rounds_or_the_time_run_out() {
        let mut job = job(JobKind::Deliverable, JobClass::Deadline, 0);
        job.start(None, now()).unwrap();
        let held = vec![Objection::new(
            ObjectionKind::Vague,
            "the plan",
            "name days",
        )];
        for round in 1..=MAX_ROUNDS {
            assert!(job.may_revise(now()), "round {round}");
            job.count_round(held.clone());
        }
        assert!(!job.may_revise(now()));
        assert_eq!(job.objections, held);

        let mut slow = self::job(JobKind::Capture, JobClass::Upkeep, 0);
        slow.start(None, now()).unwrap();
        assert!(slow.may_revise(now() + Duration::minutes(14)));
        assert!(!slow.may_revise(now() + Duration::minutes(15)));
        assert!(!self::job(JobKind::Deliverable, JobClass::Open, 0).is_out_of_time(now()));
    }

    #[test]
    fn only_a_deliverable_is_shown_without_a_pass() {
        assert_eq!(
            JobKind::Deliverable.settles_without_a_pass_as(),
            JobState::Unsure
        );
        for kind in [JobKind::FactSheet, JobKind::Heartbeat, JobKind::Rule] {
            assert_eq!(kind.settles_without_a_pass_as(), JobState::Dropped);
        }
    }

    #[test]
    fn jobs_run_by_class_then_deadline_then_age() {
        let mut soon = job(JobKind::Deliverable, JobClass::Deadline, 1);
        soon.due_at = Some(now() + Duration::days(1));
        let mut later = job(JobKind::Deliverable, JobClass::Deadline, 50);
        later.due_at = Some(now() + Duration::days(9));
        let mut jobs = vec![
            job(JobKind::Deliverable, JobClass::Open, 90),
            later.clone(),
            job(JobKind::FactSheet, JobClass::Upkeep, 5),
            soon.clone(),
            job(JobKind::Deliverable, JobClass::Asked, 2),
            job(JobKind::Capture, JobClass::Upkeep, 30),
        ];
        sort_for_running(&mut jobs);
        let order: Vec<(JobClass, JobKind)> = jobs.iter().map(|j| (j.class, j.kind)).collect();
        assert_eq!(
            order,
            [
                (JobClass::Asked, JobKind::Deliverable),
                (JobClass::Deadline, JobKind::Deliverable),
                (JobClass::Deadline, JobKind::Deliverable),
                (JobClass::Upkeep, JobKind::Capture),
                (JobClass::Upkeep, JobKind::FactSheet),
                (JobClass::Open, JobKind::Deliverable),
            ]
        );
        assert_eq!(jobs[1].id, soon.id, "the nearer deadline first");
        assert_eq!(jobs[2].id, later.id);
    }
}
