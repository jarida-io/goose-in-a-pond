//! Vision events (ffmpeg capture, motion differencing, optional local labelling) feed the same
//! `camera_events` store and EventBus as the external camera-event API.

mod ffmpeg_source;
mod motion;
mod pipeline;
mod snapshot;

pub use ffmpeg_source::{CaptureConfig, FfmpegFrameSource};
pub use motion::{MotionConfig, MotionDetector};
pub use pipeline::{run_vision_pipeline, VisionPipelineConfig};
pub use snapshot::SnapshotConfig;
