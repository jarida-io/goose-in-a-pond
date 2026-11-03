//! Two microphones into one, in the STFT domain (512-point frames, 256 hop, sqrt-Hann analysis and
//! synthesis): a GCC-PHAT direction estimate, delay-and-sum, and MVDR against a learned noise
//! covariance. Weights are per bin; the output is `conj(w0) L + conj(w1) R`.

use std::ops::{Add, Div, Mul, Neg, Sub};
use std::time::Instant;

use crate::engine::fft::RealFft;

pub const FFT: usize = 512;
pub const HOP: usize = FFT / 2;
pub const BINS: usize = FFT / 2 + 1;
/// A stream's output trails its input by this many samples.
pub const LATENCY: usize = FFT - HOP;
/// Largest lag searched, in samples at 16 kHz: 17 cm of path, past any webcam's spacing.
pub const MAX_LAG: f32 = 8.0;
const LAG_STEP: f32 = 0.125;
const RATE: f32 = 16_000.0;
/// GCC-PHAT reads this band: below it is room rumble, above it little speech.
const PHAT_BAND_HZ: (f32, f32) = (200.0, 6_000.0);
/// Noise frames the covariance averages over once it is full: about 3 s of non-speech.
pub const NOISE_MEMORY: u32 = 190;
/// Below this many frames a noise covariance is not trusted and the beam falls back to its steer.
pub const NOISE_MIN_FRAMES: u64 = 16;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C {
    pub re: f32,
    pub im: f32,
}

impl C {
    pub const ZERO: C = C { re: 0.0, im: 0.0 };
    pub const ONE: C = C { re: 1.0, im: 0.0 };

    #[inline]
    pub fn new(re: f32, im: f32) -> C {
        C { re, im }
    }

    #[inline]
    pub fn cis(a: f32) -> C {
        C::new(a.cos(), a.sin())
    }

    #[inline]
    pub fn conj(self) -> C {
        C::new(self.re, -self.im)
    }

    #[inline]
    pub fn norm2(self) -> f32 {
        self.re * self.re + self.im * self.im
    }

    #[inline]
    pub fn scale(self, s: f32) -> C {
        C::new(self.re * s, self.im * s)
    }

    pub fn sqrt(self) -> C {
        let r = self.norm2().sqrt();
        let re = ((r + self.re) * 0.5).max(0.0).sqrt();
        let im = ((r - self.re) * 0.5).max(0.0).sqrt();
        C::new(re, if self.im < 0.0 { -im } else { im })
    }
}

impl Add for C {
    type Output = C;
    #[inline]
    fn add(self, o: C) -> C {
        C::new(self.re + o.re, self.im + o.im)
    }
}

impl Sub for C {
    type Output = C;
    #[inline]
    fn sub(self, o: C) -> C {
        C::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for C {
    type Output = C;
    #[inline]
    fn mul(self, o: C) -> C {
        C::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}

impl Div for C {
    type Output = C;
    fn div(self, d: C) -> C {
        (self * d.conj()).scale(1.0 / d.norm2())
    }
}

impl Neg for C {
    type Output = C;
    #[inline]
    fn neg(self) -> C {
        C::new(-self.re, -self.im)
    }
}

/// One bin's 2x2 Hermitian covariance `[[ll, lr], [conj(lr), rr]]`, with `lr = E[L conj(R)]`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cov2 {
    pub ll: f32,
    pub rr: f32,
    pub lr: C,
}

impl Cov2 {
    #[inline]
    pub fn outer(l: C, r: C) -> Cov2 {
        Cov2 {
            ll: l.norm2(),
            rr: r.norm2(),
            lr: l * r.conj(),
        }
    }

    pub fn identity() -> Cov2 {
        Cov2 {
            ll: 1.0,
            rr: 1.0,
            lr: C::ZERO,
        }
    }

    #[inline]
    fn blend(&mut self, x: Cov2, a: f32) {
        self.ll += a * (x.ll - self.ll);
        self.rr += a * (x.rr - self.rr);
        self.lr = self.lr + (x.lr - self.lr).scale(a);
    }

    /// Diagonal loading by `eps` of the mean power: what keeps MVDR from buying directivity with
    /// sensor-noise gain where the two capsules are nearly coherent.
    pub fn loaded(self, eps: f32) -> Cov2 {
        let d = eps * 0.5 * (self.ll + self.rr) + f32::MIN_POSITIVE;
        Cov2 {
            ll: self.ll + d,
            rr: self.rr + d,
            lr: self.lr,
        }
    }

    pub fn det(self) -> f32 {
        self.ll * self.rr - self.lr.norm2()
    }

    pub fn mul_vec(self, v: [C; 2]) -> [C; 2] {
        [
            v[0].scale(self.ll) + self.lr * v[1],
            self.lr.conj() * v[0] + v[1].scale(self.rr),
        ]
    }

    /// `self^-1 v`, or `None` when the matrix is singular.
    pub fn solve(self, v: [C; 2]) -> Option<[C; 2]> {
        let det = self.det();
        if det.is_nan() || det <= 0.0 || det.is_infinite() {
            return None;
        }
        let inv = 1.0 / det;
        Some([
            (v[0].scale(self.rr) - self.lr * v[1]).scale(inv),
            (v[1].scale(self.ll) - self.lr.conj() * v[0]).scale(inv),
        ])
    }
}

/// Analysis and synthesis for one channel: sqrt of a periodic Hann, whose square sums to one at a
/// half-frame hop, so a beam that passes one microphone gives it back exactly.
pub struct Stft {
    fft: RealFft,
    win: Vec<f32>,
    frame: Vec<f32>,
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Default for Stft {
    fn default() -> Self {
        Stft::new()
    }
}

impl Stft {
    pub fn new() -> Stft {
        let win = (0..FFT)
            .map(|n| {
                (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / FFT as f64).cos()).sqrt()
                    as f32
            })
            .collect();
        Stft {
            fft: RealFft::new(FFT),
            win,
            frame: vec![0.0; FFT],
            re: vec![0.0; BINS],
            im: vec![0.0; BINS],
        }
    }

    pub fn bytes(&self) -> usize {
        self.fft.bytes() + (self.win.len() + self.frame.len() + self.re.len() + self.im.len()) * 4
    }

    /// The spectrum of `x[..FFT]` under the analysis window.
    pub fn analyse(&mut self, x: &[f32], out: &mut [C]) {
        for ((f, &v), &w) in self.frame.iter_mut().zip(x).zip(&self.win) {
            *f = v * w;
        }
        self.fft.forward(&self.frame, &mut self.re, &mut self.im);
        for (o, (&r, &i)) in out.iter_mut().zip(self.re.iter().zip(&self.im)) {
            *o = C::new(r, i);
        }
    }

    /// The windowed inverse of `spec`, added into `out[..FFT]`.
    pub fn synthesise_add(&mut self, spec: &[C], out: &mut [f32]) {
        for (k, s) in spec.iter().enumerate() {
            self.re[k] = s.re;
            self.im[k] = s.im;
        }
        self.fft.inverse(&self.re, &self.im, &mut self.frame);
        for ((o, &v), &w) in out.iter_mut().zip(&self.frame).zip(&self.win) {
            *o += v * w;
        }
    }
}

#[inline]
fn omega(k: usize) -> f32 {
    2.0 * std::f32::consts::PI * k as f32 / FFT as f32
}

/// Per-bin weights.
#[derive(Clone, Debug, PartialEq)]
pub struct Weights {
    pub w: Vec<[C; 2]>,
}

impl Weights {
    /// The two microphones averaged: what a mono capture of the pair hears.
    pub fn mean() -> Weights {
        Weights {
            w: vec![[C::new(0.5, 0.0); 2]; BINS],
        }
    }

    /// One microphone alone (0 left, 1 right).
    pub fn channel(i: usize) -> Weights {
        let pick = |c: usize| if c == i { C::ONE } else { C::ZERO };
        Weights {
            w: vec![[pick(0), pick(1)]; BINS],
        }
    }

    /// Right advanced by `tdoa` samples onto left, then averaged.
    pub fn delay_and_sum(tdoa: f32) -> Weights {
        Weights {
            w: free_field(tdoa)
                .into_iter()
                .map(|d| [d[0].scale(0.5), d[1].scale(0.5)])
                .collect(),
        }
    }

    /// Minimum variance, distortionless toward `steer` (relative to the left microphone), against
    /// `noise` loaded by `loading`. A bin whose noise matrix is singular falls back to the steer.
    pub fn mvdr(steer: &[[C; 2]], noise: &[Cov2], loading: f32) -> Weights {
        let w = steer
            .iter()
            .zip(noise)
            .map(|(&d, &n)| {
                let n = n.loaded(loading);
                let u = n.solve(d).unwrap_or(d);
                let g = d[0].conj() * u[0] + d[1].conj() * u[1];
                if g.norm2() > 0.0 && g.re.is_finite() {
                    [u[0] / g, u[1] / g]
                } else {
                    let e = d[0].norm2() + d[1].norm2();
                    [d[0].scale(1.0 / e), d[1].scale(1.0 / e)]
                }
            })
            .collect();
        Weights { w }
    }

    #[inline]
    pub fn apply(&self, l: &[C], r: &[C], out: &mut [C]) {
        for (k, o) in out.iter_mut().enumerate() {
            let [a, b] = self.w[k];
            *o = a.conj() * l[k] + b.conj() * r[k];
        }
    }

    /// Power gain of the beam on sensor noise (white, uncorrelated), per bin: `|w|^2`, 0.5 for the
    /// mean. Large values mean the beam amplifies the capsules' own hiss.
    pub fn white_noise_power(&self) -> Vec<f32> {
        self.w.iter().map(|w| w[0].norm2() + w[1].norm2()).collect()
    }
}

/// The steering vector of a plane wave reaching the right microphone `tdoa` samples after the left.
pub fn free_field(tdoa: f32) -> Vec<[C; 2]> {
    (0..BINS)
        .map(|k| [C::ONE, C::cis(-omega(k) * tdoa)])
        .collect()
}

/// The target's relative transfer function (left = 1) from the generalised eigenvector of the
/// span's covariance against the noise's: it carries the room's early reflections, which a free-field
/// steer misses. Bins where it cannot be found keep the free field of `fallback_tdoa`.
pub fn relative_transfer(
    span: &[Cov2],
    noise: &[Cov2],
    loading: f32,
    fallback_tdoa: f32,
) -> Vec<[C; 2]> {
    let ff = free_field(fallback_tdoa);
    span.iter()
        .zip(noise)
        .zip(ff)
        .map(|((&x, &n), ff)| principal_rtf(x, n.loaded(loading)).unwrap_or(ff))
        .collect()
}

fn principal_rtf(x: Cov2, n: Cov2) -> Option<[C; 2]> {
    let det = n.det();
    if det.is_nan() || det <= 0.0 {
        return None;
    }
    let inv = 1.0 / det;
    // n^-1 = inv [[rr, -lr], [-conj(lr), ll]]; m = n^-1 x.
    let (a, b, c, d) = (
        C::new(n.rr * inv, 0.0),
        (-n.lr).scale(inv),
        (-n.lr.conj()).scale(inv),
        C::new(n.ll * inv, 0.0),
    );
    let (p, q, r, s) = (C::new(x.ll, 0.0), x.lr, x.lr.conj(), C::new(x.rr, 0.0));
    let m00 = a * p + b * r;
    let m01 = a * q + b * s;
    let m10 = c * p + d * r;
    let m11 = c * q + d * s;
    let half_tr = (m00 + m11).scale(0.5);
    let disc = (half_tr * half_tr - (m00 * m11 - m01 * m10)).sqrt();
    let (l1, l2) = (half_tr + disc, half_tr - disc);
    let lambda = if l1.re >= l2.re { l1 } else { l2 };
    let v1 = [m01, lambda - m00];
    let v2 = [lambda - m11, m10];
    let v = if v1[0].norm2() + v1[1].norm2() >= v2[0].norm2() + v2[1].norm2() {
        v1
    } else {
        v2
    };
    let h = n.mul_vec(v);
    if h[0].norm2().is_nan() || h[0].norm2() <= 1e-30 {
        return None;
    }
    let rtf = [C::ONE, h[1] / h[0]];
    (rtf[1].re.is_finite() && rtf[1].im.is_finite()).then_some(rtf)
}

/// Each frame's left and right spectra over a stereo span: frame `i` reads samples
/// `[i * HOP, i * HOP + FFT)`; a span shorter than one frame has none.
pub fn for_each_frame(left: &[f32], right: &[f32], mut f: impl FnMut(usize, &[C], &[C])) {
    let n = left.len().min(right.len());
    if n < FFT {
        return;
    }
    let (mut sl, mut sr) = (Stft::new(), Stft::new());
    let (mut l, mut r) = (vec![C::ZERO; BINS], vec![C::ZERO; BINS]);
    for (i, s) in (0..=n - FFT).step_by(HOP).enumerate() {
        sl.analyse(&left[s..s + FFT], &mut l);
        sr.analyse(&right[s..s + FFT], &mut r);
        f(i, &l, &r);
    }
}

/// The average covariance of a stereo span's frames.
pub fn span_cov(left: &[f32], right: &[f32]) -> (Vec<Cov2>, usize) {
    let mut cov = vec![Cov2::default(); BINS];
    let mut frames = 0;
    for_each_frame(left, right, |_, l, r| {
        frames += 1;
        let a = 1.0 / frames as f32;
        for k in 0..BINS {
            cov[k].blend(Cov2::outer(l[k], r[k]), a);
        }
    });
    (cov, frames)
}

/// The span's covariance over only the time-frequency bins that stand `gate` (a power ratio) above
/// the noise: where a talker, not the room, dominates. Bins that never pass are left zero.
pub fn span_cov_masked(left: &[f32], right: &[f32], noise: &[Cov2], gate: f32) -> Vec<Cov2> {
    let mut cov = vec![Cov2::default(); BINS];
    let mut n = vec![0u32; BINS];
    for_each_frame(left, right, |_, l, r| {
        for k in 0..BINS {
            let floor = gate * (noise[k].ll + noise[k].rr);
            if l[k].norm2() + r[k].norm2() > floor {
                n[k] += 1;
                cov[k].blend(Cov2::outer(l[k], r[k]), 1.0 / n[k] as f32);
            }
        }
    });
    cov
}

/// Where a source sits, as the right microphone's delay behind the left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tdoa {
    /// Samples at 16 kHz; positive when the sound reaches the left microphone first.
    pub lag: f32,
    /// The phase-only correlation at the peak, 0..1: 1 is one clean direction.
    pub strength: f32,
}

/// GCC-PHAT on a span's averaged cross-spectrum (loud frames weigh more), searched over
/// `+-max_lag` samples. `None` for a span under one frame or with no energy.
pub fn gcc_phat(left: &[f32], right: &[f32], max_lag: f32) -> Option<Tdoa> {
    let mut g = vec![C::ZERO; BINS];
    let mut frames = 0;
    for_each_frame(left, right, |_, l, r| {
        frames += 1;
        for k in 0..BINS {
            g[k] = g[k] + l[k] * r[k].conj();
        }
    });
    if frames == 0 {
        return None;
    }
    phat_peak(&g, max_lag)
}

/// GCC-PHAT on what a span adds to the noise: its cross-spectrum less the noise's, so a television
/// louder than the talker does not take the steer. `None` where nothing is left.
pub fn tdoa_against(span: &[Cov2], noise: &[Cov2], max_lag: f32) -> Option<Tdoa> {
    let g: Vec<C> = span
        .iter()
        .zip(noise)
        .map(|(x, n)| if x.ll > 0.0 { x.lr - n.lr } else { C::ZERO })
        .collect();
    phat_peak(&g, max_lag)
}

/// The lag of the peak of a cross-spectrum's phase-only correlation.
pub fn phat_peak(g: &[C], max_lag: f32) -> Option<Tdoa> {
    let hz = RATE / FFT as f32;
    let (lo, hi) = (
        (PHAT_BAND_HZ.0 / hz).ceil() as usize,
        ((PHAT_BAND_HZ.1 / hz) as usize).min(BINS - 1),
    );
    let phat: Vec<(usize, C)> = (lo..=hi)
        .filter_map(|k| {
            let m = g[k].norm2().sqrt();
            (m > 0.0 && m.is_finite()).then(|| (k, g[k].scale(1.0 / m)))
        })
        .collect();
    if phat.is_empty() {
        return None;
    }
    let steps = (max_lag / LAG_STEP).round() as i32;
    let mut best = (f32::MIN, 0.0);
    for i in -steps..=steps {
        let t = i as f32 * LAG_STEP;
        let v: f32 = phat
            .iter()
            .map(|&(k, p)| (p * C::cis(-omega(k) * t)).re)
            .sum();
        if v > best.0 {
            best = (v, t);
        }
    }
    Some(Tdoa {
        lag: best.1,
        strength: best.0 / phat.len() as f32,
    })
}

/// A noise covariance learned on frames the caller says hold no target: an average that becomes
/// exponential with a memory of `memory` frames.
#[derive(Clone, Debug)]
pub struct NoiseCov {
    cov: Vec<Cov2>,
    frames: u64,
    memory: u32,
}

impl NoiseCov {
    pub fn new(memory: u32) -> NoiseCov {
        NoiseCov {
            cov: vec![Cov2::default(); BINS],
            frames: 0,
            memory: memory.max(1),
        }
    }

    pub fn reset(&mut self) {
        self.cov.fill(Cov2::default());
        self.frames = 0;
    }

    pub fn bytes(&self) -> usize {
        self.cov.len() * std::mem::size_of::<Cov2>()
    }

    pub fn learn(&mut self, l: &[C], r: &[C]) {
        self.frames += 1;
        let a = 1.0 / self.frames.min(self.memory as u64) as f32;
        for k in 0..BINS {
            self.cov[k].blend(Cov2::outer(l[k], r[k]), a);
        }
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn ready(&self) -> bool {
        self.frames >= NOISE_MIN_FRAMES
    }

    pub fn cov(&self) -> &[Cov2] {
        &self.cov
    }
}

/// A streaming beam: stereo in, mono out `LATENCY` samples late, with weights the host may change
/// between pushes. Nothing allocates after `new` except the caller's output.
pub struct Beam {
    weights: Weights,
    sl: Stft,
    sr: Stft,
    sy: Stft,
    inl: Vec<f32>,
    inr: Vec<f32>,
    fill: usize,
    ola: Vec<f32>,
    l: Vec<C>,
    r: Vec<C>,
    y: Vec<C>,
    pub ns: u64,
    pub frames: u64,
}

impl Beam {
    pub fn new(weights: Weights) -> Beam {
        Beam {
            weights,
            sl: Stft::new(),
            sr: Stft::new(),
            sy: Stft::new(),
            inl: vec![0.0; FFT],
            inr: vec![0.0; FFT],
            fill: 0,
            ola: vec![0.0; FFT],
            l: vec![C::ZERO; BINS],
            r: vec![C::ZERO; BINS],
            y: vec![C::ZERO; BINS],
            ns: 0,
            frames: 0,
        }
    }

    pub fn reset(&mut self) {
        self.inl.fill(0.0);
        self.inr.fill(0.0);
        self.ola.fill(0.0);
        self.fill = 0;
        self.ns = 0;
        self.frames = 0;
    }

    pub fn weights(&self) -> &Weights {
        &self.weights
    }

    pub fn set_weights(&mut self, w: Weights) {
        self.weights = w;
    }

    pub fn bytes(&self) -> usize {
        self.sl.bytes() * 3
            + (self.inl.len() + self.inr.len() + self.ola.len()) * 4
            + (self.l.len() + self.r.len() + self.y.len()) * 8
            + self.weights.w.len() * 16
    }

    /// The spectra of the frame just processed, for a noise estimate the host gates later.
    pub fn last_frame(&self) -> (&[C], &[C]) {
        (&self.l, &self.r)
    }

    /// Stereo in equal lengths; mono appended to `out` a hop at a time, `LATENCY` samples late.
    pub fn push(&mut self, left: &[f32], right: &[f32], out: &mut Vec<f32>) {
        let t = Instant::now();
        let n = left.len().min(right.len());
        let mut i = 0;
        while i < n {
            let take = (HOP - self.fill).min(n - i);
            let at = LATENCY + self.fill;
            self.inl[at..at + take].copy_from_slice(&left[i..i + take]);
            self.inr[at..at + take].copy_from_slice(&right[i..i + take]);
            self.fill += take;
            i += take;
            if self.fill == HOP {
                self.frame(out);
            }
        }
        self.ns += t.elapsed().as_nanos() as u64;
    }

    fn frame(&mut self, out: &mut Vec<f32>) {
        self.sl.analyse(&self.inl, &mut self.l);
        self.sr.analyse(&self.inr, &mut self.r);
        self.weights.apply(&self.l, &self.r, &mut self.y);
        self.sy.synthesise_add(&self.y, &mut self.ola);
        out.extend_from_slice(&self.ola[..HOP]);
        self.ola.copy_within(HOP.., 0);
        self.ola[FFT - HOP..].fill(0.0);
        self.inl.copy_within(HOP.., 0);
        self.inr.copy_within(HOP.., 0);
        self.fill = 0;
        self.frames += 1;
    }
}

/// A stereo span through fixed weights: mono of the span's own length and alignment.
pub fn beamform(left: &[f32], right: &[f32], weights: &Weights) -> Vec<f32> {
    let n = left.len().min(right.len());
    let mut beam = Beam::new(weights.clone());
    let mut out = Vec::with_capacity(n + FFT);
    beam.push(&left[..n], &right[..n], &mut out);
    let pad = vec![0f32; FFT];
    beam.push(&pad, &pad, &mut out);
    out.drain(..LATENCY);
    out.truncate(n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
                ((s >> 16) as f32 / 32768.0) - 1.0
            })
            .collect()
    }

    /// `x` (length a power of two) delayed by a fractional `d` samples, circularly, band-limited
    /// to 7 kHz so the shift is exact.
    fn delayed(x: &[f32], d: f32) -> Vec<f32> {
        delayed_band(x, d, 0.0)
    }

    /// `delayed`, with nothing below `lo` Hz either: two capsules a few cm apart cannot tell
    /// directions apart there, so no beam nulls it.
    fn delayed_band(x: &[f32], d: f32, lo: f32) -> Vec<f32> {
        let n = x.len();
        let mut f = RealFft::new(n);
        let (mut re, mut im) = (vec![0f32; f.bins()], vec![0f32; f.bins()]);
        f.forward(x, &mut re, &mut im);
        for k in 0..f.bins() {
            let hz = k as f32 * RATE / n as f32;
            let c = if hz > 7_000.0 || hz < lo || k == 0 {
                C::ZERO
            } else {
                C::new(re[k], im[k]) * C::cis(-2.0 * std::f32::consts::PI * k as f32 * d / n as f32)
            };
            re[k] = c.re;
            im[k] = c.im;
        }
        let mut y = vec![0f32; n];
        f.inverse(&re, &im, &mut y);
        y
    }

    fn power(x: &[f32]) -> f32 {
        x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32
    }

    fn db(x: f32) -> f32 {
        10.0 * x.log10()
    }

    #[test]
    fn one_microphone_through_the_beam_comes_back_exactly() {
        let x = noise(10_000, 7);
        let y = noise(10_000, 8);
        let out = beamform(&x, &y, &Weights::channel(0));
        let err = x
            .iter()
            .zip(&out)
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(err < 1e-4, "round trip {err}");
        let mut streamed = Vec::new();
        let mut b = Beam::new(Weights::channel(1));
        for (cl, cr) in x.chunks(37).zip(y.chunks(37)) {
            b.push(cl, cr, &mut streamed);
        }
        assert_eq!(streamed.len(), y.len() / HOP * HOP);
        let err = y
            .iter()
            .zip(&streamed[LATENCY..])
            .map(|(a, b)| (a - b).abs())
            .fold(0f32, f32::max);
        assert!(err < 1e-4, "stream {err}");
    }

    #[test]
    fn gcc_phat_finds_a_fractional_delay() {
        let s = delayed(&noise(16_384, 3), 0.0);
        for d in [-3.6f32, 0.0, 1.3, 2.4] {
            let r = delayed(&s, d);
            let t = gcc_phat(&s, &r, MAX_LAG).unwrap();
            assert!((t.lag - d).abs() <= 0.13, "{d}: {t:?}");
            assert!(t.strength > 0.9, "{d}: {t:?}");
        }
    }

    #[test]
    fn delay_and_sum_keeps_the_target_and_mvdr_nulls_a_point_interferer() {
        let n = 32_768;
        // Two samples apart: a webcam's spacing, which aliases only at Nyquist.
        let (dt, di) = (0.6f32, -1.4f32);
        let target = delayed(&noise(n, 11), 0.0);
        let jam = delayed_band(&noise(n, 12), 0.0, 300.0);
        let (tl, tr) = (target.clone(), delayed(&target, dt));
        let (jl, jr) = (jam.clone(), delayed(&jam, di));
        let mut noise_cov = NoiseCov::new(NOISE_MEMORY);
        for_each_frame(&jl, &jr, |_, l, r| noise_cov.learn(l, r));
        assert!(noise_cov.ready());
        let das = Weights::delay_and_sum(dt);
        let mvdr = Weights::mvdr(&free_field(dt), noise_cov.cov(), 1e-3);
        let inner = 2_000..n - 2_000;
        for (name, w) in [("das", &das), ("mvdr", &mvdr)] {
            let t_out = beamform(&tl, &tr, w);
            let j_out = beamform(&jl, &jr, w);
            let t_err: Vec<f32> = t_out[inner.clone()]
                .iter()
                .zip(&tl[inner.clone()])
                .map(|(a, b)| a - b)
                .collect();
            let distortion = db(power(&t_err) / power(&tl[inner.clone()]));
            let jam_gain = db(power(&j_out[inner.clone()]) / power(&jl[inner.clone()]));
            match name {
                "das" => {
                    assert!(distortion < -40.0, "das distorts the target: {distortion}");
                    assert!(jam_gain > -10.0, "das cannot null: {jam_gain}");
                }
                _ => {
                    assert!(distortion < -20.0, "mvdr distorts the target: {distortion}");
                    assert!(
                        jam_gain < -25.0,
                        "mvdr left the interferer at {jam_gain} dB"
                    );
                }
            }
        }
    }

    #[test]
    fn a_talker_quieter_than_the_noise_still_takes_the_steer() {
        let n = 32_768;
        let (dt, di) = (-1.4f32, 2.3f32);
        let talk: Vec<f32> = delayed(&noise(n, 31), 0.0)
            .iter()
            .map(|v| v * 0.6)
            .collect();
        let jam = delayed(&noise(n, 32), 0.0);
        let jam_late = delayed(&noise(n, 33), 0.0);
        let (jl, jr) = (jam.clone(), delayed(&jam, di));
        let l: Vec<f32> = talk.iter().zip(&jam_late).map(|(a, b)| a + b).collect();
        let r: Vec<f32> = delayed(&talk, dt)
            .iter()
            .zip(delayed(&jam_late, di))
            .map(|(a, b)| a + b)
            .collect();
        let plain = gcc_phat(&l, &r, MAX_LAG).unwrap();
        assert!(
            (plain.lag - di).abs() < 0.3,
            "the louder source wins plain GCC: {plain:?}"
        );
        let (x, _) = span_cov(&l, &r);
        let (nc, _) = span_cov(&jl, &jr);
        let t = tdoa_against(&x, &nc, MAX_LAG).unwrap();
        assert!((t.lag - dt).abs() < 0.3, "{t:?}");
    }

    #[test]
    fn the_masked_steer_finds_a_talker_who_speaks_in_bursts() {
        let n = 32_768;
        let (dt, di) = (1.1f32, -2.0f32);
        // On for a quarter of the time at twice the noise's level: speech over a steady television.
        let raw = noise(n, 41);
        let bursts: Vec<f32> = raw
            .iter()
            .enumerate()
            .map(|(i, v)| if (i / 2_048) % 4 == 0 { v * 2.0 } else { 0.0 })
            .collect();
        let talk = delayed(&bursts, 0.0);
        let jam = delayed(&noise(n, 42), 0.0);
        let jam_late = delayed(&noise(n, 43), 0.0);
        let l: Vec<f32> = talk.iter().zip(&jam_late).map(|(a, b)| a + b).collect();
        let r: Vec<f32> = delayed(&talk, dt)
            .iter()
            .zip(delayed(&jam_late, di))
            .map(|(a, b)| a + b)
            .collect();
        let (nc, _) = span_cov(&jam, &delayed(&jam, di));
        let masked = span_cov_masked(&l, &r, &nc, 2.0);
        let t = tdoa_against(&masked, &nc, MAX_LAG).unwrap();
        assert!((t.lag - dt).abs() < 0.3, "{t:?}");
    }

    #[test]
    fn the_relative_transfer_of_a_plane_wave_is_its_free_field() {
        let n = 32_768;
        let dt = 1.7f32;
        let target = delayed(&noise(n, 21), 0.0);
        let hiss = |seed| {
            noise(n, seed)
                .iter()
                .map(|v| v * 0.05)
                .collect::<Vec<f32>>()
        };
        let (nl, nr) = (hiss(22), hiss(23));
        let l: Vec<f32> = target.iter().zip(&nl).map(|(a, b)| a + b).collect();
        let r: Vec<f32> = delayed(&target, dt)
            .iter()
            .zip(&nr)
            .map(|(a, b)| a + b)
            .collect();
        let (x, _) = span_cov(&l, &r);
        let (nc, _) = span_cov(&nl, &nr);
        let rtf = relative_transfer(&x, &nc, 1e-3, 0.0);
        let ff = free_field(dt);
        let hz = RATE / FFT as f32;
        let worst = (0..BINS)
            .filter(|&k| (k as f32 * hz) > 200.0 && (k as f32 * hz) < 6_500.0)
            .map(|k| (rtf[k][1] - ff[k][1]).norm2().sqrt())
            .fold(0f32, f32::max);
        assert!(worst < 0.1, "rtf off the free field by {worst}");
    }

    #[test]
    fn mvdr_without_noise_learned_is_delay_and_sum() {
        let d = free_field(1.5);
        let ident = vec![Cov2::identity(); BINS];
        let a = Weights::mvdr(&d, &ident, 0.0);
        let b = Weights::delay_and_sum(1.5);
        let worst =
            a.w.iter()
                .zip(&b.w)
                .map(|(x, y)| (x[0] - y[0]).norm2() + (x[1] - y[1]).norm2())
                .fold(0f32, f32::max);
        assert!(worst < 1e-10, "{worst}");
    }
}
