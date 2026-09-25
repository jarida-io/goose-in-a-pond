//! ONNX [`FaceEmbeddingExtractor`]: align (or crop), quality-gate, infer, then L2-normalise so
//! cosine similarity is a dot product. `ORT_DYLIB_PATH` overrides the ORT shared library.

pub mod alignment;
pub mod antispoof;
pub mod antispoof_onnx;
pub mod detector;
pub mod scrfd;
pub use antispoof_onnx::OnnxAntispoof;
pub use detector::UltraFaceDetector;
pub use scrfd::ScrfdDetector;

use std::sync::OnceLock;

/// Loaded once so a failed load isn't retried per frame; `None` means the heuristic gate runs.
static ONNX_ANTISPOOF: OnceLock<Option<OnnxAntispoof>> = OnceLock::new();
/// Optional ensemble model from `$POND_FACE_ANTISPOOF_PATH_2`; the higher spoof score wins.
static ONNX_ANTISPOOF_2: OnceLock<Option<OnnxAntispoof>> = OnceLock::new();

fn antispoof_onnx() -> Option<&'static OnnxAntispoof> {
    ONNX_ANTISPOOF
        .get_or_init(|| match OnnxAntispoof::try_from_env() {
            Ok(opt) => opt,
            Err(e) => {
                tracing::warn!("Silent-Face anti-spoof load failed ({e:#}); using heuristic");
                None
            }
        })
        .as_ref()
}

fn antispoof_onnx_2() -> Option<&'static OnnxAntispoof> {
    ONNX_ANTISPOOF_2
        .get_or_init(
            || match OnnxAntispoof::try_from_env_var("POND_FACE_ANTISPOOF_PATH_2") {
                Ok(opt) => opt,
                Err(e) => {
                    tracing::warn!("Silent-Face secondary anti-spoof load failed ({e:#})");
                    None
                }
            },
        )
        .as_ref()
}

/// Secondary model's crop scale: 4.0× (V1SE's training crop), or `POND_FACE_ANTISPOOF_SCALE_2`.
fn antispoof_scale_2() -> f32 {
    std::env::var("POND_FACE_ANTISPOOF_SCALE_2")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| (1.0..=6.0).contains(v))
        .unwrap_or(4.0)
}

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use pond_core::user_data::domain::face_recognition::{BoundingBox, FaceLandmarks};
use pond_core::user_data::ports::face_embedding_extractor::FaceEmbeddingExtractor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

use crate::alignment::align_to_canonical_112;
use crate::antispoof::AntispoofReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingModel {
    /// ArcFace (ResNet-100 / R50 / etc.) — 112×112 input, 512-d output.
    ArcFace512,
    /// MobileFaceNet — 112×112 input, 128-d output.
    MobileFaceNet128,
}

impl EmbeddingModel {
    pub fn input_size(&self) -> u32 {
        112
    }
    pub fn dims(&self) -> u32 {
        match self {
            Self::ArcFace512 => 512,
            Self::MobileFaceNet128 => 128,
        }
    }
}

/// ArcFace and MobileFaceNet both take `(pixel/255 - 0.5) / 0.5`, i.e. \[-1, 1\].
const MEAN: f32 = 0.5;
const SCALE: f32 = 1.0 / 0.5;

/// `POND_FACE_EMBED_CHANNEL_ORDER=bgr|rgb` (default rgb, as `buffalo_l` expects). Many ArcFace
/// R100 re-exports want BGR; feeding them RGB makes every face score 0.98+.
fn use_bgr_input() -> bool {
    std::env::var("POND_FACE_EMBED_CHANNEL_ORDER")
        .map(|v| v.to_ascii_lowercase() == "bgr")
        .unwrap_or(false)
}

/// Min pixel variance (0-1 scale) to embed; near-uniform frames collapse to one embedding.
const MIN_CONTENT_VARIANCE: f32 = 0.006;

/// Mean-brightness gate (0-1); floor sits under real low-light frames' ~0.04, above a lens cap.
const MIN_MEAN_BRIGHTNESS: f32 = 0.025;
const MAX_MEAN_BRIGHTNESS: f32 = 0.95;

/// Blur floor, Laplacian variance ×1000: focused webcam ~30-200, motion blur ~3-10, defocus <1.
const MIN_LAPLACIAN_VAR_X1000: f32 = 4.0;

/// Mean luminance that triggers auto-exposure; normally-lit headshots sit at 0.40-0.65.
pub(crate) const LOW_LIGHT_TRIGGER: f32 = 0.30;

pub(crate) fn auto_exposure_enabled() -> bool {
    !std::env::var("POND_FACE_AUTO_EXPOSURE")
        .map(|v| v.to_ascii_lowercase() == "off")
        .unwrap_or(false)
}

/// Mean luminance of an RGB image, in [0, 1].  Rec.709 weights.
pub(crate) fn mean_luminance(img: &RgbImage) -> f32 {
    let n = (img.width() as u64) * (img.height() as u64);
    if n == 0 {
        return 0.0;
    }
    let mut acc: f64 = 0.0;
    for p in img.pixels() {
        let y = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
        acc += y as f64;
    }
    ((acc / n as f64) / 255.0) as f32
}

/// In-place low-light fix, per channel (dropping any colour cast; chroma carries no identity).
pub(crate) fn stretch_histogram_2_98(img: &mut RgbImage) {
    // `stretch` (default, ~0.2 ms) keeps global tonality but can leave shadowed faces; `clahe`
    // (~3 ms) recovers detail beside bright windows but is noisier when uniformly dim.
    match auto_exposure_mode().as_str() {
        "clahe" => clahe_per_channel(img, 8, 4.0),
        _ => stretch_histogram_2_98_linear(img),
    }
}

fn auto_exposure_mode() -> String {
    std::env::var("POND_FACE_AUTO_EXPOSURE_MODE")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "stretch".to_string())
}

/// Per-channel 2-98 percentile linear stretch.
fn stretch_histogram_2_98_linear(img: &mut RgbImage) {
    let n = (img.width() * img.height()) as usize;
    if n == 0 {
        return;
    }
    let mut hists: [[u32; 256]; 3] = [[0; 256], [0; 256], [0; 256]];
    for p in img.pixels() {
        for c in 0..3 {
            hists[c][p[c] as usize] += 1;
        }
    }
    let lo_count = (n as f32 * 0.02) as u32;
    let hi_count = (n as f32 * 0.98) as u32;
    let mut lo = [0u8; 3];
    let mut hi = [255u8; 3];
    for c in 0..3 {
        let mut acc: u32 = 0;
        for v in 0..256 {
            acc += hists[c][v];
            if acc >= lo_count {
                lo[c] = v as u8;
                break;
            }
        }
        let mut acc: u32 = 0;
        for v in 0..256 {
            acc += hists[c][v];
            if acc >= hi_count {
                hi[c] = v as u8;
                break;
            }
        }
        // Avoid div-by-zero on degenerate channels (uniform colour).
        if hi[c] <= lo[c] {
            hi[c] = lo[c].saturating_add(1);
        }
    }
    for p in img.pixels_mut() {
        for c in 0..3 {
            let v = p[c] as f32;
            let span = (hi[c] as f32 - lo[c] as f32).max(1.0);
            let mapped = ((v - lo[c] as f32) / span) * 255.0;
            p[c] = mapped.clamp(0.0, 255.0) as u8;
        }
    }
}

/// Per-channel CLAHE over `tiles_per_axis`² tiles, clipping at `clip_limit` × the mean bin.
fn clahe_per_channel(img: &mut RgbImage, tiles_per_axis: u32, clip_limit: f32) {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return;
    }
    let n_tiles = tiles_per_axis as usize;
    if n_tiles < 2 {
        return;
    }

    let tile_w = (w as f32 / tiles_per_axis as f32).ceil() as u32;
    let tile_h = (h as f32 / tiles_per_axis as f32).ceil() as u32;
    let pixels_per_tile = (tile_w as usize) * (tile_h as usize);
    let avg_per_bin = pixels_per_tile as f32 / 256.0;
    let clip_count: u32 = (clip_limit * avg_per_bin).max(1.0) as u32;

    // Per-tile CDF lookup: cdfs[ty * n_tiles + tx][channel][value].
    let mut cdfs: Vec<[[u8; 256]; 3]> = vec![[[0u8; 256]; 3]; n_tiles * n_tiles];

    for ty in 0..n_tiles {
        for tx in 0..n_tiles {
            let x0 = (tx as u32) * tile_w;
            let y0 = (ty as u32) * tile_h;
            let x1 = ((tx as u32 + 1) * tile_w).min(w);
            let y1 = ((ty as u32 + 1) * tile_h).min(h);
            let tile_pixels = ((x1 - x0) as usize) * ((y1 - y0) as usize);
            if tile_pixels == 0 {
                continue;
            }
            let mut hist: [[u32; 256]; 3] = [[0; 256], [0; 256], [0; 256]];
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = img.get_pixel(x, y).0;
                    for c in 0..3 {
                        hist[c][p[c] as usize] += 1;
                    }
                }
            }
            // Clip + redistribute excess uniformly across all 256 bins.
            for c in 0..3 {
                let mut excess: u32 = 0;
                for b in 0..256 {
                    if hist[c][b] > clip_count {
                        excess += hist[c][b] - clip_count;
                        hist[c][b] = clip_count;
                    }
                }
                let add = excess / 256;
                let mut leftover = (excess - add * 256) as usize;
                for b in 0..256 {
                    hist[c][b] += add;
                    if leftover > 0 {
                        hist[c][b] += 1;
                        leftover -= 1;
                    }
                }
                // CDF → 0..=255 lookup.
                let mut cum: u32 = 0;
                let cdf_scale = 255.0 / tile_pixels as f32;
                for b in 0..256 {
                    cum += hist[c][b];
                    cdfs[ty * n_tiles + tx][c][b] =
                        (cum as f32 * cdf_scale).round().min(255.0) as u8;
                }
            }
        }
    }

    // Bilinear interpolation between the 4 nearest tile centres for each pixel.
    let half_tw = tile_w as f32 * 0.5;
    let half_th = tile_h as f32 * 0.5;
    for y in 0..h {
        for x in 0..w {
            // Continuous tile-grid coords (centre at integer values).
            let gx = ((x as f32 - half_tw) / tile_w as f32).max(0.0);
            let gy = ((y as f32 - half_th) / tile_h as f32).max(0.0);
            let max_tile = (n_tiles - 1) as f32;
            let gx = gx.min(max_tile);
            let gy = gy.min(max_tile);
            let tx0 = gx.floor() as usize;
            let ty0 = gy.floor() as usize;
            let tx1 = (tx0 + 1).min(n_tiles - 1);
            let ty1 = (ty0 + 1).min(n_tiles - 1);
            let fx = gx - tx0 as f32;
            let fy = gy - ty0 as f32;

            let p = img.get_pixel_mut(x, y);
            for c in 0..3 {
                let v = p[c] as usize;
                let v00 = cdfs[ty0 * n_tiles + tx0][c][v] as f32;
                let v10 = cdfs[ty0 * n_tiles + tx1][c][v] as f32;
                let v01 = cdfs[ty1 * n_tiles + tx0][c][v] as f32;
                let v11 = cdfs[ty1 * n_tiles + tx1][c][v] as f32;
                let a = v00 * (1.0 - fx) + v10 * fx;
                let b = v01 * (1.0 - fx) + v11 * fx;
                let mapped = a * (1.0 - fy) + b * fy;
                p[c] = mapped.clamp(0.0, 255.0) as u8;
            }
        }
    }
}

pub struct OnnxFaceEmbeddingExtractor {
    // std Mutex, not tokio: `Session::run` needs `&mut self` and runs inside `spawn_blocking`.
    session: Arc<Mutex<Session>>,
    model: EmbeddingModel,
    model_path: PathBuf,
}

impl OnnxFaceEmbeddingExtractor {
    /// Load the model; fails if it is missing or ORT can't load (system path or `ORT_DYLIB_PATH`).
    pub fn new(model_path: impl Into<PathBuf>, model: EmbeddingModel) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!(
                "face embedding model not found at {}",
                path.display()
            ));
        }

        let session = Session::builder()
            .context(
                "failed to create ort session builder — is the ONNX Runtime library available?",
            )?
            .commit_from_file(&path)
            .with_context(|| format!("failed to load ONNX model at {}", path.display()))?;

        info!(
            model = ?model,
            path = %path.display(),
            channel_order = if use_bgr_input() { "BGR" } else { "RGB" },
            "face embedding ONNX model loaded"
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            model,
            model_path: path,
        })
    }

    /// Path the adapter is serving embeddings from (diagnostic).
    pub fn model_path(&self) -> &Path {
        &self.model_path
    }
}

/// Clamp a bbox to the image; `None` if nothing is left.
fn clamp_bbox(bbox: BoundingBox, img_w: u32, img_h: u32) -> Option<(u32, u32, u32, u32)> {
    if bbox.x >= img_w || bbox.y >= img_h || bbox.width == 0 || bbox.height == 0 {
        return None;
    }
    let w = bbox.width.min(img_w - bbox.x);
    let h = bbox.height.min(img_h - bbox.y);
    Some((bbox.x, bbox.y, w, h))
}

/// No-bbox fallback: the largest centred square, adequate only for headshot framing.
fn center_square(img_w: u32, img_h: u32) -> (u32, u32, u32, u32) {
    let side = img_w.min(img_h);
    let x = (img_w - side) / 2;
    let y = (img_h - side) / 2;
    (x, y, side, side)
}

/// Decode, align or crop, normalise into `[1, 3, 112, 112]`; `None` if a quality gate rejects.
fn preprocess(
    image_bytes: &[u8],
    model: EmbeddingModel,
    bbox: Option<BoundingBox>,
    landmarks: Option<FaceLandmarks>,
) -> Result<Option<Array4<f32>>> {
    let img = image::load_from_memory(image_bytes).context("failed to decode image bytes")?;
    let size = model.input_size();

    let mut resized = if let Some(lms) = landmarks {
        let warped = align_to_canonical_112(&img, &lms, size);
        warped.to_rgb8()
    } else {
        let (img_w, img_h) = img.dimensions();
        let (cx, cy, cw, ch) = bbox
            .and_then(|b| clamp_bbox(b, img_w, img_h))
            .unwrap_or_else(|| center_square(img_w, img_h));
        img.crop_imm(cx, cy, cw, ch)
            .resize_exact(size, size, FilterType::Triangle)
            .to_rgb8()
    };

    // Before the gates, so dim crops aren't false-rejected as dark or blurry.
    if auto_exposure_enabled() {
        let mean_pre = mean_luminance(&resized);
        if mean_pre < LOW_LIGHT_TRIGGER {
            let before = mean_pre;
            stretch_histogram_2_98(&mut resized);
            info!(
                before,
                after = mean_luminance(&resized),
                "preprocess: applied low-light auto-exposure to dim crop",
            );
        }
    }

    let h = size as usize;
    let w = size as usize;
    let mut tensor = Array4::<f32>::zeros((1, 3, h, w));

    let mut sum: f64 = 0.0;
    let mut sum_sq: f64 = 0.0;
    let npx: f64 = (h * w * 3) as f64;

    let bgr = use_bgr_input();
    for y in 0..h {
        for x in 0..w {
            let pixel = resized.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                let v = (pixel[c] as f32) / 255.0;
                let normed = (v - MEAN) * SCALE;
                let dst_c = if bgr { 2 - c } else { c };
                tensor[[0, dst_c, y, x]] = normed;
                sum += v as f64;
                sum_sq += (v * v) as f64;
            }
        }
    }

    // Quality gates, in pre-normalisation 0-1 units.
    let mean = (sum / npx) as f32;
    let variance = ((sum_sq / npx) - (mean as f64).powi(2)).max(0.0) as f32;
    if variance < MIN_CONTENT_VARIANCE {
        // info!, not debug!, so operators see which gate tripped (same for the gates below).
        info!(
            variance,
            threshold = MIN_CONTENT_VARIANCE,
            "preprocess: rejecting low-variance frame (blank / solid colour)"
        );
        return Ok(None);
    }
    if !(MIN_MEAN_BRIGHTNESS..=MAX_MEAN_BRIGHTNESS).contains(&mean) {
        info!(
            mean,
            min = MIN_MEAN_BRIGHTNESS,
            max = MAX_MEAN_BRIGHTNESS,
            "preprocess: rejecting extreme-brightness frame (too dark or too bright)"
        );
        return Ok(None);
    }

    // Blur gate — Laplacian variance on the aligned luminance plane.
    let lap_var_x1000 = laplacian_variance_luma(&resized) * 1000.0;
    if lap_var_x1000 < MIN_LAPLACIAN_VAR_X1000 {
        info!(
            lap_var_x1000,
            threshold = MIN_LAPLACIAN_VAR_X1000,
            "preprocess: rejecting blurry frame (camera autofocus hunting?)"
        );
        return Ok(None);
    }

    // Anti-spoof gate: the ONNX model if loaded, else the heuristic.
    if antispoof_enabled() {
        let (report, is_onnx) = match antispoof_onnx() {
            Some(model) => {
                // Silent-Face wants a ~2.7× loose crop; the tight aligned crop scores live ≈ 0.
                let loose = loose_antispoof_crop(&img, bbox, landmarks, 2.7);
                let primary = match model.analyse(&loose) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            "Silent-Face inference failed ({e:#}); using heuristic for this frame"
                        );
                        antispoof::analyse(&resized)
                    }
                };
                // Optional wider-crop second model; max(spoof) so either one firing rejects.
                let combined = if let Some(m2) = antispoof_onnx_2() {
                    let scale2 = antispoof_scale_2();
                    let loose2 = loose_antispoof_crop(&img, bbox, landmarks, scale2);
                    match m2.analyse(&loose2) {
                        Ok(r2) => {
                            info!(
                                primary = primary.spoof_score,
                                secondary = r2.spoof_score,
                                scale2,
                                "anti-spoof ensemble"
                            );
                            AntispoofReport {
                                spoof_score: primary.spoof_score.max(r2.spoof_score),
                                ..primary
                            }
                        }
                        Err(e) => {
                            warn!("secondary Silent-Face inference failed ({e:#}); using primary only");
                            primary
                        }
                    }
                } else {
                    primary
                };
                (combined, true)
            }
            None => (antispoof::analyse(&resized), false),
        };
        let threshold = antispoof_threshold(is_onnx);
        if report.spoof_score >= threshold {
            info!(
                score = report.spoof_score,
                threshold,
                is_onnx,
                sat_var = report.saturation_var,
                hl_density = report.highlight_density,
                skew = report.gradient_skew,
                "preprocess: rejecting likely presentation attack \
                 (set POND_FACE_ANTISPOOF=off to disable or raise POND_FACE_ANTISPOOF_THRESHOLD)"
            );
            return Ok(None);
        }
    }

    Ok(Some(tensor))
}

/// Face box scaled `scale`× about its centre, cut from the original frame (Silent-Face input).
fn loose_antispoof_crop(
    img: &DynamicImage,
    bbox: Option<BoundingBox>,
    landmarks: Option<FaceLandmarks>,
    scale: f32,
) -> RgbImage {
    let antispoof_scale: f32 = scale;
    let (img_w, img_h) = img.dimensions();

    let base: Option<(u32, u32, u32, u32)> =
        bbox.and_then(|b| clamp_bbox(b, img_w, img_h)).or_else(|| {
            landmarks.map(|lms| {
                let pts = lms.as_array();
                let (mut xmin, mut ymin) = (f32::INFINITY, f32::INFINITY);
                let (mut xmax, mut ymax) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
                for (x, y) in pts {
                    xmin = xmin.min(x);
                    ymin = ymin.min(y);
                    xmax = xmax.max(x);
                    ymax = ymax.max(y);
                }
                let x = xmin.max(0.0) as u32;
                let y = ymin.max(0.0) as u32;
                let w = (xmax - xmin).max(1.0) as u32;
                let h = (ymax - ymin).max(1.0) as u32;
                (
                    x,
                    y,
                    w.min(img_w.saturating_sub(x)),
                    h.min(img_h.saturating_sub(y)),
                )
            })
        });

    let (x, y, w, h) = match base {
        Some(b) => b,
        None => center_square(img_w, img_h),
    };

    let cx = x as f32 + w as f32 * 0.5;
    let cy = y as f32 + h as f32 * 0.5;
    let side = (w.max(h) as f32) * antispoof_scale;
    let half = side * 0.5;
    let lx = (cx - half).max(0.0) as u32;
    let ly = (cy - half).max(0.0) as u32;
    let rx = ((cx + half) as u32).min(img_w);
    let ry = ((cy + half) as u32).min(img_h);
    let cw = rx.saturating_sub(lx).max(1);
    let ch = ry.saturating_sub(ly).max(1);

    img.crop_imm(lx, ly, cw, ch).to_rgb8()
}

fn antispoof_enabled() -> bool {
    !matches!(
        std::env::var("POND_FACE_ANTISPOOF").as_deref(),
        Ok("off") | Ok("0") | Ok("false")
    )
}

fn antispoof_threshold(is_onnx: bool) -> f32 {
    if let Some(v) = std::env::var("POND_FACE_ANTISPOOF_THRESHOLD")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
    {
        return v;
    }
    // ONNX: 0.40 sits between live (0.05-0.20) and phone replays (0.42-0.48). Heuristic: 0.65
    // avoids false-rejecting real users under LED ring lights.
    if is_onnx {
        0.40
    } else {
        0.65
    }
}

/// Blur metric: variance of the 3×3 Laplacian of BT.601 luma, pixels in \[0, 1\].
fn laplacian_variance_luma(img: &image::RgbImage) -> f32 {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return 0.0;
    }
    let luma = |x: u32, y: u32| -> f32 {
        let [r, g, b] = img.get_pixel(x, y).0;
        (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) / 255.0
    };
    let n = (w as usize - 2) * (h as usize - 2);
    let mut sum = 0.0_f64;
    let mut sum_sq = 0.0_f64;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            // 3×3 Laplacian (discrete): 4·center − (N + S + E + W).
            let l = 4.0 * luma(x, y)
                - (luma(x, y - 1) + luma(x, y + 1) + luma(x - 1, y) + luma(x + 1, y));
            sum += l as f64;
            sum_sq += (l * l) as f64;
        }
    }
    let mean = sum / n as f64;
    ((sum_sq / n as f64) - mean * mean).max(0.0) as f32
}

/// L2-normalise the raw ONNX output.  Validates dimensionality.
fn postprocess(raw: &[f32], expected_dims: u32) -> Result<Vec<f32>> {
    if raw.len() != expected_dims as usize {
        return Err(anyhow!(
            "embedding model returned {} values, expected {}",
            raw.len(),
            expected_dims
        ));
    }
    let norm: f32 = raw.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm == 0.0 {
        return Err(anyhow!(
            "embedding has zero norm — likely a degenerate input"
        ));
    }
    Ok(raw.iter().map(|v| v / norm).collect())
}

#[async_trait]
impl FaceEmbeddingExtractor for OnnxFaceEmbeddingExtractor {
    async fn extract_embedding(
        &self,
        image_bytes: &[u8],
        bbox: Option<BoundingBox>,
        landmarks: Option<FaceLandmarks>,
    ) -> Result<Option<Vec<f32>>> {
        if image_bytes.is_empty() {
            return Ok(None);
        }

        let model = self.model;
        let session = self.session.clone();
        let bytes = image_bytes.to_vec();

        let embedding = tokio::task::spawn_blocking(move || -> Result<Option<Vec<f32>>> {
            let tensor = match preprocess(&bytes, model, bbox, landmarks)? {
                Some(t) => t,
                None => return Ok(None),
            };
            let input = Tensor::from_array(tensor).context("failed to wrap input as ort tensor")?;

            let mut session = session
                .lock()
                .map_err(|_| anyhow!("face embedding session mutex was poisoned"))?;
            let outputs = session
                .run(ort::inputs![input])
                .context("ONNX inference failed")?;
            let (_name, first) = outputs
                .iter()
                .next()
                .ok_or_else(|| anyhow!("ONNX model returned no outputs"))?;
            let (_shape, data) = first
                .try_extract_tensor::<f32>()
                .context("failed to extract f32 tensor from ONNX output")?;

            postprocess(data, model.dims()).map(Some)
        })
        .await
        .context("face inference task panicked")??;

        if embedding.is_none() {
            warn!("face embedding rejected by quality gate");
        }
        debug!(
            dims = embedding.as_ref().map(|e| e.len()).unwrap_or(0),
            "face embedding result"
        );
        Ok(embedding)
    }

    fn embedding_dims(&self) -> u32 {
        self.model.dims()
    }
}

// Keeps the `DynamicImage` import referenced.
#[allow(dead_code)]
fn _touch(_: &DynamicImage) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_model_dims() {
        assert_eq!(EmbeddingModel::ArcFace512.dims(), 512);
        assert_eq!(EmbeddingModel::MobileFaceNet128.dims(), 128);
        assert_eq!(EmbeddingModel::ArcFace512.input_size(), 112);
    }

    #[test]
    fn histogram_stretch_brightens_dim_image() {
        let mut img = RgbImage::from_fn(32, 32, |x, y| {
            let v = ((x + y) % 21 + 10) as u8; // 10..=30
            image::Rgb([v, v, v])
        });
        let mean_before = mean_luminance(&img);
        stretch_histogram_2_98(&mut img);
        let mean_after = mean_luminance(&img);
        assert!(
            mean_after > mean_before * 2.0,
            "expected histogram stretch to roughly double mean luminance; got {mean_before} -> {mean_after}",
        );
        assert!(mean_after > 0.4, "stretched mean too dark: {mean_after}");
    }

    #[test]
    fn histogram_stretch_idempotent_on_full_range_image() {
        let mut img = RgbImage::from_fn(32, 32, |x, _| {
            let v = ((x * 8) % 256) as u8;
            image::Rgb([v, v, v])
        });
        let mean_before = mean_luminance(&img);
        stretch_histogram_2_98(&mut img);
        let mean_after = mean_luminance(&img);
        let drift = (mean_after - mean_before).abs();
        assert!(
            drift < 0.1,
            "well-exposed image should not drift much: {mean_before} -> {mean_after}"
        );
    }

    #[test]
    fn clahe_brightens_dim_image() {
        let mut img = RgbImage::from_fn(64, 64, |x, y| {
            let v = ((x + y) % 21 + 10) as u8;
            image::Rgb([v, v, v])
        });
        let mean_before = mean_luminance(&img);
        clahe_per_channel(&mut img, 8, 4.0);
        let mean_after = mean_luminance(&img);
        assert!(
            mean_after > mean_before * 2.0,
            "CLAHE should at least double luminance on a uniformly-dim image; got {mean_before} -> {mean_after}",
        );
    }

    #[test]
    fn clahe_recovers_dark_corner_in_mixed_lighting() {
        // A global stretch is dominated by the bright quarter; per-tile CLAHE lifts the dim one.
        let mut img = RgbImage::from_fn(64, 64, |x, y| {
            let v = if x < 32 && y < 32 {
                200u8
            } else if x >= 32 && y >= 32 {
                20u8
            } else {
                110u8
            };
            image::Rgb([v, v, v])
        });
        let dim_mean = |im: &RgbImage| -> f32 {
            let mut s = 0u32;
            let mut n = 0u32;
            for y in 32..64u32 {
                for x in 32..64u32 {
                    let p = im.get_pixel(x, y).0;
                    s += p[0] as u32;
                    n += 1;
                }
            }
            s as f32 / n as f32 / 255.0
        };
        let before = dim_mean(&img);
        clahe_per_channel(&mut img, 8, 4.0);
        let after = dim_mean(&img);
        assert!(
            after > before + 0.1,
            "CLAHE should lift the dim corner by ≥ 10% absolute luminance; got {before} -> {after}",
        );
    }

    #[test]
    fn new_fails_when_model_missing() {
        let result = OnnxFaceEmbeddingExtractor::new(
            "/nonexistent/arcface.onnx",
            EmbeddingModel::ArcFace512,
        );
        let err = match result {
            Ok(_) => panic!("expected error for missing model"),
            Err(e) => e,
        };
        let msg = format!("{}", err);
        assert!(msg.contains("not found"), "unexpected error: {}", msg);
    }

    #[test]
    fn postprocess_l2_normalises() {
        let raw = vec![3.0_f32, 4.0];
        let out = postprocess(&raw, 2).unwrap();
        let norm: f32 = out.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn postprocess_rejects_dimension_mismatch() {
        let raw = vec![1.0_f32; 100];
        assert!(postprocess(&raw, 512).is_err());
    }

    #[test]
    fn postprocess_rejects_zero_vector() {
        let raw = vec![0.0_f32; 512];
        assert!(postprocess(&raw, 512).is_err());
    }

    /// Noisy PNG that passes the variance gate.
    fn noisy_png(w: u32, h: u32) -> Vec<u8> {
        let mut img = image::RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let r = ((x * 7 + y * 13) % 256) as u8;
                let g = ((x * 11 + y * 5) % 256) as u8;
                let b = ((x * 3 + y * 17) % 256) as u8;
                img.put_pixel(x, y, image::Rgb([r, g, b]));
            }
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    fn solid_png(w: u32, h: u32, px: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb(px));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    }

    #[test]
    fn preprocess_produces_correct_shape_for_noisy_image() {
        let bytes = noisy_png(120, 120);
        let tensor = preprocess(&bytes, EmbeddingModel::ArcFace512, None, None)
            .unwrap()
            .expect("noisy image should pass quality gate");
        assert_eq!(tensor.shape(), &[1, 3, 112, 112]);
    }

    #[test]
    fn preprocess_rejects_uniform_grey_frame() {
        let bytes = solid_png(120, 120, [128, 128, 128]);
        let out = preprocess(&bytes, EmbeddingModel::ArcFace512, None, None).unwrap();
        assert!(out.is_none(), "uniform frame should fail variance gate");
    }

    #[test]
    fn preprocess_rejects_solid_black() {
        let bytes = solid_png(120, 120, [0, 0, 0]);
        let out = preprocess(&bytes, EmbeddingModel::ArcFace512, None, None).unwrap();
        assert!(out.is_none(), "solid black should fail brightness gate");
    }

    #[test]
    fn center_square_picks_largest_centered_square() {
        assert_eq!(center_square(200, 100), (50, 0, 100, 100));
        assert_eq!(center_square(100, 200), (0, 50, 100, 100));
        assert_eq!(center_square(100, 100), (0, 0, 100, 100));
    }

    #[test]
    fn clamp_bbox_trims_to_image_bounds() {
        let bbox = BoundingBox {
            x: 90,
            y: 90,
            width: 50,
            height: 50,
        };
        assert_eq!(clamp_bbox(bbox, 100, 100), Some((90, 90, 10, 10)));
    }

    #[test]
    fn clamp_bbox_rejects_out_of_bounds_origin() {
        let bbox = BoundingBox {
            x: 150,
            y: 0,
            width: 10,
            height: 10,
        };
        assert!(clamp_bbox(bbox, 100, 100).is_none());
    }

    #[test]
    fn clamp_bbox_rejects_zero_area() {
        assert!(clamp_bbox(
            BoundingBox {
                x: 10,
                y: 10,
                width: 0,
                height: 20
            },
            100,
            100
        )
        .is_none());
    }

    #[test]
    fn preprocess_with_landmarks_goes_through_alignment_path() {
        // Only checks the shape; warp correctness is covered in alignment::tests.
        let bytes = noisy_png(200, 200);
        let lms = FaceLandmarks {
            left_eye: (70.0, 80.0),
            right_eye: (130.0, 80.0),
            nose: (100.0, 110.0),
            left_mouth: (80.0, 150.0),
            right_mouth: (120.0, 150.0),
        };
        let tensor = preprocess(&bytes, EmbeddingModel::ArcFace512, None, Some(lms))
            .unwrap()
            .expect("aligned noisy image should pass quality gate");
        assert_eq!(tensor.shape(), &[1, 3, 112, 112]);
    }
}
