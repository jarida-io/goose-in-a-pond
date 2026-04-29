//! ONNX-backed anti-spoofing — drop-in upgrade for the heuristic gate.
//!
//! Wraps a Silent-Face-Anti-Spoofing (MiniFASNetV2 / V1SE) ONNX model and
//! exposes the same [`AntispoofReport`] shape as the heuristic
//! [`crate::antispoof`] module so the embedding pipeline can switch between
//! the two without further code changes.
//!
//! # Why both paths exist
//!
//! The heuristic gate (saturation variance + highlight density + gradient
//! skew) is fast, dependency-free, and catches the obvious presentation
//! attacks (printed photos, flat phone screens) that we observed in
//! testing.  It misses higher-quality attacks — high-DPI prints, tablet
//! displays at the right brightness, video replays — and over-rejects in
//! some lighting (very even ring lights look "too uniform").
//!
//! The ONNX model is trained on a much wider attack catalogue and gives a
//! calibrated probability rather than a hand-tuned score.  When a model
//! file is present at `$POND_FACE_ANTISPOOF_PATH` the embedding extractor
//! prefers it; otherwise it falls back to the heuristic path.  Both paths
//! share the same threshold env var (`POND_FACE_ANTISPOOF_THRESHOLD`).
//!
//! # Pipeline shape
//!
//! Silent-Face takes an 80×80 BGR (yes, BGR — the open-source weights ship
//! that way) face crop and outputs a 3-class softmax over `[live, fake_2D,
//! fake_3D]`.  We collapse `fake_*` into a single "spoof probability" and
//! return the fraction in `[0, 1]`.  The crop must come from the same
//! aligned 112×112 we feed ArcFace; we down-sample with a Triangle filter.
//!
//! # Status
//!
//! This module compiles and runs end-to-end against any ONNX file that
//! matches the Silent-Face graph signature.  Until a real model is
//! deposited at the configured path, [`OnnxAntispoof::try_from_env`]
//! returns `Ok(None)` and the embedding pipeline keeps using the
//! heuristic.

use crate::antispoof::AntispoofReport;
use anyhow::{anyhow, Context, Result};
use image::imageops::FilterType;
use image::{DynamicImage, RgbImage};
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

/// Silent-Face input geometry.  Don't change unless you also retrain.
const INPUT_SIDE: u32 = 80;

/// DeepPixBis input geometry — fixed by the OULU-NPU Protocol-2 export.
const DEEPPIXBIS_SIDE: u32 = 224;

/// Which architecture an [`OnnxAntispoof`] instance is wrapping.
///
/// The two architectures we currently understand differ in input size,
/// channel order, pixel scale, and output shape — so the rest of
/// [`OnnxAntispoof::analyse`] dispatches on this enum rather than trying
/// to guess at run time.
///
/// Detection happens once, at construction:
///
///   * If the matching env var (`POND_FACE_ANTISPOOF_VARIANT` for the
///     primary slot, `POND_FACE_ANTISPOOF_2_VARIANT` for the secondary)
///     is set to `silentface` or `deeppixbis`, that wins.
///   * Otherwise we sniff the filename: anything containing `deeppixbis`,
///     `oulu_protocol`, `pixel_supervision`, or starting with `pixbis_`
///     is treated as DeepPixBis. Everything else is Silent-Face.
///
/// File-name detection is conservative — false positives would silently
/// feed the wrong tensor shape to the model, so we err toward the
/// established Silent-Face path unless the name strongly suggests
/// DeepPixBis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// Minivision Silent-Face (MiniFASNetV2 / V1SE) — 80×80 BGR input,
    /// multi-class softmax output (`[live, fake_2D, fake_3D]` for the
    /// 3-class export, `[spoof, live]` for the 2-class yakhyo export).
    SilentFace80,
    /// DeepPixBis trained on OULU-NPU Protocol 2 — 224×224 RGB input,
    /// dual output `(output_pixel: [1,1,14,14], output_binary: [1,1])`.
    /// We read the binary head as a sigmoid scalar and treat
    /// `1 - p_live` as the spoof score.
    DeepPixBis224,
}

pub struct OnnxAntispoof {
    session:    Arc<Mutex<Session>>,
    model_path: PathBuf,
    variant:    Variant,
}

impl OnnxAntispoof {
    /// Build a primary-slot anti-spoof — variant detection consults
    /// `POND_FACE_ANTISPOOF_VARIANT`.  Use [`Self::new_with_variant_env`]
    /// when you need to point a secondary slot at a different env var.
    pub fn new(model_path: impl Into<PathBuf>) -> Result<Self> {
        Self::new_with_variant_env(model_path, "POND_FACE_ANTISPOOF_VARIANT")
    }

    /// Build with explicit control over which env var supplies the
    /// variant override. The env var lookup falls back to the filename
    /// heuristic when unset.
    pub fn new_with_variant_env(
        model_path: impl Into<PathBuf>,
        variant_env: &str,
    ) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!(
                "Anti-spoof model not found at {}",
                path.display()
            ));
        }
        let variant = resolve_variant(&path, variant_env);
        let session = Session::builder()
            .context("failed to create ort session builder for anti-spoof")?
            .commit_from_file(&path)
            .with_context(|| {
                format!("failed to load anti-spoof ONNX model at {}", path.display())
            })?;
        info!(
            path = %path.display(), ?variant,
            "anti-spoof ONNX model loaded"
        );
        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            model_path: path,
            variant,
        })
    }

    /// Construct from `$POND_FACE_ANTISPOOF_PATH`, or return `Ok(None)` when
    /// the env var is unset / the file is missing.  Intentionally lenient:
    /// a missing model is the *expected* state on installs that haven't
    /// downloaded one yet — we don't want server boot to fail for it.
    pub fn try_from_env() -> Result<Option<Self>> {
        Self::try_from_env_var("POND_FACE_ANTISPOOF_PATH")
    }

    /// Load from an arbitrary env var.  Used to wire the secondary
    /// ensemble model (`POND_FACE_ANTISPOOF_PATH_2`).
    ///
    /// Variant override is read from the matching `*_VARIANT` env var:
    ///
    ///   * `POND_FACE_ANTISPOOF_PATH`   ↔ `POND_FACE_ANTISPOOF_VARIANT`
    ///   * `POND_FACE_ANTISPOOF_PATH_2` ↔ `POND_FACE_ANTISPOOF_2_VARIANT`
    ///   * otherwise: the path env var name with `_VARIANT` appended.
    pub fn try_from_env_var(var: &str) -> Result<Option<Self>> {
        let Ok(raw) = std::env::var(var) else {
            return Ok(None);
        };
        let path = PathBuf::from(raw);
        if !path.exists() {
            warn!(
                "{} set to {} but file does not exist; skipping",
                var, path.display()
            );
            return Ok(None);
        }
        let variant_env = match var {
            "POND_FACE_ANTISPOOF_PATH"   => "POND_FACE_ANTISPOOF_VARIANT".to_string(),
            "POND_FACE_ANTISPOOF_PATH_2" => "POND_FACE_ANTISPOOF_2_VARIANT".to_string(),
            other => format!("{}_VARIANT", other),
        };
        Ok(Some(Self::new_with_variant_env(path, &variant_env)?))
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// Score an aligned 112×112 RGB crop and return a synthetic
    /// [`AntispoofReport`] whose `spoof_score` is the model's combined
    /// fake probability.  The diagnostic fields (saturation_var etc.)
    /// are zeroed because they are not produced by the ONNX path.
    pub fn analyse(&self, img: &RgbImage) -> Result<AntispoofReport> {
        match self.variant {
            Variant::SilentFace80   => self.analyse_silent_face(img),
            Variant::DeepPixBis224 => self.analyse_deep_pix_bis(img),
        }
    }

    /// Silent-Face MiniFASNetV2 / V1SE path — 80×80 BGR input, multi-class
    /// softmax output.  See the block comment at the top of the file for
    /// why pixel scale + class ordering each have an env override.
    fn analyse_silent_face(&self, img: &RgbImage) -> Result<AntispoofReport> {
        // Down-sample 112×112 → 80×80 with a Triangle filter.
        let small = DynamicImage::ImageRgb8(img.clone())
            .resize_exact(INPUT_SIDE, INPUT_SIDE, FilterType::Triangle)
            .to_rgb8();

        // Pixel scaling differs between known Silent-Face exports:
        //   * yakhyo MiniFASNetV2 (2-class) — raw `[0, 255]` float
        //   * minivision-ai 3-class — `[0, 1]`
        // Configurable via POND_FACE_ANTISPOOF_PIXEL_SCALE.
        let divisor: f32 = match pixel_scale_from_env() {
            PixelScale::Raw => 1.0,
            PixelScale::Unit => 255.0,
        };
        let side = INPUT_SIDE as usize;
        let mut tensor = Array4::<f32>::zeros((1, 3, side, side));
        for (x, y, px) in small.enumerate_pixels() {
            let [r, g, b] = px.0;
            // BGR channel order: 0 = B, 1 = G, 2 = R.
            tensor[[0, 0, y as usize, x as usize]] = b as f32 / divisor;
            tensor[[0, 1, y as usize, x as usize]] = g as f32 / divisor;
            tensor[[0, 2, y as usize, x as usize]] = r as f32 / divisor;
        }

        let input = Tensor::from_array(tensor)
            .context("failed to wrap anti-spoof input tensor")?;
        let mut sess = self
            .session
            .lock()
            .map_err(|_| anyhow!("anti-spoof session mutex poisoned"))?;
        let outputs = sess
            .run(ort::inputs![input])
            .context("Silent-Face anti-spoof inference failed")?;
        let (_name, first) = outputs
            .iter()
            .next()
            .ok_or_else(|| anyhow!("Silent-Face returned no outputs"))?;
        let (_shape, data) = first
            .try_extract_tensor::<f32>()
            .context("failed to extract Silent-Face output tensor")?;

        // Most Silent-Face exports return raw logits; some return softmax.
        // Defensive softmax is idempotent on already-normalised dists up to
        // FP error.  Class-ordering varies (see block comment at top).
        let logits = data;
        if logits.len() < 2 {
            return Err(anyhow!(
                "Silent-Face anti-spoof returned {} values; expected ≥2",
                logits.len()
            ));
        }
        let max = logits.iter().cloned().fold(f32::MIN, f32::max);
        let exps: Vec<f32> = logits.iter().map(|v| (v - max).exp()).collect();
        let sum: f32 = exps.iter().sum();
        let probs: Vec<f32> = if sum > 0.0 {
            exps.iter().map(|e| e / sum).collect()
        } else {
            logits.to_vec()
        };

        let live_index = live_index_from_env(probs.len());
        let live = probs[live_index].clamp(0.0, 1.0);
        let spoof = (1.0 - live).clamp(0.0, 1.0);

        debug!(
            spoof, live, live_index, n_classes = probs.len(),
            probs = ?probs,
            "Silent-Face score"
        );
        Ok(AntispoofReport {
            spoof_score:       spoof,
            saturation_var:    0.0,
            highlight_density: 0.0,
            gradient_skew:     0.0,
        })
    }

    /// DeepPixBis (OULU-NPU Protocol-2) path — 224×224 RGB input, dual
    /// output `(output_pixel, output_binary)`.  We read the binary head as
    /// a single sigmoid scalar in `[0, 1]` representing live probability,
    /// and report `1 - p_live` as the spoof score so the same threshold
    /// arithmetic that `OnnxAntispoof::analyse_silent_face` returns
    /// applies.  The pixel-supervision map is intentionally ignored — for
    /// our use-case the binary head is the calibrated signal.
    ///
    /// Pre-processing follows the reference repo
    /// `ffletcherr/face-recognition-liveness`: ImageNet-style standardisation
    /// (`(x/255 - mean) / std` with mean=[0.485, 0.456, 0.406], std=[0.229,
    /// 0.224, 0.225]) on RGB pixels in NCHW order.
    fn analyse_deep_pix_bis(&self, img: &RgbImage) -> Result<AntispoofReport> {
        // Resize 112×112 → 224×224 (we receive an aligned 112 crop from
        // the embedder pipeline; DeepPixBis was trained on full-face crops
        // at 224×224 so the upscale is fine, the model's first stride
        // collapses it back down anyway).
        let big = DynamicImage::ImageRgb8(img.clone())
            .resize_exact(DEEPPIXBIS_SIDE, DEEPPIXBIS_SIDE, FilterType::Triangle)
            .to_rgb8();

        // ImageNet normalisation per channel (R, G, B).
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD:  [f32; 3] = [0.229, 0.224, 0.225];
        let side = DEEPPIXBIS_SIDE as usize;
        let mut tensor = Array4::<f32>::zeros((1, 3, side, side));
        for (x, y, px) in big.enumerate_pixels() {
            let [r, g, b] = px.0;
            // RGB channel order — matches torchvision PIL pipeline.
            let rn = ((r as f32 / 255.0) - MEAN[0]) / STD[0];
            let gn = ((g as f32 / 255.0) - MEAN[1]) / STD[1];
            let bn = ((b as f32 / 255.0) - MEAN[2]) / STD[2];
            tensor[[0, 0, y as usize, x as usize]] = rn;
            tensor[[0, 1, y as usize, x as usize]] = gn;
            tensor[[0, 2, y as usize, x as usize]] = bn;
        }

        let input = Tensor::from_array(tensor)
            .context("failed to wrap DeepPixBis input tensor")?;
        let mut sess = self
            .session
            .lock()
            .map_err(|_| anyhow!("anti-spoof session mutex poisoned"))?;
        let outputs = sess
            .run(ort::inputs![input])
            .context("DeepPixBis anti-spoof inference failed")?;

        // The OULU export emits TWO outputs: `output_pixel` (a 14×14
        // pixel-supervision map) and `output_binary` (a single sigmoid
        // scalar). Find the binary head by name first; fall back to the
        // tensor with the smallest element count if the names ever change.
        let (binary_name, binary_value) = outputs
            .iter()
            .find(|(name, _)| name.contains("output_binary") || name.contains("binary"))
            .or_else(|| {
                outputs
                    .iter()
                    .min_by_key(|(_, v)| {
                        v.try_extract_tensor::<f32>()
                            .map(|(_, d)| d.len())
                            .unwrap_or(usize::MAX)
                    })
            })
            .ok_or_else(|| anyhow!("DeepPixBis returned no outputs"))?;

        let (_shape, data) = binary_value
            .try_extract_tensor::<f32>()
            .with_context(|| format!("failed to extract DeepPixBis tensor `{binary_name}`"))?;

        if data.is_empty() {
            return Err(anyhow!(
                "DeepPixBis tensor `{binary_name}` was empty; expected ≥1 value"
            ));
        }

        // The binary head's output is described as a sigmoid probability in
        // `[0, 1]` — but some PyTorch exports leave it as a raw logit.
        // Defensive: detect by range.  If the value is already inside
        // `[0, 1]` we trust it; otherwise we apply sigmoid.
        let raw = data[0];
        let p_live = if (0.0..=1.0).contains(&raw) {
            raw
        } else {
            1.0 / (1.0 + (-raw).exp())
        };
        let spoof = (1.0 - p_live).clamp(0.0, 1.0);

        debug!(
            spoof, p_live, raw, output_name = %binary_name,
            "DeepPixBis score"
        );
        Ok(AntispoofReport {
            spoof_score:       spoof,
            saturation_var:    0.0,
            highlight_density: 0.0,
            gradient_skew:     0.0,
        })
    }

    pub fn model_path(&self) -> &std::path::Path {
        &self.model_path
    }
}

/// Pixel-scale convention for the Silent-Face input tensor.
///
/// See the block comment in [`OnnxAntispoof::analyse`] for why this matters:
/// the yakhyo MiniFASNetV2 export expects raw `[0, 255]` floats, while the
/// minivision-ai 3-class originals expect `[0, 1]`.
#[derive(Clone, Copy, Debug)]
enum PixelScale {
    /// Feed pixels as-is, in `[0, 255]`.  Divisor = 1.
    Raw,
    /// Feed pixels as `[0, 1]`.  Divisor = 255.
    Unit,
}

/// Resolve the pixel-scale convention from
/// `POND_FACE_ANTISPOOF_PIXEL_SCALE={auto,raw,unit}`.  Auto → `Raw`
/// because the yakhyo MiniFASNetV2 weights we ship install instructions
/// for are the common case and want raw `[0, 255]` input.  Operators
/// using a 3-class minivision export should set `unit` explicitly.
fn pixel_scale_from_env() -> PixelScale {
    let raw = std::env::var("POND_FACE_ANTISPOOF_PIXEL_SCALE").ok();
    match raw.as_deref().unwrap_or("auto") {
        "auto" | "" | "raw" => PixelScale::Raw,
        "unit" => PixelScale::Unit,
        other => {
            warn!(
                raw = %other,
                "invalid POND_FACE_ANTISPOOF_PIXEL_SCALE; falling back to raw"
            );
            PixelScale::Raw
        }
    }
}

/// Resolve which softmax slot holds the "live" class.
///
/// Honours `POND_FACE_ANTISPOOF_LIVE_INDEX`:
///
///   * `auto` (default) — last slot for 2-class outputs (matches the yakhyo
///     MiniFASNetV2 export), first slot for 3-class outputs (matches the
///     original minivision-ai checkpoints).
///   * A literal integer (`0`, `1`, …) — force that slot.
///
/// Clamped to a valid range so a bad env value can't panic the adapter.
fn live_index_from_env(n_classes: usize) -> usize {
    let raw = std::env::var("POND_FACE_ANTISPOOF_LIVE_INDEX").ok();
    match raw.as_deref().unwrap_or("auto") {
        "auto" | "" => {
            if n_classes == 2 {
                1 // [spoof, live]
            } else {
                0 // [live, fake_2D, fake_3D]
            }
        }
        other => match other.parse::<usize>() {
            Ok(i) if i < n_classes => i,
            _ => {
                warn!(
                    n_classes, raw = %other,
                    "invalid POND_FACE_ANTISPOOF_LIVE_INDEX; falling back to auto"
                );
                if n_classes == 2 { 1 } else { 0 }
            }
        },
    }
}

/// Resolve which architecture an anti-spoof file is — env override first,
/// filename heuristic second, Silent-Face by default.
///
/// `variant_env` is the name of the env var to consult. It accepts:
///   * `silentface` — force Silent-Face (80×80 BGR, multi-class softmax)
///   * `deeppixbis` — force DeepPixBis (224×224 RGB, sigmoid scalar)
///   * unset / `auto` / anything else → fall through to filename heuristic
///
/// The filename heuristic looks for any of `deeppixbis`, `oulu_protocol`,
/// `oulu_npu`, `pixel_supervision`, or a `pixbis_` prefix in the basename
/// (case-insensitive).
fn resolve_variant(path: &std::path::Path, variant_env: &str) -> Variant {
    if let Ok(raw) = std::env::var(variant_env) {
        match raw.trim().to_ascii_lowercase().as_str() {
            "silentface" | "silent_face" | "silent-face" => return Variant::SilentFace80,
            "deeppixbis" | "deep_pix_bis" | "deep-pix-bis" => return Variant::DeepPixBis224,
            "" | "auto" => { /* fall through */ }
            other => warn!(
                raw = %other, var = %variant_env,
                "unknown anti-spoof variant override; using filename heuristic"
            ),
        }
    }

    let lower = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let looks_like_deeppixbis =
        lower.contains("deeppixbis")
        || lower.contains("oulu_protocol")
        || lower.contains("oulu_npu")
        || lower.contains("pixel_supervision")
        || lower.starts_with("pixbis_");

    if looks_like_deeppixbis {
        Variant::DeepPixBis224
    } else {
        Variant::SilentFace80
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// `cargo test` runs tests in parallel. The variant tests below all
    /// poke `POND_FACE_ANTISPOOF_VARIANT` and would otherwise stomp on
    /// each other's reads. We could `#[serial]` them with `serial_test`,
    /// but a tiny in-mod mutex is dependency-free and equivalent.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_variant_default_is_silent_face() {
        let _g = ENV_LOCK.lock().unwrap();
        let p = std::path::Path::new("/x/2.7_80x80_MiniFASNetV2.onnx");
        unsafe { std::env::remove_var("POND_FACE_ANTISPOOF_VARIANT"); }
        assert_eq!(resolve_variant(p, "POND_FACE_ANTISPOOF_VARIANT"), Variant::SilentFace80);
    }

    #[test]
    fn resolve_variant_detects_deeppixbis_from_filename() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("POND_FACE_ANTISPOOF_VARIANT"); }
        for name in [
            "/x/OULU_Protocol_2_model_0_0.onnx",
            "/x/deeppixbis.onnx",
            "/x/DeepPixBis-OULU.onnx",
            "/x/pixbis_model.onnx",
            "/x/pixel_supervision_v1.onnx",
        ] {
            let p = std::path::Path::new(name);
            assert_eq!(
                resolve_variant(p, "POND_FACE_ANTISPOOF_VARIANT"),
                Variant::DeepPixBis224,
                "expected DeepPixBis for {name}",
            );
        }
    }

    #[test]
    fn resolve_variant_env_override_wins() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serialises every test in this file that touches
        // POND_FACE_ANTISPOOF_VARIANT; we always restore on the way out.
        let p = std::path::Path::new("/x/deeppixbis.onnx");
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_VARIANT", "silentface"); }
        assert_eq!(resolve_variant(p, "POND_FACE_ANTISPOOF_VARIANT"), Variant::SilentFace80);
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_VARIANT", "deeppixbis"); }
        let q = std::path::Path::new("/x/2.7_80x80_MiniFASNetV2.onnx");
        assert_eq!(resolve_variant(q, "POND_FACE_ANTISPOOF_VARIANT"), Variant::DeepPixBis224);
        unsafe { std::env::remove_var("POND_FACE_ANTISPOOF_VARIANT"); }
    }

    #[test]
    fn try_from_env_with_unset_var_returns_none() {
        // SAFETY: we only mutate POND_FACE_ANTISPOOF_PATH for this test,
        // and the var should not be set in the default test environment.
        unsafe { std::env::remove_var("POND_FACE_ANTISPOOF_PATH"); }
        let r = OnnxAntispoof::try_from_env().expect("should not error");
        assert!(r.is_none());
    }

    #[test]
    fn try_from_env_with_missing_file_returns_none() {
        unsafe { std::env::set_var("POND_FACE_ANTISPOOF_PATH", "/does/not/exist.onnx"); }
        let r = OnnxAntispoof::try_from_env().expect("should not error");
        assert!(r.is_none());
        unsafe { std::env::remove_var("POND_FACE_ANTISPOOF_PATH"); }
    }

    #[test]
    fn new_fails_on_missing_path() {
        assert!(OnnxAntispoof::new("/no/such/file.onnx").is_err());
    }
}
