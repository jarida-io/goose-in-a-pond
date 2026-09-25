//! [`FrameSource`] reading raw RGB24 frames from an `ffmpeg` child (RTSP or V4L2); using the CLI
//! keeps GIAP free of native libav/GStreamer linkage.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::vision::Frame;
use pond_core::user_data::ports::vision::FrameSource;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};

/// Capture config; ffmpeg downscales before the pipe, as the motion grid needs little resolution.
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// RTSP/HTTP URL, or a V4L2 device path (`/dev/videoN`).
    pub input: String,
    pub width: u32,
    pub height: u32,
    /// Frames per second sampled from the stream.
    pub fps: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            input: String::new(),
            width: 640,
            height: 360,
            fps: 2,
        }
    }
}

/// ffmpeg argv for `cfg`; the input is one argv element (no shell) and may not start with `-`.
fn build_args(cfg: &CaptureConfig) -> Result<Vec<String>> {
    let input = cfg.input.trim();
    if input.is_empty() {
        bail!("camera input is empty");
    }
    if input.starts_with('-') {
        bail!("camera input must be a URL or device path, not an option");
    }
    if cfg.width == 0 || cfg.height == 0 || cfg.fps == 0 {
        bail!("width/height/fps must be non-zero");
    }

    let mut args: Vec<String> = vec!["-nostdin".into(), "-loglevel".into(), "error".into()];
    if input.starts_with("rtsp://") {
        // TCP transport avoids UDP packet loss artifacts on home networks.
        args.extend(["-rtsp_transport".into(), "tcp".into()]);
    } else if input.starts_with("/dev/") {
        args.extend(["-f".into(), "v4l2".into()]);
    }
    args.extend(["-i".into(), input.to_string()]);
    args.extend([
        "-vf".into(),
        format!("fps={},scale={}:{}", cfg.fps, cfg.width, cfg.height),
        "-pix_fmt".into(),
        "rgb24".into(),
        "-f".into(),
        "rawvideo".into(),
        "-".into(),
    ]);
    Ok(args)
}

/// Frame source over an ffmpeg child, killed on drop so an ended pipeline leaks no process.
pub struct FfmpegFrameSource {
    // Held for its Drop (kill_on_drop) — never polled directly.
    _child: Child,
    stdout: BufReader<ChildStdout>,
    width: u32,
    height: u32,
    frame_len: usize,
}

impl FfmpegFrameSource {
    /// Spawn ffmpeg; a missing binary or bad config fails here, stream errors in `next_frame`.
    pub fn spawn(cfg: &CaptureConfig) -> Result<Self> {
        let args = build_args(cfg)?;
        let mut child = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("failed to spawn ffmpeg — is it installed and on PATH?")?;
        let stdout = child
            .stdout
            .take()
            .context("ffmpeg stdout was not captured")?;
        Ok(Self {
            _child: child,
            stdout: BufReader::new(stdout),
            width: cfg.width,
            height: cfg.height,
            frame_len: Frame::expected_len(cfg.width, cfg.height),
        })
    }
}

#[async_trait]
impl FrameSource for FfmpegFrameSource {
    async fn next_frame(&mut self) -> Result<Option<Frame>> {
        let mut buf = vec![0u8; self.frame_len];
        match self.stdout.read_exact(&mut buf).await {
            Ok(_) => Ok(Some(Frame {
                width: self.width,
                height: self.height,
                rgb: buf,
                captured_at: Utc::now(),
            })),
            // Clean end of stream (camera disconnected / ffmpeg exited).
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e).context("reading frame from ffmpeg"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtsp_args_use_tcp_transport() {
        let args = build_args(&CaptureConfig {
            input: "rtsp://cam.local/stream".into(),
            ..Default::default()
        })
        .unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("-rtsp_transport tcp"));
        assert!(joined.contains("-i rtsp://cam.local/stream"));
        assert!(joined.contains("fps=2,scale=640:360"));
        assert!(joined.ends_with("-f rawvideo -"));
    }

    #[test]
    fn v4l2_device_args() {
        let args = build_args(&CaptureConfig {
            input: "/dev/video0".into(),
            ..Default::default()
        })
        .unwrap();
        let joined = args.join(" ");
        assert!(joined.contains("-f v4l2 -i /dev/video0"));
    }

    #[test]
    fn rejects_empty_option_like_and_zero_config() {
        assert!(
            build_args(&CaptureConfig::default()).is_err(),
            "empty input"
        );
        assert!(
            build_args(&CaptureConfig {
                input: "-lavfi_something_evil".into(),
                ..Default::default()
            })
            .is_err(),
            "option injection"
        );
        assert!(build_args(&CaptureConfig {
            input: "rtsp://x".into(),
            fps: 0,
            ..Default::default()
        })
        .is_err());
    }
}
