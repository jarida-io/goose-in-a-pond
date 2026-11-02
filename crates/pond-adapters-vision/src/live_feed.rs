use crate::annotate::{annotate_jpeg, annotate_jpeg_for_model, FrameAnnotation, KnownPerson};
use chrono::{DateTime, Utc};
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::domain::vision::{self as policy, EventSignals};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, watch, Notify};

const BROADCAST_CAPACITY: usize = 16;
const EVENT_BROADCAST_CAPACITY: usize = 64;
const FPS_WINDOW: Duration = Duration::from_secs(3);
const ANNOTATION_TTL: Duration = Duration::from_secs(15);

/// A camera whose newest frame is older than this is not `live`.
pub const LIVE_FRAME_WINDOW: Duration = Duration::from_secs(10);

/// The event side of the vision settings, clamped; derived from `Settings`, never stored apart.
#[derive(Debug, Clone, PartialEq)]
pub struct VisionBusConfig {
    pub min_event_interval_secs: u64,
    pub classifier_confidence_threshold: f64,
    pub face_detection_enabled: bool,
    pub face_on_motion: bool,
    pub enabled_event_types: Vec<String>,
}

impl Default for VisionBusConfig {
    fn default() -> Self {
        Self::from_settings(&Settings::default())
    }
}

impl VisionBusConfig {
    pub fn from_settings(s: &Settings) -> Self {
        Self {
            min_event_interval_secs: policy::clamp_event_cooldown_secs(
                s.vision_event_cooldown_secs,
            ),
            classifier_confidence_threshold: policy::clamp_classifier_confidence(
                s.vision_classifier_confidence,
            ),
            face_detection_enabled: s.vision_face_recognition_enabled,
            face_on_motion: s.vision_face_on_motion,
            enabled_event_types: policy::normalize_event_types(&s.vision_event_types),
        }
    }

    pub fn supported_signals() -> &'static [&'static str] {
        &[
            "motion",
            "person",
            "pet",
            "vehicle",
            "package",
            "face_detected",
            "face_recognized",
            "face_unknown",
            "face:<profile_id>",
        ]
    }

    /// Whether an event carrying `signals` is recorded; any one enabled signal is enough.
    pub fn passes(&self, signals: EventSignals<'_>) -> bool {
        signals.passes(&self.enabled_event_types)
    }
}

/// What the vision supervisor is doing, as `GET /vision/status` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionState {
    Off,
    ConsentOff,
    NoCamera,
    Starting,
    Running,
    Reconnecting,
}

impl VisionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            VisionState::Off => "off",
            VisionState::ConsentOff => "consent_off",
            VisionState::NoCamera => "no_camera",
            VisionState::Starting => "starting",
            VisionState::Running => "running",
            VisionState::Reconnecting => "reconnecting",
        }
    }

    /// States in which the supervisor is not trying to capture.
    pub fn is_idle(&self) -> bool {
        matches!(
            self,
            VisionState::Off | VisionState::ConsentOff | VisionState::NoCamera
        )
    }
}

/// Where the capture source came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Configured,
    Discovered,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Configured => "configured",
            SourceKind::Discovered => "discovered",
        }
    }
}

/// How frames reach the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePath {
    /// The camera's own JPEGs, passed through ffmpeg; motion decodes them on the CPU.
    CameraJpeg,
    /// GStreamer on a Jetson: NVDEC, NVJPG and VIC do the decoding and scaling.
    Hardware,
    /// ffmpeg decodes every frame and the pipeline encodes the JPEG.
    Software,
}

impl CapturePath {
    pub fn as_str(&self) -> &'static str {
        match self {
            CapturePath::CameraJpeg => "camera_jpeg",
            CapturePath::Hardware => "hardware",
            CapturePath::Software => "software",
        }
    }
}

/// The supervisor's last word on capture; written only by the supervisor and by frames arriving.
#[derive(Debug, Clone, PartialEq)]
pub struct SupervisorStatus {
    pub state: VisionState,
    pub camera_id: Option<String>,
    /// Credentials already redacted.
    pub source: Option<String>,
    pub source_kind: Option<SourceKind>,
    pub fps: Option<u32>,
    /// Set while a capture process is running.
    pub capture: Option<CapturePath>,
    pub last_error: Option<String>,
    /// Pipeline runs restarted after a stream ended or a capture setting changed.
    pub restarts: u64,
    pub classifier_available: bool,
}

impl Default for SupervisorStatus {
    fn default() -> Self {
        Self {
            state: VisionState::Off,
            camera_id: None,
            source: None,
            source_kind: None,
            fps: None,
            capture: None,
            last_error: None,
            restarts: 0,
            classifier_available: false,
        }
    }
}

#[derive(Clone)]
pub struct LiveCameraRegistry {
    feeds: Arc<RwLock<HashMap<String, LiveFeedEntry>>>,
    config: Arc<RwLock<VisionBusConfig>>,
    events_tx: broadcast::Sender<CameraEvent>,
    last_event_at: Arc<RwLock<Option<DateTime<Utc>>>>,
    status: Arc<RwLock<SupervisorStatus>>,
    cameras_allowed: Arc<watch::Sender<bool>>,
    settings_changed: Arc<Notify>,
    known_people: Arc<RwLock<Vec<KnownPerson>>>,
}

#[derive(Clone)]
struct LiveFeedEntry {
    latest: Arc<RwLock<Option<LatestFrame>>>,
    frame_times: Arc<RwLock<VecDeque<Instant>>>,
    annotation: Arc<RwLock<Option<FrameAnnotation>>>,
    tx: broadcast::Sender<Arc<Vec<u8>>>,
}

struct LatestFrame {
    jpeg: Arc<Vec<u8>>,
    at: DateTime<Utc>,
}

impl LiveFeedEntry {
    fn new() -> Self {
        let (tx, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            latest: Arc::new(RwLock::new(None)),
            frame_times: Arc::new(RwLock::new(VecDeque::with_capacity(128))),
            annotation: Arc::new(RwLock::new(None)),
            tx,
        }
    }
}

// A poisoned lock only means a reader panicked mid-clone; the data is still whole.
fn read<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read().unwrap_or_else(|e| e.into_inner())
}

fn write<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write().unwrap_or_else(|e| e.into_inner())
}

impl LiveCameraRegistry {
    pub fn new() -> Self {
        let (events_tx, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        let (cameras_allowed, _) = watch::channel(Settings::default().cameras_enabled);
        Self {
            feeds: Arc::new(RwLock::new(HashMap::new())),
            config: Arc::new(RwLock::new(VisionBusConfig::default())),
            events_tx,
            last_event_at: Arc::new(RwLock::new(None)),
            status: Arc::new(RwLock::new(SupervisorStatus::default())),
            cameras_allowed: Arc::new(cameras_allowed),
            settings_changed: Arc::new(Notify::new()),
            known_people: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub fn global() -> &'static Self {
        static REGISTRY: OnceLock<LiveCameraRegistry> = OnceLock::new();
        REGISTRY.get_or_init(LiveCameraRegistry::new)
    }

    pub fn bus_config(&self) -> VisionBusConfig {
        read(&self.config).clone()
    }

    /// Re-derive everything the registry caches from the stored settings.
    pub fn sync_settings(&self, settings: &Settings) {
        *write(&self.config) = VisionBusConfig::from_settings(settings);
        self.set_cameras_allowed(settings.cameras_enabled);
    }

    /// Revoking consent drops every cached frame and ends every open MJPEG and SSE stream.
    pub fn set_cameras_allowed(&self, allowed: bool) {
        let changed = self.cameras_allowed.send_if_modified(|current| {
            let changed = *current != allowed;
            *current = allowed;
            changed
        });
        if changed && !allowed {
            write(&self.feeds).clear();
        }
    }

    pub fn cameras_allowed(&self) -> bool {
        *self.cameras_allowed.borrow()
    }

    /// Resolves once camera consent is off (at once if it already is).
    pub fn cameras_revoked(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.cameras_allowed.subscribe();
        async move {
            let _ = rx.wait_for(|allowed| !*allowed).await;
        }
    }

    /// Wake the supervisor now instead of at its next poll.
    pub fn notify_settings_changed(&self) {
        self.settings_changed.notify_one();
    }

    pub async fn settings_changed(&self) {
        self.settings_changed.notified().await;
    }

    pub fn publish_event(&self, event: CameraEvent) {
        *write(&self.last_event_at) = Some(event.created_at);
        let _ = self.events_tx.send(event);
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<CameraEvent> {
        self.events_tx.subscribe()
    }

    pub fn event_listener_count(&self) -> usize {
        self.events_tx.receiver_count()
    }

    pub fn last_event_at(&self) -> Option<DateTime<Utc>> {
        *read(&self.last_event_at)
    }

    pub fn update_frame(&self, camera_id: &str, jpeg: Arc<Vec<u8>>) {
        if !self.cameras_allowed() {
            return;
        }
        let entry = write(&self.feeds)
            .entry(camera_id.to_string())
            .or_insert_with(LiveFeedEntry::new)
            .clone();
        let now_inst = Instant::now();
        {
            let mut times = write(&entry.frame_times);
            while times
                .front()
                .is_some_and(|t| now_inst.saturating_duration_since(*t) > FPS_WINDOW)
            {
                times.pop_front();
            }
            times.push_back(now_inst);
        }
        *write(&entry.latest) = Some(LatestFrame {
            jpeg: jpeg.clone(),
            at: Utc::now(),
        });
        let _ = entry.tx.send(jpeg);

        let promote = {
            let status = read(&self.status);
            status.camera_id.as_deref() == Some(camera_id)
                && (status.state != VisionState::Running || status.last_error.is_some())
                && !status.state.is_idle()
        };
        if promote {
            let mut status = write(&self.status);
            status.state = VisionState::Running;
            status.last_error = None;
        }
    }

    pub fn measured_fps(&self, camera_id: &str) -> Option<f32> {
        let feeds = read(&self.feeds);
        let entry = feeds.get(camera_id)?;
        let times = read(&entry.frame_times);
        if times.len() < 2 {
            return None;
        }
        let first = *times.front()?;
        let last = *times.back()?;
        let span = last.saturating_duration_since(first).as_secs_f32();
        if span <= 0.001 {
            return None;
        }
        Some(((times.len() - 1) as f32 / span * 10.0).round() / 10.0)
    }

    pub fn set_known_people(&self, people: Vec<KnownPerson>) {
        *write(&self.known_people) = people;
    }

    pub fn known_people(&self) -> Vec<KnownPerson> {
        read(&self.known_people).clone()
    }

    pub fn resolve_profile_name(&self, profile_id: &str) -> Option<String> {
        read(&self.known_people)
            .iter()
            .find(|p| p.profile_id == profile_id)
            .map(|p| p.display_name.clone())
    }

    pub fn update_annotation(&self, camera_id: &str, mut annotation: FrameAnnotation) {
        if annotation.camera_id.is_empty() {
            annotation.camera_id = camera_id.to_string();
        }
        if annotation.measured_fps.is_none() {
            annotation.measured_fps = self.measured_fps(camera_id);
        }
        let now = Utc::now();
        if annotation.updated_at.is_none() {
            annotation.updated_at = Some(now);
        }
        let entry = write(&self.feeds)
            .entry(camera_id.to_string())
            .or_insert_with(LiveFeedEntry::new)
            .clone();
        // Keep a just-recognized household member across brief side-profile motion frames within TTL.
        if !annotation.face_detected && annotation.identified_profile.is_none() {
            if let Some(prev) = read(&entry.annotation).as_ref() {
                if prev.identified_profile.is_some()
                    && prev
                        .updated_at
                        .is_some_and(|at| is_fresh(at, now, ANNOTATION_TTL))
                {
                    annotation.identified_profile = prev.identified_profile.clone();
                    annotation.identified_name = prev.identified_name.clone();
                    annotation.face_confidence = prev.face_confidence;
                }
            }
        }
        *write(&entry.annotation) = Some(annotation);
    }

    pub fn get_annotation(&self, camera_id: &str) -> Option<FrameAnnotation> {
        let feeds = read(&self.feeds);
        let entry = feeds.get(camera_id)?;
        let stored = read(&entry.annotation).clone();
        let fps = self.measured_fps(camera_id);
        let now = Utc::now();
        let live_at = read(&entry.latest).as_ref().map(|f| f.at).or(Some(now));
        match stored {
            Some(mut ann)
                if ann
                    .updated_at
                    .is_some_and(|at| is_fresh(at, now, ANNOTATION_TTL)) =>
            {
                if fps.is_some() {
                    ann.measured_fps = fps;
                }
                ann.updated_at = live_at;
                Some(ann)
            }
            _ => Some(FrameAnnotation {
                camera_id: camera_id.to_string(),
                measured_fps: fps,
                updated_at: live_at,
                ..FrameAnnotation::default()
            }),
        }
    }

    pub fn get_annotated_frame(&self, camera_id: &str) -> Option<Vec<u8>> {
        let raw_jpeg = self.get_latest_frame(camera_id)?;
        let ann = self
            .get_annotation(camera_id)
            .unwrap_or_else(|| FrameAnnotation {
                camera_id: camera_id.to_string(),
                measured_fps: self.measured_fps(camera_id),
                updated_at: Some(Utc::now()),
                ..FrameAnnotation::default()
            });
        let people = self.known_people();
        Some(annotate_jpeg(&raw_jpeg, &ann, &people).unwrap_or(raw_jpeg))
    }

    pub fn get_model_annotated_frame(&self, camera_id: &str) -> Option<Vec<u8>> {
        let raw_jpeg = self.get_latest_frame(camera_id)?;
        let ann = self
            .get_annotation(camera_id)
            .unwrap_or_else(|| FrameAnnotation {
                camera_id: camera_id.to_string(),
                measured_fps: self.measured_fps(camera_id),
                updated_at: Some(Utc::now()),
                ..FrameAnnotation::default()
            });
        let people = self.known_people();
        Some(annotate_jpeg_for_model(&raw_jpeg, &ann, &people).unwrap_or(raw_jpeg))
    }

    pub fn get_latest_frame(&self, camera_id: &str) -> Option<Vec<u8>> {
        self.latest(camera_id)
            .map(|(jpeg, _)| jpeg.as_ref().clone())
    }

    /// The newest frame if it is younger than `max_age`.
    pub fn get_fresh_frame(&self, camera_id: &str, max_age: Duration) -> Option<Vec<u8>> {
        let (jpeg, at) = self.latest(camera_id)?;
        is_fresh(at, Utc::now(), max_age).then(|| jpeg.as_ref().clone())
    }

    pub fn last_frame_at(&self, camera_id: &str) -> Option<DateTime<Utc>> {
        self.latest(camera_id).map(|(_, at)| at)
    }

    pub fn is_live(&self, camera_id: &str) -> bool {
        self.last_frame_at(camera_id)
            .is_some_and(|at| is_fresh(at, Utc::now(), LIVE_FRAME_WINDOW))
    }

    fn latest(&self, camera_id: &str) -> Option<(Arc<Vec<u8>>, DateTime<Utc>)> {
        let feeds = read(&self.feeds);
        let entry = feeds.get(camera_id)?;
        let latest = read(&entry.latest);
        latest.as_ref().map(|f| (f.jpeg.clone(), f.at))
    }

    pub fn subscribe(&self, camera_id: &str) -> broadcast::Receiver<Arc<Vec<u8>>> {
        write(&self.feeds)
            .entry(camera_id.to_string())
            .or_insert_with(LiveFeedEntry::new)
            .tx
            .subscribe()
    }

    /// Forget a camera's frames; its open MJPEG streams end.
    pub fn clear_camera(&self, camera_id: &str) {
        write(&self.feeds).remove(camera_id);
    }

    /// Cameras that have produced a frame; a stream opened for an unknown id is not a camera.
    pub fn active_cameras(&self) -> Vec<String> {
        read(&self.feeds)
            .iter()
            .filter(|(_, entry)| read(&entry.latest).is_some())
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn status(&self) -> SupervisorStatus {
        read(&self.status).clone()
    }

    pub fn update_status<F: FnOnce(&mut SupervisorStatus)>(&self, f: F) {
        f(&mut write(&self.status));
    }
}

impl Default for LiveCameraRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn is_fresh(at: DateTime<Utc>, now: DateTime<Utc>, max_age: Duration) -> bool {
    chrono::Duration::from_std(max_age).is_ok_and(|window| now - at <= window)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bus_config_is_derived_from_settings_and_clamped() {
        let s = Settings {
            vision_event_cooldown_secs: 0,
            vision_classifier_confidence: 7.0,
            vision_face_recognition_enabled: false,
            vision_face_on_motion: false,
            vision_event_types: vec![" Person".into(), "person".into()],
            ..Settings::default()
        };
        let cfg = VisionBusConfig::from_settings(&s);
        assert_eq!(cfg.min_event_interval_secs, 1);
        assert_eq!(cfg.classifier_confidence_threshold, 0.99);
        assert!(!cfg.face_detection_enabled);
        assert!(!cfg.face_on_motion);
        assert_eq!(cfg.enabled_event_types, vec!["person"]);

        let registry = LiveCameraRegistry::new();
        registry.sync_settings(&s);
        assert_eq!(registry.bus_config(), cfg);
    }

    #[tokio::test]
    async fn revoking_consent_drops_frames_and_ends_streams() {
        let registry = LiveCameraRegistry::new();
        registry.update_frame("porch", Arc::new(vec![1, 2, 3]));
        let mut rx = registry.subscribe("porch");
        assert!(registry.get_latest_frame("porch").is_some());

        registry.set_cameras_allowed(false);
        assert!(registry.get_latest_frame("porch").is_none());
        assert!(
            matches!(rx.recv().await, Err(broadcast::error::RecvError::Closed)),
            "an open stream must end when consent is revoked"
        );
        tokio::time::timeout(Duration::from_secs(1), registry.cameras_revoked())
            .await
            .expect("revocation resolves at once when consent is already off");

        registry.update_frame("porch", Arc::new(vec![4]));
        assert!(
            registry.get_latest_frame("porch").is_none(),
            "a late frame is not cached without consent"
        );
    }

    #[test]
    fn subscribing_to_an_unknown_camera_does_not_list_it() {
        let registry = LiveCameraRegistry::new();
        let _rx = registry.subscribe("typo");
        assert!(registry.active_cameras().is_empty());
        registry.update_frame("porch", Arc::new(vec![1]));
        assert_eq!(registry.active_cameras(), vec!["porch".to_string()]);
    }

    #[test]
    fn a_camera_is_live_only_with_a_recent_frame() {
        let registry = LiveCameraRegistry::new();
        assert!(!registry.is_live("porch"));
        registry.update_frame("porch", Arc::new(vec![1]));
        assert!(registry.is_live("porch"));
        assert!(registry.last_frame_at("porch").is_some());

        let now = Utc::now();
        assert!(is_fresh(
            now - chrono::Duration::seconds(9),
            now,
            LIVE_FRAME_WINDOW
        ));
        assert!(!is_fresh(
            now - chrono::Duration::seconds(11),
            now,
            LIVE_FRAME_WINDOW
        ));
    }

    #[test]
    fn the_first_frame_marks_a_starting_pipeline_running_and_clears_its_error() {
        let registry = LiveCameraRegistry::new();
        registry.update_status(|s| {
            s.state = VisionState::Reconnecting;
            s.camera_id = Some("porch".into());
            s.last_error = Some("camera stream ended".into());
        });
        registry.update_frame("garage", Arc::new(vec![1]));
        assert_eq!(registry.status().state, VisionState::Reconnecting);

        registry.update_frame("porch", Arc::new(vec![1]));
        let status = registry.status();
        assert_eq!(status.state, VisionState::Running);
        assert_eq!(status.last_error, None);
    }

    #[test]
    fn publishing_an_event_stamps_the_last_event_time() {
        let registry = LiveCameraRegistry::new();
        assert!(registry.last_event_at().is_none());
        let at = Utc::now();
        registry.publish_event(CameraEvent {
            id: Some(1),
            camera_id: "porch".into(),
            event_type: "motion".into(),
            confidence: None,
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: at,
        });
        assert_eq!(registry.last_event_at(), Some(at));
    }
}
