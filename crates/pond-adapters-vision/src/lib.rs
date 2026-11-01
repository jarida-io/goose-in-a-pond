//! Vision events (ffmpeg capture, motion differencing, optional local labelling) feed the same
//! `camera_events` store and EventBus as the external camera-event API.

pub mod annotate;
mod capture;
mod discovery;
mod ffmpeg_source;
mod gst;
mod jpeg_decode;
mod jpeg_stream;
mod live_feed;
mod motion;
mod pipeline;
mod process;
mod snapshot;
mod supervisor;
mod tracker;

pub use annotate::{
    annotate_frame, annotate_jpeg, annotate_jpeg_for_model, compress_frame_for_model,
    format_known_people, FrameAnnotation, KnownPerson, MODEL_IMAGE_MAX_DIM, MODEL_JPEG_QUALITY,
};
pub use capture::{open_capture, OpenFuture, OpenedSource, FIRST_FRAME_TIMEOUT};
pub use discovery::{default_camera_source, detect_v4l2_cameras, DiscoveredCamera};
pub use ffmpeg_source::{CaptureConfig, FfmpegFrameSource};
pub use live_feed::{
    CapturePath, LiveCameraRegistry, SourceKind, SupervisorStatus, VisionBusConfig, VisionState,
    LIVE_FRAME_WINDOW,
};
pub use motion::{min_reference_age, MotionConfig, MotionDetector, MOTION_SPAN};
pub use pipeline::{run_vision_pipeline, VisionPipelineConfig};
pub use snapshot::{frame_to_jpeg_bytes, write_snapshot, write_snapshot_jpeg, SnapshotConfig};
pub use supervisor::{
    capture_change, desired_capture, redact_source, CameraDiscovery, CaptureChange, CaptureSpec,
    DesiredCapture, SourceOpener, SupervisorTiming, VisionSupervisor, RECONNECT_BACKOFF_MAX,
    RECONNECT_BACKOFF_MIN, SETTINGS_POLL_INTERVAL,
};
