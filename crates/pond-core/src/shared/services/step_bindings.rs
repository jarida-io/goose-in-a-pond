//! Which long-task step a runner's root session is working on. A helper's tool call resolves to
//! that root through [`ChildSessionRegistry`], so it reaches its own task without naming one, and
//! its report waits here for the runner. Leases unbind on drop; never persisted.
use crate::shared::domain::long_task::{StepKey, StepOutcome};
use crate::shared::services::child_sessions::ChildSessionRegistry;
use crate::user_data::domain::profile::ProfileScope;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock, Weak};

/// What a bound run does for its task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepWork {
    /// Writes or revises the plan.
    Plan,
    /// Does one step.
    Do(StepKey),
    /// Checks one step against its acceptance sentence.
    Check(StepKey),
}

impl StepWork {
    /// The step artifacts written by this run belong to; the planner writes none of its own.
    pub fn step_key(&self) -> Option<&StepKey> {
        match self {
            StepWork::Plan => None,
            StepWork::Do(key) | StepWork::Check(key) => Some(key),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepBinding {
    pub task_id: String,
    /// The task owner's scope: a helper reads and writes nothing wider.
    pub scope: ProfileScope,
    pub work: StepWork,
}

struct Entry {
    binding: StepBinding,
    report: Option<StepOutcome>,
    /// What the run saved, so a helper that did its work but never reported is not failed.
    written: Vec<String>,
}

#[derive(Default)]
pub struct StepBindings {
    entries: RwLock<HashMap<String, Entry>>,
}

impl StepBindings {
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds `root_session_id` to `binding` until the lease drops.
    pub fn bind(self: &Arc<Self>, root_session_id: &str, binding: StepBinding) -> StepBindingLease {
        self.entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                root_session_id.to_string(),
                Entry {
                    binding,
                    report: None,
                    written: Vec::new(),
                },
            );
        StepBindingLease {
            bindings: Arc::downgrade(self),
            root: root_session_id.to_string(),
        }
    }

    pub fn binding_for(&self, root_session_id: &str) -> Option<StepBinding> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(root_session_id)
            .map(|entry| entry.binding.clone())
    }

    /// The binding a helper's own engine session works under, resolved through its parent the way
    /// a tool server must: the model never supplies the task.
    pub fn binding_for_helper(
        &self,
        helper_engine_session: &str,
        children: &ChildSessionRegistry,
    ) -> Option<StepBinding> {
        self.binding_for(&children.parent_of(helper_engine_session)?)
    }

    /// Records a helper's hand-back; a later report replaces an earlier one. `false` when the run
    /// is no longer bound, so a late call is refused rather than lost silently.
    pub fn report(&self, root_session_id: &str, outcome: StepOutcome) -> bool {
        match self
            .entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(root_session_id)
        {
            Some(entry) => {
                entry.report = Some(outcome);
                true
            }
            None => false,
        }
    }
}

impl StepBindings {
    /// What the bound run has saved so far, in order; empty once the run has ended.
    pub fn written_by(&self, root_session_id: &str) -> Vec<String> {
        self.entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(root_session_id)
            .map(|entry| entry.written.clone())
            .unwrap_or_default()
    }

    /// Records an artifact the bound run saved; ignored once the run has ended.
    pub fn note_artifact(&self, root_session_id: &str, artifact_id: &str) {
        if let Some(entry) = self
            .entries
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(root_session_id)
        {
            entry.written.push(artifact_id.to_string());
        }
    }
}

/// Holds one run's binding; dropping it unbinds the run.
pub struct StepBindingLease {
    bindings: Weak<StepBindings>,
    root: String,
}

/// Whether a run has reported, readable while it is still going.
#[derive(Clone)]
pub struct ReportWatch {
    bindings: Weak<StepBindings>,
    root: String,
}

impl ReportWatch {
    pub fn is_reported(&self) -> bool {
        self.bindings.upgrade().is_some_and(|bindings| {
            bindings
                .entries
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&self.root)
                .is_some_and(|entry| entry.report.is_some())
        })
    }
}

impl StepBindingLease {
    /// A handle that says when the run has reported, for a watcher that ends the run there.
    pub fn report_watch(&self) -> ReportWatch {
        ReportWatch {
            bindings: self.bindings.clone(),
            root: self.root.clone(),
        }
    }

    /// The run's hand-back, taken once.
    pub fn take_report(&self) -> Option<StepOutcome> {
        self.bindings.upgrade().and_then(|bindings| {
            bindings
                .entries
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .get_mut(&self.root)
                .and_then(|entry| entry.report.take())
        })
    }
}

impl StepBindingLease {
    /// The artifacts the run saved, taken once.
    pub fn take_written(&self) -> Vec<String> {
        self.bindings
            .upgrade()
            .and_then(|bindings| {
                bindings
                    .entries
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .get_mut(&self.root)
                    .map(|entry| std::mem::take(&mut entry.written))
            })
            .unwrap_or_default()
    }
}

impl Drop for StepBindingLease {
    fn drop(&mut self) {
        if let Some(bindings) = self.bindings.upgrade() {
            bindings
                .entries
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.root);
        }
    }
}

/// The process's one registry: the runner binds, the task-work tool server reads and reports.
pub fn process_step_bindings() -> Arc<StepBindings> {
    static BINDINGS: OnceLock<Arc<StepBindings>> = OnceLock::new();
    BINDINGS
        .get_or_init(|| Arc::new(StepBindings::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::OutcomeKind;

    fn binding() -> StepBinding {
        StepBinding {
            task_id: "task-1".to_string(),
            scope: ProfileScope::Owner("member-1".to_string()),
            work: StepWork::Do(StepKey::parse("read-part-1").unwrap()),
        }
    }

    fn outcome(summary: &str) -> StepOutcome {
        StepOutcome::new(OutcomeKind::Completed, summary, Vec::new(), None)
    }

    #[test]
    fn a_helper_reaches_its_task_only_through_its_live_parent() {
        let bindings = Arc::new(StepBindings::new());
        let children = Arc::new(ChildSessionRegistry::new());
        let lease = bindings.bind("sched-long-task-1", binding());
        let child = children.bind("child-engine-1", "sched-long-task-1");

        assert_eq!(
            bindings.binding_for_helper("child-engine-1", &children),
            Some(binding())
        );
        assert_eq!(
            bindings.binding_for_helper("sched-long-task-1", &children),
            None,
            "the root's own id is not a helper's: only a child resolves through its parent"
        );
        drop(child);
        assert_eq!(
            bindings.binding_for_helper("child-engine-1", &children),
            None,
            "a finished helper must stop reaching its task"
        );
        drop(lease);
        assert_eq!(bindings.binding_for("sched-long-task-1"), None);
    }

    #[test]
    fn what_a_run_saved_is_kept_for_the_runner_only_while_bound() {
        let bindings = Arc::new(StepBindings::new());
        let lease = bindings.bind("root", binding());
        bindings.note_artifact("root", "a1");
        bindings.note_artifact("other", "a2");
        assert_eq!(lease.take_written(), vec!["a1".to_string()]);
        assert!(lease.take_written().is_empty(), "taken once");
        drop(lease);
        bindings.note_artifact("root", "late");
        assert_eq!(bindings.binding_for("root"), None);
    }

    #[test]
    fn a_report_waits_for_the_runner_and_a_late_one_is_refused() {
        let bindings = Arc::new(StepBindings::new());
        let lease = bindings.bind("root", binding());
        assert!(bindings.report("root", outcome("first")));
        assert!(bindings.report("root", outcome("second")));
        assert_eq!(
            lease.take_report().map(|o| o.summary().to_string()),
            Some("second".to_string()),
            "the last report is the one a helper meant"
        );
        assert_eq!(lease.take_report(), None, "a report is taken once");
        drop(lease);
        assert!(
            !bindings.report("root", outcome("late")),
            "a report after the run ended must be refused, not kept for nobody"
        );
    }
}
