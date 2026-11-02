//! My Pond's rules over the host port: what may be listed or deleted, which tasks an
//! administrator sees the words of, and when a restart would cost a running task its step.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::shared::domain::long_task::{ArtifactKind, LongTaskKind, LongTaskState};
use crate::shared::domain::pond_admin::{
    area_roots, classify, health_checks, AdminError, AreaId, AreaUsage, CacheId, CacheUsage,
    DataPath, HealthCheck, HostHealth, ListedEntry, PondOverview, PowerModeSet, PowerState,
    Visibility,
};
use crate::shared::ports::long_task::{LongTaskOwnerFilter, LongTaskRepository};
use crate::shared::ports::pond_admin::PondAdministrator;
use crate::shared::services::long_task_queue::reaches;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirectoryListing {
    pub path: String,
    pub entries: Vec<ListedEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthReport {
    #[serde(flatten)]
    pub host: HostHealth,
    pub checks: Vec<HealthCheck>,
}

/// A task as the administrator sees it. Another member's words stay theirs: `goal` and every
/// artifact `title` are `None`, as the run queue shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdminTask {
    pub id: String,
    pub goal: Option<String>,
    pub another_member: bool,
    pub state: LongTaskState,
    pub kind: LongTaskKind,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub has_report: bool,
    pub artifacts: Vec<AdminArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdminArtifact {
    pub id: String,
    pub title: Option<String>,
    pub kind: ArtifactKind,
    pub tokens: u32,
    pub created_at: DateTime<Utc>,
}

pub struct PondAdminService {
    host: Arc<dyn PondAdministrator>,
    tasks: Arc<dyn LongTaskRepository>,
}

impl PondAdminService {
    pub fn new(host: Arc<dyn PondAdministrator>, tasks: Arc<dyn LongTaskRepository>) -> Self {
        Self { host, tasks }
    }

    pub async fn overview(&self) -> Result<PondOverview, AdminError> {
        let storage = self.host.storage().await?;
        let mut by_area: HashMap<AreaId, u64> = HashMap::new();
        for ((area, _), bytes) in area_roots().iter().zip(&storage.area_root_bytes) {
            *by_area.entry(*area).or_default() += bytes;
        }
        let known: u64 = by_area.values().sum();
        by_area.insert(AreaId::Other, storage.data_dir_bytes.saturating_sub(known));
        let mut areas: Vec<AreaUsage> = by_area
            .into_iter()
            .map(|(id, bytes)| AreaUsage { id, bytes })
            .collect();
        areas.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.id.cmp(&b.id)));
        Ok(PondOverview {
            disk: storage.disk,
            data_dir_bytes: storage.data_dir_bytes,
            areas,
            databases: storage.databases,
            counts: storage.counts,
        })
    }

    /// One directory, largest first. A hidden path reads as absent, never as forbidden.
    pub async fn browse(&self, raw: &str) -> Result<DirectoryListing, AdminError> {
        let dir = DataPath::parse(raw)?;
        if classify(&dir) == Visibility::Hidden {
            return Err(AdminError::NotFound);
        }
        let mut entries = Vec::new();
        for entry in self.host.list_dir(&dir).await? {
            let Ok(path) = dir.child(&entry.name) else {
                continue;
            };
            let Visibility::Shown(protection) = classify(&path) else {
                continue;
            };
            entries.push(ListedEntry {
                name: entry.name,
                path: path.as_str().to_string(),
                kind: entry.kind,
                bytes: entry.bytes,
                modified_at: entry.modified_at,
                deletable: protection.is_none(),
                reason: protection.map(|p| p.as_str()),
            });
        }
        entries.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
        Ok(DirectoryListing {
            path: dir.as_str().to_string(),
            entries,
        })
    }

    pub async fn delete(&self, raw: &str) -> Result<u64, AdminError> {
        let path = DataPath::parse(raw)?;
        if path.is_root() {
            return Err(AdminError::InvalidPath);
        }
        match classify(&path) {
            Visibility::Hidden => Err(AdminError::NotFound),
            Visibility::Shown(Some(protection)) => Err(AdminError::Protected(protection)),
            Visibility::Shown(None) => self.host.delete(&path).await,
        }
    }

    pub async fn caches(&self) -> Result<Vec<CacheUsage>, AdminError> {
        self.host.caches().await
    }

    pub async fn clear_cache(&self, raw: &str) -> Result<u64, AdminError> {
        let cache = CacheId::parse(raw).ok_or(AdminError::UnknownCache)?;
        self.host.clear_cache(cache).await
    }

    /// Every task on the pond, newest first, as `member` may know it.
    pub async fn tasks(&self, member: &str) -> Result<Vec<AdminTask>, AdminError> {
        let tasks = self
            .tasks
            .list_tasks(LongTaskOwnerFilter::Everyone, true)
            .await?;
        let mut out = Vec::with_capacity(tasks.len());
        for task in tasks {
            let own = reaches(Some(member), &task);
            let artifacts = self.tasks.list_artifacts(task.id()).await?;
            out.push(AdminTask {
                id: task.id().to_string(),
                goal: own.then(|| task.goal().to_string()),
                another_member: !own,
                state: task.state(),
                kind: task.kind(),
                created_at: task.created_at(),
                finished_at: task.finished_at(),
                has_report: !artifacts.is_empty(),
                artifacts: artifacts
                    .into_iter()
                    .map(|a| AdminArtifact {
                        id: a.id,
                        title: own.then_some(a.title),
                        kind: a.kind,
                        tokens: a.tokens,
                        created_at: a.created_at,
                    })
                    .collect(),
            });
        }
        Ok(out)
    }

    /// Only a finished task goes: a live one is cancelled first, by its member or the queue.
    pub async fn delete_task(&self, id: &str) -> Result<(), AdminError> {
        self.finished_task(id).await?;
        if self.tasks.delete_task(id).await? {
            Ok(())
        } else {
            Err(AdminError::NotFound)
        }
    }

    pub async fn delete_artifact(
        &self,
        task_id: &str,
        artifact_id: &str,
    ) -> Result<(), AdminError> {
        self.finished_task(task_id).await?;
        if self.tasks.delete_artifact(task_id, artifact_id).await? {
            Ok(())
        } else {
            Err(AdminError::NotFound)
        }
    }

    async fn finished_task(&self, id: &str) -> Result<(), AdminError> {
        let task = self.tasks.get_task(id).await?.ok_or(AdminError::NotFound)?;
        if !task.state().is_terminal() {
            return Err(AdminError::TaskActive);
        }
        Ok(())
    }

    pub async fn health(&self) -> Result<HealthReport, AdminError> {
        let host = self.host.health().await?;
        let checks = health_checks(&host);
        Ok(HealthReport { host, checks })
    }

    pub async fn power(&self) -> Result<PowerState, AdminError> {
        self.host.power().await
    }

    pub async fn set_power_mode(&self, id: u32) -> Result<PowerModeSet, AdminError> {
        let power = self.host.power().await?;
        if !power.can_change {
            return Err(AdminError::PowerControlUnavailable);
        }
        if !power.modes.iter().any(|mode| mode.id == id) {
            return Err(AdminError::UnknownPowerMode);
        }
        self.host.set_power_mode(id).await
    }

    /// A restart kills the step a running task is on, so it waits for `force` while one runs.
    pub async fn restart(&self, force: bool) -> Result<(), AdminError> {
        if !force {
            let open = self
                .tasks
                .list_tasks(LongTaskOwnerFilter::Everyone, false)
                .await?;
            if open
                .iter()
                .any(|t| matches!(t.state(), LongTaskState::Planning | LongTaskState::Running))
            {
                return Err(AdminError::LongTaskRunning);
            }
        }
        self.host.restart().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::{Artifact, LongTask, LongTaskBudget};
    use crate::shared::domain::pond_admin::{CheckStatus, Protection};
    use crate::shared::mocks::mock_long_task::MockLongTaskRepository;
    use crate::shared::mocks::mock_pond_admin::MockPondAdministrator;
    use crate::user_data::domain::profile::ProfileScope;

    fn service(
        host: MockPondAdministrator,
    ) -> (
        Arc<MockPondAdministrator>,
        Arc<MockLongTaskRepository>,
        PondAdminService,
    ) {
        let host = Arc::new(host);
        let tasks = Arc::new(MockLongTaskRepository::new());
        let service = PondAdminService::new(host.clone(), tasks.clone());
        (host, tasks, service)
    }

    fn data_dir() -> MockPondAdministrator {
        MockPondAdministrator::new()
            .with_file("pond_system.db", 30)
            .with_file("pond_system.db-wal", 5)
            .with_file("pond_system.db.before-0062-removal", 7)
            .with_file("setup/admin.json", 1)
            .with_file(".runtime_host_credential", 1)
            .with_file("models/gguf/chat.gguf", 500)
            .with_file("hf_cache/hub/blob", 900)
            .with_file("logs/pond.log.2026-07-12", 40)
            .with_file("logs/pond.log.2026-10-29", 10)
            .with_file("engine/data/prompt-cache/v1-a.kv", 60)
            .with_file("engine/data/prompt-cache/suspended/task.kv", 20)
            .with_file("attachments/a/photo.jpg", 3)
    }

    #[tokio::test]
    async fn the_overview_totals_each_area_and_puts_the_rest_in_other() {
        let (_, _, admin) = service(data_dir());
        let overview = admin.overview().await.unwrap();
        let bytes = |id| overview.areas.iter().find(|a| a.id == id).map(|a| a.bytes);
        assert_eq!(bytes(AreaId::Models), Some(1_400));
        assert_eq!(bytes(AreaId::Conversations), Some(35));
        assert_eq!(bytes(AreaId::KvSnapshots), Some(80));
        assert_eq!(bytes(AreaId::Logs), Some(50));
        assert_eq!(bytes(AreaId::Attachments), Some(3));
        assert_eq!(
            bytes(AreaId::Other),
            Some(9),
            "the backup and the hidden files"
        );
        assert_eq!(overview.areas.first().map(|a| a.id), Some(AreaId::Models));
        assert_eq!(
            overview.areas.iter().map(|a| a.bytes).sum::<u64>(),
            overview.data_dir_bytes
        );
    }

    #[tokio::test]
    async fn the_root_lists_largest_first_without_any_credential() {
        let (_, _, admin) = service(data_dir());
        let root = admin.browse("").await.unwrap();
        let names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "hf_cache",
                "models",
                "engine",
                "logs",
                "pond_system.db",
                "pond_system.db.before-0062-removal",
                "pond_system.db-wal",
                "attachments",
            ]
        );
        let entry = |name: &str| root.entries.iter().find(|e| e.name == name).unwrap();
        assert_eq!(entry("models").reason, Some("managed_by_models"));
        assert_eq!(entry("pond_system.db").reason, Some("in_use"));
        assert_eq!(
            entry("logs").reason,
            Some("in_use"),
            "the directory itself stays"
        );
        assert!(entry("pond_system.db.before-0062-removal").deletable);
    }

    #[tokio::test]
    async fn a_hidden_directory_reads_as_absent() {
        let (_, _, admin) = service(data_dir());
        assert_eq!(admin.browse("setup").await, Err(AdminError::NotFound));
        assert_eq!(admin.browse("/setup/").await, Err(AdminError::NotFound));
        assert_eq!(
            admin.delete("setup/admin.json").await,
            Err(AdminError::NotFound)
        );
        assert_eq!(admin.browse("../setup").await, Err(AdminError::InvalidPath));
    }

    #[tokio::test]
    async fn a_directory_lists_its_entries_by_their_full_path() {
        let (_, _, admin) = service(data_dir());
        let logs = admin.browse("logs").await.unwrap();
        assert_eq!(logs.path, "logs");
        assert_eq!(logs.entries[0].path, "logs/pond.log.2026-07-12");
        assert!(logs.entries.iter().all(|e| e.deletable));
        assert_eq!(
            admin.browse("logs/pond.log.2026-07-12").await,
            Err(AdminError::NotADirectory)
        );
        assert_eq!(
            admin.browse("nothing-here").await,
            Err(AdminError::NotFound)
        );
    }

    #[tokio::test]
    async fn only_unprotected_paths_reach_the_host_for_deletion() {
        let (host, _, admin) = service(data_dir());
        assert_eq!(admin.delete("logs/pond.log.2026-07-12").await, Ok(40));
        assert_eq!(
            admin.delete("pond_system.db.before-0062-removal").await,
            Ok(7)
        );
        assert_eq!(
            admin.delete("engine/data/prompt-cache/v1-a.kv").await,
            Ok(60)
        );
        for (path, refusal) in [
            ("", AdminError::InvalidPath),
            ("/", AdminError::InvalidPath),
            ("logs", AdminError::Protected(Protection::InUse)),
            ("pond_system.db", AdminError::Protected(Protection::InUse)),
            (
                "pond_system.db-wal",
                AdminError::Protected(Protection::InUse),
            ),
            (
                "models/gguf/chat.gguf",
                AdminError::Protected(Protection::ManagedByModels),
            ),
            (
                "hf_cache",
                AdminError::Protected(Protection::ManagedByModels),
            ),
            (
                "engine/data/prompt-cache/suspended/task.kv",
                AdminError::Protected(Protection::InUse),
            ),
            (".runtime_host_credential", AdminError::NotFound),
            ("logs/../setup", AdminError::InvalidPath),
        ] {
            assert_eq!(admin.delete(path).await, Err(refusal), "{path:?}");
        }
        assert_eq!(
            host.deleted(),
            [
                "logs/pond.log.2026-07-12",
                "pond_system.db.before-0062-removal",
                "engine/data/prompt-cache/v1-a.kv",
            ],
            "nothing refused ever reached the host"
        );
    }

    #[tokio::test]
    async fn a_cache_is_cleared_only_by_its_id() {
        let host = MockPondAdministrator::new().with_cache(CacheId::KvSnapshots, 1_300);
        let (_, _, admin) = service(host);
        assert_eq!(admin.clear_cache("kv_snapshots").await, Ok(1_300));
        assert_eq!(admin.clear_cache("kv_snapshots").await, Ok(0));
        assert_eq!(
            admin.clear_cache("target").await,
            Err(AdminError::UnknownCache)
        );
        let caches = admin.caches().await.unwrap();
        assert_eq!(caches.len(), CacheId::ALL.len());
    }

    fn task(owner: Option<&str>, goal: &str) -> LongTask {
        let scope = match owner {
            Some(id) => ProfileScope::Owner(id.to_string()),
            None => ProfileScope::Household,
        };
        LongTask::start(
            scope,
            owner.map(String::from),
            goal,
            None,
            LongTaskBudget::new(10, None).unwrap(),
            Utc::now(),
        )
        .unwrap()
    }

    async fn finish(tasks: &MockLongTaskRepository, task: &LongTask) {
        tasks
            .set_task_state(task.id(), LongTaskState::Completed, Utc::now())
            .await
            .unwrap();
    }

    async fn with_artifact(tasks: &MockLongTaskRepository, task: &LongTask, title: &str) -> String {
        let artifact = Artifact::new(
            task.id(),
            None,
            ArtifactKind::Findings,
            title,
            "summary",
            "body".into(),
            Utc::now(),
        )
        .unwrap();
        tasks.insert_artifact(&artifact).await.unwrap();
        artifact.id().to_string()
    }

    #[tokio::test]
    async fn another_members_words_stay_theirs() {
        let (_, tasks, admin) = service(MockPondAdministrator::new());
        let mine = task(Some("jerry"), "plan the garden");
        let theirs = task(Some("liz"), "a surprise for jerry");
        let household = task(None, "compare energy tariffs");
        for t in [&mine, &theirs, &household] {
            tasks.create_task(t).await.unwrap();
            with_artifact(&tasks, t, "Findings on it").await;
        }
        let listed = admin.tasks("jerry").await.unwrap();
        let find = |id: &str| listed.iter().find(|t| t.id == id).unwrap();
        assert_eq!(find(mine.id()).goal.as_deref(), Some("plan the garden"));
        assert_eq!(
            find(household.id()).goal.as_deref(),
            Some("compare energy tariffs")
        );
        let other = find(theirs.id());
        assert!(other.another_member);
        assert_eq!(other.goal, None);
        assert_eq!(other.artifacts.len(), 1, "the count is shown");
        assert_eq!(other.artifacts[0].title, None, "the title is not");
        assert!(other.has_report);
    }

    #[tokio::test]
    async fn only_a_finished_task_or_its_artifact_can_be_deleted() {
        let (_, tasks, admin) = service(MockPondAdministrator::new());
        let running = task(Some("liz"), "still going");
        tasks.create_task(&running).await.unwrap();
        let artifact = with_artifact(&tasks, &running, "Draft").await;
        assert_eq!(
            admin.delete_task(running.id()).await,
            Err(AdminError::TaskActive)
        );
        assert_eq!(
            admin.delete_artifact(running.id(), &artifact).await,
            Err(AdminError::TaskActive)
        );

        finish(&tasks, &running).await;
        assert_eq!(admin.delete_artifact(running.id(), &artifact).await, Ok(()));
        assert_eq!(
            admin.delete_artifact(running.id(), &artifact).await,
            Err(AdminError::NotFound)
        );
        assert_eq!(admin.delete_task(running.id()).await, Ok(()));
        assert_eq!(
            admin.delete_task(running.id()).await,
            Err(AdminError::NotFound)
        );
        assert_eq!(
            admin.delete_task("no-such-task").await,
            Err(AdminError::NotFound)
        );
    }

    #[tokio::test]
    async fn an_artifact_is_deleted_only_through_its_own_task() {
        let (_, tasks, admin) = service(MockPondAdministrator::new());
        let a = task(None, "first");
        let b = task(None, "second");
        for t in [&a, &b] {
            tasks.create_task(t).await.unwrap();
            finish(&tasks, t).await;
        }
        let of_b = with_artifact(&tasks, &b, "B's").await;
        assert_eq!(
            admin.delete_artifact(a.id(), &of_b).await,
            Err(AdminError::NotFound)
        );
        assert_eq!(tasks.artifact_count(), 1);
    }

    #[tokio::test]
    async fn health_carries_its_checks() {
        let (_, _, admin) = service(MockPondAdministrator::new());
        let report = admin.health().await.unwrap();
        assert!(report.checks.iter().all(|c| c.status == CheckStatus::Ok));
        let json = serde_json::to_value(&report).unwrap();
        assert!(json.get("host").is_some() && json.get("checks").is_some());
    }

    #[tokio::test]
    async fn a_power_mode_must_be_one_the_host_lists() {
        let (_, _, admin) = service(MockPondAdministrator::new());
        assert_eq!(
            admin.set_power_mode(9).await,
            Err(AdminError::UnknownPowerMode)
        );
        let set = admin.set_power_mode(0).await.unwrap();
        assert_eq!(set.mode.name, "15W");
        assert_eq!(admin.power().await.unwrap().mode.unwrap().id, 0);

        let (_, _, locked) = service(MockPondAdministrator::new().without_power_control());
        assert_eq!(
            locked.set_power_mode(0).await,
            Err(AdminError::PowerControlUnavailable)
        );
        assert_eq!(
            locked.restart(true).await,
            Err(AdminError::PowerControlUnavailable)
        );
    }

    #[tokio::test]
    async fn a_restart_waits_for_force_while_a_task_is_working() {
        let (host, tasks, admin) = service(MockPondAdministrator::new());
        let working = task(None, "research");
        tasks.create_task(&working).await.unwrap();
        assert_eq!(admin.restart(false).await, Err(AdminError::LongTaskRunning));
        assert_eq!(host.restarts(), 0);

        tasks
            .set_task_state(working.id(), LongTaskState::Paused, Utc::now())
            .await
            .unwrap();
        assert_eq!(
            admin.restart(false).await,
            Ok(()),
            "a paused task loses no step"
        );

        tasks
            .set_task_state(working.id(), LongTaskState::Running, Utc::now())
            .await
            .unwrap();
        assert_eq!(admin.restart(true).await, Ok(()));
        assert_eq!(host.restarts(), 2);
    }
}
