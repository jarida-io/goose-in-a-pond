//! ln / sqrt / exp without libm: range reduction plus a short polynomial, a couple of ulp,
//! so results do not depend on the platform's libm.

/// Natural log for positive, normal `x`; the front end clamps to `f32::EPSILON` first.
#[inline(always)]
pub fn ln(x: f32) -> f32 {
    // m in [sqrt(1/2), sqrt(2)) keeps |s| <= 0.1716, where the odd series to s^9 is exact to ~4e-10.
    let bits = x.to_bits();
    let mut e = ((bits >> 23) & 0xff) as i32 - 127;
    let mut m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    if m > core::f32::consts::SQRT_2 {
        m *= 0.5;
        e += 1;
    }
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let p = s2 * (1.0 / 3.0 + s2 * (1.0 / 5.0 + s2 * (1.0 / 7.0 + s2 * (1.0 / 9.0))));
    // e*ln2 split into hi + lo so the large term carries no rounding error.
    const LN2_HI: f32 = 0.693_145_75;
    const LN2_LO: f32 = 1.428_606_8e-6;
    let ef = e as f32;
    ef * LN2_HI + (2.0 * s + (2.0 * s * p + ef * LN2_LO))
}

/// Square root: the single `fsqrt` instruction on aarch64.
#[inline(always)]
pub fn sqrt(x: f32) -> f32 {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: NEON is baseline on aarch64.
    unsafe {
        use core::arch::aarch64::*;
        vget_lane_f32::<0>(vsqrt_f32(vdup_n_f32(x)))
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        if x <= 0.0 {
            return 0.0;
        }
        let mut y = f32::from_bits((x.to_bits() >> 1) + 0x1fbd_1df5);
        for _ in 0..4 {
            y = 0.5 * (y + x / y);
        }
        y
    }
}

/// e^x, saturating instead of overflowing.
#[inline(always)]
pub fn exp(x: f32) -> f32 {
    let x = x.clamp(-87.0, 88.0);
    // x = k ln2 + r with |r| <= ln2/2, then e^r by a degree-6 polynomial.
    let k = (x * core::f32::consts::LOG2_E + if x >= 0.0 { 0.5 } else { -0.5 }) as i32;
    let kf = k as f32;
    let r = x - kf * 0.693_145_75 - kf * 1.428_606_8e-6;
    let p = 1.0
        + r * (1.0
            + r * (0.5
                + r * (1.0 / 6.0 + r * (1.0 / 24.0 + r * (1.0 / 120.0 + r * (1.0 / 720.0))))));
    p * f32::from_bits(((k + 127) as u32) << 23)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ulps(a: f32, b: f32) -> u32 {
        (a.to_bits() as i64 - b.to_bits() as i64).unsigned_abs() as u32
    }

    #[test]
    fn ln_matches_std_within_two_ulp() {
        let mut worst = (0u32, 0f32);
        let mut x = f32::EPSILON;
        while x < 1e12 {
            let u = ulps(ln(x), x.ln());
            // Near x = 1 the result is near zero, where an ulp is tiny: judge it by absolute error.
            if (x - 1.0).abs() > 1e-2 && u > worst.0 {
                worst = (u, x);
            } else if (x - 1.0).abs() <= 1e-2 {
                assert!((ln(x) - x.ln()).abs() < 1e-7, "ln({x}) abs");
            }
            x *= 1.000_37;
        }
        assert!(worst.0 <= 2, "ln worst {} ulp at {}", worst.0, worst.1);
    }

    #[test]
    fn sqrt_and_exp_track_std() {
        for i in 0..100_000 {
            let x = i as f32 * 0.0137;
            assert!((sqrt(x) - x.sqrt()).abs() <= 1e-6 * x.sqrt().max(1.0));
            let y = (i as f32 - 50_000.0) * 0.0017;
            assert!(((exp(y) - y.exp()) / y.exp()).abs() < 4e-7, "exp({y})");
        }
    }
}
