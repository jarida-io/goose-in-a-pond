//! gst-launch-1.0 pipelines that put a Jetson's NVDEC, NVJPG, VIC and camera ISP in the capture
//! path, and the check of which of them this host can run.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Result};
use tokio::process::Command;
use tokio::sync::OnceCell;

/// Separates the parts of both of the pipeline's streams.
pub const BOUNDARY: &str = "giapframe";

/// Where the detector's pictures leave the pipeline: fd 0, which the pond makes a pipe. In the
/// frames' mux their slower rate held every frame back, since multipartmux waits on all its pads.
pub const PICTURE_FD: u32 = 0;

/// Twice the motion grid's width, so VIC's filtered scale leaves a sample per grid cell and more.
pub const LUMA_WIDTH: u32 = 128;

/// YOLOX-Nano's input size, so the detector's letterbox never has to resize.
pub const ANALYSIS_WIDTH: u32 = 416;

/// How many detector pictures a second the pond keeps, whatever the capture rate; the pipeline
/// checks at most this often, and only while something moves or a person is being followed.
pub const ANALYSIS_FPS: u32 = 5;

/// RTSP jitter buffer; gst's 2 s default would delay every event by that much.
const RTSP_LATENCY_MS: u32 = 200;

/// Bounds each tee branch so a slow reader drops frames at the source instead of queueing them.
const QUEUE: [&str; 2] = ["queue", "max-size-buffers=4"];

/// The detector's branch drops what its reader has not taken, so it can never stall the decoder.
/// It has no videorate: one there held the C920's every branch to the picture rate.
const LEAKY_QUEUE: [&str; 3] = ["queue", "max-size-buffers=1", "leaky=downstream"];

/// Every element a pipeline below can use; each is looked up once per process.
pub(crate) const ELEMENTS: &[&str] = &[
    "v4l2src",
    "nvarguscamerasrc",
    "rtspsrc",
    "souphttpsrc",
    "multipartdemux",
    "jpegparse",
    "rtpjpegdepay",
    "rtph264depay",
    "rtph265depay",
    "h264parse",
    "h265parse",
    "parsebin",
    "nvv4l2decoder",
    "nvvidconv",
    "nvjpegenc",
    "videorate",
    "multipartmux",
];

const INSPECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    H265,
}

impl VideoCodec {
    /// From ffmpeg's codec name.
    pub fn from_ffmpeg(name: &str) -> Option<Self> {
        match name {
            "h264" => Some(Self::H264),
            "hevc" | "h265" => Some(Self::H265),
            _ => None,
        }
    }

    fn caps(self) -> &'static str {
        match self {
            Self::H264 => "video/x-h264",
            Self::H265 => "video/x-h265",
        }
    }

    fn parser(self) -> &'static str {
        match self {
            Self::H264 => "h264parse",
            Self::H265 => "h265parse",
        }
    }

    fn depayloader(self) -> &'static str {
        match self {
            Self::H264 => "rtph264depay",
            Self::H265 => "rtph265depay",
        }
    }
}

/// A camera's uncompressed pixel format that VIC converts straight from the capture buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawFormat {
    Yuy2,
    Uyvy,
    Nv12,
    I420,
    Gray8,
}

impl RawFormat {
    /// From ffmpeg's V4L2 pixel format name.
    pub fn from_ffmpeg(name: &str) -> Option<Self> {
        match name {
            "yuyv422" => Some(Self::Yuy2),
            "uyvy422" => Some(Self::Uyvy),
            "nv12" => Some(Self::Nv12),
            "yuv420p" => Some(Self::I420),
            "gray" => Some(Self::Gray8),
            _ => None,
        }
    }

    fn gst(self) -> &'static str {
        match self {
            Self::Yuy2 => "YUY2",
            Self::Uyvy => "UYVY",
            Self::Nv12 => "NV12",
            Self::I420 => "I420",
            Self::Gray8 => "GRAY8",
        }
    }
}

/// How a network camera's MJPEG arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JpegTransport {
    Http,
    Rtsp,
}

/// Where the hardware pipeline's pictures come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardwareHead {
    /// A USB camera's own MJPEG at one of its sizes; NVJPG decodes it for motion only.
    MjpegCamera {
        device: String,
        width: u32,
        height: u32,
    },
    /// A USB camera's uncompressed frames, which VIC converts as they arrive.
    RawCamera {
        device: String,
        format: RawFormat,
        width: u32,
        height: u32,
    },
    /// A USB camera's own H.264 or H.265, decoded by NVDEC.
    VideoCamera {
        device: String,
        codec: VideoCodec,
        width: u32,
        height: u32,
    },
    /// A CSI sensor through Argus, whose ISP leaves NV12 in GPU memory.
    Csi {
        sensor_id: u32,
        width: u32,
        height: u32,
    },
    /// A network camera's MJPEG; NVJPG decodes it for motion only.
    NetworkMjpeg {
        url: String,
        transport: JpegTransport,
    },
    /// H.264 or H.265 over RTSP, decoded by NVDEC.
    Rtsp { url: String, codec: VideoCodec },
    /// A recorded file, decoded by NVDEC at its own pace.
    File { path: String },
}

impl HardwareHead {
    /// The elements this head's pipeline uses; a host missing any of them never tries it.
    pub fn elements(&self) -> Vec<&'static str> {
        let source: Vec<&'static str> = match self {
            Self::MjpegCamera { .. } => vec!["v4l2src", "nvv4l2decoder"],
            Self::RawCamera { .. } => vec!["v4l2src"],
            Self::VideoCamera { codec, .. } => vec!["v4l2src", codec.parser(), "nvv4l2decoder"],
            Self::Csi { .. } => vec!["nvarguscamerasrc"],
            Self::NetworkMjpeg {
                transport: JpegTransport::Http,
                ..
            } => vec![
                "souphttpsrc",
                "multipartdemux",
                "jpegparse",
                "nvv4l2decoder",
            ],
            Self::NetworkMjpeg {
                transport: JpegTransport::Rtsp,
                ..
            } => vec!["rtspsrc", "rtpjpegdepay", "jpegparse", "nvv4l2decoder"],
            Self::Rtsp { codec, .. } => vec![
                "rtspsrc",
                codec.depayloader(),
                codec.parser(),
                "nvv4l2decoder",
            ],
            Self::File { .. } => vec!["parsebin", "nvv4l2decoder"],
        };
        let mut needed = source;
        needed.extend(["nvvidconv", "videorate", "multipartmux"]);
        if !self.passes_jpeg_through() {
            needed.push("nvjpegenc");
        }
        needed
    }

    /// The camera's own JPEG is the live picture; every other head encodes one with NVJPG.
    fn passes_jpeg_through(&self) -> bool {
        matches!(self, Self::MjpegCamera { .. } | Self::NetworkMjpeg { .. })
    }

    /// What the logs call this head; credentials never appear.
    pub fn describe(&self) -> String {
        match self {
            Self::MjpegCamera { width, height, .. } => format!("camera MJPEG {width}x{height}"),
            Self::RawCamera {
                format,
                width,
                height,
                ..
            } => format!("camera raw {} {width}x{height}", format.gst()),
            Self::VideoCamera {
                codec,
                width,
                height,
                ..
            } => format!("camera {codec:?} {width}x{height}"),
            Self::Csi {
                sensor_id,
                width,
                height,
            } => format!("CSI sensor {sensor_id} {width}x{height}"),
            Self::NetworkMjpeg { transport, .. } => format!("network MJPEG over {transport:?}"),
            Self::Rtsp { codec, .. } => format!("RTSP {codec:?}"),
            Self::File { .. } => "file".to_string(),
        }
    }
}

/// The gst-launch-1.0 argv for `head` at `fps`: a multipart stream on stdout of the live JPEG
/// (`live` size when re-encoded) and an 8-bit luma picture of `luma` size on every frame, and one
/// on [`PICTURE_FD`] of an RGBA picture of `analysis` size on every frame.
pub fn pipeline_args(
    head: &HardwareHead,
    fps: u32,
    live: (u32, u32),
    luma: (u32, u32),
    analysis: (u32, u32),
) -> Result<Vec<String>> {
    let mut a: Vec<String> = vec!["-q".into()];
    // nvjpegdec spent a whole core on the C920's 4:2:2 frames; this reaches NVJPG for 2%.
    const NVJPG: &[&str] = &["nvv4l2decoder", "mjpeg=1", "!"];
    let rtsp_source = |url: &str| -> Result<Vec<String>> {
        Ok(vec![
            "rtspsrc".into(),
            format!("location={}", quoted(url)?),
            "protocols=tcp".into(),
            format!("latency={RTSP_LATENCY_MS}"),
            "!".into(),
        ])
    };
    let jpeg_decoder: &[&str] = match head {
        HardwareHead::MjpegCamera {
            device,
            width,
            height,
        } => {
            let device = format!("device={}", quoted(device)?);
            let caps = format!("image/jpeg,width={width},height={height}");
            push(&mut a, &["v4l2src", &device, "!", &caps]);
            NVJPG
        }
        HardwareHead::RawCamera {
            device,
            format,
            width,
            height,
        } => {
            let device = format!("device={}", quoted(device)?);
            let caps = format!(
                "video/x-raw,format={},width={width},height={height}",
                format.gst()
            );
            // Into GPU memory once, so both branches scale there.
            push(
                &mut a,
                &[
                    "v4l2src",
                    &device,
                    "!",
                    &caps,
                    "!",
                    "nvvidconv",
                    "!",
                    "video/x-raw(memory:NVMM),format=NV12",
                ],
            );
            &[]
        }
        HardwareHead::VideoCamera {
            device,
            codec,
            width,
            height,
        } => {
            let device = format!("device={}", quoted(device)?);
            let caps = format!("{},width={width},height={height}", codec.caps());
            push(
                &mut a,
                &[
                    "v4l2src",
                    &device,
                    "!",
                    &caps,
                    "!",
                    codec.parser(),
                    "config-interval=-1",
                    "!",
                    "nvv4l2decoder",
                ],
            );
            &[]
        }
        HardwareHead::Csi {
            sensor_id,
            width,
            height,
        } => {
            let sensor = format!("sensor-id={sensor_id}");
            let caps =
                format!("video/x-raw(memory:NVMM),width={width},height={height},format=NV12");
            push(&mut a, &["nvarguscamerasrc", &sensor, "!", &caps]);
            &[]
        }
        HardwareHead::NetworkMjpeg { url, transport } => {
            match transport {
                JpegTransport::Http => {
                    let location = format!("location={}", quoted(url)?);
                    // multipartdemux carries no frame rate, and jpegparse then claims 1/1, which
                    // videorate held the stream to.
                    let caps = format!("image/jpeg,framerate={}/1", fps.max(1));
                    // A server that stamps no times would leave videorate nothing to drop by.
                    push(
                        &mut a,
                        &[
                            "souphttpsrc",
                            &location,
                            "is-live=true",
                            "do-timestamp=true",
                            "!",
                            "multipartdemux",
                            "!",
                            &caps,
                        ],
                    );
                }
                JpegTransport::Rtsp => {
                    a.extend(rtsp_source(url)?);
                    push(&mut a, &["rtpjpegdepay"]);
                }
            }
            // nvv4l2decoder needs the picture size in the caps, which a network stream lacks.
            push(&mut a, &["!", "jpegparse"]);
            NVJPG
        }
        HardwareHead::Rtsp { url, codec } => {
            a.extend(rtsp_source(url)?);
            // RTSP often carries the parameter sets only in its SDP; nvv4l2decoder output nothing
            // until the parser repeated them before each keyframe.
            push(
                &mut a,
                &[
                    codec.depayloader(),
                    "!",
                    codec.parser(),
                    "config-interval=-1",
                    "!",
                    "nvv4l2decoder",
                ],
            );
            &[]
        }
        HardwareHead::File { path } => {
            let location = format!("location={}", quoted(path)?);
            push(
                &mut a,
                &["filesrc", &location, "!", "parsebin", "!", "nvv4l2decoder"],
            );
            &[]
        }
    };

    let rate = format!("max-rate={}", fps.max(1));
    push(
        &mut a,
        &[
            "!",
            "videorate",
            "drop-only=true",
            &rate,
            "!",
            "tee",
            "name=t",
        ],
    );

    let luma_caps = format!(
        "video/x-raw,format=GRAY8,width={},height={}",
        luma.0, luma.1
    );
    let analysis_caps = format!(
        "video/x-raw,format=RGBA,width={},height={}",
        analysis.0, analysis.1
    );
    // The luma and the detector's picture share one decode of a camera's JPEG.
    let decoded = if head.passes_jpeg_through() {
        push(&mut a, &["t.", "!"]);
        push(&mut a, &QUEUE);
        push(&mut a, &["!"]);
        push(&mut a, jpeg_decoder);
        push(&mut a, &["tee", "name=d"]);
        "d."
    } else {
        "t."
    };
    // multipartmux names request pads in request order, so a named `mux.sink_1` fails to link.
    push(&mut a, &[decoded, "!"]);
    push(&mut a, &QUEUE);
    push(&mut a, &["!", "nvvidconv", "!", &luma_caps, "!", "mux."]);
    // A file has no live clock, so the sinks pace it; a camera paces itself.
    let sync = format!("sync={}", matches!(head, HardwareHead::File { .. }));
    let boundary = format!("boundary={BOUNDARY}");
    let picture_fd = format!("fd={PICTURE_FD}");
    push(&mut a, &[decoded, "!"]);
    push(&mut a, &LEAKY_QUEUE);
    push(
        &mut a,
        &[
            "!",
            "nvvidconv",
            "!",
            &analysis_caps,
            "!",
            "multipartmux",
            &boundary,
            "!",
            "fdsink",
            &picture_fd,
            &sync,
        ],
    );

    push(&mut a, &["t.", "!"]);
    push(&mut a, &QUEUE);
    if !head.passes_jpeg_through() {
        let live_caps = format!(
            "video/x-raw(memory:NVMM),format=I420,width={},height={}",
            live.0, live.1
        );
        push(
            &mut a,
            &["!", "nvvidconv", "!", &live_caps, "!", "nvjpegenc"],
        );
    }
    push(&mut a, &["!", "mux."]);

    push(
        &mut a,
        &[
            "multipartmux",
            "name=mux",
            &boundary,
            "!",
            "fdsink",
            "fd=1",
            &sync,
        ],
    );
    Ok(a)
}

fn push(args: &mut Vec<String>, tokens: &[&str]) {
    args.extend(tokens.iter().map(|t| t.to_string()));
}

/// gst-launch reads its argv as pipeline text: a value is safe inside double quotes once it
/// has no quote, backslash, whitespace or control character.
fn quoted(value: &str) -> Result<String> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|b| (0x21..=0x7E).contains(&b) && b != b'"' && b != b'\\')
    {
        bail!("this camera address cannot be passed to GStreamer");
    }
    Ok(format!("\"{value}\""))
}

/// Which Jetson capture paths this host can run, checked once per process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JetsonEngines {
    pub gst_launch: Option<PathBuf>,
    /// The pipeline elements gst-inspect found.
    pub elements: Vec<&'static str>,
}

impl JetsonEngines {
    pub fn runs(&self, head: &HardwareHead) -> bool {
        self.gst_launch.is_some() && head.elements().iter().all(|e| self.elements.contains(e))
    }
}

pub async fn jetson_engines() -> &'static JetsonEngines {
    static ENGINES: OnceCell<JetsonEngines> = OnceCell::const_new();
    ENGINES.get_or_init(detect).await
}

async fn detect() -> JetsonEngines {
    // The nv elements are Jetson-only; elsewhere skip the probe processes entirely.
    if !cfg!(target_os = "linux") {
        return JetsonEngines::default();
    }
    let (Some(launch), Some(inspect)) = (find_tool("gst-launch-1.0"), find_tool("gst-inspect-1.0"))
    else {
        return JetsonEngines::default();
    };
    let mut elements = Vec::new();
    for element in ELEMENTS {
        if has_element(&inspect, element).await {
            elements.push(*element);
        }
    }
    let missing: Vec<_> = ELEMENTS.iter().filter(|e| !elements.contains(e)).collect();
    tracing::info!(
        found = elements.len(),
        ?missing,
        "vision capture: Jetson pipeline elements checked"
    );
    JetsonEngines {
        gst_launch: Some(launch),
        elements,
    }
}

async fn has_element(inspect: &Path, element: &str) -> bool {
    let status = Command::new(inspect)
        .arg(element)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    matches!(
        tokio::time::timeout(INSPECT_TIMEOUT, status).await,
        Ok(Ok(s)) if s.success()
    )
}

/// The service's PATH can be minimal, so the system location is tried first.
fn find_tool(name: &str) -> Option<PathBuf> {
    let system = Path::new("/usr/bin").join(name);
    if system.exists() {
        return Some(system);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(name))
            .find(|p| p.exists())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(args: &[String]) -> String {
        args.join(" ")
    }

    #[test]
    fn a_usb_camera_passes_its_jpeg_through_and_nvjpg_decodes_the_luma() {
        let head = HardwareHead::MjpegCamera {
            device: "/dev/video0".into(),
            width: 640,
            height: 360,
        };
        let args = pipeline_args(&head, 15, (640, 360), (128, 72), (416, 234)).unwrap();
        assert_eq!(
            text(&args),
            "-q v4l2src device=\"/dev/video0\" ! image/jpeg,width=640,height=360 \
             ! videorate drop-only=true max-rate=15 ! tee name=t \
             t. ! queue max-size-buffers=4 ! nvv4l2decoder mjpeg=1 ! tee name=d \
             d. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw,format=GRAY8,width=128,height=72 ! mux. \
             d. ! queue max-size-buffers=1 leaky=downstream ! nvvidconv \
             ! video/x-raw,format=RGBA,width=416,height=234 \
             ! multipartmux boundary=giapframe ! fdsink fd=0 sync=false \
             t. ! queue max-size-buffers=4 ! mux. \
             multipartmux name=mux boundary=giapframe ! fdsink fd=1 sync=false"
        );
    }

    #[test]
    fn rtsp_is_decoded_by_nvdec_and_reencoded_by_nvjpg() {
        let head = HardwareHead::Rtsp {
            url: "rtsp://admin:pw@192.168.1.20:554/live".into(),
            codec: VideoCodec::H265,
        };
        let args = pipeline_args(&head, 30, (640, 360), (128, 72), (416, 234)).unwrap();
        assert_eq!(
            text(&args),
            "-q rtspsrc location=\"rtsp://admin:pw@192.168.1.20:554/live\" protocols=tcp latency=200 \
             ! rtph265depay ! h265parse config-interval=-1 ! nvv4l2decoder \
             ! videorate drop-only=true max-rate=30 ! tee name=t \
             t. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw,format=GRAY8,width=128,height=72 ! mux. \
             t. ! queue max-size-buffers=1 leaky=downstream ! nvvidconv \
             ! video/x-raw,format=RGBA,width=416,height=234 \
             ! multipartmux boundary=giapframe ! fdsink fd=0 sync=false \
             t. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw(memory:NVMM),format=I420,width=640,height=360 ! nvjpegenc ! mux. \
             multipartmux name=mux boundary=giapframe ! fdsink fd=1 sync=false"
        );
    }

    #[test]
    fn a_file_plays_at_its_own_pace() {
        let head = HardwareHead::File {
            path: "/tmp/clip.mp4".into(),
        };
        let joined = text(&pipeline_args(&head, 5, (640, 360), (128, 72), (416, 234)).unwrap());
        assert!(
            joined.starts_with("-q filesrc location=\"/tmp/clip.mp4\" ! parsebin ! nvv4l2decoder")
        );
        assert!(joined.ends_with("fdsink fd=1 sync=true"));
        assert!(joined.contains("fdsink fd=0 sync=true"), "{joined}");
    }

    #[test]
    fn an_address_that_could_rewrite_the_pipeline_is_refused() {
        for url in [
            "rtsp://cam\" ! filesink location=\"/etc/x",
            "rtsp://cam/a b",
            "rtsp://cam/\\x",
            "",
            "rtsp://cam/\u{e9}",
        ] {
            let head = HardwareHead::Rtsp {
                url: url.into(),
                codec: VideoCodec::H264,
            };
            assert!(
                pipeline_args(&head, 5, (640, 360), (128, 72), (416, 234)).is_err(),
                "{url:?}"
            );
        }
        assert_eq!(
            quoted("rtsp://a!b@h/x?y=1&z").unwrap(),
            "\"rtsp://a!b@h/x?y=1&z\""
        );
    }

    #[test]
    fn raw_frames_go_into_gpu_memory_once_and_nvjpg_encodes_the_live_picture() {
        let head = HardwareHead::RawCamera {
            device: "/dev/video0".into(),
            format: RawFormat::Yuy2,
            width: 640,
            height: 480,
        };
        assert_eq!(
            text(&pipeline_args(&head, 15, (640, 480), (128, 96), (416, 312)).unwrap()),
            "-q v4l2src device=\"/dev/video0\" ! video/x-raw,format=YUY2,width=640,height=480 \
             ! nvvidconv ! video/x-raw(memory:NVMM),format=NV12 \
             ! videorate drop-only=true max-rate=15 ! tee name=t \
             t. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw,format=GRAY8,width=128,height=96 ! mux. \
             t. ! queue max-size-buffers=1 leaky=downstream ! nvvidconv \
             ! video/x-raw,format=RGBA,width=416,height=312 \
             ! multipartmux boundary=giapframe ! fdsink fd=0 sync=false \
             t. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw(memory:NVMM),format=I420,width=640,height=480 ! nvjpegenc ! mux. \
             multipartmux name=mux boundary=giapframe ! fdsink fd=1 sync=false"
        );
    }

    #[test]
    fn a_usb_cameras_own_h264_goes_to_nvdec() {
        let head = HardwareHead::VideoCamera {
            device: "/dev/video2".into(),
            codec: VideoCodec::H264,
            width: 1280,
            height: 720,
        };
        let joined = text(&pipeline_args(&head, 5, (1280, 720), (128, 72), (416, 234)).unwrap());
        assert!(
            joined.starts_with(
                "-q v4l2src device=\"/dev/video2\" ! video/x-h264,width=1280,height=720 \
                 ! h264parse config-interval=-1 ! nvv4l2decoder ! videorate"
            ),
            "{joined}"
        );
        assert!(joined.contains("! nvjpegenc ! mux."), "{joined}");
    }

    #[test]
    fn a_csi_sensor_reads_through_argus_into_gpu_memory() {
        let head = HardwareHead::Csi {
            sensor_id: 1,
            width: 1280,
            height: 720,
        };
        let joined = text(&pipeline_args(&head, 5, (1280, 720), (128, 72), (416, 234)).unwrap());
        assert!(
            joined.starts_with(
                "-q nvarguscamerasrc sensor-id=1 \
                 ! video/x-raw(memory:NVMM),width=1280,height=720,format=NV12 ! videorate"
            ),
            "{joined}"
        );
    }

    #[test]
    fn network_mjpeg_passes_its_jpeg_through_and_nvjpg_decodes_the_luma() {
        let http = HardwareHead::NetworkMjpeg {
            url: "http://cam.local/video.mjpg".into(),
            transport: JpegTransport::Http,
        };
        assert_eq!(
            text(&pipeline_args(&http, 5, (640, 480), (128, 96), (416, 312)).unwrap()),
            "-q souphttpsrc location=\"http://cam.local/video.mjpg\" is-live=true do-timestamp=true \
             ! multipartdemux ! image/jpeg,framerate=5/1 ! jpegparse \
             ! videorate drop-only=true max-rate=5 ! tee name=t \
             t. ! queue max-size-buffers=4 ! nvv4l2decoder mjpeg=1 ! tee name=d \
             d. ! queue max-size-buffers=4 ! nvvidconv \
             ! video/x-raw,format=GRAY8,width=128,height=96 ! mux. \
             d. ! queue max-size-buffers=1 leaky=downstream ! nvvidconv \
             ! video/x-raw,format=RGBA,width=416,height=312 \
             ! multipartmux boundary=giapframe ! fdsink fd=0 sync=false \
             t. ! queue max-size-buffers=4 ! mux. \
             multipartmux name=mux boundary=giapframe ! fdsink fd=1 sync=false"
        );
        let rtsp = HardwareHead::NetworkMjpeg {
            url: "rtsp://cam.local/mjpeg".into(),
            transport: JpegTransport::Rtsp,
        };
        let joined = text(&pipeline_args(&rtsp, 5, (640, 480), (128, 96), (416, 312)).unwrap());
        assert!(
            joined.starts_with(
                "-q rtspsrc location=\"rtsp://cam.local/mjpeg\" protocols=tcp latency=200 \
                 ! rtpjpegdepay ! jpegparse ! videorate"
            ),
            "{joined}"
        );
    }

    #[test]
    fn a_host_missing_an_element_never_tries_the_head_that_needs_it() {
        let all = JetsonEngines {
            gst_launch: Some("/usr/bin/gst-launch-1.0".into()),
            elements: ELEMENTS.to_vec(),
        };
        let raw = HardwareHead::RawCamera {
            device: "/dev/video0".into(),
            format: RawFormat::Yuy2,
            width: 640,
            height: 480,
        };
        let mjpeg = HardwareHead::MjpegCamera {
            device: "/dev/video0".into(),
            width: 640,
            height: 360,
        };
        assert!(all.runs(&raw) && all.runs(&mjpeg));

        let mut no_encoder = all.clone();
        no_encoder.elements.retain(|e| *e != "nvjpegenc");
        assert!(
            !no_encoder.runs(&raw),
            "raw frames need NVJPG for the live picture"
        );
        assert!(no_encoder.runs(&mjpeg), "a camera JPEG passes through");

        let no_gst = JetsonEngines {
            gst_launch: None,
            ..all
        };
        assert!(!no_gst.runs(&mjpeg));

        let every_head = [
            raw,
            mjpeg,
            HardwareHead::VideoCamera {
                device: "/dev/video2".into(),
                codec: VideoCodec::H265,
                width: 640,
                height: 360,
            },
            HardwareHead::Csi {
                sensor_id: 0,
                width: 640,
                height: 360,
            },
            HardwareHead::NetworkMjpeg {
                url: "http://cam/x".into(),
                transport: JpegTransport::Http,
            },
            HardwareHead::NetworkMjpeg {
                url: "rtsp://cam/x".into(),
                transport: JpegTransport::Rtsp,
            },
            HardwareHead::Rtsp {
                url: "rtsp://cam/x".into(),
                codec: VideoCodec::H264,
            },
            HardwareHead::File {
                path: "/tmp/x.mp4".into(),
            },
        ];
        for head in &every_head {
            for element in head.elements() {
                assert!(
                    ELEMENTS.contains(&element),
                    "{element} is never looked up, so {} could never run",
                    head.describe()
                );
            }
        }
    }

    #[test]
    fn codec_names_follow_ffmpeg() {
        assert_eq!(VideoCodec::from_ffmpeg("h264"), Some(VideoCodec::H264));
        assert_eq!(VideoCodec::from_ffmpeg("hevc"), Some(VideoCodec::H265));
        assert_eq!(VideoCodec::from_ffmpeg("mjpeg"), None);
        assert_eq!(RawFormat::from_ffmpeg("yuyv422"), Some(RawFormat::Yuy2));
        assert_eq!(RawFormat::from_ffmpeg("bayer_rggb8"), None);
    }
}
