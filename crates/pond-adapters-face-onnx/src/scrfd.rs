//! SCRFD face detector — bbox + five landmarks, ONNX.
//!
//! SCRFD (Sample and Computation Redistribution for Face Detection, InsightFace
//! 2021) is the de-facto standard detector for pairing with ArcFace.  Unlike
//! UltraFace it produces the five canonical landmarks (eyes, nose, mouth
//! corners) we need for similarity-transform alignment.
//!
//! # Graph shape
//!
//! SCRFD's ONNX export from InsightFace has three feature pyramid levels
//! (strides 8, 16, 32) and three output tensors per level:
//!   * `score_{stride}`    — (1, anchors·1, 1)        face-ness logit
//!   * `bbox_{stride}`     — (1, anchors·4, 1)        (l,t,r,b) distances
//!   * `kps_{stride}`      — (1, anchors·10, 1)       5×(dx, dy) offsets
//!
//! Each location at a given stride has `num_anchors = 2` anchor boxes.
//! Bbox regression is expressed as distance from the anchor centre to each
//! edge; keypoints are offsets from the anchor centre.  All values are
//! already in input-image pixel units (the `mean=127.5, scale=1/128`
//! preprocessing is applied on the raw pixels, not the output decoding).
//!
//! # Fallback policy
//!
//! If the model file at `$POND_FACE_DETECTOR_PATH` is absent, the server
//! falls back to UltraFace (bbox only), and the embedding path degrades to
//! crop-and-resize with the same behaviour as phase-2 baseline.  That is,
//! alignment is a strict accuracy improvement, not a correctness requirement.

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use image::imageops::FilterType;
use image::GenericImageView;
use ndarray::Array4;
use ort::session::Session;
use ort::value::Tensor;
use pond_core::domain::face_recognition::{BoundingBox, DetectedFace, FaceLandmarks};
use pond_core::ports::face_detector::FaceDetector;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

/// Input size SCRFD is typically exported at (640×640 for SCRFD_10G;
/// 320×320 for SCRFD_500M — the small one we prefer on edge).  We resize
/// letterbox-free because SCRFD's outputs are already in input-pixel space.
const INPUT_SIDE: u32 = 640;

const MEAN: f32 = 127.5;
const SCALE: f32 = 1.0 / 128.0;

/// Strides of the three FPN levels.  SCRFD's InsightFace export uses these
/// exact values; if a different export is dropped in, override via
/// `with_strides`.
const DEFAULT_STRIDES: [u32; 3] = [8, 16, 32];
const DEFAULT_NUM_ANCHORS: usize = 2;

pub struct ScrfdDetector {
    session:       Arc<Mutex<Session>>,
    score_thresh:  f32,
    iou_thresh:    f32,
    strides:       Vec<u32>,
    num_anchors:   usize,
    input_side:    u32,
}

impl ScrfdDetector {
    /// Load an SCRFD ONNX model.
    ///
    /// `score_thresh` — face-ness threshold (default 0.5).
    /// `iou_thresh`   — NMS IoU threshold (default 0.4).
    pub fn new(
        model_path: impl Into<PathBuf>,
        score_thresh: f32,
        iou_thresh: f32,
    ) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!("SCRFD ONNX model not found at {}", path.display()));
        }
        let session = Session::builder()
            .context("failed to create ort session builder")?
            .commit_from_file(&path)
            .with_context(|| format!("failed to load SCRFD model at {}", path.display()))?;
        info!(path = %path.display(), "SCRFD detector loaded");
        Ok(Self {
            session:      Arc::new(Mutex::new(session)),
            score_thresh,
            iou_thresh,
            strides:      DEFAULT_STRIDES.to_vec(),
            num_anchors:  DEFAULT_NUM_ANCHORS,
            input_side:   INPUT_SIDE,
        })
    }

    /// Override FPN strides (some SCRFD exports use different layer strides).
    pub fn with_strides(mut self, s: Vec<u32>) -> Self { self.strides = s; self }
    /// Override the per-location anchor count (1 or 2).
    pub fn with_anchors(mut self, n: usize) -> Self { self.num_anchors = n; self }
}

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
    if union <= 0.0 { 0.0 } else { inter / union }
}

#[async_trait]
impl FaceDetector for ScrfdDetector {
    fn produces_landmarks(&self) -> bool { true }

    async fn detect_face(&self, image_bytes: &[u8]) -> Result<Option<DetectedFace>> {
        let bytes = image_bytes.to_vec();
        let session = Arc::clone(&self.session);
        // The configured score threshold (typically 0.5) is the right
        // floor for normally-lit input. In low light SCRFD's confidence
        // drops uniformly across all true faces — apply a relaxed
        // threshold below LOW_LIGHT_TRIGGER so we still detect dim
        // faces. The matcher's own threshold + the burst-liveness gates
        // remain the real spoof / quality safeguards.
        let configured_score_thresh = self.score_thresh;
        let low_light_score_thresh: f32 = std::env::var("POND_FACE_SCRFD_LOW_LIGHT_THRESH")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .filter(|v| (0.05..=1.0).contains(v))
            .unwrap_or(0.30);
        let iou_thresh = self.iou_thresh;
        let strides = self.strides.clone();
        let num_anchors = self.num_anchors;
        let input_side = self.input_side;

        let det: Option<DetectedFace> = tokio::task::spawn_blocking(move || -> Result<Option<DetectedFace>> {
            // ── Preprocess ────────────────────────────────────────────────
            let img = image::load_from_memory(&bytes).context("decode failed")?;
            let (orig_w, orig_h) = img.dimensions();
            // Letterbox-free resize to a fixed square.  Aspect change is
            // absorbed by bbox rescaling below.
            let mut resized = img
                .resize_exact(input_side, input_side, FilterType::Triangle)
                .to_rgb8();

            // Pre-detection low-light auto-exposure.  When the input frame is
            // dim (mean luminance < LOW_LIGHT_TRIGGER), apply the same
            // per-channel 2-98 percentile stretch the embedder uses on its
            // aligned crop — but here on the FULL detector tensor BEFORE
            // SCRFD looks for faces.  This is the layer that matters for
            // low light: without it, SCRFD's confidence on a dim face drops
            // below its detection threshold and the embedder never even
            // sees the frame.  Disable with POND_FACE_AUTO_EXPOSURE=off.
            //
            // Also pick a per-frame score threshold: if the input is dim
            // (after the auto-exposure attempt) we relax to the low-light
            // floor since SCRFD's own confidence drops uniformly on dim
            // faces.  The matcher threshold + burst-liveness gates remain
            // the real spoof / quality safeguards downstream.
            let pre_mean = crate::mean_luminance(&resized);
            let was_dim = pre_mean < crate::LOW_LIGHT_TRIGGER;
            if crate::auto_exposure_enabled() && was_dim {
                crate::stretch_histogram_2_98(&mut resized);
                tracing::debug!(
                    before = pre_mean,
                    after = crate::mean_luminance(&resized),
                    "scrfd: applied low-light auto-exposure to detector input"
                );
            }
            let score_thresh = if was_dim {
                low_light_score_thresh.min(configured_score_thresh)
            } else {
                configured_score_thresh
            };
            let side = input_side as usize;
            let mut arr = Array4::<f32>::zeros((1, 3, side, side));
            for (x, y, px) in resized.enumerate_pixels() {
                let [r, g, b] = px.0;
                arr[[0, 0, y as usize, x as usize]] = (r as f32 - MEAN) * SCALE;
                arr[[0, 1, y as usize, x as usize]] = (g as f32 - MEAN) * SCALE;
                arr[[0, 2, y as usize, x as usize]] = (b as f32 - MEAN) * SCALE;
            }

            // ── Inference ─────────────────────────────────────────────────
            let mut sess = session.lock().map_err(|_| anyhow!("SCRFD session mutex poisoned"))?;
            let input = Tensor::from_array(arr)?;
            let outputs = sess.run(ort::inputs![input]).context("SCRFD inference failed")?;

            // Collect outputs keyed by last-dim channel count so we can match
            // scores / bboxes / keypoints per stride.  SCRFD's InsightFace
            // exports use names "score_8" / "bbox_8" / "kps_8" etc., but we
            // stay name-agnostic and classify by tensor shape.
            let mut scores: Vec<(usize, Vec<f32>)> = Vec::new();
            let mut bboxes: Vec<(usize, Vec<f32>)> = Vec::new();
            let mut kps:    Vec<(usize, Vec<f32>)> = Vec::new();
            for (_name, value) in outputs.iter() {
                let (shape, data) = value.try_extract_tensor::<f32>()
                    .context("SCRFD output tensor extract failed")?;
                let n_elements = data.len();
                // Each output has shape (1, N·C, 1) where C ∈ {1, 4, 10}.
                // Identify by total element count modulo the stride's
                // expected tile size.  We approximate by inspecting shape
                // dims directly when possible.
                let ushape: Vec<usize> = shape.iter().map(|&d| d as usize).collect();
                let per_loc = ushape.iter().rev().find(|&&d| d > 1).copied().unwrap_or(1);
                // Flatten → a single row vector we decode below.
                match per_loc % 10 {
                    _ if per_loc == 1 => scores.push((n_elements, data.to_vec())),
                    _ if per_loc == 4 => bboxes.push((n_elements, data.to_vec())),
                    _ if per_loc == 10 => kps.push((n_elements, data.to_vec())),
                    _ => {
                        // Fallback: infer by divisibility against anchor
                        // count across strides. Rarely taken.
                        if data.len() >= 10 && data.len() % 10 == 0 && per_loc == 10 {
                            kps.push((n_elements, data.to_vec()));
                        } else if data.len() % 4 == 0 && per_loc == 4 {
                            bboxes.push((n_elements, data.to_vec()));
                        } else {
                            scores.push((n_elements, data.to_vec()));
                        }
                    }
                }
            }
            if scores.len() != strides.len()
                || bboxes.len() != strides.len()
                || kps.len()    != strides.len()
            {
                return Err(anyhow!(
                    "SCRFD output shape mismatch: scores={}, bboxes={}, kps={}, strides={}",
                    scores.len(), bboxes.len(), kps.len(), strides.len(),
                ));
            }

            // Sort each tensor list by element count ascending — this
            // mirrors the stride order (stride 8 has more anchors than 32).
            scores.sort_by_key(|(n, _)| *n);
            bboxes.sort_by_key(|(n, _)| *n);
            kps   .sort_by_key(|(n, _)| *n);
            // strides are [8, 16, 32] by default; smaller stride ⇒ larger
            // feature map ⇒ more anchors ⇒ larger tensor.  So reverse
            // (descending element count = stride 8 first).
            scores.reverse();
            bboxes.reverse();
            kps   .reverse();

            let mut candidates: Vec<([f32; 4], [(f32, f32); 5], f32)> = Vec::new();

            for (i, &stride) in strides.iter().enumerate() {
                let grid = (input_side / stride) as usize;
                let sc = &scores[i].1;
                let bb = &bboxes[i].1;
                let kp = &kps[i].1;
                let expected_anchors = grid * grid * num_anchors;
                if sc.len() != expected_anchors
                    || bb.len() != expected_anchors * 4
                    || kp.len() != expected_anchors * 10
                {
                    return Err(anyhow!(
                        "SCRFD stride {stride}: expected {expected_anchors} anchors; \
                         got scores={}, bboxes={}, kps={}",
                        sc.len(), bb.len(), kp.len(),
                    ));
                }

                for row in 0..grid {
                    for col in 0..grid {
                        for a in 0..num_anchors {
                            let anchor_idx = (row * grid + col) * num_anchors + a;
                            let score = sc[anchor_idx];
                            if score < score_thresh { continue; }

                            let cx = (col as f32 + 0.5) * stride as f32;
                            let cy = (row as f32 + 0.5) * stride as f32;

                            // Bbox distances — already scaled by stride in
                            // SCRFD's regression head, so just multiply.
                            let o = anchor_idx * 4;
                            let l = bb[o]     * stride as f32;
                            let t = bb[o + 1] * stride as f32;
                            let r = bb[o + 2] * stride as f32;
                            let b = bb[o + 3] * stride as f32;
                            let x1 = cx - l;
                            let y1 = cy - t;
                            let x2 = cx + r;
                            let y2 = cy + b;

                            // Keypoint offsets.
                            let ko = anchor_idx * 10;
                            let mut pts = [(0.0_f32, 0.0_f32); 5];
                            for j in 0..5 {
                                let dx = kp[ko + j * 2]     * stride as f32;
                                let dy = kp[ko + j * 2 + 1] * stride as f32;
                                pts[j] = (cx + dx, cy + dy);
                            }

                            candidates.push(([x1, y1, x2, y2], pts, score));
                        }
                    }
                }
            }

            debug!(n = candidates.len(), "SCRFD pre-NMS candidates");
            if candidates.is_empty() { return Ok(None); }

            candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

            // Greedy NMS.
            let mut kept: Vec<([f32; 4], [(f32, f32); 5], f32)> = Vec::new();
            for cand in candidates {
                if kept.iter().any(|k| iou(&k.0, &cand.0) > iou_thresh) { continue; }
                kept.push(cand);
            }

            let (bx, kpts, score) = match kept.first() {
                Some(v) => *v,
                None => return Ok(None),
            };

            // Rescale from the (input_side × input_side) detection frame
            // back into the source image's pixel space.
            let sx = orig_w as f32 / input_side as f32;
            let sy = orig_h as f32 / input_side as f32;
            let to_px = |(x, y): (f32, f32)| (x * sx, y * sy);
            let lms = FaceLandmarks {
                left_eye:    to_px(kpts[0]),
                right_eye:   to_px(kpts[1]),
                nose:        to_px(kpts[2]),
                left_mouth:  to_px(kpts[3]),
                right_mouth: to_px(kpts[4]),
            };
            let px1 = (bx[0] * sx).max(0.0) as u32;
            let py1 = (bx[1] * sy).max(0.0) as u32;
            let px2 = (bx[2] * sx).min(orig_w as f32) as u32;
            let py2 = (bx[3] * sy).min(orig_h as f32) as u32;

            Ok(Some(DetectedFace {
                bbox: BoundingBox {
                    x:      px1,
                    y:      py1,
                    width:  px2.saturating_sub(px1),
                    height: py2.saturating_sub(py1),
                },
                landmarks: Some(lms),
                score,
            }))
        })
        .await
        .context("SCRFD spawn_blocking join failed")??;

        Ok(det)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iou_basics() {
        assert!((iou(&[0.0, 0.0, 1.0, 1.0], &[0.0, 0.0, 1.0, 1.0]) - 1.0).abs() < 1e-6);
        assert_eq!(iou(&[0.0, 0.0, 1.0, 1.0], &[2.0, 2.0, 3.0, 3.0]), 0.0);
    }

    #[tokio::test]
    async fn new_fails_on_missing_model() {
        let res = ScrfdDetector::new("/does/not/exist.onnx", 0.5, 0.4);
        assert!(res.is_err());
    }
}
