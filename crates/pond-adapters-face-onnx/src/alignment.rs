//! Face alignment: Umeyama-fit five landmarks to the ArcFace 112×112 template, then warp.
//! Without it, different people's embeddings collapse together (everyone scores 0.5-0.7).

use image::{DynamicImage, GenericImageView, Rgb, RgbImage};
use pond_core::user_data::domain::face_recognition::FaceLandmarks;

/// ArcFace 5-point template for 112×112 output; fixed by the embedding model's training.
pub const CANONICAL_112: [(f32, f32); 5] = [
    (38.2946, 51.6963), // left eye
    (73.5318, 51.5014), // right eye
    (56.0252, 71.7366), // nose
    (41.5493, 92.3655), // left mouth
    (70.7299, 92.2041), // right mouth
];

/// Row-major 2×3 matrix `[[a, b, tx], [c, d, ty]]`: rotation+scale block plus translation.
#[derive(Debug, Clone, Copy)]
pub struct Similarity2D {
    pub m: [[f32; 3]; 2],
}

impl Similarity2D {
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.m[0][0] * x + self.m[0][1] * y + self.m[0][2],
            self.m[1][0] * x + self.m[1][1] * y + self.m[1][2],
        )
    }

    /// Invert a non-degenerate similarity transform.
    pub fn inverse(&self) -> Option<Self> {
        let a = self.m[0][0];
        let b = self.m[0][1];
        let c = self.m[1][0];
        let d = self.m[1][1];
        let tx = self.m[0][2];
        let ty = self.m[1][2];
        let det = a * d - b * c;
        if det.abs() < 1e-8 {
            return None;
        }
        let inv_det = 1.0 / det;
        let ia = d * inv_det;
        let ib = -b * inv_det;
        let ic = -c * inv_det;
        let id = a * inv_det;
        Some(Similarity2D {
            m: [
                [ia, ib, -(ia * tx + ib * ty)],
                [ic, id, -(ic * tx + id * ty)],
            ],
        })
    }
}

/// Least-squares similarity fit of `src` onto `dst` (Umeyama 1991, as InsightFace uses).
pub fn umeyama_similarity(src: &[(f32, f32); 5], dst: &[(f32, f32); 5]) -> Similarity2D {
    let n = src.len() as f32;

    let (mut sx, mut sy, mut dx, mut dy) = (0.0_f32, 0.0, 0.0, 0.0);
    for i in 0..src.len() {
        sx += src[i].0;
        sy += src[i].1;
        dx += dst[i].0;
        dy += dst[i].1;
    }
    sx /= n;
    sy /= n;
    dx /= n;
    dy /= n;

    // Centred coordinates + cross-covariance + source variance.
    let mut sig_xx = 0.0_f32;
    let mut sig_xy = 0.0_f32;
    let mut sig_yx = 0.0_f32;
    let mut sig_yy = 0.0_f32;
    let mut var_s = 0.0_f32;
    for i in 0..src.len() {
        let cx = src[i].0 - sx;
        let cy = src[i].1 - sy;
        let ex = dst[i].0 - dx;
        let ey = dst[i].1 - dy;
        sig_xx += ex * cx;
        sig_xy += ex * cy;
        sig_yx += ey * cx;
        sig_yy += ey * cy;
        var_s += cx * cx + cy * cy;
    }
    sig_xx /= n;
    sig_xy /= n;
    sig_yx /= n;
    sig_yy /= n;
    var_s /= n;

    // A = [[a b] [c d]] is the cross-covariance; det(A)'s sign drives the reflection fix.
    let a = sig_xx;
    let b = sig_xy;
    let c = sig_yx;
    let d = sig_yy;
    let det_sigma = a * d - b * c;

    // R = U · diag(1, sign(det)) · Vᵀ; the flip only matters for mirrored landmarks.
    let s_diag_2 = if det_sigma < 0.0 { -1.0_f32 } else { 1.0 };

    // 2×2 SVD via eigendecomposition of AᵀA.
    let ata_00 = a * a + c * c;
    let ata_01 = a * b + c * d;
    let ata_11 = b * b + d * d;
    let trace = ata_00 + ata_11;
    let det_ata = ata_00 * ata_11 - ata_01 * ata_01;
    let disc = (trace * trace / 4.0 - det_ata).max(0.0).sqrt();
    let lam1 = trace / 2.0 + disc;
    let lam2 = (trace / 2.0 - disc).max(0.0);
    let sigma1 = lam1.max(0.0).sqrt();
    let sigma2 = lam2.max(0.0).sqrt();

    // Right singular vectors (columns of V) are eigenvectors of AᵀA.
    let (v1x, v1y) = if ata_01.abs() > 1e-12 {
        let vy = 1.0;
        let vx = (lam1 - ata_11) / ata_01;
        let nrm = (vx * vx + vy * vy).sqrt();
        (vx / nrm, vy / nrm)
    } else if ata_00 >= ata_11 {
        (1.0, 0.0)
    } else {
        (0.0, 1.0)
    };
    let (v2x, v2y) = (-v1y, v1x);

    // Left singular vectors: U·σ = A·V → Uₖ = (A·Vₖ) / σₖ  (if σₖ > 0)
    let (u1x, u1y) = if sigma1 > 1e-12 {
        ((a * v1x + b * v1y) / sigma1, (c * v1x + d * v1y) / sigma1)
    } else {
        (1.0, 0.0)
    };
    let (u2x, u2y) = if sigma2 > 1e-12 {
        ((a * v2x + b * v2y) / sigma2, (c * v2x + d * v2y) / sigma2)
    } else {
        (-u1y, u1x)
    };

    // R = [u1 u2·s] · [v1 v2]ᵀ
    let r00 = u1x * v1x + s_diag_2 * u2x * v2x;
    let r01 = u1x * v1y + s_diag_2 * u2x * v2y;
    let r10 = u1y * v1x + s_diag_2 * u2y * v2x;
    let r11 = u1y * v1y + s_diag_2 * u2y * v2y;

    // Scale c = (σ1 + s·σ2) / var_s; var_s ≈ 0 means collinear landmarks, so fall back to 1.
    let scale = if var_s > 1e-8 {
        (sigma1 + s_diag_2 * sigma2) / var_s
    } else {
        1.0
    };

    // Translation: t = dst_centroid − c · R · src_centroid.
    let tx = dx - scale * (r00 * sx + r01 * sy);
    let ty = dy - scale * (r10 * sx + r11 * sy);

    Similarity2D {
        m: [
            [scale * r00, scale * r01, tx],
            [scale * r10, scale * r11, ty],
        ],
    }
}

/// Bilinear sample at fractional pixel (x, y) with zero-pad borders.
fn sample_bilinear(img: &DynamicImage, x: f32, y: f32) -> Rgb<u8> {
    let (w, h) = img.dimensions();
    if x < 0.0 || y < 0.0 || x > (w - 1) as f32 || y > (h - 1) as f32 {
        return Rgb([0, 0, 0]);
    }
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;

    let p00 = img.get_pixel(x0, y0).0;
    let p10 = img.get_pixel(x1, y0).0;
    let p01 = img.get_pixel(x0, y1).0;
    let p11 = img.get_pixel(x1, y1).0;

    let mut out = [0u8; 3];
    for c in 0..3 {
        let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
        let bot = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
        let v = top * (1.0 - fy) + bot * fy;
        out[c] = v.round().clamp(0.0, 255.0) as u8;
    }
    Rgb(out)
}

/// Warp to a `side`×`side` canonical pose; the 112 template scales linearly with `side`.
pub fn align_to_canonical_112(
    img: &DynamicImage,
    landmarks: &FaceLandmarks,
    side: u32,
) -> DynamicImage {
    let scale = side as f32 / 112.0;
    let dst: [(f32, f32); 5] = [
        (CANONICAL_112[0].0 * scale, CANONICAL_112[0].1 * scale),
        (CANONICAL_112[1].0 * scale, CANONICAL_112[1].1 * scale),
        (CANONICAL_112[2].0 * scale, CANONICAL_112[2].1 * scale),
        (CANONICAL_112[3].0 * scale, CANONICAL_112[3].1 * scale),
        (CANONICAL_112[4].0 * scale, CANONICAL_112[4].1 * scale),
    ];
    let src = landmarks.as_array();

    // Backward warp needs the inverse of the src → dst fit.
    let forward = umeyama_similarity(&src, &dst);
    let inverse = forward.inverse().unwrap_or(Similarity2D {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    });

    let mut out = RgbImage::new(side, side);
    for yy in 0..side {
        for xx in 0..side {
            let (sx, sy) = inverse.apply(xx as f32, yy as f32);
            let px = sample_bilinear(img, sx, sy);
            out.put_pixel(xx, yy, px);
        }
    }
    DynamicImage::ImageRgb8(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similarity_identity_round_trip() {
        let pts = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)];
        let t = umeyama_similarity(&pts, &pts);
        // Any point should map to itself (up to FP).
        for &(x, y) in pts.iter() {
            let (u, v) = t.apply(x, y);
            assert!(
                (u - x).abs() < 1e-4 && (v - y).abs() < 1e-4,
                "identity broken at ({x},{y})"
            );
        }
    }

    #[test]
    fn similarity_translation_only() {
        let src = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0), (0.5, 0.5)];
        let dst = [
            (5.0, -3.0),
            (6.0, -3.0),
            (5.0, -2.0),
            (6.0, -2.0),
            (5.5, -2.5),
        ];
        let t = umeyama_similarity(&src, &dst);
        let (u, v) = t.apply(0.0, 0.0);
        assert!(
            (u - 5.0).abs() < 1e-3 && (v + 3.0).abs() < 1e-3,
            "got ({u},{v})"
        );
    }

    #[test]
    fn similarity_rotation_and_scale() {
        // Rotate 90° CCW and scale by 2: (x,y) → (-2y, 2x)
        let src = [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0), (1.0, 1.0)];
        let dst: [(f32, f32); 5] = [
            (-0.0, 2.0),
            (-2.0, 0.0),
            (0.0, -2.0),
            (2.0, 0.0),
            (-2.0, 2.0),
        ];
        let t = umeyama_similarity(&src, &dst);
        let (u, v) = t.apply(2.0, 0.0);
        assert!(
            (u - 0.0).abs() < 1e-3 && (v - 4.0).abs() < 1e-3,
            "got ({u},{v})"
        );
    }

    #[test]
    fn inverse_cancels_forward() {
        let src = [
            (10.0, 20.0),
            (30.0, 20.0),
            (20.0, 30.0),
            (15.0, 40.0),
            (25.0, 40.0),
        ];
        let dst: [(f32, f32); 5] = [
            (38.2946, 51.6963),
            (73.5318, 51.5014),
            (56.0252, 71.7366),
            (41.5493, 92.3655),
            (70.7299, 92.2041),
        ];
        let t = umeyama_similarity(&src, &dst);
        let inv = t.inverse().expect("inverse should exist");
        for &(x, y) in src.iter() {
            let (u, v) = t.apply(x, y);
            let (x2, y2) = inv.apply(u, v);
            assert!(
                (x2 - x).abs() < 1e-2 && (y2 - y).abs() < 1e-2,
                "round trip failed ({x},{y})→({x2},{y2})"
            );
        }
    }

    #[test]
    fn align_produces_112_square() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(200, 200, Rgb([128, 128, 128])));
        let lms = FaceLandmarks {
            left_eye: (70.0, 80.0),
            right_eye: (130.0, 80.0),
            nose: (100.0, 110.0),
            left_mouth: (80.0, 150.0),
            right_mouth: (120.0, 150.0),
        };
        let warped = align_to_canonical_112(&img, &lms, 112);
        assert_eq!(warped.dimensions(), (112, 112));
    }
}
