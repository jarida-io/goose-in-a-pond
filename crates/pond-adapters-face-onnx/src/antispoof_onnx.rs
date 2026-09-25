//! ONNX anti-spoof (Silent-Face or DeepPixBis) producing an [`AntispoofReport`]; used instead
//! of the heuristic when `$POND_FACE_ANTISPOOF_PATH` names a model.

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

/// Silent-Face input side, fixed by the trained model.
const INPUT_SIDE: u32 = 80;

/// DeepPixBis input geometry — fixed by the OULU-NPU Protocol-2 export.
const DEEPPIXBIS_SIDE: u32 = 224;

/// Anti-spoof architecture; it determines input size, channel order, pixel scale and output shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// Silent-Face MiniFASNet, 80×80 BGR; softmax `[live, fake_2D, fake_3D]` or `[spoof, live]`.
    SilentFace80,
    /// DeepPixBis (OULU-NPU Protocol 2), 224×224 RGB; spoof = `1 - p_live` from `output_binary`.
    DeepPixBis224,
}

pub struct OnnxAntispoof {
    session: Arc<Mutex<Session>>,
    model_path: PathBuf,
    variant: Variant,
}

impl OnnxAntispoof {
    /// Primary-slot model; the variant override comes from `POND_FACE_ANTISPOOF_VARIANT`.
    pub fn new(model_path: impl Into<PathBuf>) -> Result<Self> {
        Self::new_with_variant_env(model_path, "POND_FACE_ANTISPOOF_VARIANT")
    }

    /// As `new`, reading the variant override from `variant_env` (filename heuristic if unset).
    pub fn new_with_variant_env(model_path: impl Into<PathBuf>, variant_env: &str) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!("Anti-spoof model not found at {}", path.display()));
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

    /// Load `$POND_FACE_ANTISPOOF_PATH`, or `Ok(None)` if unset/missing (normal, not a boot error).
    pub fn try_from_env() -> Result<Option<Self>> {
        Self::try_from_env_var("POND_FACE_ANTISPOOF_PATH")
    }

    /// Load from a path env var, e.g. the secondary `POND_FACE_ANTISPOOF_PATH_2`.
    pub fn try_from_env_var(var: &str) -> Result<Option<Self>> {
        let Ok(raw) = std::env::var(var) else {
            return Ok(None);
        };
        let path = PathBuf::from(raw);
        if !path.exists() {
            warn!(
                "{} set to {} but file does not exist; skipping",
                var,
                path.display()
            );
            return Ok(None);
        }
        let variant_env = match var {
            "POND_FACE_ANTISPOOF_PATH" => "POND_FACE_ANTISPOOF_VARIANT".to_string(),
            "POND_FACE_ANTISPOOF_PATH_2" => "POND_FACE_ANTISPOOF_2_VARIANT".to_string(),
            other => format!("{}_VARIANT", other),
        };
        Ok(Some(Self::new_with_variant_env(path, &variant_env)?))
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// Score an aligned 112×112 crop; only `spoof_score` is set, the heuristic diagnostics are 0.
    pub fn analyse(&self, img: &RgbImage) -> Result<AntispoofReport> {
        match self.variant {
            Variant::SilentFace80 => self.analyse_silent_face(img),
            Variant::DeepPixBis224 => self.analyse_deep_pix_bis(img),
        }
    }

    fn analyse_silent_face(&self, img: &RgbImage) -> Result<AntispoofReport> {
        let small = DynamicImage::ImageRgb8(img.clone())
            .resize_exact(INPUT_SIDE, INPUT_SIDE, FilterType::Triangle)
            .to_rgb8();

        let divisor: f32 = match pixel_scale_from_env() {
            PixelScale::Raw => 1.0,
            PixelScale::Unit => 255.0,
        };
        let side = INPUT_SIDE as usize;
        let mut tensor = Array4::<f32>::zeros((1, 3, side, side));
        for (x, y, px) in small.enumerate_pixels() {
            let [r, g, b] = px.0;
            // Silent-Face expects BGR.
            tensor[[0, 0, y as usize, x as usize]] = b as f32 / divisor;
            tensor[[0, 1, y as usize, x as usize]] = g as f32 / divisor;
            tensor[[0, 2, y as usize, x as usize]] = r as f32 / divisor;
        }

        let input = Tensor::from_array(tensor).context("failed to wrap anti-spoof input tensor")?;
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

        // Softmax assumes raw logits; class order varies by export (see `live_index_from_env`).
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
            spoof_score: spoof,
            saturation_var: 0.0,
            highlight_density: 0.0,
            gradient_skew: 0.0,
        })
    }

    /// Preprocessing follows `ffletcherr/face-recognition-liveness`; only the binary head is read.
    fn analyse_deep_pix_bis(&self, img: &RgbImage) -> Result<AntispoofReport> {
        // Upscaling the aligned 112 crop is fine: the model's first stride downsamples anyway.
        let big = DynamicImage::ImageRgb8(img.clone())
            .resize_exact(DEEPPIXBIS_SIDE, DEEPPIXBIS_SIDE, FilterType::Triangle)
            .to_rgb8();

        // ImageNet normalisation per channel (R, G, B).
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
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

        let input = Tensor::from_array(tensor).context("failed to wrap DeepPixBis input tensor")?;
        let mut sess = self
            .session
            .lock()
            .map_err(|_| anyhow!("anti-spoof session mutex poisoned"))?;
        let outputs = sess
            .run(ort::inputs![input])
            .context("DeepPixBis anti-spoof inference failed")?;

        // Find `output_binary` by name, else the smallest output (the other is a 14×14 pixel map).
        let (binary_name, binary_value) = outputs
            .iter()
            .find(|(name, _)| name.contains("output_binary") || name.contains("binary"))
            .or_else(|| {
                outputs.iter().min_by_key(|(_, v)| {
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

        // Some exports leave the sigmoid off; a value outside [0, 1] is taken as a logit.
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
            spoof_score: spoof,
            saturation_var: 0.0,
            highlight_density: 0.0,
            gradient_skew: 0.0,
        })
    }

    pub fn model_path(&self) -> &std::path::Path {
        &self.model_path
    }
}

/// Silent-Face input scale: yakhyo MiniFASNetV2 wants raw `[0, 255]`, minivision 3-class `[0, 1]`.
#[derive(Clone, Copy, Debug)]
enum PixelScale {
    /// Feed pixels as-is, in `[0, 255]`.  Divisor = 1.
    Raw,
    /// Feed pixels as `[0, 1]`.  Divisor = 255.
    Unit,
}

/// `POND_FACE_ANTISPOOF_PIXEL_SCALE={auto,raw,unit}`; auto is `Raw`, matching the shipped weights.
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

/// Softmax slot of the "live" class from `POND_FACE_ANTISPOOF_LIVE_INDEX`: auto is last for
/// 2-class, first for 3-class; an out-of-range index falls back to auto rather than panic.
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
                if n_classes == 2 {
                    1
                } else {
                    0
                }
            }
        },
    }
}

/// `variant_env` override, else a filename match, else Silent-Face. The match is conservative:
/// a wrong guess silently feeds the wrong tensor.
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

    let looks_like_deeppixbis = lower.contains("deeppixbis")
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

    /// Serialises the tests that set `POND_FACE_ANTISPOOF_VARIANT` (tests run in parallel).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn resolve_variant_default_is_silent_face() {
        let _g = ENV_LOCK.lock().unwrap();
        let p = std::path::Path::new("/x/2.7_80x80_MiniFASNetV2.onnx");
        unsafe {
            std::env::remove_var("POND_FACE_ANTISPOOF_VARIANT");
        }
        assert_eq!(
            resolve_variant(p, "POND_FACE_ANTISPOOF_VARIANT"),
            Variant::SilentFace80
        );
    }

    #[test]
    fn resolve_variant_detects_deeppixbis_from_filename() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("POND_FACE_ANTISPOOF_VARIANT");
        }
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
        unsafe {
            std::env::set_var("POND_FACE_ANTISPOOF_VARIANT", "silentface");
        }
        assert_eq!(
            resolve_variant(p, "POND_FACE_ANTISPOOF_VARIANT"),
            Variant::SilentFace80
        );
        unsafe {
            std::env::set_var("POND_FACE_ANTISPOOF_VARIANT", "deeppixbis");
        }
        let q = std::path::Path::new("/x/2.7_80x80_MiniFASNetV2.onnx");
        assert_eq!(
            resolve_variant(q, "POND_FACE_ANTISPOOF_VARIANT"),
            Variant::DeepPixBis224
        );
        unsafe {
            std::env::remove_var("POND_FACE_ANTISPOOF_VARIANT");
        }
    }

    #[test]
    fn try_from_env_with_unset_var_returns_none() {
        // SAFETY: we only mutate POND_FACE_ANTISPOOF_PATH for this test,
        // and the var should not be set in the default test environment.
        unsafe {
            std::env::remove_var("POND_FACE_ANTISPOOF_PATH");
        }
        let r = OnnxAntispoof::try_from_env().expect("should not error");
        assert!(r.is_none());
    }

    #[test]
    fn try_from_env_with_missing_file_returns_none() {
        unsafe {
            std::env::set_var("POND_FACE_ANTISPOOF_PATH", "/does/not/exist.onnx");
        }
        let r = OnnxAntispoof::try_from_env().expect("should not error");
        assert!(r.is_none());
        unsafe {
            std::env::remove_var("POND_FACE_ANTISPOOF_PATH");
        }
    }

    #[test]
    fn new_fails_on_missing_path() {
        assert!(OnnxAntispoof::new("/no/such/file.onnx").is_err());
    }
}
