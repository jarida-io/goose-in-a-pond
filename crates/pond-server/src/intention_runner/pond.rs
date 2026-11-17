//! The runner's ports on a real pond: the sources a job may wait on, the household's
//! schedules, and the helper that looks one thing up.

use std::collections::BTreeSet;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use pond_adapters_vision::LiveCameraRegistry;
use pond_core::models::domain::inference_bus::{
    inference_bus, running_under, InferenceClaim, InferenceHeldFloor, InferenceHold,
};
use pond_core::shared::domain::orchestration::{AgentRole, RoleError};
use pond_core::shared::domain::todo::{HookSource, Tz};
use pond_core::user_data::domain::device::POWER_KEY;
use pond_core::user_data::domain::proposal::ProposalAudience;
use pond_core::user_data::domain::schedule::TaskKind;
use pond_core::user_data::domain::vision as vision_policy;
use pond_core::user_data::ports::device_registry::DeviceRegistry;
use pond_core::user_data::ports::scheduler::SchedulerPort;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::services::intentions::ThingChoice;
use pond_core::user_data::services::proactive_review as review;

use super::{Errands, Looked, ScheduleWords, ThingSources};

const HELPERS_OFF: &str = "Not looked up: this pond has helpers turned off.";
const NOT_ALLOWED: &str = "Not looked up: the look-up was refused.";

type Settings = Arc<dyn SettingsRepository + Send + Sync>;

/// The cameras and switched devices of this pond, each under the id, label and events
/// `/me/hooks/sources` lists it with.
pub struct PondThings {
    pub settings: Settings,
    pub devices: Arc<dyn DeviceRegistry + Send + Sync>,
}

fn choice(source: HookSource) -> ThingChoice {
    ThingChoice {
        source_id: source.id,
        label: source.label,
        events: source.events.into_iter().map(|event| event.id).collect(),
    }
}

#[async_trait]
impl ThingSources for PondThings {
    async fn things(&self) -> Vec<ThingChoice> {
        let mut sources: Vec<HookSource> = Vec::new();
        // Cameras only while the household allows them: a job on one would never fire.
        if let Ok(settings) = self.settings.get().await {
            if settings.cameras_enabled {
                let mut cameras: BTreeSet<String> = LiveCameraRegistry::global()
                    .active_cameras()
                    .into_iter()
                    .collect();
                cameras.insert(vision_policy::camera_id(&settings));
                sources.extend(cameras.iter().map(|id| HookSource::camera(id)));
            }
        }
        match self.devices.list_devices().await {
            Ok(devices) => sources.extend(
                devices
                    .iter()
                    .filter(|device| device.capabilities.iter().any(|c| c == POWER_KEY))
                    .map(|device| HookSource::device(&device.id, &device.name)),
            ),
            Err(e) => tracing::warn!(error = %e, "the devices a job may wait on could not be read"),
        }
        sources.into_iter().map(choice).collect()
    }
}

/// The scheduler's tasks; a pond whose scheduler did not start has none.
pub struct PondSchedules(pub Option<Arc<dyn SchedulerPort>>);

#[async_trait]
impl ScheduleWords for PondSchedules {
    async fn words_for(&self, profile_id: &str) -> anyhow::Result<Vec<String>> {
        let Some(scheduler) = &self.0 else {
            return Ok(Vec::new());
        };
        let mut words = Vec::new();
        for schedule in scheduler.list_tasks().await? {
            if schedule.kind.member().is_some_and(|m| m != profile_id) {
                continue;
            }
            if let TaskKind::AgentPrompt { prompt } | TaskKind::MemberReminder { prompt, .. } =
                &schedule.kind
            {
                words.push(prompt.clone());
            }
            words.push(schedule.label);
        }
        Ok(words)
    }
}

/// Looks one thing up with the errand role, as a helper on the inference bus's Task lane: a
/// person's turn takes the model and the helper goes on after it.
pub struct HelperErrands {
    settings: Settings,
    role: AgentRole,
    // One at a time, so a burst of met jobs queues one claim on the bus, not several.
    one_at_a_time: tokio::sync::Mutex<()>,
}

impl HelperErrands {
    pub fn new(settings: Settings) -> Result<Self, RoleError> {
        Ok(Self {
            settings,
            role: review::errand_role()?,
            one_at_a_time: tokio::sync::Mutex::new(()),
        })
    }
}

#[async_trait]
impl Errands for HelperErrands {
    async fn look_into(
        &self,
        profile_id: &str,
        text: &str,
        why: &str,
        now: DateTime<Utc>,
        zone: Tz,
    ) -> Looked {
        let allowed = self
            .settings
            .get()
            .await
            .is_ok_and(|settings| settings.ext_orchestrator_enabled);
        let Some(orchestration) =
            pond_mcp_server::installed_orchestrator_deps().filter(|_| allowed)
        else {
            return Looked::NotRun(HELPERS_OFF.to_string());
        };
        let Ok(audience) = ProposalAudience::for_member(profile_id) else {
            return Looked::NotRun(NOT_ALLOWED.to_string());
        };
        let session_id = review::review_session_id(&uuid::Uuid::new_v4().to_string());
        let brief = review::errand_brief(text, why, now, zone);
        let spec = match review::plan_errand(&self.role, &audience, &session_id, &brief) {
            Ok(spec) => spec,
            Err(e) => {
                tracing::warn!(error = ?e, "an intention's look-up was refused");
                return Looked::NotRun(NOT_ALLOWED.to_string());
            }
        };

        let _turn = self.one_at_a_time.lock().await;
        let started = std::time::Instant::now();
        let lease = orchestration.authorities().publish(
            &session_id,
            review::review_authority(&audience, &session_id),
            CancellationToken::new(),
        );
        let held = InferenceHeldFloor::new(inference_bus(), InferenceClaim::Task);
        let give_way = tokio::spawn({
            let held = held.clone();
            async move { held.give_way().await }
        });
        let run = running_under(
            InferenceHold::Held(held),
            orchestration.orchestrator().spawn(spec),
        )
        .await;
        give_way.abort();
        drop(lease);

        let answer = run
            .as_ref()
            .ok()
            .and_then(|run| run.result_for_parent())
            .map(str::trim)
            .filter(|answer| !answer.is_empty())
            .map(str::to_string);
        // Counts and the run's state only: neither the question nor the answer is logged.
        tracing::info!(
            target: "giap::trace",
            kind = "intention_errand",
            status = ?run.as_ref().map(|run| run.status).ok(),
            answered = answer.is_some(),
            ms = started.elapsed().as_millis() as u64,
            "an intention looked something up"
        );
        match answer {
            Some(answer) => Looked::Found(answer),
            None => Looked::Nothing,
        }
    }
}
