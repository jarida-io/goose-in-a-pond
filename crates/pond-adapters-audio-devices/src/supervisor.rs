//! Keeps the microphone and speaker on devices that work: chooses at startup, and again when a
//! device comes or goes, when the mic is lost or stalls, when the speaker stops taking audio,
//! and when the household asks for a test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use pond_audio::{MicHandle, MicState};
use pond_core::models::domain::audio_device::{
    stream_route, AudioChoice, AudioDeviceStatus, AudioDirection, ChoiceBasis, ProbeOutcome,
    ProbedDevice,
};
use pond_core::models::ports::audio_devices::{AudioDeviceControl, AudioDeviceProber};
use pond_core::models::services::voice::audio_devices::{
    AudioDeviceSelector, PairRequest, ProbeMode, SelectRequest, SoundTest,
};
use serde::{Deserialize, Serialize};

const TICK: Duration = Duration::from_millis(500);
const FINGERPRINT_EVERY: Duration = Duration::from_secs(2);
/// A capture with no new frames for this long has lost its device without saying so (ALSA).
const MIC_STALL: Duration = Duration::from_secs(3);
/// Spaces out reselects after failures, so a device that keeps failing is not hammered.
const FAILURE_BACKOFF: Duration = Duration::from_secs(5);
/// How long serve waits for the voice session to answer a retest request: a full test's budget
/// plus one slow device still opening when it runs out, inside the UI's 30 s request timeout.
const VOICE_ANSWER_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The voice session: owns the microphone, so it tests inputs on its own.
    Voice,
    /// The server: never opens the microphone unless the household asks for a test and no
    /// voice session holds it.
    Serve,
    /// The server running room voice: owns the microphone and tests it as a voice session
    /// does, except while a `chat --voice` session runs, which then holds it.
    Room,
}

impl Role {
    /// Room is serve's file and source: a voice session defers to it as to any server.
    fn name(self) -> &'static str {
        match self {
            Self::Voice => "voice",
            Self::Serve | Self::Room => "serve",
        }
    }
}

/// The settings a selection reads, fresh on every pass so a new pin applies without a restart.
#[derive(Debug, Clone, Default)]
pub struct AudioPrefs {
    pub input_pin: Option<String>,
    pub output_pin: Option<String>,
    pub mic_enabled: bool,
    /// `Settings::audio_sound_test`: choose the speaker and microphone by a short test sound.
    pub sound_test: bool,
    /// `Settings::audio_manage_system`: make the choice the board's own defaults, and raise a
    /// level too low for the test sound to be heard.
    pub manage_system: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Startup,
    DevicesChanged,
    InputLost,
    OutputStalled,
    /// A pin or the mic switch changed: chosen again as at startup, never a full test.
    SettingsChanged,
    Retest,
}

pub type PrefsFn = Arc<dyn Fn() -> AudioPrefs + Send + Sync>;
/// Told after every pass that changed something, or that the household asked for.
pub type ChangeFn = Arc<dyn Fn(&AudioDeviceStatus, Trigger) + Send + Sync>;

pub struct SupervisorConfig {
    pub role: Role,
    pub data_dir: PathBuf,
    pub mic: Option<MicHandle>,
    pub prefs: PrefsFn,
    pub on_change: Option<ChangeFn>,
}

#[derive(Debug, Clone, Copy)]
struct Which {
    input: bool,
    output: bool,
}

const BOTH: Which = Which {
    input: true,
    output: true,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StatusFile {
    status: AudioDeviceStatus,
    /// The retest request this pass answered, so serve can tell its answer from an older one.
    answered: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RetestRequest {
    id: u64,
    chime: bool,
    /// False for a settings change: the voice session chooses again lazily, as at startup.
    #[serde(default = "full_by_default")]
    full: bool,
}

fn full_by_default() -> bool {
    true
}

fn audio_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("audio")
}
fn status_path(data_dir: &Path, role: Role) -> PathBuf {
    audio_dir(data_dir).join(format!("devices-{}.json", role.name()))
}
fn request_path(data_dir: &Path) -> PathBuf {
    audio_dir(data_dir).join("retest-request.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Write-then-rename, so a reader in the other process never sees half a file.
fn write_json(path: &Path, value: &impl Serialize) {
    let Ok(json) = serde_json::to_vec_pretty(value) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 checks existence and permission only; nothing is delivered.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn pid_alive(_pid: u32) -> bool {
    false
}

/// The voice session's status, if one is running in another process.
fn live_voice(data_dir: &Path) -> Option<StatusFile> {
    let file: StatusFile = read_json(&status_path(data_dir, Role::Voice))?;
    let pid = file.status.pid;
    (pid != std::process::id() && pid_alive(pid)).then_some(file)
}

struct Inner {
    role: Role,
    data_dir: PathBuf,
    mic: Option<MicHandle>,
    prefs: PrefsFn,
    on_change: Option<ChangeFn>,
    selector: AudioDeviceSelector,
    status: Mutex<Option<AudioDeviceStatus>>,
    ready: Condvar,
    /// Devices that broke mid-session, passed over until the device list changes.
    exclude: Mutex<Vec<String>>,
    /// One pass at a time: the watch thread and a retest must never probe together.
    selecting: Mutex<()>,
    /// The last pass played the test sound, so a retest need not add a chime on top.
    played: AtomicBool,
    stop: AtomicBool,
}

#[derive(Clone)]
pub struct AudioSupervisor {
    inner: Arc<Inner>,
}

impl AudioSupervisor {
    /// Start choosing on a thread of its own; [`Self::wait_ready`] waits for the first choice.
    pub fn spawn(config: SupervisorConfig, prober: Arc<dyn AudioDeviceProber>) -> Self {
        let inner = Arc::new(Inner {
            role: config.role,
            data_dir: config.data_dir,
            mic: config.mic,
            prefs: config.prefs,
            on_change: config.on_change,
            selector: AudioDeviceSelector::new(prober),
            status: Mutex::new(None),
            ready: Condvar::new(),
            exclude: Mutex::new(Vec::new()),
            selecting: Mutex::new(()),
            played: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let watcher = inner.clone();
        let spawned = std::thread::Builder::new()
            .name("audio-devices".into())
            .spawn(move || watcher.run());
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "audio device supervisor did not start");
        }
        Self { inner }
    }

    pub fn wait_ready(&self, timeout: Duration) -> Option<AudioDeviceStatus> {
        let guard = self.inner.status.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = self
            .inner
            .ready
            .wait_timeout_while(guard, timeout, |s| s.is_none())
            .unwrap_or_else(|e| e.into_inner());
        guard.clone()
    }

    pub fn status(&self) -> Option<AudioDeviceStatus> {
        self.inner
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Test every device now, apply the result, and return it.
    pub fn retest(&self, chime: bool) -> AudioDeviceStatus {
        self.inner.retest(chime, None)
    }

    /// Stop watching. A voice session also withdraws its status, so serve stops deferring to it.
    pub fn stop(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        if self.inner.role == Role::Voice {
            let _ = std::fs::remove_file(status_path(&self.inner.data_dir, Role::Voice));
        }
    }
}

impl Inner {
    /// Whether this process holds the microphone, and so tests and watches it.
    fn owns_mic(&self) -> bool {
        match self.role {
            Role::Voice => true,
            Role::Serve => false,
            Role::Room => live_voice(&self.data_dir).is_none(),
        }
    }

    fn input_choice(&self, prefs: &AudioPrefs, mode: ProbeMode, exclude: &[String]) -> AudioChoice {
        let pinned = prefs.input_pin.as_deref();
        if !prefs.mic_enabled {
            return AudioChoice::mic_off(pinned);
        }
        // Serve opens the microphone only when the household asks for a test.
        if !self.owns_mic() && mode == ProbeMode::Lazy {
            let quick = self.selector.prober().quick_list(AudioDirection::Input);
            let quick = if quick.is_empty() {
                self.selector
                    .prober()
                    .default_device(AudioDirection::Input)
                    .into_iter()
                    .collect()
            } else {
                quick
            };
            return AudioChoice::not_tested(
                AudioDirection::Input,
                quick,
                pinned,
                self.selector.prober().plugged_in_first(),
            );
        }
        self.selector.select(&SelectRequest {
            direction: AudioDirection::Input,
            pinned,
            mode,
            exclude,
            mic_enabled: prefs.mic_enabled,
        })
    }

    fn pass(
        &self,
        trigger: Trigger,
        mode: ProbeMode,
        which: Which,
        answered: Option<u64>,
    ) -> AudioDeviceStatus {
        let _one_at_a_time = self.selecting.lock().unwrap_or_else(|e| e.into_inner());
        let prefs = (self.prefs)();
        let exclude = self
            .exclude
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let previous = self
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        // Every choice is made by sound where the household allows it: serve leaves the test to a
        // voice session that is running, since that session holds the microphone.
        let in_charge = self.role == Role::Voice || live_voice(&self.data_dir).is_none();
        let by_sound = prefs.sound_test && prefs.mic_enabled && in_charge;
        let sound = by_sound.then(|| {
            self.selector.select_by_sound(&PairRequest {
                input_pin: prefs.input_pin.as_deref(),
                output_pin: prefs.output_pin.as_deref(),
                mode,
                exclude: &exclude,
                raise_levels: prefs.manage_system,
            })
        });
        self.played.store(
            matches!(
                sound,
                Some(SoundTest::Heard { .. } | SoundTest::NotHeard { .. })
            ),
            Ordering::SeqCst,
        );
        let mut raised = Vec::new();
        let (input, output) = match sound {
            Some(SoundTest::Heard {
                input,
                output,
                raised: by_raising,
            }) => {
                raised = by_raising;
                (
                    self.with_the_rest(input, mode, &prefs, &exclude),
                    self.with_the_rest(output, mode, &prefs, &exclude),
                )
            }
            not_heard => {
                let input = match (&previous, which.input) {
                    (Some(prev), false) => prev.input.clone(),
                    _ => self.input_choice(&prefs, mode, &exclude),
                };
                let mut output = match (&previous, which.output) {
                    (Some(prev), false) => prev.output.clone(),
                    _ => self.silent_choice(AudioDirection::Output, mode, &prefs, &exclude),
                };
                output.unheard = matches!(not_heard, Some(SoundTest::NotHeard { .. }));
                (input, output)
            }
        };

        // Serve leaves the board's defaults to a live voice session, whose microphone choice is
        // the real one; two processes setting them would take turns overriding each other.
        // Set before retargeting so streams routed through the sound server land on the new card.
        let sound_server = (prefs.manage_system && in_charge)
            .then(|| {
                let mic = (!matches!(input.basis, ChoiceBasis::MicOff | ChoiceBasis::NoDevice))
                    .then_some(input.device.as_ref())
                    .flatten();
                self.selector
                    .prober()
                    .set_system_defaults(mic, output.device.as_ref())
            })
            .flatten();

        if let Some(mic) = &self.mic {
            if !matches!(input.basis, ChoiceBasis::MicOff | ChoiceBasis::NotTested) {
                let route = stream_route(&input, sound_server.as_ref(), &exclude);
                mic.retarget(route.map(str::to_string));
            }
        }
        let output_route = stream_route(&output, sound_server.as_ref(), &exclude);
        pond_audio_out::set_output_device(output_route.map(str::to_string));

        let status = AudioDeviceStatus {
            input,
            output,
            updated_at_ms: now_ms(),
            source: self.role.name().to_string(),
            pid: std::process::id(),
            raised,
            sound_server,
        };
        let changed = previous.as_ref().is_none_or(|p| {
            p.input.device_id() != status.input.device_id()
                || p.output.device_id() != status.output.device_id()
                || p.input.basis != status.input.basis
                || p.output.basis != status.output.basis
        });
        if changed {
            tracing::info!(
                ?trigger,
                input = %status.input,
                output = %status.output,
                "audio devices chosen"
            );
        }
        write_json(
            &status_path(&self.data_dir, self.role),
            &StatusFile {
                status: status.clone(),
                answered,
            },
        );
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = Some(status.clone());
        self.ready.notify_all();

        if changed || matches!(trigger, Trigger::Startup | Trigger::Retest) {
            if let Some(on_change) = &self.on_change {
                on_change(&status, trigger);
            }
        }
        status
    }

    fn chime(&self, status: &AudioDeviceStatus) {
        if let Some(device) = &status.output.device {
            if let Err(e) = self.selector.prober().chime(device) {
                tracing::warn!(error = %e, "test chime did not play");
            }
        }
    }

    fn retest(&self, chime: bool, answered: Option<u64>) -> AudioDeviceStatus {
        self.exclude
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        let status = self.pass(Trigger::Retest, ProbeMode::Full, BOTH, answered);
        // Asked to hear the speaker: the sound test already played on it, unless none ran.
        if chime && !self.played.load(Ordering::SeqCst) {
            self.chime(&status);
        }
        status
    }

    fn silent_choice(
        &self,
        direction: AudioDirection,
        mode: ProbeMode,
        prefs: &AudioPrefs,
        exclude: &[String],
    ) -> AudioChoice {
        self.selector.select(&SelectRequest {
            direction,
            pinned: match direction {
                AudioDirection::Input => prefs.input_pin.as_deref(),
                AudioDirection::Output => prefs.output_pin.as_deref(),
            },
            mode,
            exclude,
            mic_enabled: prefs.mic_enabled,
        })
    }

    /// A heard pair names only the devices it paired; the picker needs the rest too. A full test
    /// also checks them silently, so the household sees what each one does.
    fn with_the_rest(
        &self,
        mut heard: AudioChoice,
        mode: ProbeMode,
        prefs: &AudioPrefs,
        exclude: &[String],
    ) -> AudioChoice {
        let rest: Vec<ProbedDevice> = match mode {
            ProbeMode::Full => {
                self.silent_choice(heard.direction, mode, prefs, exclude)
                    .devices
            }
            ProbeMode::Lazy => self
                .selector
                .prober()
                .quick_list(heard.direction)
                .into_iter()
                .map(|d| ProbedDevice::new(d, ProbeOutcome::Untested))
                .collect(),
        };
        for p in rest {
            match heard
                .devices
                .iter_mut()
                .find(|d| d.device.id == p.device.id)
            {
                // The pair test only proved its own pair; what the silent check found stands
                // for the rest.
                Some(known) if known.outcome == ProbeOutcome::Untested => known.outcome = p.outcome,
                Some(_) => {}
                None => heard.devices.push(p),
            }
        }
        heard
    }

    fn exclude(&self, device: Option<String>) {
        if let Some(device) = device.filter(|d| !d.is_empty()) {
            let mut exclude = self.exclude.lock().unwrap_or_else(|e| e.into_inner());
            if !exclude.contains(&device) {
                exclude.push(device);
            }
        }
    }

    fn current_input(&self) -> Option<String> {
        let exclude = self
            .exclude
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|s| {
                stream_route(&s.input, s.sound_server.as_ref(), &exclude).map(str::to_string)
            })
    }

    fn run(self: Arc<Self>) {
        // A request left from before this session started is not ours to answer.
        let mut handled: Option<u64> =
            read_json::<RetestRequest>(&request_path(&self.data_dir)).map(|r| r.id);
        // Taken before the first choice, not after: a device that comes or goes while it runs
        // must still read as a change, or startup misses a USB card that arrived a moment late.
        let prober = self.selector.prober().clone();
        let mut fingerprint = prober.fingerprint();
        self.pass(Trigger::Startup, ProbeMode::Lazy, BOTH, None);
        let mut fingerprint_at = Instant::now();
        let mut mic_written = 0u64;
        let mut mic_moved_at = Instant::now();
        let mut mic_was_failed = false;
        let mut last_failure_pass: Option<Instant> = None;

        loop {
            std::thread::sleep(TICK);
            // Checked after the sleep: a stopped supervisor must not take another's stall report.
            if self.stop.load(Ordering::SeqCst) {
                break;
            }

            if self.role == Role::Voice {
                if let Some(req) = read_json::<RetestRequest>(&request_path(&self.data_dir)) {
                    if handled != Some(req.id) {
                        handled = Some(req.id);
                        if req.full {
                            self.retest(req.chime, Some(req.id));
                        } else {
                            self.pass(
                                Trigger::SettingsChanged,
                                ProbeMode::Lazy,
                                BOTH,
                                Some(req.id),
                            );
                        }
                    }
                }
            }

            if fingerprint_at.elapsed() >= FINGERPRINT_EVERY {
                fingerprint_at = Instant::now();
                let now = prober.fingerprint();
                if now != fingerprint {
                    fingerprint = now;
                    pond_audio_out::route::forget_devices();
                    self.exclude
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clear();
                    self.pass(Trigger::DevicesChanged, ProbeMode::Lazy, BOTH, None);
                }
            }

            let may_retry = last_failure_pass.is_none_or(|t| t.elapsed() >= FAILURE_BACKOFF);
            if let (true, Some(mic)) = (self.owns_mic(), &self.mic) {
                let state = mic.state();
                let failed = matches!(state, MicState::Failed(_));
                let written = mic.shared().written();
                if written != mic_written || !state.is_open() {
                    mic_written = written;
                    mic_moved_at = Instant::now();
                }
                let stalled = state.is_open() && mic_moved_at.elapsed() >= MIC_STALL;
                if ((failed && !mic_was_failed) || stalled) && may_retry {
                    tracing::warn!(?state, stalled, "microphone lost; choosing another");
                    let lost = self.current_input();
                    self.exclude(lost.clone());
                    last_failure_pass = Some(Instant::now());
                    mic_moved_at = Instant::now();
                    let status = self.pass(
                        Trigger::InputLost,
                        ProbeMode::Lazy,
                        Which {
                            input: true,
                            output: false,
                        },
                        None,
                    );
                    // The only device came back as the choice: a retarget to the same name does
                    // not restart a stream that stalled while still open, so restart it.
                    if status.input.device_id() == lost.as_deref() {
                        mic.reopen();
                    }
                }
                mic_was_failed = failed;
            }

            if let Some(stalled) = pond_audio_out::route::take_stall() {
                if may_retry {
                    self.exclude(Some(stalled));
                    last_failure_pass = Some(Instant::now());
                    self.pass(
                        Trigger::OutputStalled,
                        ProbeMode::Lazy,
                        Which {
                            input: false,
                            output: true,
                        },
                        None,
                    );
                }
            }
        }
    }
}

/// The control for the API and the assistant's tool. In the server process it shows the voice
/// session's choice while one runs, since that owns the microphone, and the server's otherwise.
pub struct PondAudioControl {
    serve: AudioSupervisor,
    data_dir: PathBuf,
}

impl PondAudioControl {
    pub fn new(serve: AudioSupervisor, data_dir: PathBuf) -> Self {
        Self { serve, data_dir }
    }

    fn own_status(&self) -> AudioDeviceStatus {
        self.serve
            .status()
            .or_else(|| self.serve.wait_ready(Duration::from_secs(5)))
            .unwrap_or_else(|| AudioDeviceStatus {
                input: AudioChoice::not_tested(AudioDirection::Input, Vec::new(), None, false),
                output: AudioChoice::not_tested(AudioDirection::Output, Vec::new(), None, false),
                updated_at_ms: now_ms(),
                source: Role::Serve.name().to_string(),
                pid: std::process::id(),
                raised: Vec::new(),
                sound_server: None,
            })
    }
}

impl AudioDeviceControl for PondAudioControl {
    fn status(&self) -> AudioDeviceStatus {
        match live_voice(&self.data_dir) {
            Some(voice) => voice.status,
            None => self.own_status(),
        }
    }

    fn retest(&self, chime: bool) -> AudioDeviceStatus {
        if live_voice(&self.data_dir).is_none() {
            return self.serve.retest(chime);
        }
        // The voice session holds the microphone, so it runs the test; this process only
        // re-checks the speaker it announces on.
        let id = now_ms();
        let request = RetestRequest {
            id,
            chime,
            full: true,
        };
        write_json(&request_path(&self.data_dir), &request);
        self.serve.inner.pass(
            Trigger::Retest,
            ProbeMode::Full,
            Which {
                input: false,
                output: true,
            },
            None,
        );
        let deadline = Instant::now() + VOICE_ANSWER_WAIT;
        while Instant::now() < deadline {
            match live_voice(&self.data_dir) {
                Some(voice) if voice.answered == Some(id) => return voice.status,
                Some(_) => std::thread::sleep(Duration::from_millis(100)),
                None => return self.serve.retest(chime),
            }
        }
        tracing::warn!("the voice session did not answer the device test in time");
        self.status()
    }

    fn reselect(&self) {
        if live_voice(&self.data_dir).is_some() {
            let request = RetestRequest {
                id: now_ms(),
                chime: false,
                full: false,
            };
            write_json(&request_path(&self.data_dir), &request);
        }
        // Lazy, so serve still opens no microphone: its input stays untested or off.
        self.serve
            .inner
            .pass(Trigger::SettingsChanged, ProbeMode::Lazy, BOTH, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pond_core::models::domain::audio_device::{
        AudioDevice, DeviceKind, PairOutcome, ProbeOutcome, SoundServerDefaults,
    };
    use std::collections::HashMap;

    /// A scripted machine whose devices can be unplugged between passes.
    #[derive(Default)]
    struct Machine {
        inputs: Mutex<Vec<AudioDevice>>,
        outputs: Mutex<Vec<AudioDevice>>,
        outcomes: Mutex<HashMap<String, ProbeOutcome>>,
        probes: Mutex<Vec<String>>,
        chimes: Mutex<Vec<String>>,
        /// `(speaker, mic)` pairs that hear each other, and every pair a sound test tried.
        heard: Mutex<Vec<(String, String)>>,
        sound_tests: Mutex<Vec<(String, String)>>,
        /// Every `(mic, speaker)` made the board's default.
        defaults_set: Mutex<Vec<(Option<String>, Option<String>)>>,
    }

    impl Machine {
        fn plug(&self, dir: AudioDirection, id: &str, is_default: bool, outcome: ProbeOutcome) {
            let d = AudioDevice::new(id, is_default, DeviceKind::Hardware);
            match dir {
                AudioDirection::Input => self.inputs.lock().unwrap().push(d),
                AudioDirection::Output => self.outputs.lock().unwrap().push(d),
            }
            self.outcomes.lock().unwrap().insert(id.into(), outcome);
        }
        fn unplug(&self, id: &str) {
            self.inputs.lock().unwrap().retain(|d| d.id != id);
            self.outputs.lock().unwrap().retain(|d| d.id != id);
        }
        fn side(&self, dir: AudioDirection) -> Vec<AudioDevice> {
            match dir {
                AudioDirection::Input => self.inputs.lock().unwrap().clone(),
                AudioDirection::Output => self.outputs.lock().unwrap().clone(),
            }
        }
    }

    impl AudioDeviceProber for Machine {
        fn default_device(&self, dir: AudioDirection) -> Option<AudioDevice> {
            self.side(dir).into_iter().find(|d| d.is_default)
        }
        fn devices(&self, dir: AudioDirection) -> Result<Vec<AudioDevice>, String> {
            Ok(self.side(dir))
        }
        fn quick_list(&self, dir: AudioDirection) -> Vec<AudioDevice> {
            self.side(dir)
        }
        fn probe(&self, d: &AudioDevice, _: AudioDirection, _: Duration) -> ProbeOutcome {
            self.probes.lock().unwrap().push(d.id.clone());
            self.outcomes.lock().unwrap().get(&d.id).cloned().unwrap()
        }
        fn fingerprint(&self) -> String {
            let mut ids: Vec<String> = self
                .side(AudioDirection::Input)
                .into_iter()
                .chain(self.side(AudioDirection::Output))
                .map(|d| d.id)
                .collect();
            ids.sort();
            ids.join(",")
        }
        fn chime(&self, d: &AudioDevice) -> Result<(), String> {
            self.chimes.lock().unwrap().push(d.id.clone());
            Ok(())
        }
        fn hears(&self, speaker: &AudioDevice, mic: &AudioDevice) -> PairOutcome {
            let pair = (speaker.id.clone(), mic.id.clone());
            self.sound_tests.lock().unwrap().push(pair.clone());
            if self.heard.lock().unwrap().contains(&pair) {
                PairOutcome::Heard {
                    margin_db: 25.0,
                    room_dbfs: -55.0,
                }
            } else {
                PairOutcome::NotHeard
            }
        }
        fn set_system_defaults(
            &self,
            input: Option<&AudioDevice>,
            output: Option<&AudioDevice>,
        ) -> Option<SoundServerDefaults> {
            self.defaults_set
                .lock()
                .unwrap()
                .push((input.map(|d| d.id.clone()), output.map(|d| d.id.clone())));
            Some(SoundServerDefaults {
                server: "pulseaudio".into(),
                speaker: output.map(|d| d.label.clone()),
                mic: input.map(|d| d.label.clone()),
                problems: Vec::new(),
            })
        }
    }

    fn hearing() -> ProbeOutcome {
        ProbeOutcome::Hearing { level_dbfs: -55.0 }
    }

    fn machine() -> Arc<Machine> {
        let m = Arc::new(Machine::default());
        m.plug(AudioDirection::Input, "Built-in mic", true, hearing());
        m.plug(
            AudioDirection::Output,
            "Built-in speakers",
            true,
            ProbeOutcome::Accepting,
        );
        m
    }

    fn prefs(input_pin: Option<&str>, mic_enabled: bool, sound_test: bool) -> PrefsFn {
        let input_pin = input_pin.map(str::to_string);
        Arc::new(move || AudioPrefs {
            input_pin: input_pin.clone(),
            output_pin: None,
            mic_enabled,
            sound_test,
            manage_system: true,
        })
    }

    fn start(m: &Arc<Machine>, role: Role, dir: &Path, prefs: PrefsFn) -> AudioSupervisor {
        AudioSupervisor::spawn(
            SupervisorConfig {
                role,
                data_dir: dir.to_path_buf(),
                mic: None,
                prefs,
                on_change: None,
            },
            m.clone(),
        )
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The speaker route is process-wide; supervisor tests that read it take turns.
    static ROUTE: Mutex<()> = Mutex::new(());

    #[test]
    fn startup_chooses_both_and_writes_the_status_for_the_other_process() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, false));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(status.input.device_id(), Some("Built-in mic"));
        assert_eq!(status.output.device_id(), Some("Built-in speakers"));
        assert_eq!(
            pond_audio_out::output_device().as_deref(),
            Some("Built-in speakers")
        );
        let file: StatusFile = read_json(&status_path(dir.path(), Role::Voice)).unwrap();
        assert_eq!(file.status, status);
        assert!(
            m.chimes.lock().unwrap().is_empty(),
            "startup is never audible"
        );
        sup.stop();
    }

    #[test]
    fn serve_never_opens_the_microphone_on_its_own() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Serve, dir.path(), prefs(None, true, false));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(status.input.basis, ChoiceBasis::NotTested);
        assert_eq!(status.input.device_id(), Some("Built-in mic"));
        assert_eq!(*m.probes.lock().unwrap(), ["Built-in speakers"]);

        // Asked to, it does.
        let tested = sup.retest(false);
        assert_eq!(tested.input.basis, ChoiceBasis::SystemDefault);
        assert!(m
            .probes
            .lock()
            .unwrap()
            .contains(&"Built-in mic".to_string()));
        sup.stop();
    }

    #[test]
    fn serve_running_the_room_tests_its_microphone_at_startup() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Room, dir.path(), prefs(None, true, false));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(status.input.basis, ChoiceBasis::SystemDefault);
        assert!(m
            .probes
            .lock()
            .unwrap()
            .contains(&"Built-in mic".to_string()));
        assert_eq!(
            status.source, "serve",
            "a voice session defers to it as to serve"
        );
        assert!(read_json::<StatusFile>(&status_path(dir.path(), Role::Serve)).is_some());
        sup.stop();
    }

    #[test]
    fn the_room_leaves_the_microphone_to_a_live_voice_session() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let voice = StatusFile {
            status: AudioDeviceStatus {
                input: AudioChoice::mic_off(None),
                output: AudioChoice::not_tested(AudioDirection::Output, Vec::new(), None, false),
                updated_at_ms: 1,
                source: "voice".into(),
                pid: 1,
                raised: Vec::new(),
                sound_server: None,
            },
            answered: None,
        };
        write_json(&status_path(dir.path(), Role::Voice), &voice);
        let sup = start(&m, Role::Room, dir.path(), prefs(None, true, true));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(status.input.basis, ChoiceBasis::NotTested);
        assert!(!m
            .probes
            .lock()
            .unwrap()
            .contains(&"Built-in mic".to_string()));
        assert!(m.sound_tests.lock().unwrap().is_empty());
        sup.stop();
    }

    #[test]
    fn plugging_in_the_pinned_mic_moves_to_it_and_unplugging_it_moves_back() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(
            &m,
            Role::Voice,
            dir.path(),
            prefs(Some("USB mic"), true, false),
        );
        let first = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(first.input.device_id(), Some("Built-in mic"));
        assert_eq!(first.input.basis, ChoiceBasis::Fallback);

        m.plug(AudioDirection::Input, "USB mic", false, hearing());
        wait_until("the pinned mic", || {
            sup.status().unwrap().input.device_id() == Some("USB mic")
        });
        assert_eq!(sup.status().unwrap().input.basis, ChoiceBasis::Pinned);

        m.unplug("USB mic");
        wait_until("the fallback", || {
            sup.status().unwrap().input.device_id() == Some("Built-in mic")
        });
        sup.stop();
    }

    /// The race CI found: a mic plugged in right as the first choice lands. Plugged from the
    /// on_change callback, which runs inside that very first pass.
    #[test]
    fn a_device_plugged_in_during_the_startup_choice_is_still_noticed() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let plug = m.clone();
        let on_change: ChangeFn = Arc::new(move |_, trigger| {
            if trigger == Trigger::Startup {
                plug.plug(AudioDirection::Input, "USB mic", false, hearing());
            }
        });
        let sup = AudioSupervisor::spawn(
            SupervisorConfig {
                role: Role::Voice,
                data_dir: dir.path().to_path_buf(),
                mic: None,
                prefs: prefs(Some("USB mic"), true, false),
                on_change: Some(on_change),
            },
            m.clone(),
        );
        sup.wait_ready(Duration::from_secs(5)).unwrap();
        wait_until("the mic plugged in during startup", || {
            sup.status().unwrap().input.device_id() == Some("USB mic")
        });
        sup.stop();
    }

    #[test]
    fn headphones_arriving_as_the_new_default_take_over() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, false));
        sup.wait_ready(Duration::from_secs(5)).unwrap();

        // The OS makes connected headphones the default.
        m.outputs
            .lock()
            .unwrap()
            .iter_mut()
            .for_each(|d| d.is_default = false);
        m.plug(
            AudioDirection::Output,
            "AirPods",
            true,
            ProbeOutcome::Accepting,
        );
        wait_until("the headphones", || {
            sup.status().unwrap().output.device_id() == Some("AirPods")
        });
        assert_eq!(pond_audio_out::output_device().as_deref(), Some("AirPods"));
        assert!(
            m.chimes.lock().unwrap().is_empty(),
            "silent checks make no sound"
        );
        assert!(m.sound_tests.lock().unwrap().is_empty());
        sup.stop();
    }

    /// The nano's case: the default speaker takes audio and makes no sound. Only the pair the
    /// microphone hears is used, for both directions.
    #[test]
    fn startup_uses_the_first_speaker_and_mic_that_hear_each_other() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        m.plug(
            AudioDirection::Output,
            "USB speaker",
            false,
            ProbeOutcome::Accepting,
        );
        m.plug(AudioDirection::Input, "USB mic", false, hearing());
        m.heard
            .lock()
            .unwrap()
            .push(("USB speaker".into(), "USB mic".into()));
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, true));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(status.output.device_id(), Some("USB speaker"));
        assert_eq!(status.input.device_id(), Some("USB mic"));
        assert_eq!(status.output.basis, ChoiceBasis::Heard);
        assert_eq!(status.output.heard_with.as_deref(), Some("USB mic"));
        assert_eq!(
            pond_audio_out::output_device().as_deref(),
            Some("USB speaker")
        );
        let tried = m.sound_tests.lock().unwrap().clone();
        assert_eq!(
            tried.first().map(|(s, m)| (s.as_str(), m.as_str())),
            Some(("Built-in speakers", "Built-in mic")),
            "the defaults are paired first"
        );
        sup.stop();
    }

    #[test]
    fn a_full_test_by_sound_still_reports_what_each_other_device_does() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        m.plug(AudioDirection::Input, "Phone", false, ProbeOutcome::Silent);
        m.heard
            .lock()
            .unwrap()
            .push(("Built-in speakers".into(), "Built-in mic".into()));
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, true));
        sup.wait_ready(Duration::from_secs(5)).unwrap();
        let status = sup.retest(false);
        assert_eq!(status.input.basis, ChoiceBasis::Heard);
        let phone = status
            .input
            .devices
            .iter()
            .find(|d| d.device.id == "Phone")
            .unwrap();
        assert_eq!(phone.outcome, ProbeOutcome::Silent);
        sup.stop();
    }

    #[test]
    fn when_no_pair_is_heard_the_silent_choice_stands_and_says_so() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, true));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(status.output.device_id(), Some("Built-in speakers"));
        assert!(status.output.unheard);
        assert!(status.output.problem().unwrap().contains("turned up"));
        // The test sound already played, so asking to hear the speaker adds no chime.
        sup.retest(true);
        assert!(m.chimes.lock().unwrap().is_empty());
        sup.stop();
    }

    #[test]
    fn with_sound_tests_off_a_requested_chime_still_plays() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, false));
        sup.wait_ready(Duration::from_secs(5)).unwrap();
        sup.retest(true);
        assert_eq!(*m.chimes.lock().unwrap(), ["Built-in speakers"]);
        sup.stop();
    }

    /// Serve does not test by sound beside a voice session, which holds the microphone.
    #[test]
    fn serve_leaves_the_sound_test_to_a_live_voice_session() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let fake_voice = StatusFile {
            status: AudioDeviceStatus {
                input: AudioChoice::mic_off(None),
                output: AudioChoice::not_tested(AudioDirection::Output, Vec::new(), None, false),
                updated_at_ms: 1,
                source: "voice".into(),
                pid: 1,
                raised: Vec::new(),
                sound_server: None,
            },
            answered: None,
        };
        write_json(&status_path(dir.path(), Role::Voice), &fake_voice);
        let sup = start(&m, Role::Serve, dir.path(), prefs(None, true, true));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert!(m.sound_tests.lock().unwrap().is_empty());
        assert!(
            m.defaults_set.lock().unwrap().is_empty(),
            "the voice session sets the board's defaults, from its own microphone choice"
        );
        assert_eq!(status.sound_server, None);
        sup.stop();
    }

    #[test]
    fn the_choice_becomes_the_boards_default_unless_the_household_says_not() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, false));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        sup.stop();
        assert_eq!(
            *m.defaults_set.lock().unwrap(),
            [(
                Some("Built-in mic".to_string()),
                Some("Built-in speakers".to_string())
            )]
        );
        assert_eq!(
            status.sound_server.and_then(|s| s.speaker).as_deref(),
            Some("Built-in speakers")
        );

        let m = machine();
        let dir = tempfile::tempdir().unwrap();
        let hands_off: PrefsFn = Arc::new(|| AudioPrefs {
            mic_enabled: true,
            manage_system: false,
            ..Default::default()
        });
        let sup = start(&m, Role::Voice, dir.path(), hands_off);
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        sup.stop();
        assert!(m.defaults_set.lock().unwrap().is_empty());
        assert_eq!(status.sound_server, None);
    }

    #[test]
    fn with_the_mic_off_only_the_speaker_becomes_the_boards_default() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, false, false));
        sup.wait_ready(Duration::from_secs(5)).unwrap();
        sup.stop();
        assert_eq!(
            *m.defaults_set.lock().unwrap(),
            [(None, Some("Built-in speakers".to_string()))]
        );
    }

    #[test]
    fn a_speaker_that_stops_taking_audio_is_replaced() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        m.plug(
            AudioDirection::Output,
            "USB speaker",
            false,
            ProbeOutcome::Accepting,
        );
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, false));
        sup.wait_ready(Duration::from_secs(5)).unwrap();
        while pond_audio_out::route::take_stall().is_some() {}

        pond_audio_out::route::report_stall(Some("Built-in speakers".into()));
        wait_until("the replacement", || {
            sup.status().unwrap().output.device_id() == Some("USB speaker")
        });
        let status = sup.status().unwrap();
        assert_eq!(status.output.basis, ChoiceBasis::Fallback);
        sup.stop();
    }

    #[test]
    fn with_the_mic_off_the_voice_session_opens_no_input() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let sup = start(&m, Role::Voice, dir.path(), prefs(None, false, false));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        assert_eq!(status.input.basis, ChoiceBasis::MicOff);
        assert_eq!(*m.probes.lock().unwrap(), ["Built-in speakers"]);
        sup.retest(false);
        assert!(!m
            .probes
            .lock()
            .unwrap()
            .contains(&"Built-in mic".to_string()));
        sup.stop();
    }

    #[test]
    fn a_retest_request_from_serve_is_answered_by_the_voice_session() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        let voice = start(&m, Role::Voice, dir.path(), prefs(None, true, false));
        voice.wait_ready(Duration::from_secs(5)).unwrap();

        write_json(
            &request_path(dir.path()),
            &RetestRequest {
                id: 7,
                chime: true,
                full: true,
            },
        );
        wait_until("the answer", || {
            read_json::<StatusFile>(&status_path(dir.path(), Role::Voice))
                .is_some_and(|f| f.answered == Some(7))
        });
        assert_eq!(*m.chimes.lock().unwrap(), ["Built-in speakers"]);
        voice.stop();
    }

    #[test]
    fn a_settings_change_reselects_without_a_full_test() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = machine();
        m.plug(AudioDirection::Input, "Phone", false, hearing());
        let serve = start(&m, Role::Serve, dir.path(), prefs(None, true, false));
        serve.wait_ready(Duration::from_secs(5)).unwrap();
        let control = PondAudioControl::new(serve.clone(), dir.path().to_path_buf());
        control.reselect();
        let opened = m.probes.lock().unwrap().clone();
        assert!(
            !opened.iter().any(|p| p == "Built-in mic" || p == "Phone"),
            "serve opened a microphone for a settings change: {opened:?}"
        );

        // A voice session answers the same change lazily: the default hears, so nothing else opens.
        let voice = start(&m, Role::Voice, dir.path(), prefs(None, true, false));
        voice.wait_ready(Duration::from_secs(5)).unwrap();
        m.probes.lock().unwrap().clear();
        let request = RetestRequest {
            id: 9,
            chime: false,
            full: false,
        };
        write_json(&request_path(dir.path()), &request);
        wait_until("the lazy answer", || {
            read_json::<StatusFile>(&status_path(dir.path(), Role::Voice))
                .is_some_and(|f| f.answered == Some(9))
        });
        assert!(!m.probes.lock().unwrap().contains(&"Phone".to_string()));
        serve.stop();
        voice.stop();
    }

    #[test]
    fn a_stale_voice_status_from_a_dead_process_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let mut status = AudioDeviceStatus {
            input: AudioChoice::mic_off(None),
            output: AudioChoice::not_tested(AudioDirection::Output, Vec::new(), None, false),
            updated_at_ms: 1,
            source: "voice".into(),
            pid: u32::MAX - 1,
            raised: Vec::new(),
            sound_server: None,
        };
        write_json(
            &status_path(dir.path(), Role::Voice),
            &StatusFile {
                status: status.clone(),
                answered: None,
            },
        );
        assert!(live_voice(dir.path()).is_none(), "no such process");
        status.pid = std::process::id();
        write_json(
            &status_path(dir.path(), Role::Voice),
            &StatusFile {
                status,
                answered: None,
            },
        );
        assert!(
            live_voice(dir.path()).is_none(),
            "our own file is not another session"
        );
    }

    #[test]
    fn on_a_board_streams_go_through_the_sound_server_and_fall_back_to_the_card_if_it_stalls() {
        let _g = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let m = Arc::new(Machine::default());
        m.outputs
            .lock()
            .unwrap()
            .push(AudioDevice::new("default", true, DeviceKind::SoundServer));
        m.inputs
            .lock()
            .unwrap()
            .push(AudioDevice::new("default", true, DeviceKind::SoundServer));
        m.outcomes
            .lock()
            .unwrap()
            .insert("default".into(), ProbeOutcome::Accepting);
        let mut usb_out = AudioDevice::new("sysdefault:CARD=Device", false, DeviceKind::Hardware);
        usb_out.plugged_in = true;
        let mut usb_in = usb_out.clone();
        usb_in.label = "USB PnP Audio Device".into();
        m.outputs.lock().unwrap().push(usb_out);
        m.inputs.lock().unwrap().push(usb_in);
        m.outcomes
            .lock()
            .unwrap()
            .insert("sysdefault:CARD=Device".into(), hearing());
        m.heard.lock().unwrap().push((
            "sysdefault:CARD=Device".into(),
            "sysdefault:CARD=Device".into(),
        ));

        let sup = start(&m, Role::Voice, dir.path(), prefs(None, true, true));
        let status = sup.wait_ready(Duration::from_secs(5)).unwrap();
        while pond_audio_out::route::take_stall().is_some() {}

        assert_eq!(status.output.device_id(), Some("sysdefault:CARD=Device"));
        assert_eq!(status.input.device_id(), Some("sysdefault:CARD=Device"));
        assert_eq!(
            pond_audio_out::output_device().as_deref(),
            Some("default"),
            "once the sound server's default is the USB card, playback goes through the server"
        );
        assert_eq!(
            sup.inner.current_input().as_deref(),
            Some("default"),
            "capture goes through the server too"
        );

        pond_audio_out::route::report_stall(Some("default".into()));
        wait_until(
            "fallback to the card after the sound server stalled",
            || pond_audio_out::output_device().as_deref() == Some("sysdefault:CARD=Device"),
        );
        assert_eq!(
            sup.status().unwrap().output.device_id(),
            Some("sysdefault:CARD=Device")
        );
        sup.stop();
    }
}
