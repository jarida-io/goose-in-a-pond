//! What the pond is compiling now and which rebuilds members asked for. The compile passes write
//! it, the status route reads it, and Home shows it. In memory: a restart starts with nothing
//! building and the boot passes fill it again.
use serde::Serialize;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// The state of prefill building, as the status route reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PrefillBuild {
    pub active: bool,
    /// The agent or shape compiling right now.
    pub current: Option<String>,
    pub done: usize,
    pub total: usize,
}

/// Rebuilds members asked for, taken by the next compile pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildRequests {
    pub all: bool,
    pub agents: BTreeSet<String>,
}

impl RebuildRequests {
    pub fn is_empty(&self) -> bool {
        !self.all && self.agents.is_empty()
    }

    pub fn includes(&self, agent: &str) -> bool {
        self.all || self.agents.contains(agent)
    }
}

#[derive(Default)]
pub struct PrefillProgress {
    build: RwLock<PrefillBuild>,
    failed: RwLock<BTreeSet<String>>,
    rebuilds: Mutex<RebuildRequests>,
}

impl PrefillProgress {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `count` compiles to the pass under way; passes that overlap share one count.
    pub fn begin(&self, count: usize) {
        if count == 0 {
            return;
        }
        let mut build = self.build.write().unwrap_or_else(|e| e.into_inner());
        build.active = true;
        build.total += count;
    }

    pub fn working_on(&self, agent: &str) {
        self.build
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .current = Some(agent.to_string());
    }

    /// One compile ended; when every begun compile has, building is over.
    pub fn finished(&self, agent: &str, ok: bool) {
        {
            let mut failed = self.failed.write().unwrap_or_else(|e| e.into_inner());
            if ok {
                failed.remove(agent);
            } else {
                failed.insert(agent.to_string());
            }
        }
        let mut build = self.build.write().unwrap_or_else(|e| e.into_inner());
        build.done += 1;
        if build.done >= build.total {
            *build = PrefillBuild::default();
        }
    }

    pub fn build(&self) -> PrefillBuild {
        self.build.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Agents whose last compile failed, so the status shows it rather than a stale "ready".
    pub fn failed(&self) -> BTreeSet<String> {
        self.failed
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// A member asked for one agent's cache to be invalidated and rebuilt, or every agent's.
    pub fn request_rebuild(&self, agent: Option<&str>) {
        let mut rebuilds = self.rebuilds.lock().unwrap_or_else(|e| e.into_inner());
        match agent {
            Some(agent) => {
                rebuilds.agents.insert(agent.to_string());
            }
            None => rebuilds.all = true,
        }
    }

    pub fn pending_rebuilds(&self) -> RebuildRequests {
        self.rebuilds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The requests a compile pass is about to honour, taken once.
    pub fn take_rebuilds(&self) -> RebuildRequests {
        std::mem::take(&mut *self.rebuilds.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// The process's one record: the compile passes and the status routes share it.
pub fn process_prefill_progress() -> Arc<PrefillProgress> {
    static PROGRESS: OnceLock<Arc<PrefillProgress>> = OnceLock::new();
    PROGRESS
        .get_or_init(|| Arc::new(PrefillProgress::new()))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_passes_share_one_count_and_end_together() {
        let progress = PrefillProgress::new();
        assert!(!progress.build().active);
        progress.begin(1);
        progress.working_on("prime-typed");
        progress.begin(3);
        assert_eq!(progress.build().total, 4);
        progress.finished("prime-typed", true);
        for agent in ["long-task-planner", "long-task-worker"] {
            progress.working_on(agent);
            progress.finished(agent, true);
        }
        let build = progress.build();
        assert!(build.active, "one compile is still to go");
        assert_eq!((build.done, build.total), (3, 4));
        progress.finished("study-planner", false);
        assert_eq!(
            progress.build(),
            PrefillBuild::default(),
            "every begun compile ended"
        );
        assert_eq!(
            progress.failed(),
            BTreeSet::from(["study-planner".to_string()]),
            "a failed compile is remembered until one succeeds"
        );
        progress.begin(1);
        progress.finished("study-planner", true);
        assert!(progress.failed().is_empty());
    }

    #[test]
    fn rebuild_requests_are_taken_once() {
        let progress = PrefillProgress::new();
        assert!(progress.pending_rebuilds().is_empty());
        progress.request_rebuild(Some("study-planner"));
        assert!(progress.pending_rebuilds().includes("study-planner"));
        assert!(!progress.pending_rebuilds().includes("trip-planner"));
        progress.request_rebuild(None);
        assert!(
            progress.pending_rebuilds().includes("trip-planner"),
            "all includes every agent"
        );
        assert!(progress.take_rebuilds().all);
        assert!(progress.take_rebuilds().is_empty(), "taken once");
    }
}
