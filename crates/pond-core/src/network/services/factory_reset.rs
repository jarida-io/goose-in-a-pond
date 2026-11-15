//! Journey 7: wipe a Pond before handing it on, over ports only; the erase itself is
//! [`crate::network::ports::personal_data_wipe::PersonalDataWipe`]. Who may reset is the caller's
//! to check: by the time [`FactoryResetService::execute`] runs, the host or the main member asked.

use std::sync::Arc;

use anyhow::Result;

use crate::mcp::ports::notification::{MemberDelivery, MemberNotifier, Notification};
use crate::models::ports::voice_output::VoiceOutput;
use crate::network::domain::factory_reset_phrases::factory_reset_notice;
use crate::network::ports::personal_data_wipe::PersonalDataWipe;
use crate::network::services::setup_claim::SetupClaimService;
use crate::security::ports::handshake::Handshake;
use crate::security::ports::remote_access::RemoteRevocation;
use crate::user_data::domain::schedule::Schedule;
use crate::user_data::ports::device_registry::DeviceRegistry;
use crate::user_data::ports::onboarding::OnboardingRepository;
use crate::user_data::ports::profile::ProfileRepository;
use crate::user_data::ports::scheduler::SchedulerPort;
use crate::user_data::services::onboarding::OnboardingService;

/// Routines handed back to the caller so GOTG can save them before the Pond forgets them.
#[derive(Debug, Clone, Default)]
pub struct FactoryResetExport {
    pub routines: Vec<Schedule>,
}

/// Where `execute` is, in order. A fixed, known sequence — unlike a download's byte count,
/// there is no meaningful percentage here, only which of these a caller has reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactoryResetStep {
    Exporting,
    Pausing,
    NotifyingHousehold,
    RevokingDevices,
    ErasingData,
    SpeakingGoodbye,
    ReopeningOnboarding,
    ForgettingWifi,
}

impl FactoryResetStep {
    /// Every step in execution order; `position` and the status route's `total_steps` derive
    /// from it, same as `OnboardingStep::ALL`.
    pub const ALL: [FactoryResetStep; 8] = [
        Self::Exporting,
        Self::Pausing,
        Self::NotifyingHousehold,
        Self::RevokingDevices,
        Self::ErasingData,
        Self::SpeakingGoodbye,
        Self::ReopeningOnboarding,
        Self::ForgettingWifi,
    ];

    /// 1-based position within [`FactoryResetStep::ALL`].
    pub fn position(self) -> usize {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .map(|i| i + 1)
            .unwrap_or(0)
    }
}

/// Driven port: where `execute` reports which step it has reached, for a status route to poll.
/// Best-effort like the rest of `FactoryResetPorts` — a caller that never polls loses nothing.
pub trait FactoryResetProgress: Send + Sync {
    fn report(&self, step: FactoryResetStep);
}

/// Driven port: quiets what could write household data while the reset erases it.
#[async_trait::async_trait]
pub trait FactoryResetQuiet: Send + Sync {
    /// Stops background model work, the room's voice and new turns, then waits a bounded time
    /// for what is already running.
    async fn quiet(&self);
    /// Undoes [`Self::quiet`]; called however the reset ends.
    fn resume(&self);
}

/// Longest the reset waits for a schedule's run already going.
const SCHEDULE_SETTLE: std::time::Duration = std::time::Duration::from_secs(30);

/// Why a reset was refused before anything was touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactoryResetRefusal {
    /// The Pond has no name set, so nothing safe exists to type as confirmation — an unnamed
    /// Pond's name is `""`, which an empty `typed_pond_name` would otherwise match trivially.
    NoPondName,
    /// The typed Pond name didn't match the real one.
    NameMismatch,
    /// A trusted peer holds a nonzero mesh-credit balance.
    PaymentsHeld,
}

/// Why a reset did not finish.
#[derive(Debug)]
pub enum FactoryResetError {
    /// Refused before anything was touched.
    Refused(FactoryResetRefusal),
    /// A step the reset cannot go on without failed, and nothing after it ran: onboarding was not
    /// reopened. `export` holds the routines read before it, which may be gone from the scheduler.
    Failed {
        step: FactoryResetStep,
        reason: String,
        export: FactoryResetExport,
    },
}

impl From<FactoryResetRefusal> for FactoryResetError {
    fn from(refusal: FactoryResetRefusal) -> Self {
        Self::Refused(refusal)
    }
}

/// Every port the reset touches. Most are `Option` because the step they back is best-effort —
/// nothing here aborts the reset over a missing TTS engine or an unconfigured scheduler.
pub struct FactoryResetPorts {
    pub onboarding: Arc<dyn OnboardingRepository + Send + Sync>,
    pub setup_claims: Option<Arc<SetupClaimService>>,
    pub scheduler: Option<Arc<dyn SchedulerPort>>,
    pub device_registry: Arc<dyn DeviceRegistry + Send + Sync>,
    pub handshake: Arc<dyn Handshake>,
    pub remote: Option<Arc<dyn RemoteRevocation>>,
    pub profile_repo: Arc<dyn ProfileRepository + Send + Sync>,
    /// Delivers the household notice to each member's own devices, in their own language —
    /// never a broadcast, so no member's copy is ever rendered in another's.
    pub member_notifier: Option<Arc<dyn MemberNotifier>>,
    pub tts: Option<Arc<dyn VoiceOutput>>,
    pub credit_ledger: Arc<dyn crate::mesh::ports::credit_ledger::CreditLedger + Send + Sync>,
    pub data_wipe: Arc<dyn PersonalDataWipe>,
    pub progress: Option<Arc<dyn FactoryResetProgress>>,
    pub quiet: Option<Arc<dyn FactoryResetQuiet>>,
}

/// Keeps the pond quiet until dropped, however the reset ends.
struct Quieted(Option<Arc<dyn FactoryResetQuiet>>);

impl Quieted {
    async fn start(quiet: Option<Arc<dyn FactoryResetQuiet>>) -> Self {
        if let Some(quiet) = &quiet {
            quiet.quiet().await;
        }
        Self(quiet)
    }
}

impl Drop for Quieted {
    fn drop(&mut self) {
        if let Some(quiet) = &self.0 {
            quiet.resume();
        }
    }
}

pub struct FactoryResetService;

impl FactoryResetService {
    /// Runs the whole journey: refuse (name/payments) → export → pause → notify → revoke → erase
    /// → speak → rearm → reset onboarding → forget Wi-Fi. Members are notified while their
    /// devices still resolve. Notifying, revoking, the goodbye, the rearm and the Wi-Fi are best
    /// effort; a failed erase or onboarding reset stops the reset there, resumes the schedules it
    /// paused and keeps the Wi-Fi, and is returned, never reported as done.
    pub async fn execute(
        ports: &FactoryResetPorts,
        typed_pond_name: &str,
        actual_pond_name: &str,
    ) -> Result<FactoryResetExport, FactoryResetError> {
        if actual_pond_name.trim().is_empty() {
            return Err(FactoryResetRefusal::NoPondName.into());
        }
        if typed_pond_name != actual_pond_name {
            return Err(FactoryResetRefusal::NameMismatch.into());
        }

        // Fail safe: an unreadable ledger is treated as "money might be held", never as clear.
        if ports.credit_ledger.has_any_balance().await.unwrap_or(true) {
            return Err(FactoryResetRefusal::PaymentsHeld.into());
        }

        Self::report(ports, FactoryResetStep::Exporting);
        let routines = match &ports.scheduler {
            Some(scheduler) => scheduler.list_tasks().await.unwrap_or_else(|error| {
                tracing::warn!(%error, "factory reset: could not export routines");
                Vec::new()
            }),
            None => Vec::new(),
        };

        Self::report(ports, FactoryResetStep::Pausing);
        let _quiet = Quieted::start(ports.quiet.clone()).await;
        let paused = Self::pause_every_schedule(ports).await;

        let finished = Self::erase_and_reopen(ports, routines).await;
        if finished.is_err() {
            Self::resume_schedules(ports, &paused).await;
            return finished;
        }

        Self::report(ports, FactoryResetStep::ForgettingWifi);
        if let Err(error) = ports.data_wipe.forget_networks().await {
            tracing::warn!(error = %format!("{error:#}"), "factory reset: saved Wi-Fi networks were not forgotten");
        }
        finished
    }

    /// From the household notice to onboarding reopened; a step that must not fail stops it.
    async fn erase_and_reopen(
        ports: &FactoryResetPorts,
        routines: Vec<Schedule>,
    ) -> Result<FactoryResetExport, FactoryResetError> {
        // Snapshot every member's own language before `profiles` is erased — the household
        // notice is rendered now, even though it isn't sent until after revocation below.
        let household_languages: Vec<(String, String)> = ports
            .profile_repo
            .list()
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "factory reset: could not read profiles for the household notice");
                Vec::new()
            })
            .into_iter()
            .map(|profile| {
                let language = profile.preferences.get("language").cloned().unwrap_or_default();
                (profile.id, language)
            })
            .collect();

        // Notify before revoking: `notify_household` resolves each member's own devices through
        // `devices_for_profile`, which `revoke_every_device` deletes the rows behind.
        Self::report(ports, FactoryResetStep::NotifyingHousehold);
        Self::notify_household(ports, &household_languages).await;

        Self::report(ports, FactoryResetStep::RevokingDevices);
        Self::revoke_every_device(ports).await;

        Self::report(ports, FactoryResetStep::ErasingData);
        let erased = match Self::delete_every_schedule(ports).await {
            Ok(()) => ports.data_wipe.wipe().await,
            Err(error) => Err(error),
        };
        if let Err(error) = erased {
            return Err(Self::failed(FactoryResetStep::ErasingData, error, routines));
        }

        Self::report(ports, FactoryResetStep::SpeakingGoodbye);
        if let Some(tts) = &ports.tts {
            if let Err(error) = tts.speak("Goodbye. This Pond has been reset.").await {
                tracing::warn!(%error, "factory reset: could not speak the goodbye");
            }
        }

        if let Some(setup_claims) = &ports.setup_claims {
            if let Err(error) = setup_claims.rearm().await {
                tracing::warn!(%error, "factory reset: could not rearm the setup card");
            }
        }

        Self::report(ports, FactoryResetStep::ReopeningOnboarding);
        let onboarding = OnboardingService::new(ports.onboarding.clone());
        let reopened = match onboarding.reset().await {
            Ok(()) => onboarding.start().await,
            Err(error) => Err(error),
        };
        if let Err(error) = reopened {
            return Err(Self::failed(
                FactoryResetStep::ReopeningOnboarding,
                error,
                routines,
            ));
        }

        Ok(FactoryResetExport { routines })
    }

    fn failed(
        step: FactoryResetStep,
        error: anyhow::Error,
        routines: Vec<Schedule>,
    ) -> FactoryResetError {
        tracing::error!(?step, error = %format!("{error:#}"), "factory reset: stopped before it finished");
        FactoryResetError::Failed {
            step,
            reason: format!("{error:#}"),
            export: FactoryResetExport { routines },
        }
    }

    /// Pauses every active schedule so none fires mid-erase, then waits a bounded time for runs
    /// already going. The ones it paused, to resume if the reset stops.
    async fn pause_every_schedule(ports: &FactoryResetPorts) -> Vec<String> {
        let Some(scheduler) = &ports.scheduler else {
            return Vec::new();
        };
        let tasks = scheduler.list_tasks().await.unwrap_or_else(|error| {
            tracing::warn!(%error, "factory reset: could not list the schedules to pause");
            Vec::new()
        });
        let mut paused = Vec::new();
        for task in tasks.iter().filter(|task| !task.paused) {
            match scheduler.pause_task(&task.id).await {
                Ok(()) => paused.push(task.id.clone()),
                Err(error) => {
                    tracing::warn!(task = %task.id, %error, "factory reset: could not pause a schedule")
                }
            }
        }
        let deadline = tokio::time::Instant::now() + SCHEDULE_SETTLE;
        while scheduler
            .list_tasks()
            .await
            .is_ok_and(|tasks| tasks.iter().any(|task| task.currently_running))
        {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!("factory reset: a schedule is still running; erasing beside it");
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        paused
    }

    /// The paused schedules a stopped reset left in place.
    async fn resume_schedules(ports: &FactoryResetPorts, paused: &[String]) {
        let Some(scheduler) = &ports.scheduler else {
            return;
        };
        let Ok(left) = scheduler.list_tasks().await else {
            return;
        };
        for task in left.iter().filter(|task| paused.contains(&task.id)) {
            if let Err(error) = scheduler.resume_task(&task.id).await {
                tracing::warn!(task = %task.id, %error, "factory reset: could not resume a schedule");
            }
        }
    }

    /// Through the scheduler, so its routines stop firing now, not at the next start.
    async fn delete_every_schedule(ports: &FactoryResetPorts) -> Result<()> {
        let Some(scheduler) = &ports.scheduler else {
            return Ok(());
        };
        for task in scheduler.list_tasks().await? {
            if let Err(error) = scheduler.delete_task(&task.id).await {
                tracing::warn!(task = %task.id, %error, "factory reset: could not delete a schedule");
            }
        }
        let left = scheduler.list_tasks().await?.len();
        if left > 0 {
            anyhow::bail!("{left} schedules could not be deleted");
        }
        Ok(())
    }

    fn report(ports: &FactoryResetPorts, step: FactoryResetStep) {
        if let Some(progress) = &ports.progress {
            progress.report(step);
        }
    }

    async fn revoke_every_device(ports: &FactoryResetPorts) {
        let devices = ports
            .device_registry
            .list_devices()
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "factory reset: could not list paired devices to revoke");
                Vec::new()
            });
        for device in &devices {
            if let Err(error) = ports.handshake.revoke_device(&device.id).await {
                tracing::warn!(device_id = %device.id, %error, "factory reset: could not revoke a device's sessions");
            }
            if let Some(remote) = &ports.remote {
                if let Err(error) = remote.queue(&device.id).await {
                    tracing::warn!(device_id = %device.id, %error, "factory reset: could not queue a device's remote revocation");
                }
            }
            if let Err(error) = ports.device_registry.unregister(&device.id).await {
                tracing::warn!(device_id = %device.id, %error, "factory reset: could not unregister a device");
            }
        }

        // The enrolled-for-remote-access set isn't necessarily identical to `DeviceRegistry`'s
        // paired devices, so it's revoked separately rather than assumed to be the same ids.
        if let Some(remote) = &ports.remote {
            match remote.enrolled_devices().await {
                Ok(enrolled) => {
                    for device_id in enrolled {
                        if let Err(error) = remote.queue(&device_id).await {
                            tracing::warn!(device_id = %device_id, %error, "factory reset: could not queue an enrolled device's remote revocation");
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "factory reset: could not list remote-access-enrolled devices")
                }
            }
        }
    }

    /// Tells each member their own Pond was reset, in their own language — never everyone at
    /// once in the same wording, since `(profile_id, language)` differs per member.
    async fn notify_household(ports: &FactoryResetPorts, household_languages: &[(String, String)]) {
        let Some(notifier) = &ports.member_notifier else {
            return;
        };
        for (profile_id, language) in household_languages {
            let notification = Notification {
                id: format!("factory-reset-{}", uuid::Uuid::new_v4()),
                target: String::new(), // overwritten by `notify_member()`
                category: "alert".to_string(),
                title: "Pond reset".to_string(),
                body: factory_reset_notice(language).to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                data: None,
            };
            if let MemberDelivery::Failed(reason) =
                notifier.notify_member(profile_id, notification).await
            {
                tracing::warn!(
                    profile_id = %profile_id,
                    reason = %reason,
                    "factory reset: could not notify a member of the reset"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::mocks::mock_member_notifier::MockMemberNotifier;
    use crate::mesh::domain::millisats::Millisats;
    use crate::mesh::domain::peer_id::PeerId;
    use crate::mesh::mocks::mock_credit_ledger::MockCreditLedger;
    use crate::mesh::ports::credit_ledger::CreditLedger as _;
    use crate::network::mocks::mock_setup_code_store::MockSetupCodeStore;
    use crate::user_data::domain::onboarding::OnboardingStep;
    use crate::user_data::domain::profile::CreateProfileRequest;
    use crate::user_data::mocks::mock_profile::MockProfileRepository;
    use crate::user_data::ports::device_registry::{
        Device, RegisterDeviceRequest, UpdateDeviceRequest,
    };
    use crate::user_data::ports::scheduler::{CreateScheduleRequest, UpdateScheduleRequest};
    use std::sync::Mutex;

    /// Records every port call it makes, in order, so tests can assert the execution sequence.
    #[derive(Default)]
    struct CallLog(Mutex<Vec<&'static str>>);

    impl CallLog {
        fn record(&self, call: &'static str) {
            self.0.lock().unwrap().push(call);
        }
        fn calls(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().clone()
        }
    }

    struct FakeOnboarding(Arc<CallLog>);
    #[async_trait::async_trait]
    impl OnboardingRepository for FakeOnboarding {
        async fn get_current_step(&self) -> Option<OnboardingStep> {
            None
        }
        async fn save_step(&self, _: OnboardingStep) -> Result<()> {
            Ok(())
        }
        async fn reset(&self) -> Result<()> {
            self.0.record("onboarding.reset");
            Ok(())
        }
        async fn is_complete(&self) -> Result<bool> {
            Ok(true)
        }
    }

    struct FakeScheduler(Arc<CallLog>, Mutex<Vec<Schedule>>);
    impl FakeScheduler {
        fn new(log: Arc<CallLog>, tasks: Vec<Schedule>) -> Self {
            Self(log, Mutex::new(tasks))
        }
    }
    #[async_trait::async_trait]
    impl SchedulerPort for FakeScheduler {
        async fn create_task(&self, _: CreateScheduleRequest) -> Result<Schedule> {
            unreachable!()
        }
        async fn list_tasks(&self) -> Result<Vec<Schedule>> {
            self.0.record("scheduler.list_tasks");
            Ok(self.1.lock().unwrap().clone())
        }
        async fn delete_task(&self, id: &str) -> Result<()> {
            self.0.record("scheduler.delete_task");
            if id.starts_with("stuck") {
                anyhow::bail!("schedules.json is read-only");
            }
            self.1.lock().unwrap().retain(|task| task.id != id);
            Ok(())
        }
        async fn pause_task(&self, id: &str) -> Result<()> {
            self.0.record("scheduler.pause_task");
            for task in self.1.lock().unwrap().iter_mut().filter(|t| t.id == id) {
                task.paused = true;
            }
            Ok(())
        }
        async fn resume_task(&self, id: &str) -> Result<()> {
            self.0.record("scheduler.resume_task");
            for task in self.1.lock().unwrap().iter_mut().filter(|t| t.id == id) {
                task.paused = false;
            }
            Ok(())
        }
        async fn run_now(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn update_task(&self, _: &str, _: UpdateScheduleRequest) -> Result<Schedule> {
            unreachable!()
        }
        async fn get_runs(
            &self,
            _: &str,
            _: u32,
        ) -> Result<Vec<crate::user_data::domain::schedule::ScheduleRun>> {
            Ok(Vec::new())
        }
        async fn list_upcoming(&self, _: u32) -> Result<Vec<Schedule>> {
            Ok(Vec::new())
        }
        async fn set_executor(
            &self,
            _: Arc<dyn crate::user_data::ports::schedule_execution::ScheduleExecutor>,
        ) -> Result<()> {
            Ok(())
        }
    }

    struct FakeDeviceRegistry(Arc<CallLog>, Vec<Device>);
    #[async_trait::async_trait]
    impl DeviceRegistry for FakeDeviceRegistry {
        async fn register(&self, _: RegisterDeviceRequest) -> Result<Device> {
            unreachable!()
        }
        async fn list_devices(&self) -> Result<Vec<Device>> {
            self.0.record("devices.list_devices");
            Ok(self.1.clone())
        }
        async fn get_device(&self, _: &str) -> Result<Option<Device>> {
            Ok(None)
        }
        async fn unregister(&self, _: &str) -> Result<()> {
            self.0.record("devices.unregister");
            Ok(())
        }
        async fn heartbeat(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn update(&self, _: &str, _: UpdateDeviceRequest) -> Result<Device> {
            unreachable!()
        }
    }

    struct FakeHandshake(Arc<CallLog>);
    #[async_trait::async_trait]
    impl Handshake for FakeHandshake {
        async fn handshake(
            &self,
            _: crate::security::ports::handshake::HandshakeRequest,
        ) -> Result<crate::security::ports::handshake::HandshakeResponse> {
            unreachable!()
        }
        async fn validate_token(&self, _: &str) -> Result<bool> {
            Ok(false)
        }
        async fn revoke_device(&self, _: &str) -> Result<u64> {
            self.0.record("handshake.revoke_device");
            Ok(1)
        }
        async fn revoke_token(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    struct FakeRemote(Arc<CallLog>, Vec<String>);
    #[async_trait::async_trait]
    impl RemoteRevocation for FakeRemote {
        async fn queue(&self, _: &str) -> Result<()> {
            self.0.record("remote.queue");
            Ok(())
        }
        async fn enrolled_devices(&self) -> Result<Vec<String>> {
            self.0.record("remote.enrolled_devices");
            Ok(self.1.clone())
        }
    }

    struct FakeVoiceOutput(Arc<CallLog>);
    #[async_trait::async_trait]
    impl VoiceOutput for FakeVoiceOutput {
        async fn speak(&self, _: &str) -> Result<()> {
            self.0.record("tts.speak");
            Ok(())
        }
    }

    struct FailingVoiceOutput;
    #[async_trait::async_trait]
    impl VoiceOutput for FailingVoiceOutput {
        async fn speak(&self, _: &str) -> Result<()> {
            anyhow::bail!("no audio device")
        }
    }

    struct LoggingMemberNotifier(Arc<CallLog>);
    #[async_trait::async_trait]
    impl MemberNotifier for LoggingMemberNotifier {
        async fn notify_member(&self, _: &str, _: Notification) -> MemberDelivery {
            self.0.record("member_notifier.notify_member");
            MemberDelivery::NoPhone
        }
    }

    struct FakeWipe(Arc<CallLog>);
    #[async_trait::async_trait]
    impl PersonalDataWipe for FakeWipe {
        async fn wipe(&self) -> Result<()> {
            self.0.record("data_wipe.wipe");
            Ok(())
        }
        async fn forget_networks(&self) -> Result<()> {
            self.0.record("data_wipe.forget_networks");
            Ok(())
        }
    }

    struct FakeQuiet(Arc<CallLog>);
    #[async_trait::async_trait]
    impl FactoryResetQuiet for FakeQuiet {
        async fn quiet(&self) {
            self.0.record("quiet.quiet");
        }
        fn resume(&self) {
            self.0.record("quiet.resume");
        }
    }

    fn device(id: &str) -> Device {
        Device {
            id: id.to_string(),
            name: id.to_string(),
            device_type: "gotg".to_string(),
            hostname: None,
            ip_address: None,
            capabilities: Vec::new(),
            registered_at: "2026-01-01T00:00:00Z".to_string(),
            last_seen: None,
            is_online: true,
            room_id: None,
        }
    }

    /// Everything wired to succeed, with `log` shared so a test can inspect call order; `owner`
    /// (if `Some`) is created first in `profile_repo` so `MockProfileRepository::create`'s
    /// first-profile-is-owner rule gives it `Role::Owner`.
    async fn happy_ports(
        log: Arc<CallLog>,
        devices: Vec<Device>,
        owner_language: Option<&str>,
    ) -> FactoryResetPorts {
        let profile_repo = MockProfileRepository::new();
        let owner = profile_repo
            .create(CreateProfileRequest {
                display_name: "Owner".to_string(),
                avatar_emoji: "🦆".to_string(),
            })
            .await
            .unwrap();
        if let Some(language) = owner_language {
            let mut prefs = std::collections::HashMap::new();
            prefs.insert("language".to_string(), language.to_string());
            profile_repo
                .update_preferences(&owner.id, prefs)
                .await
                .unwrap();
        }

        FactoryResetPorts {
            onboarding: Arc::new(FakeOnboarding(log.clone())),
            setup_claims: Some(Arc::new(SetupClaimService::new(Arc::new(
                MockSetupCodeStore::unclaimed(),
            )))),
            scheduler: Some(Arc::new(FakeScheduler::new(log.clone(), Vec::new()))),
            device_registry: Arc::new(FakeDeviceRegistry(log.clone(), devices.clone())),
            handshake: Arc::new(FakeHandshake(log.clone())),
            remote: Some(Arc::new(FakeRemote(log.clone(), Vec::new()))),
            profile_repo: Arc::new(profile_repo),
            member_notifier: Some(Arc::new(MockMemberNotifier::new())),
            tts: Some(Arc::new(FakeVoiceOutput(log.clone()))),
            credit_ledger: Arc::new(MockCreditLedger::new()),
            data_wipe: Arc::new(FakeWipe(log.clone())),
            progress: None,
            quiet: Some(Arc::new(FakeQuiet(log))),
        }
    }

    fn refusal(result: Result<FactoryResetExport, FactoryResetError>) -> FactoryResetRefusal {
        match result {
            Err(FactoryResetError::Refused(refusal)) => refusal,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    struct BrokenWipe;
    #[async_trait::async_trait]
    impl PersonalDataWipe for BrokenWipe {
        async fn wipe(&self) -> Result<()> {
            anyhow::bail!("disk I/O error")
        }
        async fn forget_networks(&self) -> Result<()> {
            panic!("a failed reset must keep the pond on its network")
        }
    }

    #[tokio::test]
    async fn a_failed_erase_stops_the_reset_before_onboarding_reopens() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log.clone(), vec![device("phone-1")], None).await;
        ports.data_wipe = Arc::new(BrokenWipe);
        ports.scheduler = Some(Arc::new(FakeScheduler::new(
            log.clone(),
            vec![routine("routine-1")],
        )));

        let result =
            FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond").await;

        match result {
            Err(FactoryResetError::Failed {
                step,
                reason,
                export,
            }) => {
                assert_eq!(step, FactoryResetStep::ErasingData);
                assert!(reason.contains("disk I/O error"), "{reason}");
                assert_eq!(
                    export.routines.len(),
                    1,
                    "the routines still reach the caller"
                );
            }
            other => panic!("a failed erase must not read as done: {other:?}"),
        }
        let calls = log.calls();
        assert!(
            !calls.contains(&"onboarding.reset"),
            "onboarding stays as it was"
        );
        assert!(
            !calls.contains(&"tts.speak"),
            "nothing after the erase runs"
        );
    }

    #[tokio::test]
    async fn name_mismatch_refuses_before_any_mutation() {
        let log = Arc::new(CallLog::default());
        let ports = happy_ports(log.clone(), vec![device("phone-1")], None).await;

        let result = FactoryResetService::execute(&ports, "Wrong Name", "The Kitchen Pond").await;

        assert_eq!(refusal(result), FactoryResetRefusal::NameMismatch);
        assert!(
            log.calls().is_empty(),
            "nothing should run before the name check"
        );
    }

    #[tokio::test]
    async fn an_unnamed_pond_refuses_rather_than_let_an_empty_typed_name_match() {
        let log = Arc::new(CallLog::default());
        let ports = happy_ports(log.clone(), vec![device("phone-1")], None).await;

        let result = FactoryResetService::execute(&ports, "", "").await;

        assert_eq!(refusal(result), FactoryResetRefusal::NoPondName);
        assert!(
            log.calls().is_empty(),
            "nothing should run before the name check"
        );
    }

    #[tokio::test]
    async fn payments_held_refuses_before_any_mutation() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log.clone(), vec![device("phone-1")], None).await;
        let ledger = MockCreditLedger::new();
        ledger
            .credit(PeerId::from([1u8; 32]), Millisats::new(1000))
            .await
            .unwrap();
        ports.credit_ledger = Arc::new(ledger);

        let result =
            FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond").await;

        assert_eq!(refusal(result), FactoryResetRefusal::PaymentsHeld);
        assert!(
            log.calls().is_empty(),
            "nothing should run before the payment check"
        );
    }

    fn schedule(id: &str, kind: crate::user_data::domain::schedule::TaskKind) -> Schedule {
        Schedule {
            id: id.to_string(),
            label: "Morning routine".to_string(),
            cron: "0 0 7 * * *".to_string(),
            fire_at: None,
            timezone: "UTC".to_string(),
            kind,
            paused: false,
            currently_running: false,
            last_run: None,
            next_run: None,
            created_at: chrono::Utc::now(),
            ask_if_missed: false,
            missed_run: None,
        }
    }

    fn routine(id: &str) -> Schedule {
        schedule(
            id,
            crate::user_data::domain::schedule::TaskKind::Routine { actions: vec![] },
        )
    }

    #[tokio::test]
    async fn every_schedule_is_deleted_once_it_is_exported() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log.clone(), vec![], None).await;
        let scheduler = Arc::new(FakeScheduler::new(
            log.clone(),
            vec![routine("routine-1"), routine("routine-2")],
        ));
        ports.scheduler = Some(scheduler.clone());

        let export = FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond")
            .await
            .unwrap();

        assert_eq!(
            export.routines.len(),
            2,
            "the export is read before anything goes"
        );
        assert!(
            scheduler.list_tasks().await.unwrap().is_empty(),
            "a routine left behind keeps firing for the next household"
        );
        let calls = log.calls();
        let deleted = calls
            .iter()
            .position(|c| *c == "scheduler.delete_task")
            .unwrap();
        let onboarding = calls.iter().position(|c| *c == "onboarding.reset").unwrap();
        assert!(deleted < onboarding);
    }

    #[tokio::test]
    async fn the_pond_goes_quiet_before_anything_is_erased_and_forgets_wifi_last() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log.clone(), vec![device("phone-1")], None).await;
        ports.scheduler = Some(Arc::new(FakeScheduler::new(
            log.clone(),
            vec![routine("routine-1")],
        )));

        FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond")
            .await
            .unwrap();

        let calls = log.calls();
        let at = |call: &str| {
            calls
                .iter()
                .position(|c| *c == call)
                .unwrap_or_else(|| panic!("{call} never ran: {calls:?}"))
        };
        assert!(at("quiet.quiet") < at("scheduler.pause_task"));
        assert!(at("scheduler.pause_task") < at("handshake.revoke_device"));
        assert!(at("handshake.revoke_device") < at("data_wipe.wipe"));
        assert!(
            at("onboarding.reset") < at("data_wipe.forget_networks"),
            "the pond keeps its network until setup has reopened"
        );
        assert_eq!(calls.last(), Some(&"quiet.resume"));
    }

    #[tokio::test]
    async fn a_stopped_reset_resumes_what_it_paused_and_keeps_the_wifi() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log.clone(), vec![], None).await;
        let scheduler = Arc::new(FakeScheduler::new(
            log.clone(),
            vec![routine("stuck-1"), routine("routine-2")],
        ));
        ports.scheduler = Some(scheduler.clone());

        let result =
            FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond").await;

        assert!(
            matches!(
                result,
                Err(FactoryResetError::Failed {
                    step: FactoryResetStep::ErasingData,
                    ..
                })
            ),
            "{result:?}"
        );
        let calls = log.calls();
        let left = scheduler.list_tasks().await.unwrap();
        assert_eq!(
            left.len(),
            1,
            "the one that could not be deleted is still there"
        );
        assert!(
            !left[0].paused,
            "and fires again, as it did before the reset"
        );
        assert!(!calls.contains(&"data_wipe.wipe"));
        assert!(!calls.contains(&"data_wipe.forget_networks"));
        assert_eq!(calls.last(), Some(&"quiet.resume"));
    }

    #[tokio::test]
    async fn happy_path_runs_in_order_and_exports_routines() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(
            log.clone(),
            vec![device("phone-1"), device("phone-2")],
            None,
        )
        .await;
        let routine = routine("routine-1");
        ports.scheduler = Some(Arc::new(FakeScheduler::new(
            log.clone(),
            vec![routine.clone()],
        )));
        ports.member_notifier = Some(Arc::new(LoggingMemberNotifier(log.clone())));

        let export = FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond")
            .await
            .unwrap();

        assert_eq!(export.routines.len(), 1);
        assert_eq!(export.routines[0].id, "routine-1");

        let calls = log.calls();
        // Export and the household snapshot happen before anything is revoked or erased.
        let export_idx = calls
            .iter()
            .position(|c| *c == "scheduler.list_tasks")
            .unwrap();
        let notify_idx = calls
            .iter()
            .position(|c| *c == "member_notifier.notify_member")
            .unwrap();
        let revoke_idx = calls
            .iter()
            .position(|c| *c == "handshake.revoke_device")
            .unwrap();
        let wipe_idx = calls.iter().position(|c| *c == "data_wipe.wipe").unwrap();
        let onboarding_idx = calls.iter().position(|c| *c == "onboarding.reset").unwrap();
        assert!(
            export_idx < revoke_idx,
            "routines must be exported before devices are revoked"
        );
        assert!(
            notify_idx < revoke_idx,
            "members must be notified while their devices still resolve, before revocation deletes them"
        );
        assert!(
            revoke_idx < wipe_idx,
            "devices must be revoked before data is erased"
        );
        assert!(
            wipe_idx < onboarding_idx,
            "data must be erased before onboarding resets"
        );
        assert!(calls.contains(&"tts.speak"));
        assert!(calls.contains(&"remote.queue"));
        assert!(calls.contains(&"devices.unregister"));
    }

    #[tokio::test]
    async fn a_failed_goodbye_never_fails_the_reset() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log.clone(), vec![device("phone-1")], None).await;
        ports.tts = Some(Arc::new(FailingVoiceOutput));

        let export =
            FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond").await;

        assert!(export.is_ok(), "a TTS failure must not abort the reset");
        assert!(log.calls().contains(&"onboarding.reset"));
    }

    #[tokio::test]
    async fn a_failed_notification_never_fails_the_reset() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log.clone(), vec![device("phone-1")], None).await;
        ports.member_notifier = Some(Arc::new(MockMemberNotifier::new().failing_next(1)));

        let export =
            FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond").await;

        assert!(
            export.is_ok(),
            "a notification failure must not abort the reset"
        );
    }

    #[tokio::test]
    async fn the_household_notice_reaches_each_member_in_their_own_language() {
        let log = Arc::new(CallLog::default());
        let notifier = Arc::new(MockMemberNotifier::new());
        let mut ports = happy_ports(log, vec![], Some("sw")).await;
        let owner = ports
            .profile_repo
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.role == crate::user_data::domain::profile::Role::Owner)
            .unwrap();
        let member = ports
            .profile_repo
            .create(CreateProfileRequest {
                display_name: "Member".to_string(),
                avatar_emoji: "🦆".to_string(),
            })
            .await
            .unwrap();
        let mut prefs = std::collections::HashMap::new();
        prefs.insert("language".to_string(), "fr".to_string());
        ports
            .profile_repo
            .update_preferences(&member.id, prefs)
            .await
            .unwrap();
        ports.member_notifier = Some(notifier.clone());

        FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond")
            .await
            .unwrap();

        let sent = notifier.sent();
        assert_eq!(
            sent.len(),
            2,
            "each member gets their own copy, not one broadcast"
        );
        let owner_copy = sent.iter().find(|(id, _)| *id == owner.id).unwrap();
        let member_copy = sent.iter().find(|(id, _)| *id == member.id).unwrap();
        assert!(
            owner_copy.1.body.contains("kaya"),
            "the owner's copy is in their own language"
        );
        assert!(
            member_copy.1.body.contains("foyer"),
            "the member's copy is in their own language"
        );
    }

    #[derive(Default)]
    struct RecordingProgress(Mutex<Vec<FactoryResetStep>>);
    impl FactoryResetProgress for RecordingProgress {
        fn report(&self, step: FactoryResetStep) {
            self.0.lock().unwrap().push(step);
        }
    }

    #[tokio::test]
    async fn every_step_is_reported_once_in_order() {
        let log = Arc::new(CallLog::default());
        let mut ports = happy_ports(log, vec![device("phone-1")], None).await;
        let progress = Arc::new(RecordingProgress::default());
        ports.progress = Some(progress.clone());

        FactoryResetService::execute(&ports, "The Kitchen Pond", "The Kitchen Pond")
            .await
            .unwrap();

        assert_eq!(
            progress.0.lock().unwrap().clone(),
            FactoryResetStep::ALL.to_vec()
        );
    }
}
