//! Frames → motion → optional classifier → `CameraEvent`, persisted then published exactly as
//! `POST /api/v1/camera/events` does. With a detector that says where things are, people are
//! followed from frame to frame instead: one event per person, and their face matched until known.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use pond_core::shared::domain::session_activity::{PresenceTransition, ProfilePresence};
use pond_core::shared::ports::event_bus::{BusEvent, EventBus};
use pond_core::user_data::domain::face_recognition::{BoundingBox, FaceLandmarks};
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::domain::session::IdentificationSource;
use pond_core::user_data::domain::vision::{CapturedFrame, DetectedObject, EventSignals, Frame};
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::face_recognition::FaceRecognition;
use pond_core::user_data::ports::vision::{FrameSource, VisionClassifier};
use pond_core::user_data::services::face_recognition_slot::FaceRecognitionSlot;

use crate::annotate::FrameAnnotation;
use crate::jpeg_decode::{decode_rgb, LumaDecoder};
use crate::live_feed::LiveCameraRegistry;
use crate::motion::{MotionConfig, MotionDetector};
use crate::tracker::{PersonTracker, Track, TrackChange};

#[derive(Debug, Clone)]
pub struct VisionPipelineConfig {
    /// The `camera_id` on emitted events, which automation rules match on.
    pub camera_id: String,
    pub motion: MotionConfig,
    /// Minimum gap between events; `None` follows the registry's live cooldown setting.
    pub min_event_interval: Option<Duration>,
    /// Save the triggering frame as the event's `snapshot_path` JPEG; `None` writes nothing.
    pub snapshots: Option<crate::snapshot::SnapshotConfig>,
}

impl Default for VisionPipelineConfig {
    fn default() -> Self {
        Self {
            camera_id: "camera-1".to_string(),
            motion: MotionConfig::default(),
            min_event_interval: None,
            snapshots: None,
        }
    }
}

/// How often the detector looks while it is looking: the hardware analysis branch's rate.
const CHECK_EVERY: Duration = Duration::from_millis(1000 / crate::gst::ANALYSIS_FPS as u64);

/// How long the detector keeps looking after the last motion while nobody is in view.
const LOOK_AFTER_MOTION: Duration = Duration::from_secs(5);

/// Change that sets the detector looking, below the motion events' threshold: a person across
/// the room changes a few percent of the grid, and a look costs the GPU about 8 ms.
const LOOK_ON_CHANGE: f64 = 0.02;

/// Face matches tried per person before giving up on them: a back turned stays unknown.
const FACE_TRIES: u32 = 5;

/// The wait between one person's face tries.
const FACE_RETRY: Duration = Duration::from_secs(1);

/// The recognition to match with now, or `None` without one. Logs when matching is skipped
/// because the models are not ready, and again when they arrive, rather than on every frame.
fn ready_face(
    slot: Option<&FaceRecognitionSlot>,
    camera: &str,
    waiting_logged: &mut bool,
) -> Option<Arc<dyn FaceRecognition>> {
    match slot?.current() {
        Ok(face) => {
            if std::mem::take(waiting_logged) {
                tracing::info!(
                    kind = "vision_face_matching_resumed",
                    camera,
                    "face recognition is ready; camera events carry face matches again"
                );
            }
            Some(face)
        }
        Err(why) => {
            if !*waiting_logged {
                *waiting_logged = true;
                tracing::info!(
                    kind = "vision_face_matching_skipped",
                    camera,
                    reason = %why,
                    "face recognition is not ready; camera events carry no face match until it is"
                );
            }
            None
        }
    }
}

/// What one face match found, as events and the live view carry it.
#[derive(Debug, Clone, Default)]
struct FaceOutcome {
    detected: bool,
    unknown: bool,
    profile: Option<String>,
    confidence: Option<f32>,
    bbox: Option<BoundingBox>,
    landmarks: Option<FaceLandmarks>,
}

async fn identify_face(face_rec: &dyn FaceRecognition, jpeg: &[u8], camera: &str) -> FaceOutcome {
    let details = match face_rec.identify_with_diagnostics(jpeg, None).await {
        Ok(details) => details,
        Err(e) => {
            tracing::warn!(camera, error = %e, "face identification failed");
            return FaceOutcome::default();
        }
    };
    let ident = details.identification;
    let mut outcome = FaceOutcome {
        bbox: details.bbox,
        landmarks: details.landmarks,
        ..Default::default()
    };
    if ident.identified {
        tracing::info!(
            camera,
            profile = ?ident.profile_id,
            confidence = ?ident.confidence,
            "face identified in vision event"
        );
        outcome.detected = true;
        outcome.profile = ident.profile_id;
        outcome.confidence = ident.confidence;
    } else if outcome.bbox.is_some() || outcome.landmarks.is_some() || ident.confidence.is_some() {
        tracing::info!(
            camera,
            confidence = ?ident.confidence,
            "unrecognized face detected in vision event"
        );
        outcome.detected = true;
        outcome.unknown = true;
        outcome.confidence = ident.confidence;
    }
    outcome
}

/// The picture's width and height from its header alone.
fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::with_format(std::io::Cursor::new(jpeg), image::ImageFormat::Jpeg)
        .into_dimensions()
        .ok()
}

/// Whether the face found in `jpeg` is `track`'s: inside their box, or they are the only person.
fn face_belongs(face: &FaceOutcome, track: &Track, people: usize, jpeg: &[u8]) -> bool {
    let (Some(bbox), true) = (face.bbox, people > 1) else {
        return true;
    };
    let Some((w, h)) = jpeg_size(jpeg) else {
        return false;
    };
    let cx = (bbox.x as f32 + bbox.width as f32 / 2.0) / w as f32;
    let cy = (bbox.y as f32 + bbox.height as f32 / 2.0) / h as f32;
    track.bbox.contains(cx, cy)
}

/// The picture the detector reads: the hardware's own, else the raw frame, else the JPEG decoded.
fn detector_picture(
    analysis: Option<Frame>,
    raw: Option<&Frame>,
    jpeg: Option<&[u8]>,
    captured_at: DateTime<Utc>,
) -> Option<Frame> {
    analysis
        .or_else(|| raw.cloned())
        .or_else(|| jpeg.and_then(|bytes| decode_rgb(bytes, captured_at).ok()))
}

/// The best label above `floor` among `objects`, the way events name what moved.
fn best_label(objects: &[DetectedObject], floor: f64) -> Option<(String, f64)> {
    objects
        .iter()
        .filter(|o| o.confidence >= floor)
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
        .map(|o| (o.label.clone(), o.confidence))
}

/// Who the detector is following, and when it last looked.
#[derive(Default)]
struct PeopleWatch {
    tracker: PersonTracker,
    last_check: Option<Instant>,
    moved_at: Option<Instant>,
    /// The most that changed since the detector last looked, for the motion event it may name.
    pending_motion: Option<f64>,
    /// The capture sends its own detector pictures at the detector's rate, so wait for those.
    paced_by_capture: bool,
    faces: HashMap<u64, FaceTries>,
}

/// One followed person's face matching so far.
struct FaceTries {
    tries: u32,
    next_at: Instant,
    done: bool,
}

impl PeopleWatch {
    /// A change worth a motion event, which the next look names.
    fn moved(&mut self, changed_fraction: f64, now: Instant) {
        self.moved_at = Some(now);
        self.pending_motion = Some(
            self.pending_motion
                .map_or(changed_fraction, |m| m.max(changed_fraction)),
        );
    }

    /// Look while someone is followed or something moved lately, at the detector's rate.
    fn due(&self, now: Instant, picture_attached: bool) -> bool {
        let looking = self.tracker.following()
            || self
                .moved_at
                .is_some_and(|at| now.duration_since(at) < LOOK_AFTER_MOTION);
        let paced = if self.paced_by_capture {
            picture_attached
        } else {
            self.last_check
                .is_none_or(|at| now.duration_since(at) >= CHECK_EVERY)
        };
        looking && paced
    }

    /// Confirmed people not yet announced, or whose face is still unknown and due another try.
    fn faces_due(&self, now: Instant) -> Vec<u64> {
        self.tracker
            .confirmed()
            .filter(|t| {
                self.faces
                    .get(&t.id)
                    .is_none_or(|f| !f.done && f.tries < FACE_TRIES && now >= f.next_at)
            })
            .map(|t| t.id)
            .collect()
    }
}

/// One event, ready to record.
struct Draft<'a> {
    event_type: String,
    confidence: Option<f64>,
    changed_fraction: Option<f64>,
    face: FaceOutcome,
    jpeg: Option<&'a [u8]>,
    captured_at: DateTime<Utc>,
    classify_ms: u64,
    face_ms: u64,
    track: Option<u64>,
}

/// Annotate the live view, filter, snapshot, persist and publish; whether the event was recorded.
async fn record(
    draft: Draft<'_>,
    storage: &dyn CameraStorage,
    bus: &dyn EventBus,
    registry: &LiveCameraRegistry,
    cfg: &VisionPipelineConfig,
) -> bool {
    let face = draft.face;
    let identified_name = face
        .profile
        .as_deref()
        .and_then(|pid| registry.resolve_profile_name(pid));
    registry.update_annotation(
        &cfg.camera_id,
        FrameAnnotation {
            camera_id: cfg.camera_id.clone(),
            event_type: Some(draft.event_type.clone()),
            event_confidence: draft.confidence,
            changed_fraction: draft.changed_fraction,
            face_detected: face.detected,
            face_unknown: face.unknown,
            identified_profile: face.profile.clone(),
            identified_name: identified_name.clone(),
            face_confidence: face.confidence,
            face_bbox: face.bbox,
            face_landmarks: face.landmarks,
            measured_fps: registry.measured_fps(&cfg.camera_id),
            updated_at: Some(draft.captured_at),
        },
    );

    if !registry.bus_config().passes(EventSignals {
        event_type: &draft.event_type,
        face_detected: face.detected,
        face_recognized: face.profile.is_some(),
        face_unknown: face.unknown,
    }) {
        return false;
    }

    // After the filter, so a dropped event writes nothing and prunes no kept snapshot.
    // Best-effort: a failed snapshot write must never suppress the event.
    let snapshot_path = cfg.snapshots.as_ref().and_then(|snap_cfg| {
        let bytes = draft.jpeg?;
        match crate::snapshot::write_snapshot_jpeg(
            snap_cfg,
            &cfg.camera_id,
            draft.captured_at,
            bytes,
        ) {
            Ok(path) => Some(path.to_string_lossy().into_owned()),
            Err(e) => {
                tracing::warn!(camera = %cfg.camera_id, error = %e, "snapshot write failed");
                None
            }
        }
    });

    let mut metadata = serde_json::json!({
        "source": "vision",
        "face_detected": face.detected,
        "face_unknown": face.unknown,
    });
    if let Some(fraction) = draft.changed_fraction {
        metadata["changed_fraction"] = serde_json::json!(fraction);
    }
    if let Some(track) = draft.track {
        metadata["track_id"] = serde_json::json!(track);
    }
    if let Some(ref pid) = face.profile {
        metadata["identified_profile"] = serde_json::json!(pid);
    }
    if let Some(ref name) = identified_name {
        metadata["identified_name"] = serde_json::json!(name);
    }
    if let Some(conf) = face.confidence {
        metadata["face_confidence"] = serde_json::json!(conf);
    }
    if let Some(bbox) = face.bbox {
        metadata["face_bbox"] = serde_json::json!({
            "x": bbox.x,
            "y": bbox.y,
            "width": bbox.width,
            "height": bbox.height,
        });
    }
    if let Some(ref lm) = face.landmarks {
        metadata["face_landmarks"] = serde_json::json!({
            "left_eye": lm.left_eye,
            "right_eye": lm.right_eye,
            "nose": lm.nose,
            "left_mouth": lm.left_mouth,
            "right_mouth": lm.right_mouth,
        });
    }

    let mut event = CameraEvent {
        id: None,
        camera_id: cfg.camera_id.clone(),
        event_type: draft.event_type,
        confidence: draft.confidence,
        snapshot_path,
        metadata: Some(metadata.to_string()),
        acknowledged: false,
        created_at: Utc::now(),
    };

    // Persist first, then publish — same contract as the camera route.
    match storage.record_event(event.clone()).await {
        Ok(id) => {
            event.id = Some(id);
            // From the frame's arrival to now: what a household waits for an event.
            let latency_ms = (Utc::now() - draft.captured_at).num_milliseconds();
            tracing::info!(
                camera = %cfg.camera_id,
                event_type = %event.event_type,
                changed_fraction = ?draft.changed_fraction,
                face_detected = face.detected,
                track = ?draft.track,
                latency_ms,
                classify_ms = draft.classify_ms,
                face_ms = draft.face_ms,
                "vision event detected"
            );
            registry.publish_event(event.clone());
            bus.publish(BusEvent::Camera(event));
            if let Some(profile_id) = face.profile {
                bus.publish(BusEvent::Presence(ProfilePresence {
                    profile_id,
                    transition: PresenceTransition::Arrived,
                    source: IdentificationSource::Face,
                    confidence: face.confidence,
                    session_id: format!("camera:{}", cfg.camera_id),
                    at: Utc::now(),
                }));
            }
            true
        }
        Err(e) => {
            tracing::warn!(camera = %cfg.camera_id, error = %e, "failed to persist camera event");
            false
        }
    }
}

/// Run until the frame source ends (`Ok`) or fails (`Err`); spawn once per camera.
pub async fn run_vision_pipeline(
    source: &mut dyn FrameSource,
    classifier: Option<Arc<dyn VisionClassifier>>,
    face_recognizer: Option<Arc<FaceRecognitionSlot>>,
    storage: Arc<dyn CameraStorage>,
    bus: Arc<dyn EventBus>,
    registry: &LiveCameraRegistry,
    cfg: VisionPipelineConfig,
) -> anyhow::Result<()> {
    let mut detector = MotionDetector::new(cfg.motion.clone());
    let mut luma_decoder = LumaDecoder::default();
    let mut last_event = last_recorded_event(storage.as_ref(), &cfg.camera_id).await;
    let mut last_luma_decode: Option<Instant> = None;
    let locator = classifier.clone().filter(|c| c.locates());
    let mut people = PeopleWatch::default();
    tracing::info!(
        camera = %cfg.camera_id,
        follows_people = locator.is_some(),
        "vision pipeline started"
    );
    // Recognition may still be downloading; said once when matching stops and once when it starts.
    let mut face_waiting_logged = false;

    let ended = loop {
        let captured = match source.next_frame().await {
            Ok(Some(f)) => f,
            Ok(None) => break Ok(()),
            Err(e) => {
                tracing::warn!(camera = %cfg.camera_id, error = %e, "frame read failed; stopping");
                break Err(e);
            }
        };
        let captured_at = captured.captured_at();

        // One JPEG per frame, shared by the live feed, snapshots and face recognition.
        let (jpeg, raw, luma, analysis) = match captured {
            CapturedFrame::Raw(frame) => match crate::snapshot::frame_to_jpeg_bytes(&frame) {
                Ok(bytes) => (Some(Arc::new(bytes)), Some(frame), None, None),
                Err(e) => {
                    tracing::debug!(camera = %cfg.camera_id, error = %e, "frame could not be encoded");
                    (None, Some(frame), None, None)
                }
            },
            CapturedFrame::Jpeg(j) => (Some(j.jpeg), None, j.luma, j.analysis),
        };
        if let Some(bytes) = &jpeg {
            registry.update_frame(&cfg.camera_id, bytes.clone());
        }

        let bus_cfg = registry.bus_config();
        let min_interval = cfg
            .min_event_interval
            .unwrap_or(Duration::from_secs(bus_cfg.min_event_interval_secs));
        let in_cooldown = last_event.is_some_and(|at| at.elapsed() < min_interval);

        // During cooldown at high FPS, throttle CPU JPEG luma decoding to ~5 Hz.
        if in_cooldown
            && raw.is_none()
            && luma.is_none()
            && last_luma_decode.is_some_and(|t| t.elapsed() < Duration::from_millis(180))
        {
            continue;
        }

        let observed = match (&raw, &luma, &jpeg) {
            (Some(frame), _, _) => detector.observe(frame),
            (None, Some(plane), _) => detector.observe_luma(plane, captured_at),
            (None, None, Some(bytes)) => {
                last_luma_decode = Some(Instant::now());
                match luma_decoder.decode(bytes) {
                    Ok(plane) => detector.observe_luma(plane, captured_at),
                    Err(e) => {
                        tracing::debug!(camera = %cfg.camera_id, error = %e, "camera jpeg could not be decoded");
                        None
                    }
                }
            }
            (None, None, None) => None,
        };
        let min_classifier_conf = bus_cfg.classifier_confidence_threshold;
        let jpeg_bytes = jpeg.as_deref().map(Vec::as_slice);
        let motion_fallback = |fraction: f64| ("motion".to_string(), Some(fraction.min(1.0)));

        let (mut event_type, confidence, changed_fraction, classify_ms) = match &locator {
            // People are followed while anything moves or anyone is in view; what else moved is
            // named by the same look.
            Some(locator) => {
                let now = Instant::now();
                if let Some(fraction) = observed {
                    people.moved(fraction, now);
                } else if detector
                    .last_change()
                    .is_some_and(|f| f >= LOOK_ON_CHANGE.min(cfg.motion.changed_fraction))
                {
                    people.moved_at = Some(now);
                }
                people.paced_by_capture |= analysis.is_some();
                if !people.due(now, analysis.is_some()) {
                    continue;
                }
                let Some(picture) =
                    detector_picture(analysis, raw.as_ref(), jpeg_bytes, captured_at)
                else {
                    continue;
                };
                people.last_check = Some(now);
                let detect_started = Instant::now();
                let objects = match locator.detect(&picture).await {
                    Ok(objects) => Some(objects),
                    Err(e) => {
                        tracing::warn!(camera = %cfg.camera_id, error = %e, "detector failed");
                        None
                    }
                };
                let detect_ms = detect_started.elapsed().as_millis() as u64;

                if let Some(objects) = &objects {
                    let persons: Vec<_> = objects
                        .iter()
                        .filter(|o| o.label == "person" && o.confidence >= min_classifier_conf)
                        .map(|o| (o.bbox, o.confidence))
                        .collect();
                    for change in people.tracker.update(&persons, now) {
                        match change {
                            TrackChange::Confirmed(id) => {
                                tracing::info!(camera = %cfg.camera_id, track = id, detect_ms, "person in view");
                            }
                            TrackChange::Lost(id) => {
                                people.faces.remove(&id);
                                tracing::info!(camera = %cfg.camera_id, track = id, "person out of view");
                            }
                        }
                    }
                }

                // Each person: one event when they appear, another once their face is known.
                let due = people.faces_due(now);
                let face_rec = (!due.is_empty() && bus_cfg.face_detection_enabled)
                    .then(|| {
                        ready_face(
                            face_recognizer.as_deref(),
                            &cfg.camera_id,
                            &mut face_waiting_logged,
                        )
                    })
                    .flatten();
                let in_view = people.tracker.confirmed().count();
                // Recognition finds the frame's one best face, so it runs once a look and the
                // face goes to the person whose box holds it.
                let mut face_this_look: Option<FaceOutcome> = None;
                for id in due {
                    let Some(track) = people.tracker.get(id).cloned() else {
                        continue;
                    };
                    let face_started = Instant::now();
                    let face = match (&face_rec, jpeg_bytes) {
                        (Some(rec), Some(bytes)) => {
                            let found = match &face_this_look {
                                Some(found) => found.clone(),
                                None => {
                                    let found =
                                        identify_face(rec.as_ref(), bytes, &cfg.camera_id).await;
                                    face_this_look.insert(found).clone()
                                }
                            };
                            if face_belongs(&found, &track, in_view, bytes) {
                                found
                            } else {
                                FaceOutcome::default()
                            }
                        }
                        _ => FaceOutcome::default(),
                    };
                    let face_ms = face_started.elapsed().as_millis() as u64;
                    let first = !people.faces.contains_key(&id);
                    let known = face.profile.is_some();
                    let tries = people.faces.entry(id).or_insert(FaceTries {
                        tries: 0,
                        next_at: now,
                        done: false,
                    });
                    tries.tries += 1;
                    tries.next_at = now + FACE_RETRY;
                    tries.done |= known || !bus_cfg.face_detection_enabled || jpeg_bytes.is_none();
                    if !(first || known) {
                        continue;
                    }
                    let draft = Draft {
                        event_type: "person".to_string(),
                        confidence: Some(track.confidence),
                        changed_fraction: observed,
                        face,
                        jpeg: jpeg_bytes,
                        captured_at,
                        classify_ms: detect_ms,
                        face_ms,
                        track: Some(id),
                    };
                    if record(draft, storage.as_ref(), bus.as_ref(), registry, &cfg).await {
                        last_event = Some(Instant::now());
                    }
                }

                // While anyone is followed their events speak for whatever moved.
                let moved = people.pending_motion.take();
                if people.tracker.following() || in_cooldown {
                    continue;
                }
                let Some(changed_fraction) = moved else {
                    continue;
                };
                let (label, confidence) = objects
                    .as_deref()
                    .and_then(|o| best_label(o, min_classifier_conf))
                    .map(|(label, conf)| (label, Some(conf)))
                    .unwrap_or_else(|| motion_fallback(changed_fraction));
                (label, confidence, changed_fraction, detect_ms)
            }
            None => {
                let Some(changed_fraction) = observed else {
                    continue;
                };
                if in_cooldown {
                    continue;
                }

                // Optional enrichment: let the on-device model name what moved.
                let decoded: Option<Frame> = match (&classifier, &raw, jpeg_bytes) {
                    (Some(_), None, Some(bytes)) => decode_rgb(bytes, captured_at)
                        .map_err(|e| {
                            tracing::warn!(camera = %cfg.camera_id, error = %e, "candidate frame could not be decoded for the classifier");
                        })
                        .ok(),
                    _ => None,
                };
                let classify_started = Instant::now();
                let (event_type, confidence) = match (
                    &classifier,
                    raw.as_ref().or(decoded.as_ref()),
                ) {
                    (Some(c), Some(frame)) => match c.classify(frame).await {
                        Ok(dets) => dets
                            .into_iter()
                            .filter(|d| d.confidence >= min_classifier_conf)
                            .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
                            .map(|d| (d.label, Some(d.confidence)))
                            .unwrap_or_else(|| motion_fallback(changed_fraction)),
                        Err(e) => {
                            tracing::warn!(camera = %cfg.camera_id, error = %e, "classifier failed; emitting motion");
                            motion_fallback(changed_fraction)
                        }
                    },
                    _ => motion_fallback(changed_fraction),
                };
                let classify_ms = classify_started.elapsed().as_millis() as u64;
                (event_type, confidence, changed_fraction, classify_ms)
            }
        };

        // Face detection and recognition enrichment on person or motion events.
        let face_started = Instant::now();
        let mut face = FaceOutcome::default();
        let should_check_face = bus_cfg.face_detection_enabled
            && (event_type == "person" || (bus_cfg.face_on_motion && event_type == "motion"));
        if should_check_face {
            if let Some(face_rec) = ready_face(
                face_recognizer.as_deref(),
                &cfg.camera_id,
                &mut face_waiting_logged,
            ) {
                match jpeg_bytes {
                    Some(bytes) => {
                        face = identify_face(face_rec.as_ref(), bytes, &cfg.camera_id).await;
                        if face.detected && event_type == "motion" {
                            event_type = "person".to_string();
                        }
                    }
                    None => {
                        tracing::warn!(camera = %cfg.camera_id, "no encoded frame for face recognition");
                    }
                }
            }
        }
        let face_ms = face_started.elapsed().as_millis() as u64;

        let draft = Draft {
            event_type,
            confidence,
            changed_fraction: Some(changed_fraction),
            face,
            jpeg: jpeg_bytes,
            captured_at,
            classify_ms,
            face_ms,
            track: None,
        };
        if record(draft, storage.as_ref(), bus.as_ref(), registry, &cfg).await {
            last_event = Some(Instant::now());
        }
    };
    tracing::info!(camera = %cfg.camera_id, "vision pipeline stopped");
    ended
}

/// When this camera's newest stored event happened, so a restart keeps the cooldown it started.
async fn last_recorded_event(storage: &dyn CameraStorage, camera_id: &str) -> Option<Instant> {
    let events = storage.list_events(camera_id, 1).await.ok()?;
    let ago = (Utc::now() - events.first()?.created_at).to_std().ok()?;
    Instant::now().checked_sub(ago)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use futures_lite_shim::collect_one_camera_event;
    use pond_core::shared::services::in_process_event_bus::InProcessEventBus;
    use pond_core::user_data::domain::vision::{Detection, JpegFrame};
    use pond_core::user_data::mocks::mock_sensor::MockCameraStorage;
    use std::collections::VecDeque;

    /// Deterministic frame source for tests.
    struct ScriptedFrameSource(VecDeque<CapturedFrame>);

    impl ScriptedFrameSource {
        fn raw(frames: VecDeque<Frame>) -> Self {
            Self(frames.into_iter().map(CapturedFrame::from).collect())
        }

        /// The same pictures as the camera's own JPEGs, with no decoded luma.
        fn jpeg(frames: VecDeque<Frame>) -> Self {
            Self(
                frames
                    .into_iter()
                    .map(|f| {
                        CapturedFrame::Jpeg(JpegFrame {
                            jpeg: Arc::new(crate::snapshot::frame_to_jpeg_bytes(&f).unwrap()),
                            luma: None,
                            analysis: None,
                            captured_at: f.captured_at,
                        })
                    })
                    .collect(),
            )
        }
    }

    #[async_trait]
    impl FrameSource for ScriptedFrameSource {
        async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
            Ok(self.0.pop_front())
        }
    }

    /// Always sees a pet (above the confidence floor).
    struct PetClassifier;
    #[async_trait]
    impl VisionClassifier for PetClassifier {
        async fn classify(&self, _frame: &Frame) -> Result<Vec<Detection>> {
            Ok(vec![Detection {
                label: "pet".into(),
                confidence: 0.9,
            }])
        }
    }

    fn flat(luma: u8) -> Frame {
        Frame {
            width: 64,
            height: 36,
            rgb: vec![luma; Frame::expected_len(64, 36)],
            captured_at: Utc::now(),
        }
    }

    fn with_square(background: u8) -> Frame {
        let mut f = flat(background);
        for y in 0..18u32 {
            for x in 0..32u32 {
                let i = ((y * 64 + x) * 3) as usize;
                f.rgb[i] = 255;
                f.rgb[i + 1] = 255;
                f.rgb[i + 2] = 255;
            }
        }
        f
    }

    fn cfg() -> VisionPipelineConfig {
        VisionPipelineConfig {
            camera_id: "backyard-cam".into(),
            motion: MotionConfig::default(),
            min_event_interval: Some(Duration::from_secs(60)),
            snapshots: None,
        }
    }

    // Small helper module so the bus subscription read is bounded.
    mod futures_lite_shim {
        use super::*;
        use futures::StreamExt;
        use pond_core::shared::ports::event_bus::BusStream;

        pub async fn collect_one_camera_event(mut stream: BusStream) -> Option<CameraEvent> {
            match tokio::time::timeout(Duration::from_secs(2), stream.next()).await {
                Ok(Some(BusEvent::Camera(e))) => Some(e),
                _ => None,
            }
        }
    }

    #[tokio::test]
    async fn frame_motion_persists_publishes_and_matches_a_rule() {
        use pond_core::user_data::domain::schedule::{
            SensorTriggerSpec, TriggerAction, TriggerCondition, TriggerSource, TriggerSourceKind,
        };

        // Still, still, square appears (motion), square gone within the rate limit (suppressed).
        let frames = VecDeque::from(vec![flat(20), flat(20), with_square(20), flat(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());
        let subscription = bus.subscribe();

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            None,
            None,
            storage.clone(),
            bus.clone(),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        // Persisted exactly once (rate limit swallowed the second change).
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1, "one motion event persisted");
        assert_eq!(stored[0].event_type, "motion");
        assert!(stored[0].confidence.unwrap() > 0.0);

        // Published on the bus…
        let event = collect_one_camera_event(subscription)
            .await
            .expect("camera event on the bus");
        assert_eq!(event.camera_id, "backyard-cam");

        // …and an automation rule matches it.
        let rule = SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Camera,
                device_id: Some("backyard-cam".into()),
                signal: Some("motion".into()),
            },
            condition: TriggerCondition::default(),
            actions: vec![TriggerAction::Notify {
                title: "Motion".into(),
                body: "Backyard camera motion".into(),
            }],
            cooldown_secs: 60,
        };
        let bus_event = BusEvent::Camera(event);
        let view = bus_event
            .trigger_view()
            .expect("a camera event is device-shaped");
        assert!(
            rule.matches(&view, chrono::NaiveTime::from_hms_opt(20, 0, 0).unwrap()),
            "the automation rule must match the on-device detection"
        );
    }

    #[tokio::test]
    async fn classifier_enriches_motion_into_labelled_event() {
        let frames = VecDeque::from(vec![flat(20), with_square(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            Some(Arc::new(PetClassifier)),
            None,
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].event_type, "pet", "classifier label used");
        assert_eq!(stored[0].confidence, Some(0.9));
    }

    #[tokio::test]
    async fn motion_event_carries_a_snapshot_of_the_triggering_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let frames = VecDeque::from(vec![flat(20), with_square(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());
        let mut cfg = cfg();
        cfg.snapshots = Some(crate::snapshot::SnapshotConfig::new(
            tmp.path().to_path_buf(),
        ));

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            None,
            None,
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg,
        )
        .await
        .unwrap();

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        let path = stored[0]
            .snapshot_path
            .as_deref()
            .expect("event must carry a snapshot path");
        assert!(
            std::path::Path::new(path).exists(),
            "snapshot file missing: {path}"
        );
    }

    #[tokio::test]
    async fn still_frames_emit_nothing() {
        let frames = VecDeque::from(vec![flat(20), flat(20), flat(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            None,
            None,
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        assert!(storage
            .list_events("backyard-cam", 10)
            .await
            .unwrap()
            .is_empty());
    }

    struct PersonClassifier;

    #[async_trait]
    impl VisionClassifier for PersonClassifier {
        async fn classify(&self, _frame: &Frame) -> Result<Vec<Detection>> {
            Ok(vec![Detection {
                label: "person".into(),
                confidence: 0.95,
            }])
        }
    }

    struct MockPersonFaceRecognition {
        profile_id: String,
        confidence: f32,
    }

    #[async_trait]
    impl FaceRecognition for MockPersonFaceRecognition {
        async fn register_face(
            &self,
            _profile_id: &str,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceEmbedding> {
            unimplemented!()
        }

        async fn identify_face(
            &self,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceIdentification> {
            Ok(
                pond_core::user_data::domain::face_recognition::FaceIdentification::found(
                    self.profile_id.clone(),
                    self.confidence,
                ),
            )
        }

        async fn list_embeddings(
            &self,
            _profile_id: &str,
        ) -> Result<Vec<pond_core::user_data::domain::face_recognition::FaceEmbedding>> {
            Ok(vec![])
        }

        async fn delete_embeddings(&self, _profile_id: &str) -> Result<u64> {
            Ok(0)
        }

        async fn pairwise_similarities(
            &self,
        ) -> Result<Vec<pond_core::user_data::ports::face_recognition::PairwiseSimilarity>>
        {
            Ok(vec![])
        }

        fn match_threshold(&self) -> f32 {
            0.7
        }
    }

    #[tokio::test]
    async fn a_person_seen_while_face_models_download_is_kept_without_a_match() {
        let storage = Arc::new(MockCameraStorage::new());
        let slot = Arc::new(FaceRecognitionSlot::downloading());

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(VecDeque::from(vec![flat(20), with_square(20)])),
            Some(Arc::new(PersonClassifier)),
            Some(slot.clone()),
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(
            stored.len(),
            1,
            "the event is not lost for want of a face model"
        );
        assert_eq!(stored[0].event_type, "person");
        let meta: serde_json::Value =
            serde_json::from_str(stored[0].metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["face_detected"], false);
        assert!(meta.get("identified_profile").is_none());

        // The models land: the same slot, handed to the next run, now matches.
        slot.install(Arc::new(MockPersonFaceRecognition {
            profile_id: "jerry".to_string(),
            confidence: 0.88,
        }));
        let storage = Arc::new(MockCameraStorage::new());
        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(VecDeque::from(vec![flat(20), with_square(20)])),
            Some(Arc::new(PersonClassifier)),
            Some(slot),
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        let meta: serde_json::Value =
            serde_json::from_str(stored[0].metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["identified_profile"], "jerry");
    }

    #[tokio::test]
    async fn person_classifier_identifies_face_and_emits_presence() {
        use futures::StreamExt;

        let frames = VecDeque::from(vec![flat(20), with_square(20)]);
        let storage = Arc::new(MockCameraStorage::new());
        let bus = Arc::new(InProcessEventBus::new());
        let mut subscription = bus.subscribe();

        let face_rec = Arc::new(MockPersonFaceRecognition {
            profile_id: "jerry".to_string(),
            confidence: 0.88,
        });

        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            Some(Arc::new(PersonClassifier)),
            Some(Arc::new(FaceRecognitionSlot::ready(face_rec))),
            storage.clone(),
            bus,
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();

        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].event_type, "person");
        let meta: serde_json::Value =
            serde_json::from_str(stored[0].metadata.as_ref().unwrap()).unwrap();
        assert_eq!(meta["identified_profile"], "jerry");
        assert!((meta["face_confidence"].as_f64().unwrap() - 0.88).abs() < 1e-4);

        let mut got_camera = false;
        let mut got_presence = false;
        while let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(500), subscription.next()).await
        {
            match ev {
                BusEvent::Camera(c) => {
                    assert_eq!(c.event_type, "person");
                    got_camera = true;
                }
                BusEvent::Presence(p) => {
                    assert_eq!(p.profile_id, "jerry");
                    assert_eq!(p.transition, PresenceTransition::Arrived);
                    assert_eq!(p.source, IdentificationSource::Face);
                    assert_eq!(p.confidence, Some(0.88));
                    got_presence = true;
                }
                _ => {}
            }
            if got_camera && got_presence {
                break;
            }
        }
        assert!(got_camera, "Camera event was not published");
        assert!(got_presence, "Presence event was not published");
    }

    /// A face recogniser that finds `profile_id`, or an unknown face when it is `None`.
    struct FixedFace(Option<&'static str>);

    #[async_trait]
    impl FaceRecognition for FixedFace {
        async fn register_face(
            &self,
            _profile_id: &str,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceEmbedding> {
            unimplemented!()
        }

        async fn identify_face(
            &self,
            _image_bytes: &[u8],
            _bbox: Option<pond_core::user_data::domain::face_recognition::BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceIdentification> {
            use pond_core::user_data::domain::face_recognition::FaceIdentification;
            Ok(match self.0 {
                Some(id) => FaceIdentification::found(id.to_string(), 0.9),
                None => FaceIdentification {
                    identified: false,
                    profile_id: None,
                    confidence: Some(0.2),
                },
            })
        }

        async fn list_embeddings(
            &self,
            _profile_id: &str,
        ) -> Result<Vec<pond_core::user_data::domain::face_recognition::FaceEmbedding>> {
            Ok(vec![])
        }

        async fn delete_embeddings(&self, _profile_id: &str) -> Result<u64> {
            Ok(0)
        }

        async fn pairwise_similarities(
            &self,
        ) -> Result<Vec<pond_core::user_data::ports::face_recognition::PairwiseSimilarity>>
        {
            Ok(vec![])
        }

        fn match_threshold(&self) -> f32 {
            0.7
        }
    }

    fn registry_with(types: &[&str], cooldown_secs: u64) -> LiveCameraRegistry {
        let registry = LiveCameraRegistry::new();
        registry.sync_settings(&pond_core::user_data::domain::settings::Settings {
            vision_event_types: types.iter().map(|t| t.to_string()).collect(),
            vision_event_cooldown_secs: cooldown_secs,
            ..Default::default()
        });
        registry
    }

    /// Two motion detections: the square appears, then goes.
    fn two_motions() -> VecDeque<Frame> {
        VecDeque::from(vec![flat(20), with_square(20), flat(20)])
    }

    async fn run(
        frames: VecDeque<Frame>,
        classifier: Option<Arc<dyn VisionClassifier>>,
        face: Option<Arc<dyn FaceRecognition>>,
        registry: &LiveCameraRegistry,
        cfg: VisionPipelineConfig,
    ) -> Vec<CameraEvent> {
        let storage = Arc::new(MockCameraStorage::new());
        run_vision_pipeline(
            &mut ScriptedFrameSource::raw(frames),
            classifier,
            face.map(|f| Arc::new(FaceRecognitionSlot::ready(f))),
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            registry,
            cfg.clone(),
        )
        .await
        .unwrap();
        storage.list_events(&cfg.camera_id, 10).await.unwrap()
    }

    #[tokio::test]
    async fn without_an_override_the_cooldown_is_the_live_setting() {
        let mut cfg = cfg();
        cfg.min_event_interval = None;

        let long = run(
            two_motions(),
            None,
            None,
            &registry_with(&[], 3600),
            cfg.clone(),
        )
        .await;
        assert_eq!(
            long.len(),
            1,
            "a one-hour cooldown swallows the second motion"
        );

        cfg.min_event_interval = Some(Duration::ZERO);
        let overridden = run(two_motions(), None, None, &registry_with(&[], 3600), cfg).await;
        assert_eq!(
            overridden.len(),
            2,
            "an explicit interval wins over the setting, whatever its value"
        );
    }

    #[tokio::test]
    async fn the_default_interval_is_ten_seconds_from_the_setting_not_a_sentinel() {
        // The default config has no override; the setting's default is the ten seconds.
        assert_eq!(VisionPipelineConfig::default().min_event_interval, None);
        let cfg = VisionPipelineConfig {
            camera_id: "backyard-cam".into(),
            min_event_interval: Some(Duration::from_secs(10)),
            ..VisionPipelineConfig::default()
        };
        let events = run(two_motions(), None, None, &registry_with(&[], 1), cfg).await;
        assert_eq!(
            events.len(),
            1,
            "an explicit 10 s is honoured, not read as unset"
        );
    }

    #[tokio::test]
    async fn enabling_only_face_recognized_keeps_a_recognised_person() {
        let registry = registry_with(&["face_recognized"], 10);
        let events = run(
            two_motions(),
            Some(Arc::new(PersonClassifier)),
            Some(Arc::new(FixedFace(Some("jerry")))),
            &registry,
            cfg(),
        )
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "person");
        let meta: serde_json::Value =
            serde_json::from_str(events[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["identified_profile"], "jerry");
    }

    #[tokio::test]
    async fn enabling_only_face_unknown_keeps_a_stranger_and_drops_a_household_member() {
        let registry = registry_with(&["face_unknown"], 10);
        let stranger = run(
            two_motions(),
            Some(Arc::new(PersonClassifier)),
            Some(Arc::new(FixedFace(None))),
            &registry,
            cfg(),
        )
        .await;
        assert_eq!(stranger.len(), 1);
        let meta: serde_json::Value =
            serde_json::from_str(stranger[0].metadata.as_deref().unwrap()).unwrap();
        assert_eq!(meta["face_unknown"], true);

        let member = run(
            two_motions(),
            Some(Arc::new(PersonClassifier)),
            Some(Arc::new(FixedFace(Some("jerry")))),
            &registry,
            cfg(),
        )
        .await;
        assert!(member.is_empty(), "a recognised face is not face_unknown");
    }

    #[tokio::test]
    async fn a_filtered_out_event_writes_no_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = cfg();
        cfg.snapshots = Some(crate::snapshot::SnapshotConfig::new(
            tmp.path().to_path_buf(),
        ));
        let events = run(
            two_motions(),
            Some(Arc::new(PetClassifier)),
            None,
            &registry_with(&["person"], 10),
            cfg,
        )
        .await;
        assert!(events.is_empty(), "pet is not an enabled type");
        let written = std::fs::read_dir(tmp.path())
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(
            written, 0,
            "a dropped event must not write or prune snapshots"
        );
    }

    #[tokio::test]
    async fn the_classifier_floor_follows_the_live_setting() {
        // PetClassifier answers 0.9: a 0.95 floor turns it back into plain motion.
        let registry = LiveCameraRegistry::new();
        registry.sync_settings(&pond_core::user_data::domain::settings::Settings {
            vision_classifier_confidence: 0.95,
            ..Default::default()
        });
        let events = run(
            VecDeque::from(vec![flat(20), with_square(20)]),
            Some(Arc::new(PetClassifier)),
            None,
            &registry,
            cfg(),
        )
        .await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "motion");
    }

    /// Records what the classifier was shown.
    struct SeenClassifier(std::sync::Mutex<Vec<(u32, u32)>>);

    #[async_trait]
    impl VisionClassifier for SeenClassifier {
        async fn classify(&self, frame: &Frame) -> Result<Vec<Detection>> {
            self.0.lock().unwrap().push((frame.width, frame.height));
            Ok(vec![Detection {
                label: "pet".into(),
                confidence: 0.9,
            }])
        }
    }

    #[tokio::test]
    async fn camera_jpegs_feed_the_live_view_and_snapshots_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg = cfg();
        cfg.snapshots = Some(crate::snapshot::SnapshotConfig::new(
            tmp.path().to_path_buf(),
        ));
        let mut source = ScriptedFrameSource::jpeg(VecDeque::from(vec![flat(20), with_square(20)]));
        let CapturedFrame::Jpeg(last) = source.0.back().unwrap().clone() else {
            unreachable!()
        };
        let classifier = Arc::new(SeenClassifier(Default::default()));
        let storage = Arc::new(MockCameraStorage::new());
        let registry = LiveCameraRegistry::new();

        run_vision_pipeline(
            &mut source,
            Some(classifier.clone()),
            None,
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &registry,
            cfg,
        )
        .await
        .unwrap();

        assert_eq!(
            registry.get_latest_frame("backyard-cam").as_deref(),
            Some(last.jpeg.as_slice()),
            "the live view carries the camera's bytes, not a re-encode"
        );
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1, "motion found by decoding the jpeg");
        assert_eq!(stored[0].event_type, "pet");
        let snapshot = std::fs::read(stored[0].snapshot_path.as_deref().unwrap()).unwrap();
        assert_eq!(
            snapshot, *last.jpeg,
            "the snapshot is the camera's own jpeg"
        );
        assert_eq!(
            *classifier.0.lock().unwrap(),
            vec![(64, 36)],
            "only the one candidate is decoded, at full size"
        );
    }

    #[tokio::test]
    async fn a_decoded_luma_picture_is_used_instead_of_decoding_the_jpeg() {
        // The JPEG is garbage: only the luma that came with it can show the motion.
        let luma = |v: u8, square: bool| {
            let mut pixels = vec![v; 64 * 36];
            if square {
                for y in 0..18 {
                    pixels[y * 64..y * 64 + 32].fill(255);
                }
            }
            pond_core::user_data::domain::vision::LumaPlane {
                width: 64,
                height: 36,
                pixels,
            }
        };
        let frames = [luma(20, false), luma(20, true)]
            .into_iter()
            .map(|l| {
                CapturedFrame::Jpeg(JpegFrame {
                    jpeg: Arc::new(b"not a jpeg".to_vec()),
                    luma: Some(l),
                    analysis: None,
                    captured_at: Utc::now(),
                })
            })
            .collect();
        let storage = Arc::new(MockCameraStorage::new());
        run_vision_pipeline(
            &mut ScriptedFrameSource(frames),
            None,
            None,
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await
        .unwrap();
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(
            stored[0].event_type, "motion",
            "an undecodable candidate stays motion"
        );
    }

    #[tokio::test]
    async fn a_restarted_pipeline_keeps_the_cooldown_the_last_event_started() {
        let storage = Arc::new(MockCameraStorage::new());
        for _ in 0..2 {
            run_vision_pipeline(
                &mut ScriptedFrameSource::raw(VecDeque::from(vec![flat(20), with_square(20)])),
                None,
                None,
                storage.clone(),
                Arc::new(InProcessEventBus::new()),
                &LiveCameraRegistry::new(),
                cfg(),
            )
            .await
            .unwrap();
        }
        let stored = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(
            stored.len(),
            1,
            "the second run starts inside the first event's cooldown"
        );
    }

    #[tokio::test]
    async fn a_failed_frame_read_is_reported_as_the_reason_the_pipeline_ended() {
        struct Broken;
        #[async_trait]
        impl FrameSource for Broken {
            async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
                anyhow::bail!("ffmpeg exited")
            }
        }
        let ended = run_vision_pipeline(
            &mut Broken,
            None,
            None,
            Arc::new(MockCameraStorage::new()),
            Arc::new(InProcessEventBus::new()),
            &LiveCameraRegistry::new(),
            cfg(),
        )
        .await;
        assert!(ended.unwrap_err().to_string().contains("ffmpeg exited"));
    }

    /// Finds the same objects in every picture it is shown, and keeps each picture's size.
    struct Locator {
        objects: Vec<DetectedObject>,
        pictures: std::sync::Mutex<Vec<(u32, u32)>>,
        classified: std::sync::atomic::AtomicUsize,
    }

    impl Locator {
        fn seeing(objects: &[(&str, f32)]) -> Arc<Self> {
            let objects = objects
                .iter()
                .map(|&(label, x)| DetectedObject {
                    label: label.into(),
                    confidence: 0.9,
                    bbox: pond_core::user_data::domain::vision::FractionBox {
                        x,
                        y: 0.2,
                        w: 0.2,
                        h: 0.6,
                    },
                })
                .collect();
            Arc::new(Self {
                objects,
                pictures: Default::default(),
                classified: Default::default(),
            })
        }
    }

    #[async_trait]
    impl VisionClassifier for Locator {
        async fn classify(&self, _frame: &Frame) -> Result<Vec<Detection>> {
            self.classified
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Vec::new())
        }

        fn locates(&self) -> bool {
            true
        }

        async fn detect(&self, frame: &Frame) -> Result<Vec<DetectedObject>> {
            self.pictures
                .lock()
                .unwrap()
                .push((frame.width, frame.height));
            Ok(self.objects.clone())
        }
    }

    /// The detector's own picture, smaller than the camera's so a test can tell them apart.
    fn analysis_picture() -> Frame {
        Frame {
            width: 32,
            height: 18,
            rgb: vec![20; Frame::expected_len(32, 18)],
            captured_at: Utc::now(),
        }
    }

    /// Camera JPEGs as the Jetson capture sends them; `with_picture` says which carry the
    /// detector's picture, and `wait` holds a frame back that long.
    struct HardwareFrames(VecDeque<(Duration, CapturedFrame)>);

    impl HardwareFrames {
        fn new(frames: Vec<(Frame, bool, Duration)>) -> Self {
            Self(
                frames
                    .into_iter()
                    .map(|(f, with_picture, wait)| {
                        let frame = CapturedFrame::Jpeg(JpegFrame {
                            jpeg: Arc::new(crate::snapshot::frame_to_jpeg_bytes(&f).unwrap()),
                            luma: None,
                            analysis: with_picture.then(analysis_picture),
                            captured_at: f.captured_at,
                        });
                        (wait, frame)
                    })
                    .collect(),
            )
        }

        /// Still, then something appears and stays for `stay` frames, each with a picture.
        fn someone_arrives(stay: usize) -> Self {
            let mut frames = vec![(flat(20), true, Duration::ZERO)];
            frames.extend((0..stay).map(|_| (with_square(20), true, Duration::ZERO)));
            Self::new(frames)
        }
    }

    #[async_trait]
    impl FrameSource for HardwareFrames {
        async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
            let Some((wait, frame)) = self.0.pop_front() else {
                return Ok(None);
            };
            tokio::time::sleep(wait).await;
            Ok(Some(frame))
        }
    }

    /// An unknown face until call `known_from`, then the household's `jerry`.
    struct FaceAfter {
        known_from: usize,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl FaceRecognition for FaceAfter {
        async fn register_face(
            &self,
            _profile_id: &str,
            _image_bytes: &[u8],
            _bbox: Option<BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceEmbedding> {
            unimplemented!()
        }

        async fn identify_face(
            &self,
            _image_bytes: &[u8],
            _bbox: Option<BoundingBox>,
        ) -> Result<pond_core::user_data::domain::face_recognition::FaceIdentification> {
            use pond_core::user_data::domain::face_recognition::FaceIdentification;
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            Ok(if call >= self.known_from {
                FaceIdentification::found("jerry".into(), 0.9)
            } else {
                FaceIdentification {
                    identified: false,
                    profile_id: None,
                    confidence: Some(0.2),
                }
            })
        }

        async fn list_embeddings(
            &self,
            _profile_id: &str,
        ) -> Result<Vec<pond_core::user_data::domain::face_recognition::FaceEmbedding>> {
            Ok(vec![])
        }

        async fn delete_embeddings(&self, _profile_id: &str) -> Result<u64> {
            Ok(0)
        }

        async fn pairwise_similarities(
            &self,
        ) -> Result<Vec<pond_core::user_data::ports::face_recognition::PairwiseSimilarity>>
        {
            Ok(vec![])
        }

        fn match_threshold(&self) -> f32 {
            0.7
        }
    }

    /// No cooldown, so only the tracking can be what holds events back.
    fn no_cooldown() -> VisionPipelineConfig {
        VisionPipelineConfig {
            min_event_interval: Some(Duration::ZERO),
            ..cfg()
        }
    }

    async fn follow(
        mut source: HardwareFrames,
        locator: Arc<Locator>,
        face: Option<Arc<dyn FaceRecognition>>,
    ) -> Vec<(CameraEvent, serde_json::Value)> {
        let storage = Arc::new(MockCameraStorage::new());
        let registry = LiveCameraRegistry::new();
        registry.sync_settings(&pond_core::user_data::domain::settings::Settings {
            vision_event_types: vec!["person".into(), "pet".into(), "motion".into()],
            ..Default::default()
        });
        run_vision_pipeline(
            &mut source,
            Some(locator),
            face.map(|f| Arc::new(FaceRecognitionSlot::ready(f))),
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &registry,
            no_cooldown(),
        )
        .await
        .unwrap();
        let mut events = storage.list_events("backyard-cam", 100).await.unwrap();
        events.sort_by_key(|e| e.id);
        events
            .into_iter()
            .map(|e| {
                let meta = serde_json::from_str(e.metadata.as_deref().unwrap()).unwrap();
                (e, meta)
            })
            .collect()
    }

    #[tokio::test]
    async fn a_person_who_stays_in_view_is_one_event_and_one_face_try() {
        let face = Arc::new(FaceAfter {
            known_from: 1,
            calls: Default::default(),
        });
        let locator = Locator::seeing(&[("person", 0.1)]);
        let events = follow(
            HardwareFrames::someone_arrives(8),
            locator.clone(),
            Some(face.clone()),
        )
        .await;

        assert_eq!(
            events.len(),
            1,
            "one event however long they stay: {events:?}"
        );
        let (event, meta) = &events[0];
        assert_eq!(event.event_type, "person");
        assert_eq!(meta["track_id"], 1);
        assert_eq!(meta["identified_profile"], "jerry");
        assert_eq!(
            face.calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a known face is matched once per person"
        );
        assert!(
            locator.pictures.lock().unwrap().len() >= 7,
            "the detector keeps looking while they are in view"
        );
    }

    #[tokio::test]
    async fn an_unknown_face_is_tried_again_and_announced_once_it_is_known() {
        let face = Arc::new(FaceAfter {
            known_from: 2,
            calls: Default::default(),
        });
        let later = FACE_RETRY + Duration::from_millis(100);
        let source = HardwareFrames::new(vec![
            (flat(20), true, Duration::ZERO),
            (with_square(20), true, Duration::ZERO),
            (with_square(20), true, Duration::ZERO),
            (with_square(20), true, Duration::ZERO),
            (with_square(20), true, later),
            (with_square(20), true, Duration::ZERO),
        ]);
        let events = follow(
            source,
            Locator::seeing(&[("person", 0.1)]),
            Some(face.clone()),
        )
        .await;

        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(
            events[0].1["face_unknown"], true,
            "announced at once, face unknown"
        );
        assert_eq!(events[1].1["identified_profile"], "jerry");
        assert_eq!(
            events[0].1["track_id"], events[1].1["track_id"],
            "the same person"
        );
        assert_eq!(face.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn two_people_in_view_are_two_events() {
        let events = follow(
            HardwareFrames::someone_arrives(4),
            Locator::seeing(&[("person", 0.05), ("person", 0.7)]),
            None,
        )
        .await;
        let tracks: Vec<_> = events.iter().map(|(_, m)| m["track_id"].clone()).collect();
        assert_eq!(tracks, vec![serde_json::json!(1), serde_json::json!(2)]);
    }

    #[tokio::test]
    async fn a_person_too_small_for_a_motion_event_still_sets_the_detector_looking() {
        let storage = Arc::new(MockCameraStorage::new());
        let mut cfg = no_cooldown();
        cfg.motion.changed_fraction = 0.9;
        run_vision_pipeline(
            &mut HardwareFrames::someone_arrives(3),
            Some(Locator::seeing(&[("person", 0.1)])),
            None,
            storage.clone(),
            Arc::new(InProcessEventBus::new()),
            &LiveCameraRegistry::new(),
            cfg,
        )
        .await
        .unwrap();
        let events = storage.list_events("backyard-cam", 10).await.unwrap();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].event_type, "person");
    }

    #[tokio::test]
    async fn two_people_seen_at_once_cost_one_face_match() {
        let face = Arc::new(FaceAfter {
            known_from: 99,
            calls: Default::default(),
        });
        let events = follow(
            HardwareFrames::someone_arrives(3),
            Locator::seeing(&[("person", 0.05), ("person", 0.7)]),
            Some(face.clone()),
        )
        .await;
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(face.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn motion_without_people_is_named_by_the_detectors_look() {
        let locator = Locator::seeing(&[("pet", 0.4)]);
        let events = follow(HardwareFrames::someone_arrives(1), locator.clone(), None).await;

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].0.event_type, "pet");
        assert!(events[0].1.get("track_id").is_none());
        assert_eq!(
            locator.classified.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no second model run to name it"
        );
    }

    #[tokio::test]
    async fn the_detector_only_reads_the_capture_s_own_pictures() {
        // The picture rides on every other frame, as the 5 fps branch does under a faster camera.
        let mut frames = vec![(flat(20), true, Duration::ZERO)];
        frames.extend((0..8).map(|i| (with_square(20), i % 2 == 0, Duration::ZERO)));
        let locator = Locator::seeing(&[("person", 0.1)]);
        follow(HardwareFrames::new(frames), locator.clone(), None).await;

        let pictures = locator.pictures.lock().unwrap().clone();
        assert_eq!(pictures.len(), 4, "one look per picture: {pictures:?}");
        assert!(
            pictures.iter().all(|&size| size == (32, 18)),
            "never a CPU decode of the camera's JPEG: {pictures:?}"
        );
    }
}
