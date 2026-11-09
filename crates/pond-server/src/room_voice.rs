//! Room voice: serve runs the voice loop on the pond's own microphone and speaker, with serve's
//! own agent, Whisper and Kokoro, so the pond holds one model and one KV cache. It runs only
//! where it can, says why not where it cannot, and heals on its own.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use pond_adapters_audio_devices::AudioSupervisor;
use pond_api::runs::{RunSupervisor, TurnInFlight};
use pond_audio::MicHandle;
use pond_core::models::domain::audio_device::AudioDeviceStatus;
use pond_core::models::domain::room_voice::{
    LastHeard, RoomEvent, RoomInterrupts, RoomState, RoomStatus, RoomUnavailable, RoomWake,
};
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::room_voice::RoomVoiceControl;
use pond_core::models::ports::voice_input::ModelHears;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::security::ports::telemetry::TelemetryPort;
use pond_core::shared::domain::agent::{ThrottledAudioLevelSink, WorkflowEvent, WorkflowState};
use pond_core::shared::services::chat::{unrecognised_voice_scope, ChatService, WorkflowEventSink};
use pond_core::shared::services::voice_control::{StopReason, VoiceControl};
use pond_core::user_data::ports::profile::ProfileRepository;
use pond_core::user_data::ports::prompt_extra::PromptExtraRepository;
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
use pond_core::user_data::ports::voice_print::VoicePrintRepository;
use tokio::sync::{broadcast, watch};

use crate::model_download;
use crate::voice_lock::VoiceLock;
use crate::voice_rig;

/// How often a room that cannot listen looks again: a voice session may end, a mic be plugged in.
const RETRY: Duration = Duration::from_secs(30);
/// The first restart after the loop fails, doubled per failure in a row up to the ceiling.
const BACKOFF_FIRST: Duration = Duration::from_secs(5);
const BACKOFF_CEILING: Duration = Duration::from_secs(60);
/// A loop that ended on its own (an exit phrase) starts again after this.
const RESTART_AFTER_EXIT: Duration = Duration::from_secs(1);
const EVENT_BACKLOG: usize = 256;
/// How long a room start waits for tiny.en's first download; offline, the fetch fails at once.
const TINY_FETCH_WAIT: Duration = Duration::from_secs(60);
/// The model claimed at a wake is let go after this if no turn took it: a wake heard as noise.
const EARLY_TURN_UNUSED_FOR: Duration = Duration::from_secs(30);

/// The microphone and speaker the room would use, as serve's audio supervisor knows them.
pub trait RoomAudio: Send + Sync {
    fn status(&self) -> Option<AudioDeviceStatus>;
    /// Test every device now; blocking.
    fn retest(&self);
}

impl RoomAudio for AudioSupervisor {
    fn status(&self) -> Option<AudioDeviceStatus> {
        AudioSupervisor::status(self)
    }

    fn retest(&self) {
        AudioSupervisor::retest(self, false);
    }
}

/// What the room runs on, all of it serve's own.
pub struct RoomDeps {
    pub agent: Arc<dyn Agent>,
    pub storage: Arc<dyn SessionStorage>,
    pub settings: Arc<dyn SettingsRepository + Send + Sync>,
    pub telemetry: Option<Arc<dyn TelemetryPort>>,
    pub data_dir: PathBuf,
    pub mic: MicHandle,
    pub whisper: Option<Arc<crate::serve_whisper::ServeWhisper>>,
    pub output: Option<Arc<dyn VoiceOutput>>,
    pub audio: Arc<dyn RoomAudio>,
    pub runs: Arc<RunSupervisor>,
    /// Voice Match: prints and wake calibration, the speaker model, and whose context a
    /// recognised voice brings.
    pub voice_prints: Option<Arc<dyn VoicePrintRepository>>,
    pub voice_match: Option<Arc<crate::voice_match::ServeVoiceMatch>>,
    pub profiles: Option<Arc<dyn ProfileRepository + Send + Sync>>,
    pub prompt_extras: Option<Arc<dyn PromptExtraRepository + Send + Sync>>,
}

/// The active chat model hears a recording once its encoder's audio tower is ready, as the
/// capabilities route reports it.
struct ActiveModelHears {
    agent: Arc<dyn Agent>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
}

/// Whether a voice turn may carry its recording: room voice's check, for the voice child too.
pub(crate) fn model_hearing(
    agent: Arc<dyn Agent>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
) -> Arc<dyn ModelHears> {
    Arc::new(ActiveModelHears { agent, settings })
}

#[async_trait]
impl ModelHears for ActiveModelHears {
    async fn model_hears(&self) -> bool {
        let Ok(settings) = self.settings.get().await else {
            return false;
        };
        let agent = self.agent.clone();
        tokio::task::spawn_blocking(move || {
            agent.audio_input_state(&settings.chat_provider, &settings.chat_model)
        })
        .await
        .ok()
        .flatten()
        .is_some_and(|state| state.is_ready())
    }

    async fn room_to_hear(&self) -> bool {
        use pond_core::models::domain::audio_input::HearingRoom;
        let Ok(settings) = self.settings.get().await else {
            return true;
        };
        let agent = self.agent.clone();
        let room = tokio::task::spawn_blocking(move || {
            agent.hearing_room(&settings.chat_provider, &settings.chat_model)
        })
        .await
        .unwrap_or(HearingRoom::Unknown);
        if let HearingRoom::Short {
            available_mb,
            needed_mb,
        } = room
        {
            tracing::info!(
                target: "giap::trace",
                kind = "voice_turn_as_words",
                available_mb,
                needed_mb,
                "too little memory to load the encoder; the model gets this turn as words"
            );
        }
        room.admits()
    }
}

/// The part of the status the loop moves.
struct View {
    available: bool,
    unavailable_reason: Option<RoomUnavailable>,
    state: RoomState,
    wake: RoomWake,
    last_heard: Option<LastHeard>,
    session_id: Option<String>,
    /// Someone is enrolled and the running ear recognises voices.
    recognises: bool,
}

struct Inner {
    deps: RoomDeps,
    stopping: std::sync::atomic::AtomicBool,
    enabled: watch::Sender<bool>,
    events: broadcast::Sender<RoomEvent>,
    view: Mutex<View>,
    /// The running loop's stop and interrupt; `None` between runs.
    control: Mutex<Option<VoiceControl>>,
    /// Held from Thinking until the turn ends, so lane jobs wait for a room turn too.
    turn: Mutex<Option<TurnInFlight>>,
    /// The process as the running loop started, for `room voice memory` after each turn.
    memory_at_start: Mutex<Option<pond_adapters_ear::ProcessMemory>>,
    /// The running ear, so a new print reaches it without a restart.
    ear: Mutex<Option<Arc<pond_adapters_ear::EarRig>>>,
    /// Deaf until then, as the ear and the status show it; never persisted.
    paused: Mutex<Option<(std::time::Instant, chrono::DateTime<chrono::Utc>)>>,
    /// The ear's Whisper prompt: the wake phrase and the members' names, rewritten as they change.
    prompt: voice_rig::WhisperPrompt,
}

pub struct RoomVoice {
    inner: Arc<Inner>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

enum Ran {
    /// Switched off while it ran.
    Stopped,
    Ended(anyhow::Result<()>),
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl RoomVoice {
    /// Start supervising the room; it listens once switched on and able to.
    pub fn start(deps: RoomDeps, enabled: bool, wake: RoomWake) -> Arc<RoomVoice> {
        if let Some(whisper) = &deps.whisper {
            whisper.set_hearing(Arc::new(ActiveModelHears {
                agent: deps.agent.clone(),
                settings: deps.settings.clone(),
            }));
        }
        let inner = Arc::new(Inner {
            deps,
            stopping: std::sync::atomic::AtomicBool::new(false),
            enabled: watch::channel(enabled).0,
            events: broadcast::channel(EVENT_BACKLOG).0,
            view: Mutex::new(View {
                available: false,
                unavailable_reason: None,
                state: RoomState::Off,
                wake,
                last_heard: None,
                session_id: None,
                recognises: false,
            }),
            control: Mutex::new(None),
            turn: Mutex::new(None),
            memory_at_start: Mutex::new(None),
            ear: Mutex::new(None),
            paused: Mutex::new(None),
            prompt: Default::default(),
        });
        let task = tokio::spawn(supervise(inner.clone()));
        Arc::new(RoomVoice {
            inner,
            task: Mutex::new(Some(task)),
        })
    }

    /// Stop the loop and wait for it, before serve releases the model.
    pub async fn shutdown(&self) {
        self.inner
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.inner.enabled.send_replace(false);
        let task = lock(&self.task).take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
    }
}

#[async_trait]
impl RoomVoiceControl for RoomVoice {
    fn status(&self) -> RoomStatus {
        let paused_until = self.inner.paused_until();
        let view = lock(&self.inner.view);
        RoomStatus {
            enabled: *self.inner.enabled.borrow(),
            available: view.available,
            unavailable_reason: view.unavailable_reason,
            state: match paused_until {
                Some(_) => RoomState::Paused,
                None => view.state,
            },
            wake: view.wake.clone(),
            interrupts: if view.recognises {
                RoomInterrupts::EnrolledOrWake
            } else {
                RoomInterrupts::WakeOnly
            },
            last_heard: view.last_heard.clone(),
            session_id: view.session_id.clone(),
            paused_until: paused_until.map(|(_, at)| at.to_rfc3339()),
        }
    }

    async fn set_enabled(&self, enabled: bool) -> RoomStatus {
        self.inner.enabled.send_replace(enabled);
        {
            let mut view = lock(&self.inner.view);
            if enabled && view.available && view.state == RoomState::Off {
                view.state = RoomState::Starting;
            }
        }
        self.status()
    }

    fn interrupt(&self) {
        if let Some(control) = lock(&self.inner.control).as_ref() {
            control.interrupt();
        }
    }

    fn subscribe(&self) -> broadcast::Receiver<RoomEvent> {
        self.inner.events.subscribe()
    }

    fn household_changed(&self) {
        let inner = self.inner.clone();
        tokio::spawn(async move {
            inner.refresh_prompt(None).await;
            let voices = household(&inner.deps).await;
            let ear = lock(&inner.ear).clone();
            let Some(ear) = ear else {
                return;
            };
            let recognises = lock(&inner.view).recognises;
            if !voices.prints.is_empty() && !recognises {
                // Loaded into the running ear: a restart here would cut a turn in flight.
                let Some(engine) = inner.deps.voice_match.clone() else {
                    return;
                };
                let loaded = tokio::task::spawn_blocking({
                    let ear = ear.clone();
                    move || match engine.speaker_model() {
                        Some(model) => ear.load_speaker(model).map(|()| true),
                        None => Ok(false),
                    }
                })
                .await;
                match loaded {
                    Ok(Ok(true)) => tracing::info!(
                        "room voice: someone enrolled; the running ear now recognises voices"
                    ),
                    Ok(Ok(false)) => return,
                    Ok(Err(e)) => {
                        tracing::warn!(
                            error = format!("{e:#}"),
                            "room voice: the speaker model did not load into the ear"
                        );
                        return;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "room voice: loading the speaker model panicked");
                        return;
                    }
                }
            }
            let enrolled = voices.prints.len();
            ear.set_household(voices);
            inner.set_view(|v| v.recognises = enrolled > 0);
        });
    }

    fn pause(&self, span: Duration) {
        let until = std::time::Instant::now() + span;
        let at = chrono::Utc::now() + chrono::Duration::from_std(span).unwrap_or_default();
        {
            let mut paused = lock(&self.inner.paused);
            if paused.is_some_and(|(held, _)| held >= until) {
                return;
            }
            *paused = Some((until, at));
            // Sent under the lock, so a resume racing it cannot be heard first.
            let _ = self.inner.events.send(RoomEvent::Paused {
                until: at.to_rfc3339(),
            });
        }
        tracing::info!(
            seconds = span.as_secs(),
            "room voice: paused; it hears no wake word"
        );
        if let Some(ear) = lock(&self.inner.ear).as_ref() {
            ear.pause_until(Some(until));
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let inner = Arc::downgrade(&self.inner);
            runtime.spawn(async move {
                tokio::time::sleep_until(until.into()).await;
                if let Some(inner) = inner.upgrade() {
                    inner.end_pause(Some(until));
                }
            });
        }
    }

    fn resume(&self) {
        self.inner.end_pause(None);
    }
}

/// The enrolled members the ear may recognise: everyone with a full run of prints.
async fn household(deps: &RoomDeps) -> pond_adapters_ear::Household {
    let (Some(repo), Some(engine)) = (&deps.voice_prints, &deps.voice_match) else {
        return pond_adapters_ear::Household::default();
    };
    use pond_core::models::ports::voice_match::VoiceMatchEngine as _;
    let prints = match repo.prints(engine.model_name()).await {
        Ok(prints) => prints,
        Err(e) => {
            tracing::warn!(error = %e, "room voice: could not read the voice prints");
            return pond_adapters_ear::Household::default();
        }
    };
    let mut voices = Vec::new();
    for member in pond_core::user_data::services::voice_match::members(&prints) {
        if !member.enrolled {
            continue;
        }
        let name = match &deps.profiles {
            Some(profiles) => profiles
                .get(&member.profile_id)
                .await
                .ok()
                .flatten()
                .map(|p| p.display_name),
            None => None,
        };
        voices.push(pond_adapters_ear::Print {
            name: name.unwrap_or_else(|| member.profile_id.clone()),
            id: member.profile_id,
            embs: member.embeddings,
        });
    }
    // No cohort: the policy's default threshold with its single-profile margin.
    pond_adapters_ear::Household {
        prints: voices,
        ..Default::default()
    }
}

/// Whether the wake CNN heard every taught member's latest run of the household's phrase.
async fn cnn_hears_household(deps: &RoomDeps, phrase: &str) -> bool {
    let Some(repo) = &deps.voice_prints else {
        return false;
    };
    let phrase = pond_voice::text::normalize_transcript(phrase);
    match repo.wake_takes(&phrase).await {
        Ok(takes) => {
            pond_core::user_data::services::voice_match::wake_verdict(&takes).cnn_hears_household
        }
        Err(e) => {
            tracing::warn!(error = %e, "room voice: could not read the wake calibration");
            false
        }
    }
}

/// A recognised speaker's context for their turn, as the routes build it for a typed one.
fn speaker_context(deps: &RoomDeps) -> pond_core::shared::services::chat::SpeakerContext {
    let profiles = deps.profiles.clone();
    let extras = deps.prompt_extras.clone();
    Arc::new(move |profile_id: String| {
        let profiles = profiles.clone();
        let extras = extras.clone();
        Box::pin(async move {
            let profile = profiles?.get(&profile_id).await.ok().flatten()?;
            let notes = match extras {
                Some(extras) => extras
                    .list_for_profile(&profile_id)
                    .await
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            Some(pond_core::prompts::ProfileContext::of_member(
                &profile.display_name,
                &profile.preferences,
                notes,
            ))
        })
    })
}

impl Inner {
    /// The pause in force, if it has not run out; only `end_pause` clears it, so its end is heard.
    fn paused_until(&self) -> Option<(std::time::Instant, chrono::DateTime<chrono::Utc>)> {
        lock(&self.paused).filter(|(until, _)| std::time::Instant::now() < *until)
    }

    /// End the pause: now, or (`only`) when it is still the one that runs out then.
    fn end_pause(&self, only: Option<std::time::Instant>) {
        let state = lock(&self.view).state;
        {
            let mut paused = lock(&self.paused);
            match (*paused, only) {
                (None, _) => return,
                (Some((until, _)), Some(only)) if until != only => return,
                _ => *paused = None,
            }
            let _ = self.events.send(RoomEvent::Resumed { state });
        }
        tracing::info!("room voice: resumed");
        if let Some(ear) = lock(&self.ear).as_ref() {
            ear.pause_until(None);
        }
    }

    fn set_view(&self, change: impl FnOnce(&mut View)) {
        change(&mut lock(&self.view));
    }

    /// Rewrite the Whisper prompt from the settings (`settings`, else read now) and the profiles.
    async fn refresh_prompt(
        &self,
        settings: Option<&pond_core::user_data::domain::settings::Settings>,
    ) {
        let read;
        let settings = match settings {
            Some(s) => s,
            None => {
                read = self.deps.settings.get().await.unwrap_or_default();
                &read
            }
        };
        let prompt =
            voice_rig::household_prompt(&settings.voice_wake_word, self.deps.profiles.as_deref())
                .await;
        tracing::debug!(
            words = prompt
                .as_deref()
                .map_or(0, |p| p.split_whitespace().count()),
            "room voice: Whisper's prompt"
        );
        *self.prompt.write().unwrap_or_else(|e| e.into_inner()) = prompt;
    }

    fn prioritize_speaker_prompt(&self, speaker: &str) {
        let mut prompt_guard = self.prompt.write().unwrap_or_else(|e| e.into_inner());
        if let Some(current) = prompt_guard.as_ref() {
            if let Some(reordered) =
                pond_core::models::domain::speech_prompt::prioritize_name(current, speaker)
            {
                *prompt_guard = Some(reordered);
            }
        }
    }

    /// What stands between the room and listening, if anything; holds the voice lock if not.
    async fn ready(&self, take_lock: bool) -> Result<Option<VoiceLock>, RoomUnavailable> {
        if self.deps.whisper.is_none() || self.deps.output.is_none() {
            return Err(RoomUnavailable::ModelsMissing);
        }
        if model_download::ensure_silero_model(&self.deps.data_dir)
            .await
            .is_none()
        {
            return Err(RoomUnavailable::ModelsMissing);
        }
        if !VoiceLock::is_free() {
            return Err(RoomUnavailable::VoiceRunningElsewhere);
        }
        let pair = self
            .deps
            .audio
            .status()
            .is_some_and(|status| status.pair_works());
        if !pair {
            return Err(RoomUnavailable::NoMicSpeakerPair);
        }
        let held = if take_lock {
            Some(VoiceLock::acquire().map_err(|_| RoomUnavailable::VoiceRunningElsewhere)?)
        } else {
            None
        };
        Ok(held)
    }

    /// One event of the running loop: the status follows it, then every subscriber gets it.
    fn on_event(&self, event: &WorkflowEvent) {
        match event {
            WorkflowEvent::StateChanged { state } => {
                self.set_view(|v| v.state = (*state).into());
                let mut turn = lock(&self.turn);
                match state {
                    WorkflowState::Thinking | WorkflowState::Speak => {
                        if turn.is_none() {
                            *turn = Some(self.deps.runs.turn_started());
                        }
                    }
                    WorkflowState::Wait | WorkflowState::Listen => {
                        // Back to waiting for a wake: a claim no turn took lets the model go.
                        if matches!(state, WorkflowState::Wait) {
                            drop(pond_core::models::domain::inference_bus::take_early_turn());
                        }
                        if turn.take().is_some() {
                            crate::room_memory::log_turn(
                                *lock(&self.memory_at_start),
                                self.deps.whisper.as_ref().and_then(|w| w.held_states()),
                            );
                        }
                    }
                }
            }
            WorkflowEvent::Wake { who } => {
                self.deps.runs.preempt();
                // Claimed now, so whatever holds the model stops while the person is speaking.
                pond_core::models::domain::inference_bus::claim_turn_early(EARLY_TURN_UNUSED_FOR);
                if let Some(who) = who {
                    let heard = LastHeard {
                        profile_id: who.profile_id.clone(),
                        name: who.name.clone(),
                        at: chrono::Utc::now().to_rfc3339(),
                    };
                    self.set_view(|v| v.last_heard = Some(heard));
                    self.prioritize_speaker_prompt(&who.name);
                }
            }
            WorkflowEvent::Exit { .. } => *lock(&self.turn) = None,
            // A different voice's wake moves the loop to a session of its own.
            WorkflowEvent::Ready { session_id } => {
                self.set_view(|v| v.session_id = Some(session_id.clone()))
            }
            _ => {}
        }
        let _ = self.events.send(RoomEvent::Loop(event.clone()));
    }
}

async fn supervise(inner: Arc<Inner>) {
    let mut enabled = inner.enabled.subscribe();
    let mut failures = 0u32;
    let mut said: Option<Option<RoomUnavailable>> = None;
    // A serve that started with the room off never tested its microphone: test once when it is
    // switched on, rather than wait for a device to change.
    let mut tested = false;
    loop {
        if inner.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let on = *enabled.borrow_and_update();
        tested &= on;
        let settings = inner.deps.settings.get().await.unwrap_or_default();
        let calibrated = cnn_hears_household(&inner.deps, &settings.voice_wake_word).await;
        let (detector, reason) = voice_rig::expected_wake(&settings, calibrated);
        inner.set_view(|v| {
            v.wake = RoomWake {
                phrase: settings.voice_wake_word.clone(),
                detector,
                reason,
            }
        });

        let mut ready = inner.ready(on).await;
        // Once per switch-on, whatever the startup test found: on the nano it chose both devices
        // without proving them a pair, and the room stayed silent until someone retested.
        if on && !tested && matches!(ready, Err(RoomUnavailable::NoMicSpeakerPair)) {
            tested = true;
            let audio = inner.deps.audio.clone();
            let _ = tokio::task::spawn_blocking(move || audio.retest()).await;
            ready = inner.ready(on).await;
        }
        let reason = ready.as_ref().err().copied();
        inner.set_view(|v| {
            v.available = reason.is_none();
            v.unavailable_reason = reason;
            v.state = RoomState::Off;
            v.session_id = None;
        });
        if said != Some(reason) {
            said = Some(reason);
            match reason {
                Some(reason) => tracing::info!(
                    kind = "room_voice_unavailable",
                    reason = ?reason,
                    enabled = on,
                    "room voice: not listening"
                ),
                None if !on => tracing::info!(
                    kind = "room_voice_off",
                    "room voice: switched off (voice_room_enabled)"
                ),
                None => {}
            }
        }

        let lock_held = match ready {
            Ok(Some(lock)) if on => lock,
            _ => {
                wait_or_change(&mut enabled, RETRY).await;
                continue;
            }
        };

        inner.set_view(|v| v.state = RoomState::Starting);
        match run_once(&inner, &settings, &mut enabled).await {
            Ran::Stopped => {
                failures = 0;
                tracing::info!(kind = "room_voice_stopped", "room voice: stopped");
            }
            Ran::Ended(Ok(())) => {
                failures = 0;
                tracing::info!(
                    kind = "room_voice_ended",
                    "room voice: the loop ended; starting again"
                );
                wait_or_change(&mut enabled, RESTART_AFTER_EXIT).await;
            }
            Ran::Ended(Err(e)) => {
                failures += 1;
                let backoff = BACKOFF_FIRST
                    .saturating_mul(1 << (failures - 1).min(4))
                    .min(BACKOFF_CEILING);
                tracing::warn!(
                    kind = "room_voice_failed",
                    error = format!("{e:#}"),
                    failures,
                    retry_in_secs = backoff.as_secs(),
                    "room voice: the loop failed; restarting"
                );
                drop(lock_held);
                wait_or_change(&mut enabled, backoff).await;
                continue;
            }
        }
        drop(lock_held);
        if inner.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        said = None;
    }
}

/// Wait out `span`, or less if the switch moves.
async fn wait_or_change(enabled: &mut watch::Receiver<bool>, span: Duration) {
    let _ = tokio::time::timeout(span, enabled.changed()).await;
}

async fn run_once(
    inner: &Arc<Inner>,
    settings: &pond_core::user_data::domain::settings::Settings,
    enabled: &mut watch::Receiver<bool>,
) -> Ran {
    let (Some(speech), Some(output)) = (inner.deps.whisper.clone(), inner.deps.output.clone())
    else {
        return Ran::Ended(Err(anyhow::anyhow!("no speech model or voice")));
    };
    let hearing = Arc::new(ActiveModelHears {
        agent: inner.deps.agent.clone(),
        settings: inner.deps.settings.clone(),
    });
    // A model that hears takes the request as audio, so Whisper only spots the wake word and
    // writes the rough transcript: a wake-only copy of tiny.en does that, leaving the encoder room.
    let hears = hearing.model_hears().await;
    if hears
        && tokio::time::timeout(TINY_FETCH_WAIT, speech.fetch_tiny())
            .await
            .is_err()
    {
        tracing::warn!(
            "room voice: tiny.en is still downloading; starting on the full model's copy"
        );
    }
    let chosen = tokio::task::spawn_blocking(move || {
        if hears {
            match speech.wake() {
                Ok(whisper) => return Ok((whisper, true)),
                Err(e) => tracing::warn!(
                    error = format!("{e:#}"),
                    "room voice: no wake-only Whisper; using the full model"
                ),
            }
        }
        speech.full().map(|whisper| (whisper, false))
    })
    .await;
    let (whisper, whisper_wake_only) = match chosen {
        Ok(Ok(chosen)) => chosen,
        Ok(Err(e)) => return Ran::Ended(Err(e)),
        Err(e) => return Ran::Ended(Err(anyhow::anyhow!("loading Whisper panicked: {e}"))),
    };
    // Its row is made at the first turn: a room switched on and off leaves no empty conversation.
    let session_id = uuid::Uuid::new_v4().to_string();

    let events = inner.events.clone();
    let levels = Arc::new(ThrottledAudioLevelSink::new(Box::new(move |rms: f32| {
        let _ = events.send(RoomEvent::Loop(WorkflowEvent::AudioLevel { rms }));
    })));
    let memory_before = crate::room_memory::now();
    let voices = household(&inner.deps).await;
    let speaker = match (&inner.deps.voice_match, voices.prints.is_empty()) {
        (Some(engine), false) => {
            let engine = engine.clone();
            tokio::task::spawn_blocking(move || engine.speaker_model())
                .await
                .ok()
                .flatten()
        }
        _ => None,
    };
    let recognises = speaker.is_some();
    let cnn_hears_household = cnn_hears_household(&inner.deps, &settings.voice_wake_word).await;
    inner.refresh_prompt(Some(settings)).await;
    let listening = voice_rig::build_listening(voice_rig::ListeningOptions {
        settings,
        data_dir: &inner.deps.data_dir,
        mic: &inner.deps.mic,
        whisper,
        wake_word: None,
        no_wake_word: false,
        audio_level: Some(levels),
        speaker,
        cnn_hears_household,
        whisper_prompt: Some(inner.prompt.clone()),
    })
    .await;
    let recognises = recognises && listening.ear.is_some();
    if let Some(ear) = &listening.ear {
        ear.set_household(voices);
        ear.set_forward_unworded(whisper_wake_only);
        // A pause asked for while the room was starting still holds.
        if let Some((until, _)) = inner.paused_until() {
            ear.pause_until(Some(until));
        }
    }
    *lock(&inner.ear) = listening.ear.clone();
    let (detector, reason) = listening.wake();
    inner.set_view(|v| {
        v.wake = RoomWake {
            phrase: listening.wake_word.clone(),
            detector,
            reason,
        };
        v.session_id = Some(session_id.clone());
        v.recognises = recognises;
    });

    let control = VoiceControl::new();
    *lock(&inner.control) = Some(control.clone());
    // Weak: the sink lives in the service, which must not keep the room alive.
    let room: Weak<Inner> = Arc::downgrade(inner);
    let sink: WorkflowEventSink = Arc::new(move |event: &WorkflowEvent| {
        if let Some(room) = room.upgrade() {
            room.on_event(event);
        }
    });
    let mut chat = ChatService::new(
        inner.deps.agent.clone(),
        session_id.clone(),
        inner.deps.storage.clone(),
    )
    .with_system_prompt(pond_core::prompts::build_system_prompt(settings))
    .with_thinking_tone(settings.voice_thinking_tone_enabled)
    .with_voice_output(Arc::new(crate::room_memory::MeasuredVoice(output)))
    .with_control(control.clone())
    .with_stdout_diagnostics(false)
    .with_event_sink(sink)
    .with_session_on_first_turn()
    .with_voice_identity(speaker_context(&inner.deps))
    .with_unrecognised_scope(unrecognised_voice_scope(inner.deps.profiles.clone()))
    .with_model_hearing(hearing);
    if let Some(telemetry) = &inner.deps.telemetry {
        chat = chat.with_telemetry(telemetry.clone());
    }
    let chat = listening.apply(chat);

    tracing::info!(
        kind = "room_voice_started",
        session_id = %session_id,
        wake_word = %listening.wake_word,
        front = %listening.describe(),
        "room voice: listening"
    );
    crate::room_memory::log_start(
        memory_before,
        &listening.ear_parts,
        inner.deps.whisper.as_ref().and_then(|w| w.held_states()),
    );
    *lock(&inner.memory_at_start) = crate::room_memory::now();
    inner.on_event(&WorkflowEvent::Ready {
        session_id: session_id.clone(),
    });

    let ran = {
        let run = chat.run_loop();
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => Ran::Ended(result),
            () = switched_off(enabled) => {
                control.stop(StopReason::Requested);
                let _ = tokio::time::timeout(Duration::from_secs(10), &mut run).await;
                Ran::Stopped
            }
        }
    };

    *lock(&inner.control) = None;
    *lock(&inner.turn) = None;
    *lock(&inner.ear) = None;
    pond_audio_out::set_playback_tap(None);
    pond_audio_out::set_spoken_tap(None);
    drop(chat);
    drop(listening);
    inner.set_view(|v| {
        v.state = RoomState::Off;
        v.session_id = None;
    });
    ran
}

async fn switched_off(enabled: &mut watch::Receiver<bool>) {
    loop {
        if !*enabled.borrow_and_update() {
            return;
        }
        if enabled.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::wake_detector::{WakeDetector, WakeDetectorReason};
    use pond_core::shared::domain::agent::VoiceSpeaker;
    use pond_core::shared::mocks::mock_agent::MockAgent;
    use pond_core::user_data::mocks::mock_session::InMemorySessionStorage;
    use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;

    struct NoDevices;

    impl RoomAudio for NoDevices {
        fn status(&self) -> Option<AudioDeviceStatus> {
            None
        }
        fn retest(&self) {}
    }

    fn deps(runs: Arc<RunSupervisor>) -> RoomDeps {
        let capture = pond_audio::testing::ScriptedCapture::silence(1_000, 20);
        let (mic, _owner) = pond_audio::spawn(Box::new(capture), 16_000, 1_000, true);
        RoomDeps {
            agent: Arc::new(MockAgent::new()),
            storage: Arc::new(InMemorySessionStorage::new()),
            settings: Arc::new(MockSettingsRepository::new()),
            telemetry: None,
            data_dir: std::env::temp_dir(),
            mic,
            whisper: None,
            output: None,
            audio: Arc::new(NoDevices),
            runs,
            voice_prints: None,
            voice_match: None,
            profiles: None,
            prompt_extras: None,
        }
    }

    fn wake() -> RoomWake {
        RoomWake {
            phrase: "hey goose".into(),
            detector: WakeDetector::Whisper,
            reason: WakeDetectorReason::NotCalibrated,
        }
    }

    fn state(s: WorkflowState) -> WorkflowEvent {
        WorkflowEvent::StateChanged { state: s }
    }

    #[tokio::test]
    async fn a_room_turn_holds_lane_jobs_back_from_thinking_until_it_ends() {
        let runs = Arc::new(RunSupervisor::default());
        let room = RoomVoice::start(deps(runs.clone()), false, wake());
        let inner = room.inner.clone();
        inner.on_event(&state(WorkflowState::Thinking));
        assert_eq!(runs.turns_in_flight(), 1);
        inner.on_event(&state(WorkflowState::Speak));
        assert_eq!(
            runs.turns_in_flight(),
            1,
            "one turn, however many states it passes"
        );
        inner.on_event(&state(WorkflowState::Listen));
        assert_eq!(runs.turns_in_flight(), 0);
        inner.on_event(&state(WorkflowState::Thinking));
        inner.on_event(&WorkflowEvent::Exit {
            reason: "stopped".into(),
        });
        assert_eq!(runs.turns_in_flight(), 0, "a stopped loop holds nothing");
        room.shutdown().await;
    }

    #[tokio::test]
    async fn the_status_follows_the_loop_and_its_subscribers_hear_every_event() {
        let room = RoomVoice::start(deps(Arc::new(RunSupervisor::default())), false, wake());
        let mut events = room.subscribe();
        room.inner.on_event(&state(WorkflowState::Listen));
        room.inner.on_event(&WorkflowEvent::Wake {
            who: Some(VoiceSpeaker {
                profile_id: "p-ana".into(),
                name: "Ana".into(),
            }),
        });
        let status = room.status();
        assert_eq!(status.state, RoomState::Listen);
        let heard = status.last_heard.expect("the wake named someone");
        assert_eq!(
            (heard.profile_id.as_str(), heard.name.as_str()),
            ("p-ana", "Ana")
        );
        assert!(matches!(
            events.recv().await.unwrap(),
            RoomEvent::Loop(WorkflowEvent::StateChanged {
                state: WorkflowState::Listen
            })
        ));
        assert!(matches!(
            events.recv().await.unwrap(),
            RoomEvent::Loop(WorkflowEvent::Wake { .. })
        ));
        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_pause_shows_and_runs_out_and_a_longer_one_is_kept() {
        let room = RoomVoice::start(deps(Arc::new(RunSupervisor::default())), false, wake());
        let mut events = room.subscribe();
        room.pause(Duration::from_secs(60));
        room.pause(Duration::from_secs(5));
        let status = room.status();
        assert_eq!(status.state, RoomState::Paused);
        let until = chrono::DateTime::parse_from_rfc3339(status.paused_until.as_deref().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(
            until - chrono::Utc::now() > chrono::Duration::seconds(50),
            "the longer pause held"
        );
        room.resume();
        room.resume();
        assert_eq!(room.status().paused_until, None);
        assert_ne!(room.status().state, RoomState::Paused);

        room.pause(Duration::from_millis(30));
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(
            room.status().paused_until,
            None,
            "a pause runs out on its own"
        );

        let mut heard = Vec::new();
        while let Ok(event) = events.try_recv() {
            heard.push(event.to_ndjson().unwrap());
        }
        let states: Vec<&str> = heard
            .iter()
            .map(|line| {
                if line.contains(r#""state":"paused""#) {
                    "paused"
                } else {
                    "resumed"
                }
            })
            .collect();
        assert_eq!(
            states,
            ["paused", "resumed", "paused", "resumed"],
            "the kept pause, one resume of two, and a pause that ran out: {heard:?}"
        );
        room.shutdown().await;
    }

    #[tokio::test]
    async fn whisper_is_told_the_wake_phrase_and_every_members_name_as_they_change() {
        use pond_core::user_data::domain::profile::CreateProfileRequest;
        use pond_core::user_data::mocks::mock_profile::MockProfileRepository;
        let profiles = Arc::new(MockProfileRepository::new());
        let mut d = deps(Arc::new(RunSupervisor::default()));
        d.profiles = Some(profiles.clone());
        let room = RoomVoice::start(d, false, wake());
        let ana = CreateProfileRequest {
            display_name: "Ana Wanjiru".into(),
            avatar_emoji: "duck".into(),
        };
        profiles.create(ana).await.unwrap();
        room.inner.refresh_prompt(None).await;
        let prompt = room.inner.prompt.read().unwrap().clone().unwrap();
        assert!(prompt.starts_with("Hey goose."), "{prompt}");
        assert!(prompt.contains("Ana Wanjiru"), "{prompt}");

        let otieno = CreateProfileRequest {
            display_name: "Otieno".into(),
            avatar_emoji: "duck".into(),
        };
        profiles.create(otieno).await.unwrap();
        room.inner.refresh_prompt(None).await;
        let prompt = room.inner.prompt.read().unwrap().clone().unwrap();
        assert!(
            prompt.contains("Otieno"),
            "a new member reaches the prompt: {prompt}"
        );

        room.inner.on_event(&WorkflowEvent::Wake {
            who: Some(pond_core::shared::domain::agent::VoiceSpeaker {
                profile_id: "otieno-id".into(),
                name: "Otieno".into(),
            }),
        });
        let prioritized = room.inner.prompt.read().unwrap().clone().unwrap();
        assert_eq!(
            prioritized, "Hey goose. Otieno, Ana Wanjiru.",
            "the active speaker is prioritized in the prompt: {prioritized}"
        );

        room.shutdown().await;
    }

    #[tokio::test]
    async fn a_room_without_its_models_says_so_and_switches_off() {
        let room = RoomVoice::start(deps(Arc::new(RunSupervisor::default())), true, wake());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while room.status().unavailable_reason.is_none() {
            assert!(std::time::Instant::now() < deadline, "no verdict");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let status = room.status();
        assert!(status.enabled && !status.available);
        assert_eq!(
            status.unavailable_reason,
            Some(RoomUnavailable::ModelsMissing)
        );
        assert_eq!(status.state, RoomState::Off);
        assert_eq!(status.interrupts, RoomInterrupts::WakeOnly);

        let off = room.set_enabled(false).await;
        assert!(!off.enabled);
        assert_eq!(off.state, RoomState::Off);
        room.shutdown().await;
    }
}
