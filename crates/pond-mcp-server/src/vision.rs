use std::sync::{Arc, OnceLock};

use base64::Engine as _;
use pond_adapters_vision::{
    annotate_jpeg_for_model, format_known_people, FrameAnnotation, LiveCameraRegistry,
    VisionBusConfig,
};
use pond_core::user_data::domain::schedule::{
    CompareOp, SensorTriggerSpec, TaskKind, TriggerAction, TriggerCondition, TriggerSource,
    TriggerSourceKind,
};
use pond_core::user_data::domain::sensor::CameraEvent;
use pond_core::user_data::domain::vision as vision_policy;
use pond_core::user_data::ports::camera_storage::CameraStorage;
use pond_core::user_data::ports::scheduler::CreateScheduleRequest;
use pond_core::user_data::ports::settings::SettingsRepository;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;
const DEFAULT_WINDOW_FRAMES: usize = 3;
const WINDOW_EVENT_SCAN: usize = 30;

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct RecentCameraEventsParams {
    /// Camera to read (default: the household's camera).
    pub camera_id: Option<String>,
    /// Default 10, max 50.
    pub limit: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct AcknowledgeCameraEventParams {
    /// Event ID from get_recent_camera_events.
    pub event_id: Option<i64>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LookAtSnapshotParams {
    /// Camera to look at (default: the household's camera).
    pub camera_id: Option<String>,
    /// Specific event from get_recent_camera_events; omit for the newest frame.
    pub event_id: Option<i64>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct LookAtWindowParams {
    /// Camera to look at (default: the household's camera).
    pub camera_id: Option<String>,
    /// Frames to sample, evenly spaced across the window. Default 3, max 4.
    pub frames: Option<u32>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SendSnapshotParams {
    /// Camera to look at (default: the household's camera).
    pub camera_id: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ConfigureVisionBusParams {
    /// Minimum seconds between vision events (1 to 3600, default 10).
    pub min_event_interval_secs: Option<u64>,
    /// Detector confidence needed to name an event instead of "motion" (0.05 to 0.99, default 0.5).
    pub classifier_confidence_threshold: Option<f64>,
    /// Recognise household faces in camera events.
    pub face_detection_enabled: Option<bool>,
    /// Run face detection on motion events even before a person classifier runs.
    pub face_on_motion: Option<bool>,
    /// Event types to record (e.g. ["motion", "person", "pet", "vehicle", "package", "face_detected", "face_recognized", "face_unknown"]); an empty list records every type.
    pub enabled_event_types: Option<Vec<String>>,
    /// Capture frame rate in FPS (1 to 30).
    pub vision_fps: Option<u32>,
    /// Fraction of the picture that must change to count as motion (0.005 to 0.50).
    pub vision_motion_threshold: Option<f64>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct CreateVisionAutomationParams {
    /// Label for the automation rule (e.g. "Welcome Jerry home").
    pub name: Option<String>,
    /// Camera ID to listen to (default: the household's camera, or "any" for all cameras).
    pub camera_id: Option<String>,
    /// Vision or face signal to match: "motion", "person", "pet", "vehicle", "package", "face_detected", "face_recognized", "face_unknown", or "face:<profile_id>".
    pub signal: Option<String>,
    /// Optional minimum confidence (0.0 to 1.0).
    pub min_confidence: Option<f64>,
    /// Fire only after this local time ("HH:MM", 24-hour).
    pub after: Option<String>,
    /// Fire only before this local time ("HH:MM", 24-hour).
    pub before: Option<String>,
    /// Action: prompt sent to the assistant when the event fires.
    pub prompt: Option<String>,
    /// Action: notification title.
    pub notify_title: Option<String>,
    /// Action: notification body.
    pub notify_body: Option<String>,
    /// Action: smart-home device ID to power on or off.
    pub power_device_id: Option<String>,
    /// true = power on (default), false = power off.
    pub power_on: Option<bool>,
    /// Cooldown seconds between triggers (default 60).
    pub cooldown_secs: Option<u64>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListVisionAutomationsParams {
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct DeleteVisionAutomationParams {
    /// Automation rule ID from list_vision_automations.
    pub rule_id: Option<String>,
    /// Catch-all for unexpected fields the model sends.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

async fn snapshot_content(path: &str) -> Option<ContentBlock> {
    use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;

    let p = std::path::Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        other => {
            tracing::debug!(
                path,
                ext = other,
                "vision: snapshot is not a readable image"
            );
            return None;
        }
    };

    match tokio::fs::metadata(p).await {
        Ok(m) if m.len() as usize > MAX_IMAGE_BYTES => {
            tracing::warn!(
                path,
                bytes = m.len(),
                "vision: snapshot exceeds per-image limit"
            );
            return None;
        }
        Ok(_) => {}
        Err(e) => {
            tracing::debug!(path, error = %e, "vision: snapshot file is gone");
            return None;
        }
    }

    match tokio::fs::read(p).await {
        Ok(bytes) => Some(ContentBlock::image(
            base64::engine::general_purpose::STANDARD.encode(&bytes),
            mime.to_string(),
        )),
        Err(e) => {
            tracing::warn!(path, error = %e, "vision: could not read snapshot");
            None
        }
    }
}

async fn snapshot_annotated_or_raw(path: &str, event: &CameraEvent) -> Option<ContentBlock> {
    use pond_core::models::domain::image_limits::MAX_IMAGE_BYTES;
    let p = std::path::Path::new(path);
    if let Ok(m) = tokio::fs::metadata(p).await {
        if m.len() as usize <= MAX_IMAGE_BYTES {
            if let Ok(bytes) = tokio::fs::read(p).await {
                let reg = LiveCameraRegistry::global();
                let ann =
                    FrameAnnotation::from_event(event, reg.measured_fps(&event.camera_id), |pid| {
                        reg.resolve_profile_name(pid)
                    });
                if let Ok(compressed) = annotate_jpeg_for_model(&bytes, &ann, &reg.known_people()) {
                    return Some(ContentBlock::image(
                        base64::engine::general_purpose::STANDARD.encode(&compressed),
                        "image/jpeg".to_string(),
                    ));
                }
            }
        }
    }
    snapshot_content(path).await
}

fn event_face_tag(e: &CameraEvent) -> String {
    let Some(raw) = e.metadata.as_deref() else {
        return String::new();
    };
    let Ok(meta) = serde_json::from_str::<serde_json::Value>(raw) else {
        return String::new();
    };
    let bbox_note = meta
        .get("face_bbox")
        .and_then(|b| {
            Some(format!(
                " [box {},{} {}x{}]",
                b.get("x")?.as_u64()?,
                b.get("y")?.as_u64()?,
                b.get("width")?.as_u64()?,
                b.get("height")?.as_u64()?,
            ))
        })
        .unwrap_or_default();
    if let Some(pid) = meta.get("identified_profile").and_then(|v| v.as_str()) {
        if !pid.is_empty() {
            let display = meta
                .get("identified_name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| LiveCameraRegistry::global().resolve_profile_name(pid))
                .unwrap_or_else(|| pid.to_string());
            let conf = meta
                .get("face_confidence")
                .and_then(|v| v.as_f64())
                .map(|c| format!(" {:.0}%", c * 100.0))
                .unwrap_or_default();
            return format!(" · face: {display}{conf}{bbox_note}");
        }
    }
    if meta.get("face_unknown").and_then(|v| v.as_bool()) == Some(true) {
        return format!(" · face: unknown{bbox_note}");
    }
    String::new()
}

fn frame_caption(e: &CameraEvent) -> String {
    format!(
        "Frame from '{}' at {} (detector said: {}{}{})",
        e.camera_id,
        e.created_at.format("%Y-%m-%d %H:%M:%S"),
        e.event_type,
        e.confidence
            .map(|c| format!(", {:.0}% confidence", c * 100.0))
            .unwrap_or_default(),
        event_face_tag(e),
    )
}

fn pick_evenly_spaced(len: usize, count: usize) -> Vec<usize> {
    if len == 0 || count == 0 {
        return Vec::new();
    }
    if count >= len {
        return (0..len).collect();
    }
    if count == 1 {
        return vec![len - 1];
    }
    let mut picked = Vec::with_capacity(count);
    let step = (len - 1) as f64 / (count - 1) as f64;
    for i in 0..count {
        let idx = (i as f64 * step).round() as usize;
        if !picked.contains(&idx) {
            picked.push(idx);
        }
    }
    picked
}

#[derive(Clone)]
pub struct VisionMcpServer {
    camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl VisionMcpServer {
    /// The camera to use, or the refusal to return: no frame reaches the model without consent.
    async fn camera_for_frames(&self, requested: Option<String>) -> Result<String, CallToolResult> {
        match self.settings_repo.get().await {
            Ok(s) if !s.cameras_enabled => Err(CallToolResult::success(vec![ContentBlock::text(
                "Camera access is turned off in Privacy settings, so I can't look at the cameras.",
            )])),
            Ok(s) => Ok(requested
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| vision_policy::camera_id(&s))),
            Err(e) => {
                tracing::warn!(error = %e, "vision: could not read camera consent");
                Err(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't check whether the cameras are allowed right now.",
                )]))
            }
        }
    }

    async fn default_camera(&self, requested: Option<String>) -> String {
        match requested.filter(|c| !c.trim().is_empty()) {
            Some(id) => id,
            None => self
                .settings_repo
                .get()
                .await
                .map(|s| vision_policy::camera_id(&s))
                .unwrap_or_else(|_| vision_policy::DEFAULT_CAMERA_ID.to_string()),
        }
    }

    async fn recent_face_context(&self, camera_id: &str, ann: Option<&FrameAnnotation>) -> String {
        if let Some(a) = ann {
            if let Some(name) = a
                .identified_name
                .as_deref()
                .or(a.identified_profile.as_deref())
            {
                let conf = a
                    .face_confidence
                    .map(|c| format!(" ({:.0}% confidence)", c * 100.0))
                    .unwrap_or_default();
                return format!("Face recognized in frame: {name}{conf}.");
            }
            if a.face_unknown {
                return "Face in frame: unrecognized person (not matching any enrolled profile)."
                    .to_string();
            }
        }
        let now = chrono::Utc::now();
        if let Ok(events) = self
            .camera_storage
            .list_events(camera_id, WINDOW_EVENT_SCAN)
            .await
        {
            let reg = LiveCameraRegistry::global();
            for ev in &events {
                let age_secs = (now - ev.created_at).num_seconds();
                if !(0..=600).contains(&age_secs) {
                    continue;
                }
                let ev_ann =
                    FrameAnnotation::from_event(ev, None, |pid| reg.resolve_profile_name(pid));
                if let Some(name) = ev_ann
                    .identified_name
                    .as_deref()
                    .or(ev_ann.identified_profile.as_deref())
                {
                    let conf = ev_ann
                        .face_confidence
                        .map(|c| format!(" ({:.0}% confidence)", c * 100.0))
                        .unwrap_or_default();
                    return format!(
                        "Face in this exact frame: no frontal face visible (person may be turned or in profile). \
                         Most recent face recognized on this camera: {name}{conf} ({age_secs}s ago at {} UTC).",
                        ev.created_at.format("%H:%M:%S")
                    );
                }
            }
        }
        "Face in this frame: no face recognized (do NOT guess or assign a person's name)."
            .to_string()
    }
}

#[tool_router]
impl VisionMcpServer {
    pub fn new(
        camera_storage: Arc<dyn CameraStorage + Send + Sync>,
        settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
    ) -> Self {
        Self {
            camera_storage,
            settings_repo,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
CALL THIS FIRST for fixed security cameras, never an image attached to the message. Fast \
text-only summary of who is currently in the room, live detections, historical face sightings, \
and recent events (motion, pet, package, person) with zero image overhead. Never guess what a \
camera saw.")]
    async fn get_recent_camera_events(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<RecentCameraEventsParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let household_camera = self.default_camera(None).await;
        let mut camera_id = self.default_camera(params.0.camera_id).await;
        let limit = params
            .0
            .limit
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);
        let scan_limit = limit.max(250);

        // A model names a camera it imagined ("front door"); an empty answer then reads as "nothing
        // happened" and it gives up, so a camera with no events reads the household's instead.
        let mut redirected = None;
        if camera_id != household_camera
            && self
                .camera_storage
                .list_events(&camera_id, 1)
                .await
                .is_ok_and(|events| events.is_empty())
        {
            redirected = Some(std::mem::replace(&mut camera_id, household_camera));
        }

        let reg = LiveCameraRegistry::global();
        let live_ann = reg.get_annotation(&camera_id);
        let live_summary = live_ann
            .as_ref()
            .map(|a| a.summary_line())
            .unwrap_or_else(|| "status=live".to_string());
        let live_face_ctx = self
            .recent_face_context(&camera_id, live_ann.as_ref())
            .await;

        let answer = match self
            .camera_storage
            .list_events(&camera_id, scan_limit)
            .await
        {
            Ok(events) if events.is_empty() => {
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    crate::format::format_no_results(
                        &format!("recent events from camera '{camera_id}'"),
                        &["giap-vision__look_at_camera_snapshot"],
                    ),
                )]))
            }
            Ok(events) => {
                let mut sightings: Vec<(String, i64, String, Option<f32>, usize)> = Vec::new();
                for e in &events {
                    let ann =
                        FrameAnnotation::from_event(e, None, |pid| reg.resolve_profile_name(pid));
                    if let Some(name) = ann.identified_name.or(ann.identified_profile) {
                        if let Some(existing) = sightings.iter_mut().find(|s| s.0 == name) {
                            existing.4 += 1;
                        } else {
                            sightings.push((
                                name,
                                e.id.unwrap_or(-1),
                                e.created_at.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
                                ann.face_confidence,
                                1,
                            ));
                        }
                    }
                }
                let sightings_block = if sightings.is_empty() {
                    "Historical face sightings: none recorded in recent history.".to_string()
                } else {
                    let s_lines: Vec<String> = sightings
                        .iter()
                        .map(|(name, eid, ts, conf, count)| {
                            let c_str = conf
                                .map(|c| format!(", {:.0}% confidence", c * 100.0))
                                .unwrap_or_default();
                            format!(
                                "  * {name}: last seen at {ts} [event #{eid}{c_str}; {count} sighting(s)]"
                            )
                        })
                        .collect();
                    format!(
                        "Historical face sightings (pass event_id to look_at_camera_snapshot only if you must inspect pixels):\n{}",
                        s_lines.join("\n")
                    )
                };

                let lines: Vec<String> = events
                    .iter()
                    .take(limit)
                    .map(|e| {
                        format!(
                            "- [{}] {} {}{}{}{}",
                            e.id.map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
                            e.created_at.format("%Y-%m-%d %H:%M"),
                            e.event_type,
                            e.confidence
                                .map(|c| format!(" ({:.0}% confidence)", c * 100.0))
                                .unwrap_or_default(),
                            event_face_tag(e),
                            if e.acknowledged {
                                " · acknowledged"
                            } else {
                                ""
                            },
                        )
                    })
                    .collect();
                let fps_str = reg
                    .measured_fps(&camera_id)
                    .map(|f| format!("{f:.1} fps"))
                    .unwrap_or_else(|| "idle".to_string());
                let known = format_known_people(&reg.known_people());
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Camera '{camera_id}' status (stream: {fps_str}; live detection: {live_summary}):\n\
                     {live_face_ctx}\n\
                     {sightings_block}\n\
                     Enrolled face database (configuration only, NOT who is currently in room): {known}\n\n\
                     Latest {} event(s):\n{}",
                    lines.len(),
                    lines.join("\n"),
                ))]))
            }
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't read the camera events right now.",
                )]))
            }
        };
        match (redirected, answer) {
            (Some(asked), Ok(mut result)) => {
                result.content.insert(
                    0,
                    ContentBlock::text(format!(
                        "There is no camera '{asked}' with events. These are from the household's \
                         camera, '{camera_id}'."
                    )),
                );
                Ok(result)
            }
            (_, answer) => answer,
        }
    }

    #[tool(description = "\
LAST RESORT for fixed security cameras only: call ONLY when get_recent_camera_events text is \
not enough and you must visually inspect pixel details (clothing, objects, posture, or a \
specific event_id). Returns one compressed lightweight frame. If the user attached an image, \
just look at it - do NOT call this.")]
    async fn look_at_camera_snapshot(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<LookAtSnapshotParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let camera_id = match self.camera_for_frames(params.0.camera_id).await {
            Ok(id) => id,
            Err(refusal) => return Ok(refusal),
        };

        let reg = LiveCameraRegistry::global();
        if params.0.event_id.is_none() {
            if let Some(live_jpeg) = reg
                .get_model_annotated_frame(&camera_id)
                .or_else(|| reg.get_latest_frame(&camera_id))
            {
                let ann = reg.get_annotation(&camera_id);
                let ann_summary = ann
                    .as_ref()
                    .map(|a| a.summary_line())
                    .unwrap_or_else(|| "status=live".to_string());
                let face_ctx = self.recent_face_context(&camera_id, ann.as_ref()).await;
                let img = ContentBlock::image(
                    base64::engine::general_purpose::STANDARD.encode(&live_jpeg),
                    "image/jpeg".to_string(),
                );
                return Ok(CallToolResult::success(vec![
                    ContentBlock::text(format!(
                        "Live compressed snapshot from camera '{camera_id}' captured NOW at {} UTC.\n\
                         Detection: {ann_summary}\n\
                         {face_ctx}\n\n\
                         ![Camera Snapshot](/api/v1/camera/{camera_id}/snapshot?annotated=1)\n\
                         ![Live Camera Feed](/api/v1/camera/{camera_id}/stream?annotated=1)",
                        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
                    )),
                    img,
                ]));
            }
        }

        let chosen_owned: Option<CameraEvent> = match params.0.event_id {
            Some(id) => match self.camera_storage.get_event(id).await {
                Ok(Some(ev)) => Some(ev),
                _ => self
                    .camera_storage
                    .list_events(&camera_id, WINDOW_EVENT_SCAN)
                    .await
                    .ok()
                    .and_then(|evs| evs.into_iter().find(|e| e.id == Some(id))),
            },
            None => {
                let events = match self
                    .camera_storage
                    .list_events(&camera_id, WINDOW_EVENT_SCAN)
                    .await
                {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "vision: camera event listing failed");
                        return Ok(CallToolResult::success(vec![ContentBlock::text(
                            "Sorry, I couldn't reach the camera store right now.",
                        )]));
                    }
                };
                events.into_iter().find(|e| e.snapshot_path.is_some())
            }
        };

        let Some(ref event) = chosen_owned else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "No frame available from camera '{camera_id}'."
            ))]));
        };
        let Some(path) = event.snapshot_path.as_deref() else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Event {} has no saved frame.",
                event.id.unwrap_or(-1)
            ))]));
        };
        let Some(image) = snapshot_annotated_or_raw(path, event).await else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "The saved frame for event {} is no longer readable.",
                event.id.unwrap_or(-1)
            ))]));
        };

        let event_id_str = event.id.map(|i| i.to_string()).unwrap_or_default();
        let ann = FrameAnnotation::from_event(event, reg.measured_fps(&camera_id), |pid| {
            reg.resolve_profile_name(pid)
        });
        let face_ctx = self.recent_face_context(&camera_id, Some(&ann)).await;
        Ok(CallToolResult::success(vec![
            ContentBlock::text(format!(
                "{}\nDetection: {}\n{face_ctx}\n\n\
                 ![Camera Snapshot](/api/v1/camera/events/{event_id_str}/snapshot?annotated=1)\n\
                 ![Live Camera Feed](/api/v1/camera/{camera_id}/stream?annotated=1)",
                frame_caption(event),
                ann.summary_line()
            )),
            image,
        ]))
    }

    #[tool(description = "\
LAST RESORT for fixed security cameras only: up to 4 compressed frames sampled across time \
when get_recent_camera_events text is not enough to see visual changes. If the user attached \
an image, do NOT call this.")]
    async fn look_at_camera_window(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<LookAtWindowParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let camera_id = match self.camera_for_frames(params.0.camera_id).await {
            Ok(id) => id,
            Err(refusal) => return Ok(refusal),
        };
        let frames = params
            .0
            .frames
            .map(|f| f as usize)
            .unwrap_or(DEFAULT_WINDOW_FRAMES)
            .clamp(1, 4);

        let events = match self
            .camera_storage
            .list_events(&camera_id, WINDOW_EVENT_SCAN)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't reach the camera store right now.",
                )]));
            }
        };

        let mut with_frames: Vec<&CameraEvent> = events
            .iter()
            .filter(|e| e.snapshot_path.is_some())
            .collect();
        with_frames.reverse();
        if with_frames.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "No frames available from camera '{camera_id}'."
            ))]));
        }

        let picked = pick_evenly_spaced(with_frames.len(), frames);
        let mut parts: Vec<ContentBlock> = Vec::with_capacity(picked.len() * 2 + 1);
        parts.push(ContentBlock::text(format!(
            "{} compressed frame(s) from camera '{camera_id}', oldest first, sampled across the last {} events.",
            picked.len(),
            with_frames.len(),
        )));

        for idx in picked {
            let event = with_frames[idx];
            if let Some(path) = event.snapshot_path.as_deref() {
                if let Some(image) = snapshot_annotated_or_raw(path, event).await {
                    parts.push(ContentBlock::text(frame_caption(event)));
                    parts.push(image);
                }
            }
        }

        Ok(CallToolResult::success(parts))
    }

    #[tool(description = "\
Send the annotated live camera picture and live video stream markdown links to the user in \
this conversation (text-only for the model, zero image context cost). Use this whenever the \
user asks to see, send, show, or share a picture, photo, annotated camera feed, or live video \
stream from the camera. Never for an attached image.")]
    async fn send_camera_snapshot(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<SendSnapshotParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let camera_id = match self.camera_for_frames(params.0.camera_id).await {
            Ok(id) => id,
            Err(refusal) => return Ok(refusal),
        };

        let reg = LiveCameraRegistry::global();
        if reg.get_latest_frame(&camera_id).is_some() {
            let ann = reg.get_annotation(&camera_id);
            let ann_summary = ann
                .as_ref()
                .map(|a| a.summary_line())
                .unwrap_or_else(|| "status=live".to_string());
            let face_ctx = self.recent_face_context(&camera_id, ann.as_ref()).await;
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Here is the live annotated picture and video feed from camera '{camera_id}' \
                 ({ann_summary}):\n{face_ctx}\n\n\
                 ![Camera Snapshot](/api/v1/camera/{camera_id}/snapshot?annotated=1)\n\n\
                 ![Live Camera Feed](/api/v1/camera/{camera_id}/stream?annotated=1)"
            ))]));
        }

        let events = match self
            .camera_storage
            .list_events(&camera_id, WINDOW_EVENT_SCAN)
            .await
        {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "vision: camera event listing failed");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't reach the camera store right now.",
                )]));
            }
        };

        if let Some(event) = events.iter().find(|e| e.snapshot_path.is_some()) {
            let event_id_str = event.id.map(|i| i.to_string()).unwrap_or_default();
            let ann = FrameAnnotation::from_event(event, reg.measured_fps(&camera_id), |pid| {
                reg.resolve_profile_name(pid)
            });
            let face_ctx = self.recent_face_context(&camera_id, Some(&ann)).await;
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Here is the annotated picture from camera '{camera_id}' \
                 ({}):\n{face_ctx}\n\n\
                 ![Camera Snapshot](/api/v1/camera/events/{event_id_str}/snapshot?annotated=1)\n\n\
                 ![Live Camera Feed](/api/v1/camera/{camera_id}/stream?annotated=1)",
                ann.summary_line()
            ))]));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "No snapshot is currently available from camera '{camera_id}'."
        ))]))
    }

    #[tool(description = "\
Acknowledge a camera event by ID to stop re-alerts once the user has seen it.")]
    async fn acknowledge_camera_event(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<AcknowledgeCameraEventParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let Some(event_id) = params.0.event_id else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "I need an `event_id` (see get_recent_camera_events).",
            )]));
        };
        match self.camera_storage.acknowledge(event_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Camera event {event_id} acknowledged."
            ))])),
            Err(e) => {
                tracing::warn!(error = %e, event_id, "vision: acknowledge failed");
                Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                    "Couldn't acknowledge event {event_id} — check the ID."
                ))]))
            }
        }
    }

    #[tool(description = "\
View or update the vision bus configuration (event cooldown, classifier confidence floor, \
face recognition, enabled event types, FPS, and motion threshold). Changes are saved to \
settings. Never for an image attached to the message.")]
    async fn configure_vision_bus(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<ConfigureVisionBusParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let p = params.0;
        let mut s = match self.settings_repo.get().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "vision: settings unavailable");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't read the vision settings right now.",
                )]));
            }
        };

        let mut touched = std::collections::HashSet::new();
        if let Some(v) = p.min_event_interval_secs {
            s.vision_event_cooldown_secs = v;
            touched.insert("vision_event_cooldown_secs".to_string());
        }
        if let Some(v) = p.classifier_confidence_threshold {
            s.vision_classifier_confidence = v;
            touched.insert("vision_classifier_confidence".to_string());
        }
        if let Some(v) = p.face_detection_enabled {
            s.vision_face_recognition_enabled = v;
            touched.insert("vision_face_recognition_enabled".to_string());
        }
        if let Some(v) = p.face_on_motion {
            s.vision_face_on_motion = v;
            touched.insert("vision_face_on_motion".to_string());
        }
        if let Some(v) = p.enabled_event_types {
            s.vision_event_types = v;
            touched.insert("vision_event_types".to_string());
        }
        if let Some(v) = p.vision_fps {
            s.vision_fps = v;
            touched.insert("vision_fps".to_string());
        }
        if let Some(v) = p.vision_motion_threshold {
            s.vision_motion_threshold = v;
            touched.insert("vision_motion_threshold".to_string());
        }
        vision_policy::normalize_settings(&mut s);

        let mut saved_note = "";
        if !touched.is_empty() {
            if let Err(e) = self.settings_repo.update_fields(&s, Some(&touched)).await {
                tracing::warn!(error = %e, "vision: saving the bus configuration failed");
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Sorry, I couldn't save the vision settings.",
                )]));
            }
            if let Err(e) = self.settings_repo.mark_user_set(&touched).await {
                tracing::warn!(error = %e, "vision: could not record the settings as chosen");
            }
            LiveCameraRegistry::global().sync_settings(&s);
            LiveCameraRegistry::global().notify_settings_changed();
            saved_note = "\nSaved to settings; it applies now and after a restart.";
        }

        let bus = VisionBusConfig::from_settings(&s);
        let reg = LiveCameraRegistry::global();
        let active_cams = reg.active_cameras();
        let cam_id = vision_policy::camera_id(&s);
        let measured_fps = reg
            .measured_fps(&cam_id)
            .map(|f| format!("{f:.1} fps"))
            .unwrap_or_else(|| "idle".to_string());
        let known = format_known_people(&reg.known_people());
        let listeners = reg.event_listener_count();
        Ok(CallToolResult::success(vec![ContentBlock::text(format!(
            "Vision bus configuration:\n\
             - Active cameras: {}\n\
             - Capture target: {} fps (range 1–30 fps), measured: {measured_fps}, motion threshold {:.3}\n\
             - Event cooldown: {}s\n\
             - Classifier confidence floor: {:.2}\n\
             - Face recognition enabled: {} (known faces: {known})\n\
             - Face detection on motion: {}\n\
             - Enabled event types: {}\n\
             - Supported automation signals: {}\n\
             - Annotated live stream: /api/v1/camera/{cam_id}/stream?annotated=1\n\
             - Annotated snapshot: /api/v1/camera/{cam_id}/snapshot?annotated=1\n\
             - Live stream listeners: {}{}",
            if active_cams.is_empty() {
                "none".to_string()
            } else {
                active_cams.join(", ")
            },
            s.vision_fps,
            s.vision_motion_threshold,
            bus.min_event_interval_secs,
            bus.classifier_confidence_threshold,
            bus.face_detection_enabled,
            bus.face_on_motion,
            if bus.enabled_event_types.is_empty() {
                "all".to_string()
            } else {
                bus.enabled_event_types.join(", ")
            },
            VisionBusConfig::supported_signals().join(", "),
            listeners,
            saved_note,
        ))]))
    }

    #[tool(description = "\
Register an automation rule or trigger on the vision bus for camera and face events (motion, \
person, pet, vehicle, package, face_detected, face_recognized, face_unknown, or \
face:<profile_id>). Never for an image attached to the message.")]
    async fn create_vision_automation(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<CreateVisionAutomationParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let Some((scheduler, settings_repo)) = crate::schedule::scheduler_and_settings() else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Vision automation scheduler is unavailable in this process.",
            )]));
        };

        let p = params.0;
        let camera_id = match p.camera_id.as_deref().map(str::trim) {
            None | Some("") => Some(self.default_camera(None).await),
            Some("any") | Some("*") => None,
            Some(id) => Some(id.to_string()),
        };
        let signal = p
            .signal
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && *s != "any" && *s != "*")
            .map(str::to_string)
            .unwrap_or_else(|| "person".to_string());

        let mut actions = Vec::new();
        if let Some(prompt) = p.prompt.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            actions.push(TriggerAction::AgentPrompt {
                prompt: prompt.to_string(),
            });
        }
        if let Some(dev_id) = p
            .power_device_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            actions.push(TriggerAction::DevicePower {
                device_id: dev_id.to_string(),
                on: p.power_on.unwrap_or(true),
            });
        }
        let notify_title = p
            .notify_title
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        let notify_body = p
            .notify_body
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            .to_string();
        if !notify_title.is_empty() || !notify_body.is_empty() {
            actions.push(TriggerAction::Notify {
                title: if notify_title.is_empty() {
                    format!("Vision alert ({signal})")
                } else {
                    notify_title
                },
                body: if notify_body.is_empty() {
                    format!(
                        "Camera '{}' detected '{}'",
                        camera_id.as_deref().unwrap_or("any"),
                        signal
                    )
                } else {
                    notify_body
                },
            });
        }
        if actions.is_empty() {
            actions.push(TriggerAction::Notify {
                title: format!("Vision alert ({signal})"),
                body: format!(
                    "Camera '{}' detected '{}'",
                    camera_id.as_deref().unwrap_or("any"),
                    signal
                ),
            });
        }

        let condition = TriggerCondition {
            op: p.min_confidence.map(|_| CompareOp::Gte),
            value: p.min_confidence,
            after: p.after.filter(|s| !s.trim().is_empty()),
            before: p.before.filter(|s| !s.trim().is_empty()),
        };

        let spec = SensorTriggerSpec {
            source: TriggerSource {
                kind: TriggerSourceKind::Camera,
                device_id: camera_id.clone(),
                signal: Some(signal.clone()),
            },
            condition,
            actions,
            cooldown_secs: p.cooldown_secs.unwrap_or(60),
        };

        if let Err(rej) = spec.validate() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Invalid vision automation rule: {rej}"
            ))]));
        }

        let label = p.name.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
            format!(
                "Vision: {} on {}",
                signal,
                camera_id.as_deref().unwrap_or("any camera")
            )
        });
        let timezone = settings_repo
            .get()
            .await
            .map(|s| s.timezone)
            .unwrap_or_else(|_| "UTC".to_string());
        let id = uuid::Uuid::new_v4().to_string();
        let req = CreateScheduleRequest {
            ask_if_missed: false,
            id: id.clone(),
            label: label.clone(),
            cron: "@event".to_string(),
            fire_at: None,
            once: false,
            timezone,
            kind: TaskKind::SensorTrigger(spec),
        };

        match scheduler.create_task(req).await {
            Ok(created) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Vision automation created: \"{}\" [id: {}] — camera={}, signal={}, cooldown={}s.",
                created.label,
                created.id,
                camera_id.as_deref().unwrap_or("any"),
                signal,
                p.cooldown_secs.unwrap_or(60),
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Failed to register vision automation: {e}"
            ))])),
        }
    }

    #[tool(description = "\
List all registered camera and face detection automation rules on the vision bus. Never for an \
image attached to the message.")]
    async fn list_vision_automations(
        &self,
        _ctx: RequestContext<RoleServer>,
        _params: Parameters<ListVisionAutomationsParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let Some((scheduler, _)) = crate::schedule::scheduler_and_settings() else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Vision automation scheduler is unavailable in this process.",
            )]));
        };

        match scheduler.list_tasks().await {
            Ok(tasks) => {
                let rules: Vec<String> = tasks
                    .into_iter()
                    .filter_map(|t| match t.kind {
                        TaskKind::SensorTrigger(spec)
                            if spec.source.kind == TriggerSourceKind::Camera =>
                        {
                            Some(format!(
                                "- [{}] \"{}\" — camera={}, signal={}, cooldown={}s, actions={}{}",
                                t.id,
                                t.label,
                                spec.source.device_id.as_deref().unwrap_or("any"),
                                spec.source.signal.as_deref().unwrap_or("any"),
                                spec.cooldown_secs,
                                spec.actions.len(),
                                if t.paused { " (paused)" } else { "" },
                            ))
                        }
                        _ => None,
                    })
                    .collect();

                if rules.is_empty() {
                    Ok(CallToolResult::success(vec![ContentBlock::text(
                        "No vision or face detection automations are currently registered.",
                    )]))
                } else {
                    Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                        "Registered vision bus automations ({}):\n{}",
                        rules.len(),
                        rules.join("\n"),
                    ))]))
                }
            }
            Err(e) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Failed to list vision automations: {e}"
            ))])),
        }
    }

    #[tool(description = "\
Delete a camera or face detection automation rule from the vision bus by rule_id. Never for an \
image attached to the message.")]
    async fn delete_vision_automation(
        &self,
        _ctx: RequestContext<RoleServer>,
        params: Parameters<DeleteVisionAutomationParams>,
    ) -> Result<CallToolResult, rmcp::model::ErrorData> {
        let Some((scheduler, _)) = crate::schedule::scheduler_and_settings() else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Vision automation scheduler is unavailable in this process.",
            )]));
        };
        let Some(rule_id) = params.0.rule_id.filter(|s| !s.trim().is_empty()) else {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "Please provide `rule_id` (see list_vision_automations).",
            )]));
        };
        match scheduler.delete_task(&rule_id).await {
            Ok(()) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Vision automation '{rule_id}' deleted."
            ))])),
            Err(e) => Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Could not delete vision automation '{rule_id}': {e}"
            ))])),
        }
    }
}

#[tool_handler]
impl ServerHandler for VisionMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                "giap-vision",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Vision MCP server — the FIXED HOME CAMERAS and VISION BUS, from the local, \
                 on-device event store (never guess what a camera saw).\n\n\
                 None of these tools are for an image the user attached to a message: an \
                 attachment is already visible to you, so look at it and answer directly.\n\n\
                 Tools: get_recent_camera_events, look_at_camera_snapshot, look_at_camera_window, \
                 send_camera_snapshot, acknowledge_camera_event, configure_vision_bus, \
                 create_vision_automation, list_vision_automations, delete_vision_automation.",
            )
    }
}

use tokio::io::DuplexStream;

struct VisionDeps {
    camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
}

static VISION_DEPS: OnceLock<VisionDeps> = OnceLock::new();

pub fn init_vision_deps(
    camera_storage: Arc<dyn CameraStorage + Send + Sync>,
    settings_repo: Arc<dyn SettingsRepository + Send + Sync>,
) {
    let _ = VISION_DEPS.set(VisionDeps {
        camera_storage,
        settings_repo,
    });
}

pub async fn vision_runtime_context() -> Option<String> {
    let deps = VISION_DEPS.get()?;
    let settings = deps.settings_repo.get().await.ok()?;
    if !settings.cameras_enabled || !settings.vision_enabled {
        return None;
    }
    let camera_id = if settings.vision_camera_id.trim().is_empty() {
        vision_policy::DEFAULT_CAMERA_ID.to_string()
    } else {
        settings.vision_camera_id.trim().to_string()
    };
    let reg = LiveCameraRegistry::global();
    let fps_note = match reg.measured_fps(&camera_id) {
        Some(m) => format!("target {} fps, measured {:.1} fps", settings.vision_fps, m),
        None => format!("target {} fps", settings.vision_fps),
    };
    let latest_note = reg
        .get_annotation(&camera_id)
        .map(|a| format!("; latest: {}", a.summary_line()))
        .unwrap_or_default();
    Some(format!(
        "## Vision Bus Status\n\
         - Camera `{camera_id}` active ({fps_note}{latest_note})\n\
         - Live annotated video feed: `/api/v1/camera/{camera_id}/stream?annotated=1`\n\
         - Annotated snapshot: `/api/v1/camera/{camera_id}/snapshot?annotated=1`"
    ))
}

pub fn spawn_vision_server(reader: DuplexStream, writer: DuplexStream) {
    let Some(deps) = VISION_DEPS.get() else {
        tracing::error!(
            "giap-vision: init_vision_deps() was never called for this entry point; \
             vision MCP server not started"
        );
        return;
    };
    let server = VisionMcpServer::new(deps.camera_storage.clone(), deps.settings_repo.clone());
    crate::serve_builtin("giap-vision", server, reader, writer);
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use async_trait::async_trait;
    use pond_core::user_data::domain::sensor::CameraEvent;

    struct StubStorage(Vec<CameraEvent>);

    #[async_trait]
    impl CameraStorage for StubStorage {
        async fn record_event(&self, _event: CameraEvent) -> Result<i64> {
            Ok(1)
        }
        async fn list_events(&self, camera_id: &str, limit: usize) -> Result<Vec<CameraEvent>> {
            Ok(self
                .0
                .iter()
                .filter(|e| e.camera_id == camera_id)
                .take(limit)
                .cloned()
                .collect())
        }
        async fn get_event(&self, event_id: i64) -> Result<Option<CameraEvent>> {
            Ok(self.0.iter().find(|e| e.id == Some(event_id)).cloned())
        }
        async fn acknowledge(&self, _event_id: i64) -> Result<()> {
            Ok(())
        }
    }

    async fn server(cameras_enabled: bool) -> VisionMcpServer {
        use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
        let settings = MockSettingsRepository::new();
        let s = pond_core::user_data::domain::settings::Settings {
            cameras_enabled,
            vision_camera_id: "porch".into(),
            ..Default::default()
        };
        settings.update(&s).await.unwrap();
        VisionMcpServer::new(Arc::new(StubStorage(vec![])), Arc::new(settings))
    }

    #[tokio::test]
    async fn server_constructs() {
        let _server = server(true).await;
    }

    #[tokio::test]
    async fn a_camera_with_no_events_reads_the_households_own_and_says_so() {
        use pond_core::user_data::mocks::mock_settings::MockSettingsRepository;
        use rmcp::model::RequestId;
        let settings = MockSettingsRepository::new();
        settings
            .update(&pond_core::user_data::domain::settings::Settings {
                vision_camera_id: "porch".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let seen = CameraEvent {
            id: Some(1),
            camera_id: "porch".into(),
            event_type: "person".into(),
            confidence: Some(0.9),
            snapshot_path: None,
            metadata: None,
            acknowledged: false,
            created_at: chrono::Utc::now(),
        };
        let server = VisionMcpServer::new(Arc::new(StubStorage(vec![seen])), Arc::new(settings));
        let (_client, stream) = tokio::io::duplex(64);
        let running = rmcp::service::serve_directly(server.clone(), stream, None);
        let ctx = RequestContext::new(RequestId::Number(0), running.peer().clone());
        let asked = |camera: &str| RecentCameraEventsParams {
            camera_id: Some(camera.into()),
            ..Default::default()
        };

        let text = text_of(
            &server
                .get_recent_camera_events(ctx.clone(), Parameters(asked("front door")))
                .await
                .unwrap(),
        );
        assert!(
            text.starts_with("There is no camera 'front door' with events."),
            "{text}"
        );
        assert!(text.contains("person"), "{text}");

        let text = text_of(
            &server
                .get_recent_camera_events(ctx, Parameters(asked("porch")))
                .await
                .unwrap(),
        );
        assert!(!text.contains("There is no camera"), "{text}");
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|c| c.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn no_camera_frame_reaches_the_model_without_consent() {
        let refused = server(false)
            .await
            .camera_for_frames(None)
            .await
            .expect_err("consent off must refuse");
        assert!(text_of(&refused).contains("turned off in Privacy"));

        let allowed = server(true).await.camera_for_frames(None).await.ok();
        assert_eq!(
            allowed.as_deref(),
            Some("porch"),
            "defaults to the configured camera"
        );
        let named = server(true)
            .await
            .camera_for_frames(Some("garage".into()))
            .await
            .ok();
        assert_eq!(named.as_deref(), Some("garage"));
    }

    #[tokio::test]
    async fn a_real_jpeg_becomes_image_content() {
        let dir = std::env::temp_dir().join(format!("giap-vision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("camera-1-20260728T000000000Z.jpg");
        std::fs::write(&path, b"\xFF\xD8\xFF\xE0 pretend jpeg").unwrap();

        let content = snapshot_content(path.to_str().unwrap())
            .await
            .expect("readable jpeg");
        match content {
            rmcp::model::ContentBlock::Image(img) => {
                assert_eq!(img.mime_type, "image/jpeg");
                assert_eq!(
                    base64::engine::general_purpose::STANDARD
                        .decode(&img.data)
                        .unwrap(),
                    b"\xFF\xD8\xFF\xE0 pretend jpeg"
                );
            }
            other => panic!("expected image content, got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_missing_or_non_image_snapshot_yields_nothing() {
        assert!(snapshot_content("/nope/does-not-exist.jpg").await.is_none());

        let dir = std::env::temp_dir().join(format!("giap-vision-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, b"not an image").unwrap();
        assert!(snapshot_content(path.to_str().unwrap()).await.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_caption_names_the_camera_time_and_detection() {
        let e = CameraEvent {
            id: Some(7),
            camera_id: "front-door".into(),
            event_type: "person".into(),
            confidence: Some(0.91),
            snapshot_path: Some("/tmp/x.jpg".into()),
            metadata: None,
            acknowledged: false,
            created_at: chrono::Utc::now(),
        };
        let caption = frame_caption(&e);
        assert!(caption.contains("front-door"));
        assert!(caption.contains("person"));
        assert!(caption.contains("91% confidence"));
    }

    #[test]
    fn every_camera_tool_guards_against_attached_images() {
        const NEGATIONS: &[&str] = &["not", "never", "dont", "don"];

        let router = VisionMcpServer::tool_router();
        let tools = router.list_all();
        assert!(!tools.is_empty());

        let guarded = [
            "get_recent_camera_events",
            "look_at_camera_snapshot",
            "look_at_camera_window",
            "send_camera_snapshot",
        ];
        for name in guarded {
            let tool = tools
                .iter()
                .find(|t| t.name == name)
                .unwrap_or_else(|| panic!("{name} is not registered"));
            let description = tool
                .description
                .as_deref()
                .unwrap_or_else(|| panic!("{name} has no description"));
            let lower = description.to_lowercase();
            assert!(
                lower.contains("attach"),
                "{name} must mention images attached to the message: {description}"
            );
            let guarded_clause = lower.split('.').any(|sentence| {
                sentence.contains("attach")
                    && sentence
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .any(|word| NEGATIONS.contains(&word))
            });
            assert!(
                guarded_clause,
                "{name}: mentioning attachments is not a guard - one clause must both name \
                 the attachment and rule this tool out for it: {description}"
            );
        }
    }
}
