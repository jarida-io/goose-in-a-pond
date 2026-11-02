//! Opens a camera by the cheapest path this host and camera allow: a Jetson's hardware engines
//! for whichever format the camera offers, then the camera's own JPEGs through ffmpeg, then ffmpeg
//! decoding every frame.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use chrono::Utc;
use pond_core::user_data::domain::vision::{CapturedFrame, Frame, JpegFrame, LumaPlane, FPS_RANGE};
use pond_core::user_data::ports::vision::FrameSource;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::ffmpeg_source::{checked_input, resolve_ffmpeg_bin, CaptureConfig, FfmpegFrameSource};
use crate::gst::{self, HardwareHead, JetsonEngines, JpegTransport, RawFormat, VideoCodec};
use crate::jpeg_stream::{JpegSplitter, MultipartParser, Part, RateGate};
use crate::live_feed::CapturePath;
use crate::process::CaptureProcess;

/// A path tried before a cheaper fallback must show a frame within this, or the next is tried.
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

/// ffmpeg reads a network stream's header within this, or the stream is treated as unknown.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Bytes read from a capture pipe at a time; about one camera JPEG.
const READ_CHUNK: usize = 64 * 1024;

/// A camera opened for the pipeline, and the path its frames take.
pub struct OpenedSource {
    pub source: Box<dyn FrameSource>,
    pub capture: CapturePath,
}

pub type OpenFuture = Pin<Box<dyn Future<Output = Result<OpenedSource>> + Send>>;

/// Probes the camera, then starts the cheapest path that delivers a frame.
pub async fn open_capture(cfg: CaptureConfig) -> Result<OpenedSource> {
    let input = checked_input(&cfg)?.to_string();
    let ffmpeg = resolve_ffmpeg_bin();
    let kind = InputKind::of(&input);
    let probe = match kind {
        InputKind::V4l2 => Probe::Formats(probe_formats(&ffmpeg, &input).await),
        _ => Probe::Stream(probe_stream(&ffmpeg, &input, kind).await),
    };
    let engines = gst::jetson_engines().await;
    let candidates = plan(&cfg, &input, kind, &probe, engines);

    let last = candidates.len() - 1;
    for (i, candidate) in candidates.into_iter().enumerate() {
        let capture = candidate.path();
        let via = candidate.describe();
        let mut source = match start(&candidate, &cfg, &ffmpeg, engines) {
            Ok(source) => source,
            Err(e) if i < last => {
                tracing::warn!(capture = capture.as_str(), %via, error = %format!("{e:#}"), "capture path could not start; trying the next");
                continue;
            }
            Err(e) => return Err(e),
        };
        // The last path is not trialled: its failures surface as the stream ending, with backoff.
        if i == last {
            return Ok(OpenedSource { source, capture });
        }
        let why = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, source.next_frame()).await {
            Ok(Ok(Some(first))) => {
                tracing::info!(capture = capture.as_str(), %via, "vision capture path chosen");
                return Ok(OpenedSource {
                    source: Box::new(Primed {
                        first: Some(first),
                        inner: source,
                    }),
                    capture,
                });
            }
            Ok(Ok(None)) => "the stream ended before a frame".to_string(),
            Ok(Err(e)) => format!("{e:#}"),
            Err(_) => format!("no frame within {}s", FIRST_FRAME_TIMEOUT.as_secs()),
        };
        source.close().await;
        tracing::warn!(capture = capture.as_str(), %via, %why, "capture path did not start; trying the next");
    }
    Err(anyhow!("no capture path could start"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    V4l2,
    Rtsp,
    Http,
    File,
    OtherUrl,
}

impl InputKind {
    fn of(input: &str) -> Self {
        let lower = input.to_ascii_lowercase();
        if input.starts_with("/dev/") {
            Self::V4l2
        } else if lower.starts_with("rtsp://") || lower.starts_with("rtsps://") {
            Self::Rtsp
        } else if lower.starts_with("http://") || lower.starts_with("https://") {
            Self::Http
        } else if input.contains("://") {
            Self::OtherUrl
        } else {
            Self::File
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Probe {
    /// The formats a V4L2 camera offers, in the order it lists them.
    Formats(Vec<CameraFormat>),
    /// The first video stream of a URL or file, when ffmpeg could read it.
    Stream(Option<StreamInfo>),
}

/// One pixel format a V4L2 camera offers, and its sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CameraFormat {
    kind: FormatKind,
    sizes: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatKind {
    Mjpeg,
    Video(VideoCodec),
    Raw(RawFormat),
    /// A sensor's own output; only the Jetson's ISP, through Argus, turns it into pictures.
    Bayer,
}

impl FormatKind {
    /// Cheapest first: a camera's JPEG crosses USB smallest and keeps the live picture for free.
    fn cost(self) -> u8 {
        match self {
            Self::Mjpeg => 0,
            Self::Video(_) => 1,
            Self::Raw(_) => 2,
            Self::Bayer => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StreamInfo {
    codec: String,
    size: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
enum Candidate {
    Hardware {
        head: HardwareHead,
        live: (u32, u32),
        luma: (u32, u32),
        /// The detector's picture, [`gst::ANALYSIS_WIDTH`] wide at the live picture's shape.
        analysis: (u32, u32),
    },
    CameraJpeg {
        args: Vec<String>,
    },
    Software,
}

impl Candidate {
    fn path(&self) -> CapturePath {
        match self {
            Candidate::Hardware { .. } => CapturePath::Hardware,
            Candidate::CameraJpeg { .. } => CapturePath::CameraJpeg,
            Candidate::Software => CapturePath::Software,
        }
    }

    /// The source a path reads, for logs.
    fn describe(&self) -> String {
        match self {
            Candidate::Hardware { head, .. } => head.describe(),
            Candidate::CameraJpeg { .. } => "camera JPEG through ffmpeg".to_string(),
            Candidate::Software => "ffmpeg decoding every frame".to_string(),
        }
    }
}

/// Cheapest first; always ends with [`Candidate::Software`].
fn plan(
    cfg: &CaptureConfig,
    input: &str,
    kind: InputKind,
    probe: &Probe,
    engines: &JetsonEngines,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    let hardware =
        |out: &mut Vec<Candidate>, head: HardwareHead, live: (u32, u32), luma: (u32, u32)| {
            if engines.runs(&head) {
                out.push(Candidate::Hardware {
                    head,
                    live,
                    luma,
                    analysis: scaled(live, gst::ANALYSIS_WIDTH),
                });
            }
        };
    match probe {
        Probe::Formats(formats) => {
            let mut by_cost: Vec<&CameraFormat> = formats.iter().collect();
            by_cost.sort_by_key(|f| f.kind.cost());
            // One head per kind of format: a second raw format would fail the same way.
            by_cost.dedup_by_key(|f| f.kind.cost());
            let mut mjpeg_size = None;
            for format in by_cost {
                let Some((width, height)) = pick_size(&format.sizes, (cfg.width, cfg.height))
                else {
                    continue;
                };
                let device = input.to_string();
                let head = match format.kind {
                    FormatKind::Mjpeg => {
                        mjpeg_size = Some((width, height));
                        HardwareHead::MjpegCamera {
                            device,
                            width,
                            height,
                        }
                    }
                    FormatKind::Video(codec) => HardwareHead::VideoCamera {
                        device,
                        codec,
                        width,
                        height,
                    },
                    FormatKind::Raw(format) => HardwareHead::RawCamera {
                        device,
                        format,
                        width,
                        height,
                    },
                    FormatKind::Bayer => HardwareHead::Csi {
                        sensor_id: csi_sensor_id(input),
                        width,
                        height,
                    },
                };
                let size = (width, height);
                hardware(&mut out, head, size, scaled(size, gst::LUMA_WIDTH));
            }
            if let Some(size) = mjpeg_size {
                out.push(Candidate::CameraJpeg {
                    args: camera_jpeg_args(input, kind, Some(size)),
                });
            }
        }
        Probe::Stream(Some(info)) if info.codec == "mjpeg" => {
            let transport = match kind {
                InputKind::Http => Some(JpegTransport::Http),
                InputKind::Rtsp => Some(JpegTransport::Rtsp),
                _ => None,
            };
            if let Some(transport) = transport {
                let source = info.size.unwrap_or((cfg.width, cfg.height));
                let head = HardwareHead::NetworkMjpeg {
                    url: input.to_string(),
                    transport,
                };
                hardware(&mut out, head, source, scaled(source, gst::LUMA_WIDTH));
            }
            out.push(Candidate::CameraJpeg {
                args: camera_jpeg_args(input, kind, None),
            });
        }
        Probe::Stream(Some(info)) => {
            let head = match (kind, VideoCodec::from_ffmpeg(&info.codec)) {
                (InputKind::Rtsp, Some(codec)) => Some(HardwareHead::Rtsp {
                    url: input.to_string(),
                    codec,
                }),
                (InputKind::File, Some(_)) => Some(HardwareHead::File {
                    path: input.to_string(),
                }),
                _ => None,
            };
            if let Some(head) = head {
                let source = info.size.unwrap_or((cfg.width, cfg.height));
                hardware(
                    &mut out,
                    head,
                    scaled(source, cfg.width),
                    scaled(source, gst::LUMA_WIDTH),
                );
            }
        }
        Probe::Stream(None) => {}
    }
    out.push(Candidate::Software);
    out
}

/// Argus's sensor index, assumed to match the `/dev/videoN` number.
fn csi_sensor_id(device: &str) -> u32 {
    let digits = device
        .trim_end_matches(|c: char| !c.is_ascii_digit())
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()
        .unwrap_or("");
    digits.parse().unwrap_or(0)
}

fn start(
    candidate: &Candidate,
    cfg: &CaptureConfig,
    ffmpeg: &std::ffi::OsStr,
    engines: &JetsonEngines,
) -> Result<Box<dyn FrameSource>> {
    Ok(match candidate {
        Candidate::Hardware {
            head,
            live,
            luma,
            analysis,
        } => {
            let Some(launch) = engines.gst_launch.as_deref() else {
                bail!("gst-launch-1.0 is not installed");
            };
            let args = gst::pipeline_args(head, cfg.fps, *live, *luma, *analysis)?;
            let (process, pictures) = CaptureProcess::spawn_with_second_stream(
                launch.as_os_str(),
                &args,
                "gst-launch-1.0",
                &cfg.input,
            )?;
            Box::new(HardwareSource::new(
                process,
                Some(Box::new(pictures)),
                *luma,
                *analysis,
                cfg.fps,
            ))
        }
        Candidate::CameraJpeg { args } => {
            let process = CaptureProcess::spawn(ffmpeg, args, "ffmpeg", &cfg.input)?;
            Box::new(CameraJpegSource::new(process, cfg.fps))
        }
        Candidate::Software => Box::new(FfmpegFrameSource::spawn(cfg)?),
    })
}

/// `width` wide at `source`'s aspect, with an even height as the scalers want.
fn scaled(source: (u32, u32), width: u32) -> (u32, u32) {
    let (sw, sh) = (source.0.max(1) as u64, source.1.max(1) as u64);
    let height = ((width as u64 * sh + sw / 2) / sw).max(2) as u32;
    (width, height + height % 2)
}

/// The exact size, else the smallest that covers it, else the largest offered.
fn pick_size(sizes: &[(u32, u32)], target: (u32, u32)) -> Option<(u32, u32)> {
    let area = |s: &(u32, u32)| s.0 as u64 * s.1 as u64;
    if sizes.contains(&target) {
        return Some(target);
    }
    sizes
        .iter()
        .filter(|s| s.0 >= target.0 && s.1 >= target.1)
        .min_by_key(|s| area(s))
        .or_else(|| sizes.iter().max_by_key(|s| area(s)))
        .copied()
}

/// The camera is asked for its full rate: UVC cameras halve it in dim light to expose longer,
/// and [`RateGate`] drops whatever exceeds the setting.
fn camera_jpeg_args(input: &str, kind: InputKind, size: Option<(u32, u32)>) -> Vec<String> {
    let mut args: Vec<String> = vec!["-nostdin".into(), "-loglevel".into(), "error".into()];
    match kind {
        InputKind::V4l2 => {
            args.extend([
                "-f".into(),
                "v4l2".into(),
                "-input_format".into(),
                "mjpeg".into(),
            ]);
            if let Some((w, h)) = size {
                args.extend(["-video_size".into(), format!("{w}x{h}")]);
            }
            args.extend(["-framerate".into(), FPS_RANGE.end().to_string()]);
        }
        InputKind::Rtsp => args.extend(["-rtsp_transport".into(), "tcp".into()]),
        _ => {}
    }
    args.extend(["-i".into(), input.to_string()]);
    args.extend(
        [
            "-map",
            "0:v:0",
            "-c:v",
            "copy",
            "-f",
            "mjpeg",
            "-flush_packets",
            "1",
            "-",
        ]
        .map(String::from),
    );
    args
}

async fn probe_output(ffmpeg: &std::ffi::OsStr, args: &[String]) -> Option<String> {
    let run = Command::new(ffmpeg)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(PROBE_TIMEOUT, run).await {
        Ok(Ok(out)) => Some(String::from_utf8_lossy(&out.stderr).into_owned()),
        _ => None,
    }
}

async fn probe_formats(ffmpeg: &std::ffi::OsStr, device: &str) -> Vec<CameraFormat> {
    let args = [
        "-hide_banner",
        "-nostdin",
        "-f",
        "v4l2",
        "-list_formats",
        "all",
        "-i",
    ]
    .iter()
    .map(|s| s.to_string())
    .chain([device.to_string()])
    .collect::<Vec<_>>();
    probe_output(ffmpeg, &args)
        .await
        .map(|text| parse_formats(&text))
        .unwrap_or_default()
}

async fn probe_stream(
    ffmpeg: &std::ffi::OsStr,
    input: &str,
    kind: InputKind,
) -> Option<StreamInfo> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into()];
    if kind == InputKind::Rtsp {
        args.extend(["-rtsp_transport".into(), "tcp".into()]);
    }
    args.extend(["-i".into(), input.to_string()]);
    probe_output(ffmpeg, &args)
        .await
        .and_then(|text| parse_stream_info(&text))
}

/// From `ffmpeg -list_formats all`, a line per format:
/// `[in#0 @ 0x..] Raw       :     yuyv422 :           YUYV 4:2:2 : 640x480 160x90 ...`.
fn parse_formats(text: &str) -> Vec<CameraFormat> {
    text.lines()
        .filter_map(|line| {
            let body = line.split_once("] ").map_or(line, |(_, rest)| rest);
            let (category, rest) = body.split_once(':')?;
            if !matches!(category.trim(), "Raw" | "Compressed" | "Unsupported") {
                return None;
            }
            // A description such as `YUYV 4:2:2` holds colons, but never one between spaces.
            let mut fields = rest.split(" : ");
            let name = fields.next()?.trim();
            let description = fields.next()?.trim();
            let kind = if name == "mjpeg" {
                FormatKind::Mjpeg
            } else if let Some(codec) = VideoCodec::from_ffmpeg(name) {
                FormatKind::Video(codec)
            } else if let Some(raw) = RawFormat::from_ffmpeg(name) {
                FormatKind::Raw(raw)
            } else if name.starts_with("bayer_") || description.contains("Bayer") {
                FormatKind::Bayer
            } else {
                return None;
            };
            let sizes = fields
                .next()
                .unwrap_or("")
                .split_whitespace()
                .filter_map(parse_size)
                .collect();
            Some(CameraFormat { kind, sizes })
        })
        .collect()
}

/// From ffmpeg's input dump: `Stream #0:0: Video: h264 (High), yuv420p, 1920x1080 [SAR 1:1]`.
fn parse_stream_info(text: &str) -> Option<StreamInfo> {
    let line = text.lines().find(|l| l.contains("Video: "))?;
    let rest = &line[line.find("Video: ")? + "Video: ".len()..];
    let codec = rest
        .split([' ', ','])
        .next()
        .filter(|c| !c.is_empty())?
        .to_ascii_lowercase();
    let size = rest
        .split([' ', ',', '[', ']', '(', ')'])
        .find_map(parse_size);
    Some(StreamInfo { codec, size })
}

fn parse_size(token: &str) -> Option<(u32, u32)> {
    // `0x31637661` in a codec tag is hex, not a size.
    if token.starts_with("0x") {
        return None;
    }
    let (w, h) = token.split_once('x')?;
    let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
    (w >= 16 && h >= 16).then_some((w, h))
}

/// Hands out the frame a path proved itself with before reading on.
struct Primed {
    first: Option<CapturedFrame>,
    inner: Box<dyn FrameSource>,
}

#[async_trait]
impl FrameSource for Primed {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        match self.first.take() {
            Some(first) => Ok(Some(first)),
            None => self.inner.next_frame().await,
        }
    }

    async fn close(&mut self) {
        self.inner.close().await;
    }
}

/// Reads more of a capture pipe into `buf`; `Ok(0)` at its end.
async fn read_more(process: &mut CaptureProcess, buf: &mut Vec<u8>) -> Result<usize> {
    buf.reserve(READ_CHUNK);
    Ok(process.stdout.read_buf(buf).await?)
}

/// The camera's own JPEGs from `ffmpeg -c:v copy -f mjpeg`.
struct CameraJpegSource {
    process: CaptureProcess,
    splitter: JpegSplitter,
    gate: RateGate,
}

impl CameraJpegSource {
    fn new(process: CaptureProcess, fps: u32) -> Self {
        Self {
            process,
            splitter: JpegSplitter::default(),
            gate: RateGate::new(fps),
        }
    }
}

#[async_trait]
impl FrameSource for CameraJpegSource {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        loop {
            while let Some(jpeg) = self.splitter.next_jpeg()? {
                if self.gate.admit(Instant::now()) {
                    return Ok(Some(CapturedFrame::Jpeg(JpegFrame {
                        jpeg: Arc::new(jpeg),
                        luma: None,
                        analysis: None,
                        captured_at: Utc::now(),
                    })));
                }
            }
            if read_more(&mut self.process, self.splitter.buffer_mut()).await? == 0 {
                bail!("camera stream ended ({})", self.process.exit_reason().await);
            }
        }
    }

    async fn close(&mut self) {
        self.process.close().await;
    }
}

/// gst-launch's multipart stream: each live JPEG paired with the luma VIC scaled from it. The
/// detector's RGBA pictures come on a stream of their own, and the latest rides on the next pair.
struct HardwareSource {
    process: CaptureProcess,
    parser: MultipartParser,
    luma_size: (u32, u32),
    analysis_size: (u32, u32),
    // videorate let 20+ fps through at max-rate=5 on a live H.264 RTSP stream.
    gate: RateGate,
    picture_gate: RateGate,
    jpeg: Option<Vec<u8>>,
    luma: Option<Vec<u8>>,
    pictures: Option<Pictures>,
}

/// The newest detector picture, kept by a task reading the pictures' stream as fast as it comes.
struct Pictures {
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    reader: tokio::task::JoinHandle<()>,
}

type PictureStream = Box<dyn AsyncRead + Send + Unpin>;

impl HardwareSource {
    fn new(
        process: CaptureProcess,
        pictures: Option<PictureStream>,
        luma_size: (u32, u32),
        analysis_size: (u32, u32),
        fps: u32,
    ) -> Self {
        let pictures = pictures.map(|stream| {
            let latest = Arc::new(Mutex::new(None));
            let reader = tokio::spawn(read_pictures(stream, analysis_size, latest.clone()));
            Pictures { latest, reader }
        });
        Self {
            process,
            parser: MultipartParser::new(gst::BOUNDARY),
            luma_size,
            analysis_size,
            gate: RateGate::new(fps),
            picture_gate: RateGate::new(gst::ANALYSIS_FPS),
            jpeg: None,
            luma: None,
            pictures,
        }
    }

    /// The newest picture when the detector is due one.
    fn take_picture(&mut self, now: Instant) -> Option<Frame> {
        let mut latest = self.pictures.as_ref()?.latest.lock().ok()?;
        if latest.is_none() || !self.picture_gate.admit(now) {
            return None;
        }
        latest
            .take()
            .map(|rgba| rgba_to_frame(&rgba, self.analysis_size))
    }
}

impl Drop for HardwareSource {
    fn drop(&mut self) {
        if let Some(pictures) = &self.pictures {
            pictures.reader.abort();
        }
    }
}

/// Keeps the newest RGBA picture of `size` from `stream` until it ends or stops making sense.
async fn read_pictures(
    mut stream: PictureStream,
    (width, height): (u32, u32),
    latest: Arc<Mutex<Option<Vec<u8>>>>,
) {
    let mut parser = MultipartParser::new(gst::BOUNDARY);
    loop {
        loop {
            match parser.next_part() {
                Ok(Some(Part::Raw(rgba))) if rgba.len() == (width * height * 4) as usize => {
                    if let Ok(mut slot) = latest.lock() {
                        *slot = Some(rgba);
                    }
                }
                Ok(Some(_)) | Err(_) => {
                    tracing::warn!(
                        "the detector's pictures are not {width}x{height} RGBA; people are not followed"
                    );
                    return;
                }
                Ok(None) => break,
            }
        }
        let buf = parser.buffer_mut();
        buf.reserve(READ_CHUNK);
        if !matches!(stream.read_buf(buf).await, Ok(n) if n > 0) {
            return;
        }
    }
}

/// RGBA from VIC as the packed RGB the detector reads.
fn rgba_to_frame(rgba: &[u8], (width, height): (u32, u32)) -> Frame {
    let rgb = rgba
        .chunks_exact(4)
        .flat_map(|px| [px[0], px[1], px[2]])
        .collect();
    Frame {
        width,
        height,
        rgb,
        captured_at: Utc::now(),
    }
}

#[async_trait]
impl FrameSource for HardwareSource {
    async fn next_frame(&mut self) -> Result<Option<CapturedFrame>> {
        let (w, h) = self.luma_size;
        loop {
            while let Some(part) = self.parser.next_part()? {
                // A part whose partner never came is replaced by the next of its kind.
                match part {
                    Part::Jpeg(bytes) => self.jpeg = Some(bytes),
                    Part::Raw(bytes) if bytes.len() == (w * h) as usize => self.luma = Some(bytes),
                    Part::Raw(bytes) => bail!(
                        "the hardware pipeline sent {} raw bytes, expected luma {}x{}",
                        bytes.len(),
                        w,
                        h
                    ),
                }
                let now = Instant::now();
                match (self.jpeg.take(), self.luma.take()) {
                    (Some(_), Some(_)) if !self.gate.admit(now) => {}
                    (Some(jpeg), Some(pixels)) => {
                        let analysis = self.take_picture(now);
                        return Ok(Some(CapturedFrame::Jpeg(JpegFrame {
                            jpeg: Arc::new(jpeg),
                            luma: Some(LumaPlane {
                                width: w,
                                height: h,
                                pixels,
                            }),
                            analysis,
                            captured_at: Utc::now(),
                        })));
                    }
                    (jpeg, luma) => {
                        self.jpeg = jpeg;
                        self.luma = luma;
                    }
                }
            }
            if read_more(&mut self.process, self.parser.buffer_mut()).await? == 0 {
                bail!("camera stream ended ({})", self.process.exit_reason().await);
            }
        }
    }

    async fn close(&mut self) {
        self.process.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C920: &str = "[in#0 @ 0xaaab21335890] Raw       :     yuyv422 :           YUYV 4:2:2 : 640x480 160x90 1280x720 1920x1080\n\
        [in#0 @ 0xaaab21335890] Compressed:       mjpeg :          Motion-JPEG : 640x480 160x90 640x360 1280x720 1920x1080\n\
        Error opening input file /dev/video0.\n";

    fn cfg(input: &str) -> CaptureConfig {
        CaptureConfig {
            input: input.into(),
            fps: 15,
            ..Default::default()
        }
    }

    fn jetson() -> JetsonEngines {
        JetsonEngines {
            gst_launch: Some("/usr/bin/gst-launch-1.0".into()),
            elements: gst::ELEMENTS.to_vec(),
        }
    }

    fn format(kind: FormatKind, sizes: &[(u32, u32)]) -> CameraFormat {
        CameraFormat {
            kind,
            sizes: sizes.to_vec(),
        }
    }

    #[test]
    fn every_format_is_read_with_its_sizes() {
        assert_eq!(
            parse_formats(C920),
            vec![
                format(
                    FormatKind::Raw(RawFormat::Yuy2),
                    &[(640, 480), (160, 90), (1280, 720), (1920, 1080)]
                ),
                format(
                    FormatKind::Mjpeg,
                    &[(640, 480), (160, 90), (640, 360), (1280, 720), (1920, 1080)]
                ),
            ]
        );
        // Older ffmpeg names the demuxer instead of the input.
        let old = "[video4linux2,v4l2 @ 0x55] Compressed:       mjpeg :          Motion-JPEG : 1280x720 640x480";
        assert_eq!(parse_formats(old)[0].sizes, vec![(1280, 720), (640, 480)]);
        let h264 =
            "[in#0 @ 0x1] Compressed:        h264 :                H.264 : 1920x1080 1280x720";
        assert_eq!(
            parse_formats(h264)[0].kind,
            FormatKind::Video(VideoCodec::H264)
        );
        let bayer =
            "[in#0 @ 0x1] Unsupported :        RG10 : 10-bit Bayer RGRG/GBGB : 3280x2464 1920x1080";
        assert_eq!(
            parse_formats(bayer),
            vec![format(FormatKind::Bayer, &[(3280, 2464), (1920, 1080)])]
        );
        assert!(
            parse_formats("[in#0 @ 0x1] Raw : rgb565le : RGB565 : 640x480").is_empty(),
            "a format no path reads"
        );
    }

    #[test]
    fn the_closest_mjpeg_size_is_chosen() {
        let target = (640, 360);
        assert_eq!(
            pick_size(&[(1280, 720), (640, 360)], target),
            Some((640, 360))
        );
        assert_eq!(
            pick_size(&[(1920, 1080), (640, 480), (320, 240)], target),
            Some((640, 480)),
            "smallest that covers it"
        );
        assert_eq!(
            pick_size(&[(320, 240), (160, 120)], target),
            Some((320, 240))
        );
        assert_eq!(pick_size(&[], target), None);
    }

    #[test]
    fn stream_info_reads_codec_and_size_and_skips_hex_tags() {
        let h264 = "  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 1920x1080 [SAR 1:1 DAR 16:9], 30 fps";
        assert_eq!(
            parse_stream_info(h264),
            Some(StreamInfo {
                codec: "h264".into(),
                size: Some((1920, 1080))
            })
        );
        let mjpeg = "  Stream #0:0: Video: mjpeg (Baseline), yuvj420p(pc, bt470bg/unknown/unknown), 640x360, 25 tbr";
        assert_eq!(parse_stream_info(mjpeg).unwrap().codec, "mjpeg");
        assert_eq!(parse_stream_info("Connection refused"), None);
    }

    #[test]
    fn a_usb_camera_on_a_jetson_tries_its_jpeg_then_its_raw_frames_on_the_engines() {
        let probe = Probe::Formats(parse_formats(C920));
        let plan = plan(
            &cfg("/dev/video0"),
            "/dev/video0",
            InputKind::V4l2,
            &probe,
            &jetson(),
        );
        let paths: Vec<_> = plan.iter().map(Candidate::path).collect();
        assert_eq!(
            paths,
            vec![
                CapturePath::Hardware,
                CapturePath::Hardware,
                CapturePath::CameraJpeg,
                CapturePath::Software
            ]
        );
        assert_eq!(
            plan[0],
            Candidate::Hardware {
                head: HardwareHead::MjpegCamera {
                    device: "/dev/video0".into(),
                    width: 640,
                    height: 360
                },
                live: (640, 360),
                luma: (128, 72),
                analysis: (416, 234),
            }
        );
        assert_eq!(
            plan[1],
            Candidate::Hardware {
                head: HardwareHead::RawCamera {
                    device: "/dev/video0".into(),
                    format: RawFormat::Yuy2,
                    width: 640,
                    height: 480
                },
                live: (640, 480),
                luma: (128, 96),
                analysis: (416, 312),
            },
            "the raw sizes lack 640x360, so the smallest that covers it"
        );
        let Candidate::CameraJpeg { args } = &plan[2] else {
            unreachable!()
        };
        assert_eq!(
            args.join(" "),
            "-nostdin -loglevel error -f v4l2 -input_format mjpeg -video_size 640x360 -framerate 30 \
             -i /dev/video0 -map 0:v:0 -c:v copy -f mjpeg -flush_packets 1 -"
        );
    }

    #[test]
    fn elsewhere_a_usb_camera_uses_its_jpeg_through_ffmpeg() {
        let probe = Probe::Formats(parse_formats(C920));
        let plan = plan(
            &cfg("/dev/video0"),
            "/dev/video0",
            InputKind::V4l2,
            &probe,
            &JetsonEngines::default(),
        );
        let paths: Vec<_> = plan.iter().map(Candidate::path).collect();
        assert_eq!(paths, vec![CapturePath::CameraJpeg, CapturePath::Software]);

        let nothing = Probe::Formats(vec![]);
        let plan = plan_paths("/dev/video0", InputKind::V4l2, &nothing, &jetson());
        assert_eq!(plan, vec![CapturePath::Software]);
    }

    #[test]
    fn a_camera_without_jpeg_still_reaches_the_engines() {
        let raw_only = Probe::Formats(vec![format(
            FormatKind::Raw(RawFormat::Uyvy),
            &[(1280, 720), (640, 360)],
        )]);
        assert_eq!(
            plan_paths("/dev/video2", InputKind::V4l2, &raw_only, &jetson()),
            vec![CapturePath::Hardware, CapturePath::Software]
        );

        // Listed raw first, but NVDEC takes the camera's own H.264 ahead of VIC converting raw.
        let h264 = Probe::Formats(vec![
            format(FormatKind::Raw(RawFormat::Yuy2), &[(640, 360)]),
            format(
                FormatKind::Video(VideoCodec::H264),
                &[(1920, 1080), (640, 360)],
            ),
            format(FormatKind::Raw(RawFormat::Nv12), &[(640, 360)]),
        ]);
        let plan = plan(
            &cfg("/dev/video2"),
            "/dev/video2",
            InputKind::V4l2,
            &h264,
            &jetson(),
        );
        assert_eq!(
            plan[0],
            Candidate::Hardware {
                head: HardwareHead::VideoCamera {
                    device: "/dev/video2".into(),
                    codec: VideoCodec::H264,
                    width: 640,
                    height: 360
                },
                live: (640, 360),
                luma: (128, 72),
                analysis: (416, 234),
            }
        );
        assert!(matches!(
            &plan[1],
            Candidate::Hardware {
                head: HardwareHead::RawCamera {
                    format: RawFormat::Yuy2,
                    ..
                },
                ..
            }
        ));
        assert_eq!(
            plan.len(),
            3,
            "one raw head, then the full decode: {plan:?}"
        );
    }

    #[test]
    fn a_csi_sensor_goes_through_argus_or_nowhere() {
        let bayer = Probe::Formats(vec![format(
            FormatKind::Bayer,
            &[(3280, 2464), (1920, 1080), (1280, 720)],
        )]);
        let plan = plan(
            &cfg("/dev/video1"),
            "/dev/video1",
            InputKind::V4l2,
            &bayer,
            &jetson(),
        );
        assert_eq!(
            plan[0],
            Candidate::Hardware {
                head: HardwareHead::Csi {
                    sensor_id: 1,
                    width: 1280,
                    height: 720
                },
                live: (1280, 720),
                luma: (128, 72),
                analysis: (416, 234),
            }
        );
        let mut no_argus = jetson();
        no_argus.elements.retain(|e| *e != "nvarguscamerasrc");
        assert_eq!(
            plan_paths("/dev/video1", InputKind::V4l2, &bayer, &no_argus),
            vec![CapturePath::Software]
        );
    }

    /// Reads `n` frames through one planned path as `open_capture` trials it; the first's delay.
    async fn frames_through(
        candidate: &Candidate,
        cfg: &CaptureConfig,
        n: usize,
    ) -> Result<Duration> {
        let engines = gst::jetson_engines().await;
        let started = Instant::now();
        let mut source = start(candidate, cfg, &resolve_ffmpeg_bin(), engines)?;
        let mut first = None;
        let mut analysed = 0;
        let mut first_at = None;
        for _ in 0..n {
            let frame = tokio::time::timeout(FIRST_FRAME_TIMEOUT, source.next_frame()).await??;
            let Some(CapturedFrame::Jpeg(frame)) = frame else {
                bail!("expected a JPEG frame, got {frame:?}");
            };
            first.get_or_insert(started.elapsed());
            first_at.get_or_insert_with(Instant::now);
            let picture = crate::jpeg_decode::decode_rgb(&frame.jpeg, frame.captured_at)?;
            if let Candidate::Hardware {
                live,
                luma,
                analysis,
                ..
            } = candidate
            {
                assert_eq!((picture.width, picture.height), *live, "live picture size");
                let plane = frame.luma.expect("the hardware path pairs a luma plane");
                assert_eq!((plane.width, plane.height), *luma, "luma size");
                if let Some(picture) = frame.analysis {
                    assert_eq!((picture.width, picture.height), *analysis, "detector size");
                    assert!(picture.is_well_formed());
                    analysed += 1;
                }
            }
        }
        source.close().await;
        if matches!(candidate, Candidate::Hardware { .. }) {
            assert!(analysed > 0, "no detector picture in {n} frames");
            // A branch that stalls the mux still lets a frame through now and then.
            let rate =
                (n - 1) as f64 / first_at.expect("n is at least one").elapsed().as_secs_f64();
            assert!(
                rate >= f64::from(cfg.fps) / 3.0,
                "{:.1} fps against {} asked",
                rate,
                cfg.fps
            );
        }
        Ok(first.expect("n is at least one"))
    }

    #[tokio::test]
    #[ignore = "needs a Jetson and a V4L2 camera nothing else holds; GIAP_TEST_CAMERA picks one"]
    async fn every_hardware_head_this_camera_offers_delivers_frames() {
        let device = std::env::var("GIAP_TEST_CAMERA").unwrap_or_else(|_| "/dev/video0".into());
        let cfg = CaptureConfig {
            input: device.clone(),
            fps: 15,
            ..Default::default()
        };
        let formats = probe_formats(&resolve_ffmpeg_bin(), &device).await;
        assert!(!formats.is_empty(), "ffmpeg listed no formats for {device}");
        let engines = gst::jetson_engines().await;
        let plan = plan(
            &cfg,
            &device,
            InputKind::V4l2,
            &Probe::Formats(formats),
            engines,
        );
        let heads: Vec<_> = plan
            .iter()
            .filter(|c| matches!(c, Candidate::Hardware { .. }))
            .collect();
        assert!(!heads.is_empty(), "no hardware head for {device}: {plan:?}");
        for candidate in heads {
            let first = frames_through(candidate, &cfg, 10)
                .await
                .unwrap_or_else(|e| panic!("{}: {e:#}", candidate.describe()));
            eprintln!(
                "{}: first frame after {} ms",
                candidate.describe(),
                first.as_millis()
            );
        }
    }

    #[tokio::test]
    #[ignore = "needs a Jetson's GStreamer and ffmpeg"]
    async fn network_mjpeg_over_http_reaches_the_engines() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/video.mjpg", listener.local_addr().unwrap());
        std::thread::spawn(move || serve_mjpeg(listener));
        let cfg = CaptureConfig {
            input: url.clone(),
            fps: 10,
            ..Default::default()
        };
        let info = probe_stream(&resolve_ffmpeg_bin(), &url, InputKind::Http).await;
        let engines = gst::jetson_engines().await;
        let plan = plan(&cfg, &url, InputKind::Http, &Probe::Stream(info), engines);
        assert!(
            matches!(
                &plan[0],
                Candidate::Hardware {
                    head: HardwareHead::NetworkMjpeg { .. },
                    ..
                }
            ),
            "{plan:?}"
        );
        let first = frames_through(&plan[0], &cfg, 10).await.unwrap();
        eprintln!(
            "{}: first frame after {} ms",
            plan[0].describe(),
            first.as_millis()
        );
    }

    /// Sends every client an endless multipart stream of 640x480 JPEGs, ten a second.
    fn serve_mjpeg(listener: std::net::TcpListener) {
        use std::io::{Read, Write};
        let jpegs: Vec<Vec<u8>> = (0..10u8)
            .map(|i| {
                crate::snapshot::frame_to_jpeg_bytes(&pond_core::user_data::domain::vision::Frame {
                    width: 640,
                    height: 480,
                    rgb: vec![i * 20; 640 * 480 * 3],
                    captured_at: Utc::now(),
                })
                .unwrap()
            })
            .collect();
        for mut stream in listener.incoming().flatten() {
            let jpegs = jpegs.clone();
            std::thread::spawn(move || {
                let _ = stream.read(&mut [0u8; 2048]);
                let header = b"HTTP/1.0 200 OK\r\n\
                    Content-Type: multipart/x-mixed-replace; boundary=frame\r\n\r\n";
                if stream.write_all(header).is_err() {
                    return;
                }
                for jpeg in jpegs.iter().cycle() {
                    let part = format!(
                        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
                        jpeg.len()
                    );
                    let sent = stream
                        .write_all(part.as_bytes())
                        .and_then(|_| stream.write_all(jpeg))
                        .and_then(|_| stream.write_all(b"\r\n"));
                    if sent.is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
        }
    }

    #[test]
    fn the_sensor_index_is_the_node_number() {
        assert_eq!(csi_sensor_id("/dev/video0"), 0);
        assert_eq!(csi_sensor_id("/dev/video12"), 12);
        assert_eq!(csi_sensor_id("/dev/camera"), 0);
    }

    fn plan_paths(
        input: &str,
        kind: InputKind,
        probe: &Probe,
        engines: &JetsonEngines,
    ) -> Vec<CapturePath> {
        plan(&cfg(input), input, kind, probe, engines)
            .iter()
            .map(Candidate::path)
            .collect()
    }

    fn stream(codec: &str, size: Option<(u32, u32)>) -> Probe {
        Probe::Stream(Some(StreamInfo {
            codec: codec.into(),
            size,
        }))
    }

    #[test]
    fn network_streams_choose_by_codec() {
        let rtsp = "rtsp://u:p@cam/live";
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &stream("h264", None), &jetson()),
            vec![CapturePath::Hardware, CapturePath::Software]
        );
        assert_eq!(
            plan_paths(
                rtsp,
                InputKind::Rtsp,
                &stream("h264", None),
                &JetsonEngines::default()
            ),
            vec![CapturePath::Software]
        );
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &stream("vp9", None), &jetson()),
            vec![CapturePath::Software]
        );
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &Probe::Stream(None), &jetson()),
            vec![CapturePath::Software],
            "an unreadable stream gets the full-decode path"
        );
        let http = "http://cam.local/mjpeg";
        let mjpeg = stream("mjpeg", Some((640, 480)));
        let plan = plan(&cfg(http), http, InputKind::Http, &mjpeg, &jetson());
        assert_eq!(
            plan[0],
            Candidate::Hardware {
                head: HardwareHead::NetworkMjpeg {
                    url: http.into(),
                    transport: JpegTransport::Http
                },
                live: (640, 480),
                luma: (128, 96),
                analysis: (416, 312),
            }
        );
        assert_eq!(
            plan.iter().map(Candidate::path).collect::<Vec<_>>(),
            vec![
                CapturePath::Hardware,
                CapturePath::CameraJpeg,
                CapturePath::Software
            ]
        );
        assert_eq!(
            plan_paths(rtsp, InputKind::Rtsp, &mjpeg, &jetson()),
            vec![
                CapturePath::Hardware,
                CapturePath::CameraJpeg,
                CapturePath::Software
            ]
        );
        assert_eq!(
            plan_paths(http, InputKind::Http, &mjpeg, &JetsonEngines::default()),
            vec![CapturePath::CameraJpeg, CapturePath::Software]
        );
        assert_eq!(
            plan_paths(http, InputKind::Http, &stream("h264", None), &jetson()),
            vec![CapturePath::Software],
            "no hardware head reads HTTP video"
        );
    }

    #[test]
    fn a_4_3_stream_keeps_its_shape() {
        let plan = plan(
            &cfg("/tmp/clip.mp4"),
            "/tmp/clip.mp4",
            InputKind::File,
            &stream("hevc", Some((1280, 960))),
            &jetson(),
        );
        assert_eq!(
            plan[0],
            Candidate::Hardware {
                head: HardwareHead::File {
                    path: "/tmp/clip.mp4".into()
                },
                live: (640, 480),
                luma: (128, 96),
                analysis: (416, 312),
            }
        );
        assert_eq!(scaled((1920, 1080), 640), (640, 360));
        assert_eq!(scaled((1366, 768), 128), (128, 72));
    }

    #[test]
    fn inputs_are_classified_by_scheme() {
        assert_eq!(InputKind::of("/dev/video2"), InputKind::V4l2);
        assert_eq!(InputKind::of("RTSP://cam/x"), InputKind::Rtsp);
        assert_eq!(InputKind::of("https://cam/x"), InputKind::Http);
        assert_eq!(InputKind::of("udp://239.0.0.1:1234"), InputKind::OtherUrl);
        assert_eq!(InputKind::of("/home/nano/clip.mp4"), InputKind::File);
    }

    /// A replayed stream arrives all at once; at this rate the gate's interval is zero.
    const NO_RATE_LIMIT: u32 = u32::MAX;

    /// A stand-in for gst-launch: replays a multipart stream from a file on stdout.
    fn replay(stream: &[u8]) -> (tempfile::TempDir, CaptureProcess) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stream.bin");
        std::fs::write(&path, stream).unwrap();
        let process = CaptureProcess::spawn(
            std::ffi::OsStr::new("cat"),
            &[path.to_string_lossy().into_owned()],
            "gst-launch-1.0",
            "/dev/video0",
        )
        .unwrap();
        (dir, process)
    }

    fn part(kind: &str, data: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "--{}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n",
            gst::BOUNDARY,
            data.len()
        )
        .into_bytes();
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
        out
    }

    #[tokio::test]
    async fn the_hardware_stream_pairs_each_jpeg_with_its_luma_in_either_order() {
        let luma = |v: u8| vec![v; 4 * 2];
        let stream = [
            part("image/jpeg", b"j1"),
            part("video/x-raw", &luma(1)),
            part("video/x-raw", &luma(2)),
            part("image/jpeg", b"j2"),
            part("image/jpeg", b"lost"),
            part("image/jpeg", b"j3"),
            part("video/x-raw", &luma(3)),
        ]
        .concat();
        let (_dir, process) = replay(&stream);
        let mut source = HardwareSource::new(process, None, (4, 2), (3, 3), NO_RATE_LIMIT);
        let mut got = Vec::new();
        let ended = loop {
            match source.next_frame().await {
                Ok(Some(CapturedFrame::Jpeg(f))) => {
                    got.push((f.jpeg.to_vec(), f.luma.unwrap().pixels[0]));
                }
                Ok(other) => panic!("unexpected {other:?}"),
                Err(e) => break e,
            }
        };
        assert_eq!(
            got,
            vec![
                (b"j1".to_vec(), 1),
                (b"j2".to_vec(), 2),
                (b"j3".to_vec(), 3)
            ]
        );
        assert!(ended.to_string().contains("camera stream ended"), "{ended}");
    }

    /// Pairs from `stream`, with the pictures read off their own stream before the first pair.
    async fn with_pictures(
        stream: &[u8],
        pictures: Vec<u8>,
    ) -> (tempfile::TempDir, HardwareSource) {
        let (dir, process) = replay(stream);
        let pictures: PictureStream = Box::new(std::io::Cursor::new(pictures));
        let mut source =
            HardwareSource::new(process, Some(pictures), (4, 2), (3, 3), NO_RATE_LIMIT);
        (&mut source.pictures.as_mut().unwrap().reader)
            .await
            .unwrap();
        (dir, source)
    }

    fn pairs(n: u8) -> Vec<u8> {
        (1..=n)
            .flat_map(|i| [part("image/jpeg", &[i]), part("video/x-raw", &[i; 8])].concat())
            .collect()
    }

    #[tokio::test]
    async fn the_newest_picture_rides_on_the_next_pair_without_its_alpha() {
        let older = vec![9; 36];
        let newer: Vec<u8> = (0..36).collect();
        let pictures = [part("video/x-raw", &older), part("video/x-raw", &newer)].concat();
        let (_dir, mut source) = with_pictures(&pairs(2), pictures).await;
        let mut seen = Vec::new();
        while let Ok(Some(CapturedFrame::Jpeg(f))) = source.next_frame().await {
            let analysis = f.analysis.map(|a| (a.width, a.height, a.rgb[..6].to_vec()));
            seen.push((f.jpeg.to_vec(), analysis));
        }
        assert_eq!(
            seen,
            vec![
                (vec![1], Some((3, 3, vec![0, 1, 2, 4, 5, 6]))),
                (vec![2], None),
            ]
        );
    }

    #[tokio::test]
    async fn pictures_never_come_faster_than_the_detector_looks() {
        let (_dir, mut source) = with_pictures(&pairs(2), part("video/x-raw", &[7; 36])).await;
        let first = source.next_frame().await.unwrap();
        assert!(matches!(first, Some(CapturedFrame::Jpeg(ref f)) if f.analysis.is_some()));
        *source.pictures.as_ref().unwrap().latest.lock().unwrap() = Some(vec![8; 36]);
        match source.next_frame().await.unwrap() {
            Some(CapturedFrame::Jpeg(f)) => assert!(f.analysis.is_none(), "within 1/5 s"),
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            source
                .pictures
                .as_ref()
                .unwrap()
                .latest
                .lock()
                .unwrap()
                .is_some(),
            "kept for the next pair once due"
        );
    }

    #[tokio::test]
    async fn pictures_of_the_wrong_size_stop_the_pictures_not_the_frames() {
        let (_dir, mut source) = with_pictures(&pairs(1), part("video/x-raw", &[7; 5])).await;
        match source.next_frame().await.unwrap() {
            Some(CapturedFrame::Jpeg(f)) => assert!(f.analysis.is_none()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_hardware_stream_is_held_to_the_setting_whatever_videorate_does() {
        let stream: Vec<u8> = (0..4u8)
            .flat_map(|i| [part("image/jpeg", &[i]), part("video/x-raw", &[i; 8])].concat())
            .collect();
        let (_dir, process) = replay(&stream);
        let mut source = HardwareSource::new(process, None, (4, 2), (3, 3), 1);
        match source.next_frame().await.unwrap() {
            Some(CapturedFrame::Jpeg(f)) => assert_eq!(*f.jpeg, vec![0]),
            other => panic!("unexpected {other:?}"),
        }
        assert!(
            source.next_frame().await.is_err(),
            "the three pairs that came within the second were dropped"
        );
    }

    #[tokio::test]
    async fn a_luma_part_of_the_wrong_size_is_an_error() {
        let stream = [part("image/jpeg", b"j1"), part("video/x-raw", &[0; 5])].concat();
        let (_dir, process) = replay(&stream);
        let mut source = HardwareSource::new(process, None, (4, 2), (3, 3), NO_RATE_LIMIT);
        let err = source.next_frame().await.unwrap_err().to_string();
        assert!(err.contains("expected luma 4x2"), "{err}");
    }

    #[tokio::test]
    async fn camera_jpegs_are_split_and_gated() {
        let jpeg = |seed: u8| {
            crate::snapshot::frame_to_jpeg_bytes(&pond_core::user_data::domain::vision::Frame {
                width: 16,
                height: 8,
                rgb: vec![seed; 16 * 8 * 3],
                captured_at: Utc::now(),
            })
            .unwrap()
        };
        let (a, b) = (jpeg(10), jpeg(200));
        let (_dir, process) = replay(&[a.clone(), b].concat());
        // Both arrive at once: a 1 fps gate keeps the first.
        let mut source = CameraJpegSource::new(process, 1);
        match source.next_frame().await.unwrap() {
            Some(CapturedFrame::Jpeg(f)) => {
                assert_eq!(*f.jpeg, a);
                assert!(f.luma.is_none(), "decoding is left to the pipeline");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(source.next_frame().await.is_err(), "the stream then ends");
    }

    #[tokio::test]
    async fn primed_hands_out_the_proving_frame_first() {
        let frame = CapturedFrame::Raw(pond_core::user_data::domain::vision::Frame {
            width: 1,
            height: 1,
            rgb: vec![0; 3],
            captured_at: Utc::now(),
        });
        let (_dir, process) = replay(b"");
        let mut primed = Primed {
            first: Some(frame),
            inner: Box::new(CameraJpegSource::new(process, 5)),
        };
        assert!(matches!(
            primed.next_frame().await,
            Ok(Some(CapturedFrame::Raw(_)))
        ));
        assert!(primed.next_frame().await.is_err());
        primed.close().await;
    }
}
