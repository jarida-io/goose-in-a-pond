//! UltraFace (RFB-320) ONNX [`FaceDetector`]: boxes only, no landmarks. Model:
//! <https://github.com/onnx/models/tree/main/validated/vision/body_analysis/ultraface>

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use image::imageops::FilterType;
use image::GenericImageView;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use pond_core::user_data::domain::face_recognition::{BoundingBox, DetectedFace};
use pond_core::user_data::ports::face_detector::FaceDetector;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

/// Default input size for the RFB-320 variant.
const INPUT_W: u32 = 320;
const INPUT_H: u32 = 240;

/// Per-channel mean subtracted from raw pixel values (0-255).
const MEAN: [f32; 3] = [127.0, 127.0, 127.0];
/// Inverse std applied after mean subtraction.
const SCALE: f32 = 1.0 / 128.0;

pub struct UltraFaceDetector {
    session: Arc<Mutex<Session>>,
    score_thresh: f32,
    iou_thresh: f32,
    input_w: u32,
    input_h: u32,
}

impl UltraFaceDetector {
    /// Load a model; 0.7 `score_thresh` suits indoor cameras, 0.3 is a typical NMS `iou_thresh`.
    pub fn new(model_path: impl Into<PathBuf>, score_thresh: f32, iou_thresh: f32) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!(
                "UltraFace ONNX model not found at {}",
                path.display()
            ));
        }
        let session = Session::builder()
            .context("failed to create ort session builder")?
            .commit_from_file(&path)
            .with_context(|| {
                format!("failed to load UltraFace ONNX model at {}", path.display())
            })?;
        info!(path = %path.display(), "UltraFace detector loaded");
        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            score_thresh,
            iou_thresh,
            input_w: INPUT_W,
            input_h: INPUT_H,
        })
    }

    /// Decode into the `[1,3,H,W]` input tensor, plus the original size for rescaling boxes.
    fn preprocess(&self, bytes: &[u8]) -> Result<(Array4<f32>, u32, u32)> {
        let img = image::load_from_memory(bytes).context("failed to decode image")?;
        let (orig_w, orig_h) = img.dimensions();
        let resized = img
            .resize_exact(self.input_w, self.input_h, FilterType::Triangle)
            .to_rgb8();

        let mut arr = Array4::<f32>::zeros((1, 3, self.input_h as usize, self.input_w as usize));
        for (x, y, px) in resized.enumerate_pixels() {
            let [r, g, b] = px.0;
            arr[[0, 0, y as usize, x as usize]] = (r as f32 - MEAN[0]) * SCALE;
            arr[[0, 1, y as usize, x as usize]] = (g as f32 - MEAN[1]) * SCALE;
            arr[[0, 2, y as usize, x as usize]] = (b as f32 - MEAN[2]) * SCALE;
        }
        Ok((arr, orig_w, orig_h))
    }
}

#[async_trait]
impl FaceDetector for UltraFaceDetector {
    async fn detect_face(&self, image_bytes: &[u8]) -> Result<Option<DetectedFace>> {
        let bytes = image_bytes.to_vec();
        let session = Arc::clone(&self.session);
        let score_thresh = self.score_thresh;
        let iou_thresh = self.iou_thresh;
        let this_input = (self.input_w, self.input_h);

        let res = tokio::task::spawn_blocking(move || -> Result<Option<DetectedFace>> {
            // Decode here too, off the async runtime.
            let det = UltraFaceDetector {
                session: session.clone(),
                score_thresh,
                iou_thresh,
                input_w: this_input.0,
                input_h: this_input.1,
            };
            let (input, orig_w, orig_h) = det.preprocess(&bytes)?;

            let mut sess = session
                .lock()
                .map_err(|_| anyhow!("UltraFace session mutex poisoned"))?;
            let input_tensor = Tensor::from_array(input)?;
            let outputs = sess
                .run(ort::inputs![input_tensor])
                .context("UltraFace inference failed")?;

            // scores [1, N, 2] = (background, face); boxes [1, N, 4] = normalised x1,y1,x2,y2.
            // Output names vary, so match on the last dim.
            let mut scores_vec: Option<Vec<f32>> = None;
            let mut scores_shape: Vec<usize> = vec![];
            let mut boxes_vec: Option<Vec<f32>> = None;
            let mut boxes_shape: Vec<usize> = vec![];
            for (_name, value) in outputs.iter() {
                let (shape, data) = value
                    .try_extract_tensor::<f32>()
                    .context("failed to extract UltraFace output tensor")?;
                let ushape: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
                match ushape.last().copied().unwrap_or(0) {
                    4 => {
                        boxes_vec = Some(data.to_vec());
                        boxes_shape = ushape;
                    }
                    2 => {
                        scores_vec = Some(data.to_vec());
                        scores_shape = ushape;
                    }
                    other => {
                        debug!(last_dim = other, shape = ?ushape, "unexpected UltraFace output");
                    }
                }
            }
            let scores = scores_vec
                .ok_or_else(|| anyhow!("UltraFace scores tensor missing (last-dim 2)"))?;
            let boxes =
                boxes_vec.ok_or_else(|| anyhow!("UltraFace boxes tensor missing (last-dim 4)"))?;

            let n = *scores_shape.get(1).unwrap_or(&0);
            if n == 0 || boxes_shape.get(1) != Some(&n) {
                return Err(anyhow!(
                    "UltraFace shape mismatch: scores={:?}, boxes={:?}",
                    scores_shape,
                    boxes_shape,
                ));
            }

            let mut candidates: Vec<([f32; 4], f32)> = Vec::new();
            for i in 0..n {
                let face_score = scores[i * 2 + 1];
                if face_score < score_thresh {
                    continue;
                }
                let b = &boxes[i * 4..i * 4 + 4];
                let x1 = b[0].clamp(0.0, 1.0);
                let y1 = b[1].clamp(0.0, 1.0);
                let x2 = b[2].clamp(0.0, 1.0);
                let y2 = b[3].clamp(0.0, 1.0);
                if x2 <= x1 || y2 <= y1 {
                    continue;
                }
                candidates.push(([x1, y1, x2, y2], face_score));
            }
            debug!(
                n_candidates = candidates.len(),
                "UltraFace pre-NMS candidates"
            );

            if candidates.is_empty() {
                return Ok(None);
            }

            // Sort by score desc, then greedy NMS.
            candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let mut kept: Vec<([f32; 4], f32)> = Vec::new();
            for cand in candidates {
                let overlap = kept.iter().any(|k| iou(&k.0, &cand.0) > iou_thresh);
                if !overlap {
                    kept.push(cand);
                }
            }

            // UltraFace has no landmarks, so embedding falls back to crop-and-resize, unaligned.
            let top = kept.first().copied();
            Ok(top.map(|([x1, y1, x2, y2], score)| {
                let px1 = (x1 * orig_w as f32).round().clamp(0.0, orig_w as f32 - 1.0) as u32;
                let py1 = (y1 * orig_h as f32).round().clamp(0.0, orig_h as f32 - 1.0) as u32;
                let px2 = (x2 * orig_w as f32).round().clamp(0.0, orig_w as f32) as u32;
                let py2 = (y2 * orig_h as f32).round().clamp(0.0, orig_h as f32) as u32;
                DetectedFace {
                    bbox: BoundingBox {
                        x: px1,
                        y: py1,
                        width: px2.saturating_sub(px1),
                        height: py2.saturating_sub(py1),
                    },
                    landmarks: None,
                    score,
                }
            }))
        })
        .await
        .context("UltraFace spawn_blocking join failed")??;

        Ok(res)
    }
}

/// Intersection-over-union of two [x1,y1,x2,y2] boxes (normalised coords OK).
fn iou(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = a[2].min(b[2]);
    let iy2 = a[3].min(b[3]);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    let area_a = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let area_b = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_disjoint_is_zero() {
        let a = [0.0, 0.0, 0.2, 0.2];
        let b = [0.5, 0.5, 0.8, 0.8];
        assert_eq!(iou(&a, &b), 0.0);
    }

    #[test]
    fn iou_identical_is_one() {
        let a = [0.1, 0.1, 0.5, 0.5];
        assert!((iou(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn iou_half_overlap() {
        let a = [0.0, 0.0, 1.0, 1.0];
        let b = [0.5, 0.0, 1.5, 1.0];
        // intersection = 0.5, union = 1.5 → 1/3.
        let v = iou(&a, &b);
        assert!((v - (1.0 / 3.0)).abs() < 1e-6, "iou was {v}");
    }

    #[tokio::test]
    async fn new_returns_err_on_missing_file() {
        let err = UltraFaceDetector::new("/nonexistent/model.onnx", 0.7, 0.3);
        assert!(err.is_err());
    }
}
