//! [`VisionClassifier`] over local YOLOX ONNX; on the Jetson's GPU once [`pond_onnx_accel`] has
//! its engine, on the CPU until then and everywhere else.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use ort::value::Tensor;
use pond_core::user_data::domain::vision::{DetectedObject, Detection, FractionBox, Frame};
use pond_core::user_data::ports::vision::VisionClassifier;
use pond_onnx_accel::{AcceleratedSession, ModelSpec};

use crate::decode::{decode_yolox, nms, RawDetection};
use crate::labels::coco_to_giap_label;
use crate::preprocess::letterbox_bgr_chw;

/// YOLOX-Nano's released input resolution.
const INPUT_SIZE: usize = 416;
const NUM_CLASSES: usize = 80;
/// Pre-NMS score floor; the pipeline's `MIN_CLASSIFIER_CONFIDENCE` gates on top.
const SCORE_THRESH: f32 = 0.25;
const IOU_THRESH: f32 = 0.45;

pub use pond_core::models::domain::acceleration::VISION_CLASSIFIER as MODEL_NAME;

/// What the GPU warm-up feeds YOLOX: one letterboxed frame.
pub fn model_spec() -> ModelSpec {
    ModelSpec {
        name: MODEL_NAME,
        input_shape: vec![1, 3, INPUT_SIZE as i64, INPUT_SIZE as i64],
        gpu: true,
    }
}

pub struct OnnxVisionClassifier {
    session: AcceleratedSession,
}

impl OnnxVisionClassifier {
    /// Load the model; on failure the caller degrades to unlabelled `"motion"` events.
    pub fn new(model_path: impl Into<PathBuf>) -> Result<Self> {
        let path = model_path.into();
        if !path.exists() {
            return Err(anyhow!(
                "vision classifier model not found at {}",
                path.display()
            ));
        }
        let session = AcceleratedSession::load(&path, model_spec())
            .with_context(|| format!("failed to load vision ONNX model at {}", path.display()))?;
        tracing::info!(path = %path.display(), "vision classifier loaded (YOLOX)");
        Ok(Self { session })
    }

    pub fn session(&self) -> &AcceleratedSession {
        &self.session
    }
}

impl OnnxVisionClassifier {
    /// The labelled boxes YOLOX keeps for `frame`, off the async runtime.
    async fn objects(&self, frame: &Frame) -> Result<Vec<DetectedObject>> {
        let frame = frame.clone();
        let session = self.session.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<DetectedObject>> {
            let input = letterbox_bgr_chw(&frame, INPUT_SIZE)?;
            let tensor = Tensor::from_array(input)?;
            let raw = session.run(|sess| {
                let outputs = sess
                    .run(ort::inputs![tensor])
                    .context("vision classifier inference failed")?;
                // Single output: [1, N, 85] raw predictions.
                let (_name, value) = outputs
                    .iter()
                    .next()
                    .ok_or_else(|| anyhow!("vision model produced no outputs"))?;
                let (_shape, data) = value
                    .try_extract_tensor::<f32>()
                    .context("failed to extract vision model output tensor")?;
                Ok(decode_yolox(data, NUM_CLASSES, INPUT_SIZE, SCORE_THRESH))
            })?;
            let scale = letterbox_scale(&frame, INPUT_SIZE);
            Ok(nms(raw, IOU_THRESH)
                .into_iter()
                .filter_map(|det| {
                    let label = coco_to_giap_label(det.class_idx)?;
                    Some(DetectedObject {
                        label: label.to_string(),
                        confidence: det.score as f64,
                        bbox: to_fractions(&det, scale, frame.width, frame.height),
                    })
                })
                .collect())
        })
        .await
        .context("vision classifier task panicked")?
    }
}

/// Input pixels per frame pixel: the letterbox scales by the tighter side and pads the rest.
fn letterbox_scale(frame: &Frame, size: usize) -> f32 {
    (size as f32 / frame.width as f32).min(size as f32 / frame.height as f32)
}

/// A box in letterboxed-input pixels, as fractions of the frame it came from, clipped to it.
fn to_fractions(det: &RawDetection, scale: f32, width: u32, height: u32) -> FractionBox {
    let (fw, fh) = (width as f32 * scale, height as f32 * scale);
    let x0 = ((det.cx - det.w / 2.0) / fw).clamp(0.0, 1.0);
    let y0 = ((det.cy - det.h / 2.0) / fh).clamp(0.0, 1.0);
    let x1 = ((det.cx + det.w / 2.0) / fw).clamp(0.0, 1.0);
    let y1 = ((det.cy + det.h / 2.0) / fh).clamp(0.0, 1.0);
    FractionBox {
        x: x0,
        y: y0,
        w: x1 - x0,
        h: y1 - y0,
    }
}

#[async_trait]
impl VisionClassifier for OnnxVisionClassifier {
    async fn classify(&self, frame: &Frame) -> Result<Vec<Detection>> {
        // Best confidence per label: events carry what was seen, not where.
        let mut best: Vec<Detection> = Vec::new();
        for object in self.objects(frame).await? {
            match best.iter_mut().find(|d| d.label == object.label) {
                Some(existing) => existing.confidence = existing.confidence.max(object.confidence),
                None => best.push(Detection {
                    label: object.label,
                    confidence: object.confidence,
                }),
            }
        }
        Ok(best)
    }

    fn locates(&self) -> bool {
        true
    }

    async fn detect(&self, frame: &Frame) -> Result<Vec<DetectedObject>> {
        self.objects(frame).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_on_the_letterbox_maps_back_to_the_frame() {
        // A 640x360 frame letterboxes into 416x234 with grey below; the box covers its left half.
        let frame = Frame {
            width: 640,
            height: 360,
            rgb: vec![0; 640 * 360 * 3],
            captured_at: chrono::Utc::now(),
        };
        let scale = letterbox_scale(&frame, INPUT_SIZE);
        assert!((scale - 0.65).abs() < 1e-6);
        let det = RawDetection {
            cx: 104.0,
            cy: 117.0,
            w: 208.0,
            h: 234.0,
            score: 0.9,
            class_idx: 0,
        };
        let b = to_fractions(&det, scale, 640, 360);
        assert!(b.x.abs() < 1e-6 && b.y.abs() < 1e-6, "{b:?}");
        assert!(
            (b.w - 0.5).abs() < 1e-6 && (b.h - 1.0).abs() < 1e-6,
            "{b:?}"
        );

        // A box reaching into the grey padding is clipped to the frame.
        let below = RawDetection { cy: 300.0, ..det };
        let clipped = to_fractions(&below, scale, 640, 360);
        assert!((clipped.y + clipped.h - 1.0).abs() < 1e-6, "{clipped:?}");
    }
}
