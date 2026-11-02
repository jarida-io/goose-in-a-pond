//! Room voice: serve runs the voice loop on the pond's own microphone and speaker, with serve's
//! own agent, Whisper and Kokoro, so the pond holds one model and one KV cache. It runs only
//! where it can, says why not where it cannot, and heals on its own.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use pond_adapters_audio_devices::AudioSupervisor;
use pond_adapters_whisper::WhisperRsInput;
use pond_api::runs::{RunSupervisor, TurnInFlight};
use pond_audio::MicHandle;
use pond_core::models::domain::audio_device::{AudioDeviceStatus, ChoiceBasis};
use pond_core::models::domain::room_voice::{
    LastHeard, RoomInterrupts, RoomState, RoomStatus, RoomUnavailable, RoomWake,
};
use pond_core::models::ports::agent::Agent;
use pond_core::models::ports::room_voice::RoomVoiceControl;
use pond_core::models::ports::voice_output::VoiceOutput;
use pond_core::security::ports::telemetry::TelemetryPort;
use pond_core::shared::domain::agent::{ThrottledAudioLevelSink, WorkflowEvent, WorkflowState};
use pond_core::shared::services::chat::{ChatService, WorkflowEventSink};
use pond_core::shared::services::voice_control::{StopReason, VoiceControl};
use pond_core::user_data::ports::session_storage::SessionStorage;
use pond_core::user_data::ports::settings::SettingsRepository;
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
    pub whisper: Option<Arc<WhisperRsInput>>,
    pub output: Option<Arc<dyn VoiceOutput>>,
    pub audio: Arc<dyn RoomAudio>,
    pub runs: Arc<RunSupervisor>,
}

/// The part of the status the loop moves.
struct View {
    available: bool,
    unavailable_reason: Option<RoomUnavailable>,
    state: RoomState,
    wake: RoomWake,
    last_heard: Option<LastHeard>,
    session_id: Option<String>,
}

struct Inner {
    deps: RoomDeps,
    enabled: watch::Sender<bool>,
    events: broadcast::Sender<WorkflowEvent>,
    view: Mutex<View>,
    /// The running loop's stop and interrupt; `None` between runs.
    control: Mutex<Option<VoiceControl>>,
    /// Held from Thinking until the turn ends, so lane jobs wait for a room turn too.
    turn: Mutex<Option<TurnInFlight>>,
    /// The process as the running loop started, for `room voice memory` after each turn.
    memory_at_start: Mutex<Option<pond_adapters_ear::ProcessMemory>>,
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
        let inner = Arc::new(Inner {
            deps,
            enabled: watch::channel(enabled).0,
            events: broadcast::channel(EVENT_BACKLOG).0,
            view: Mutex::new(View {
                available: false,
                unavailable_reason: None,
                state: RoomState::Off,
                wake,
                last_heard: None,
                session_id: None,
            }),
            control: Mutex::new(None),
            turn: Mutex::new(None),
            memory_at_start: Mutex::new(None),
        });
        let task = tokio::spawn(supervise(inner.clone()));
        Arc::new(RoomVoice {
            inner,
            task: Mutex::new(Some(task)),
        })
    }

    /// Stop the loop and wait for it, before serve releases the model.
    pub async fn shutdown(&self) {
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
        let view = lock(&self.inner.view);
        RoomStatus {
            enabled: *self.inner.enabled.borrow(),
            available: view.available,
            unavailable_reason: view.unavailable_reason,
            state: view.state,
            wake: view.wake.clone(),
            // Nobody can be enrolled for Voice Match yet.
            interrupts: RoomInterrupts::WakeOnly,
            last_heard: view.last_heard.clone(),
            session_id: view.session_id.clone(),
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

    fn subscribe(&self) -> broadcast::Receiver<WorkflowEvent> {
        self.inner.events.subscribe()
    }
}

impl Inner {
    fn set_view(&self, change: impl FnOnce(&mut View)) {
        change(&mut lock(&self.view));
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
        let held = if take_lock {
            Some(VoiceLock::acquire().map_err(|_| RoomUnavailable::VoiceRunningElsewhere)?)
        } else if VoiceLock::is_free() {
            None
        } else {
            return Err(RoomUnavailable::VoiceRunningElsewhere);
        };
        let pair = self
            .deps
            .audio
            .status()
            .is_some_and(|status| status.pair_works());
        if !pair {
            return Err(RoomUnavailable::NoMicSpeakerPair);
        }
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
                        if turn.take().is_some() {
                            crate::room_memory::log_turn(
                                *lock(&self.memory_at_start),
                                self.deps.whisper.as_ref().and_then(|w| w.held_states()),
                            );
                        }
                    }
                }
            }
            WorkflowEvent::Wake { who: Some(who) } => {
                let heard = LastHeard {
                    profile_id: who.profile_id.clone(),
                    name: who.name.clone(),
                    at: chrono::Utc::now().to_rfc3339(),
                };
                self.set_view(|v| v.last_heard = Some(heard));
            }
            WorkflowEvent::Exit { .. } => *lock(&self.turn) = None,
            _ => {}
        }
        let _ = self.events.send(event.clone());
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
        let on = *enabled.borrow_and_update();
        tested &= on;
        let settings = inner.deps.settings.get().await.unwrap_or_default();
        let (detector, reason) = voice_rig::expected_wake(&settings);
        inner.set_view(|v| {
            v.wake = RoomWake {
                phrase: settings.voice_wake_word.clone(),
                detector,
                reason,
            }
        });

        let mut ready = inner.ready(on).await;
        let untested = inner
            .deps
            .audio
            .status()
            .is_some_and(|s| s.input.basis == ChoiceBasis::NotTested);
        if on && !tested && untested && matches!(ready, Err(RoomUnavailable::NoMicSpeakerPair)) {
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
    let (Some(whisper), Some(output)) = (inner.deps.whisper.clone(), inner.deps.output.clone())
    else {
        return Ran::Ended(Err(anyhow::anyhow!("no speech model or voice")));
    };
    // Its row is made at the first turn: a room switched on and off leaves no empty conversation.
    let session_id = uuid::Uuid::new_v4().to_string();

    let events = inner.events.clone();
    let levels = Arc::new(ThrottledAudioLevelSink::new(Box::new(move |rms: f32| {
        let _ = events.send(WorkflowEvent::AudioLevel { rms });
    })));
    let memory_before = crate::room_memory::now();
    let listening = voice_rig::build_listening(voice_rig::ListeningOptions {
        settings,
        data_dir: &inner.deps.data_dir,
        mic: &inner.deps.mic,
        whisper,
        wake_word: None,
        no_wake_word: false,
        audio_level: Some(levels),
    })
    .await;
    let (detector, reason) = listening.wake();
    inner.set_view(|v| {
        v.wake = RoomWake {
            phrase: listening.wake_word.clone(),
            detector,
            reason,
        };
        v.session_id = Some(session_id.clone());
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
    .with_session_on_first_turn();
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
            WorkflowEvent::StateChanged {
                state: WorkflowState::Listen
            }
        ));
        assert!(matches!(
            events.recv().await.unwrap(),
            WorkflowEvent::Wake { .. }
        ));
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
