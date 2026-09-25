//! YOLOX output decoding and NMS. The exported model sigmoids in-graph but leaves grid decode:
//! rows are `[cx, cy, w, h, obj, cls...]`, `xy = (raw_xy + grid) * stride`,
//! `wh = exp(raw_wh) * stride`.

/// One decoded box in letterboxed-input coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDetection {
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
    /// `objectness * class_probability` — both already sigmoided in-graph.
    pub score: f32,
    pub class_idx: usize,
}

/// The strides YOLOX predicts at.
const STRIDES: [usize; 3] = [8, 16, 32];

/// Number of prediction rows a YOLOX model emits for a square input.
pub fn expected_rows(input_size: usize) -> usize {
    STRIDES
        .iter()
        .map(|s| (input_size / s) * (input_size / s))
        .sum()
}

/// Scored boxes clearing `score_thresh`, from a flat `[N, 5 + num_classes]` square-input tensor.
pub fn decode_yolox(
    preds: &[f32],
    num_classes: usize,
    input_size: usize,
    score_thresh: f32,
) -> Vec<RawDetection> {
    let row_len = 5 + num_classes;
    let rows = expected_rows(input_size);
    if preds.len() != rows * row_len {
        tracing::warn!(
            got = preds.len(),
            expected = rows * row_len,
            "unexpected YOLOX output shape; skipping frame"
        );
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut row = 0usize;
    for stride in STRIDES {
        let grid = input_size / stride;
        for gy in 0..grid {
            for gx in 0..grid {
                let p = &preds[row * row_len..(row + 1) * row_len];
                row += 1;

                let obj = p[4];
                if obj <= score_thresh {
                    continue; // score = obj * cls ≤ obj — cannot clear the bar
                }
                let (best_idx, best_cls) =
                    p[5..]
                        .iter()
                        .enumerate()
                        .fold(
                            (0usize, 0.0f32),
                            |acc, (i, &v)| {
                                if v > acc.1 {
                                    (i, v)
                                } else {
                                    acc
                                }
                            },
                        );
                let score = obj * best_cls;
                if score <= score_thresh {
                    continue;
                }
                out.push(RawDetection {
                    cx: (p[0] + gx as f32) * stride as f32,
                    cy: (p[1] + gy as f32) * stride as f32,
                    w: p[2].exp() * stride as f32,
                    h: p[3].exp() * stride as f32,
                    score,
                    class_idx: best_idx,
                });
            }
        }
    }
    out
}

/// Intersection-over-union of two center-format boxes.
fn iou(a: &RawDetection, b: &RawDetection) -> f32 {
    let (ax1, ay1, ax2, ay2) = (
        a.cx - a.w / 2.0,
        a.cy - a.h / 2.0,
        a.cx + a.w / 2.0,
        a.cy + a.h / 2.0,
    );
    let (bx1, by1, bx2, by2) = (
        b.cx - b.w / 2.0,
        b.cy - b.h / 2.0,
        b.cx + b.w / 2.0,
        b.cy + b.h / 2.0,
    );
    let iw = (ax2.min(bx2) - ax1.max(bx1)).max(0.0);
    let ih = (ay2.min(by2) - ay1.max(by1)).max(0.0);
    let inter = iw * ih;
    let union = a.w * a.h + b.w * b.h - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Class-aware NMS: per class, keep the best box of each cluster with IoU > `iou_thresh`.
pub fn nms(mut dets: Vec<RawDetection>, iou_thresh: f32) -> Vec<RawDetection> {
    dets.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<RawDetection> = Vec::new();
    for det in dets {
        let overlaps = kept
            .iter()
            .any(|k| k.class_idx == det.class_idx && iou(k, &det) > iou_thresh);
        if !overlaps {
            kept.push(det);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    const NUM_CLASSES: usize = 80;
    const INPUT: usize = 416;

    fn zero_preds() -> Vec<f32> {
        vec![0.0; expected_rows(INPUT) * (5 + NUM_CLASSES)]
    }

    /// Row index of grid cell (gx, gy) at the stride-32 level.
    fn stride32_row(gx: usize, gy: usize) -> usize {
        let s8 = (INPUT / 8) * (INPUT / 8);
        let s16 = (INPUT / 16) * (INPUT / 16);
        s8 + s16 + gy * (INPUT / 32) + gx
    }

    #[test]
    fn expected_rows_matches_yolox_416() {
        assert_eq!(expected_rows(416), 3549); // 52² + 26² + 13²
    }

    #[test]
    fn decodes_a_cat_at_the_right_grid_cell() {
        let mut preds = zero_preds();
        let row = stride32_row(3, 5);
        let base = row * (5 + NUM_CLASSES);
        preds[base] = 0.5; // cx offset within cell
        preds[base + 1] = 0.5;
        preds[base + 2] = 0.0; // exp(0) * 32 = 32px box
        preds[base + 3] = 0.0;
        preds[base + 4] = 1.0; // objectness
        preds[base + 5 + 15] = 0.9; // COCO 15 = cat

        let dets = decode_yolox(&preds, NUM_CLASSES, INPUT, 0.5);
        assert_eq!(dets.len(), 1);
        let d = &dets[0];
        assert_eq!(d.class_idx, 15);
        assert!((d.score - 0.9).abs() < 1e-6);
        assert!((d.cx - (3.5 * 32.0)).abs() < 1e-3);
        assert!((d.cy - (5.5 * 32.0)).abs() < 1e-3);
        assert!((d.w - 32.0).abs() < 1e-3);
    }

    #[test]
    fn low_scores_and_wrong_shapes_produce_nothing() {
        let mut preds = zero_preds();
        let base = stride32_row(0, 0) * (5 + NUM_CLASSES);
        preds[base + 4] = 0.4; // obj * cls can never clear 0.5
        preds[base + 5] = 0.9;
        assert!(decode_yolox(&preds, NUM_CLASSES, INPUT, 0.5).is_empty());
        // Truncated tensor → fail closed, no panic.
        assert!(decode_yolox(&preds[..100], NUM_CLASSES, INPUT, 0.5).is_empty());
    }

    #[test]
    fn nms_collapses_same_class_overlaps_but_keeps_distinct_classes() {
        let person_a = RawDetection {
            cx: 100.0,
            cy: 100.0,
            w: 50.0,
            h: 50.0,
            score: 0.9,
            class_idx: 0,
        };
        let person_b = RawDetection {
            cx: 105.0,
            cy: 102.0,
            w: 50.0,
            h: 50.0,
            score: 0.7,
            class_idx: 0,
        };
        let dog_same_spot = RawDetection {
            cx: 102.0,
            cy: 101.0,
            w: 50.0,
            h: 50.0,
            score: 0.8,
            class_idx: 16,
        };
        let kept = nms(
            vec![person_b.clone(), person_a.clone(), dog_same_spot.clone()],
            0.45,
        );
        assert_eq!(kept.len(), 2, "overlapping persons collapsed, dog kept");
        assert!(kept.contains(&person_a), "highest-scoring person wins");
        assert!(kept.contains(&dog_same_spot));
    }
}
