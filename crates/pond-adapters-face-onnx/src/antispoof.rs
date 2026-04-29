//! Passive presentation-attack detection (PAD).
//!
//! Addresses the "printed photo / phone screen in front of the camera"
//! failure mode the user reported.  A full PAD model (MiniFASNet ONNX) is
//! a future addition; in the meantime we use three lightweight passive
//! heuristics computed on the already-aligned 112×112 crop:
//!
//! 1. **Saturation variance** — natural skin under real lighting shows
//!    noticeable chroma variation across the face (warmer around cheeks,
//!    cooler around forehead / brow).  Flat prints and LCD captures tend
//!    to present a more uniform chroma because they're a two-step tone
//!    reproduction.  A collapsed S-channel is a strong cue.
//!
//! 2. **Highlight density** — real skin has microscale specular
//!    reflections (from subtle oils, curvature).  Those show up on a
//!    webcam as scattered bright-and-low-saturation pixels in the crop.
//!    A photo of a photo almost never shows them (the printing / LCD
//!    pipeline has already averaged them out).  We count how many pixels
//!    satisfy `V > 0.85` AND `S < 0.20` and require a minimum density.
//!
//! 3. **Edge-density skew** — screens and low-quality prints exhibit
//!    strong horizontal banding from scanline and halftone patterns.
//!    We measure the ratio of horizontal to vertical Sobel gradients; a
//!    value far from 1.0 suggests a non-real surface.
//!
//! Each heuristic contributes to a combined [0, 1] `spoof_score`.  The
//! caller can turn the gate off or adjust the threshold via
//! `POND_FACE_ANTISPOOF_THRESHOLD` (default 0.65 — permissive enough
//! that real users rarely trip but prints / screens do).

use image::RgbImage;

/// Result of the anti-spoof analysis.
#[derive(Debug, Clone, Copy)]
pub struct AntispoofReport {
    /// Scalar in \[0, 1\].  Higher = more likely to be a presentation attack.
    pub spoof_score:      f32,
    /// Variance of the HSV saturation channel (raw diagnostic).
    pub saturation_var:   f32,
    /// Fraction of pixels that look like skin specular highlights.
    pub highlight_density: f32,
    /// |h_gradient_mag - v_gradient_mag| / (h + v).  0 = isotropic; near 1 = strongly banded.
    pub gradient_skew:    f32,
}

/// Runs the passive anti-spoof heuristics on an aligned 112×112 RGB crop.
pub fn analyse(img: &RgbImage) -> AntispoofReport {
    let (w, h) = img.dimensions();
    let n = (w * h) as f64;

    // Accumulate saturation moments and highlight counts.
    let mut sat_sum = 0.0_f64;
    let mut sat_sq  = 0.0_f64;
    let mut hl_count: u32 = 0;
    for px in img.pixels() {
        let [r, g, b] = px.0;
        let (_, s, v) = rgb_to_hsv(r, g, b);
        sat_sum += s as f64;
        sat_sq  += (s * s) as f64;
        if v > 0.85 && s < 0.20 { hl_count += 1; }
    }
    let sat_mean = sat_sum / n;
    let sat_var  = ((sat_sq / n) - sat_mean * sat_mean).max(0.0) as f32;
    let hl_density = hl_count as f32 / n as f32;

    // Sobel-style horizontal vs vertical gradient magnitude (luma).
    let mut h_sum = 0.0_f32;
    let mut v_sum = 0.0_f32;
    let luma = |x: u32, y: u32| -> f32 {
        let [r, g, b] = img.get_pixel(x, y).0;
        0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32
    };
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let gx = (luma(x + 1, y) - luma(x - 1, y)).abs();
            let gy = (luma(x, y + 1) - luma(x, y - 1)).abs();
            h_sum += gx;
            v_sum += gy;
        }
    }
    let gradient_skew = if h_sum + v_sum > 0.0 {
        (h_sum - v_sum).abs() / (h_sum + v_sum)
    } else {
        0.0
    };

    // ── Scoring ─────────────────────────────────────────────────────────
    // Each component maps raw statistic → [0, 1] where 1 = strongly spoofy.
    // Thresholds picked empirically; loosened slightly to avoid hair-trigger
    // rejection of real users in poor lighting.
    let sat_component = (0.012 - sat_var).max(0.0) / 0.012;
    let hl_component  = (0.004 - hl_density).max(0.0) / 0.004;
    let skew_component = (gradient_skew - 0.12).max(0.0) / 0.30;
    let skew_component = skew_component.min(1.0);

    // Weighted combination.  Saturation variance is the strongest signal
    // across our failure cases, so it gets the largest weight.
    let spoof_score = (0.5 * sat_component
                     + 0.3 * hl_component
                     + 0.2 * skew_component).clamp(0.0, 1.0);

    AntispoofReport {
        spoof_score,
        saturation_var:    sat_var,
        highlight_density: hl_density,
        gradient_skew,
    }
}

/// Convert an 8-bit RGB pixel to HSV in \[0, 1\].
fn rgb_to_hsv(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32 / 255.0;
    let g = g as f32 / 255.0;
    let b = b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h } / 360.0;
    let s = if max == 0.0 { 0.0 } else { delta / max };
    (h, s, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn uniform_grey_scores_as_spoofy() {
        // A flat grey image has zero saturation variance and no highlights
        // → spoof_score should be high.
        let img = RgbImage::from_pixel(40, 40, Rgb([128, 128, 128]));
        let r = analyse(&img);
        assert!(r.spoof_score > 0.6, "spoof_score was {} for flat grey", r.spoof_score);
    }

    #[test]
    fn colourful_noisy_image_scores_as_live() {
        // Pseudo-random colour image has high saturation variance → low score.
        let mut img = RgbImage::new(40, 40);
        for y in 0..40 {
            for x in 0..40 {
                let r = ((x * 29 + y * 41) % 256) as u8;
                let g = ((x * 97 + y * 13) % 256) as u8;
                let b = ((x * 53 + y * 71) % 256) as u8;
                img.put_pixel(x, y, Rgb([r, g, b]));
            }
        }
        let r = analyse(&img);
        assert!(r.spoof_score < 0.5, "spoof_score was {} for noisy", r.spoof_score);
    }

    #[test]
    fn rgb_to_hsv_primary_colours() {
        // Pure red
        let (h, s, v) = rgb_to_hsv(255, 0, 0);
        assert!((h * 360.0).abs() < 1e-3);
        assert!((s - 1.0).abs() < 1e-6);
        assert!((v - 1.0).abs() < 1e-6);
        // Pure grey → zero saturation
        let (_, s, _) = rgb_to_hsv(128, 128, 128);
        assert_eq!(s, 0.0);
    }
}
