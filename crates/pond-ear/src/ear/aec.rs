//! Echo cancellation fed the exact audio the pond plays, on the microphone's clock: PBFDAF behind a
//! GCC-PHAT bulk delay, then residual suppression. Design and sources in docs/developer/pond-ear.md.

use crate::engine::fft::RealFft;

/// Samples per block: 8 ms, the latency unit of everything here.
pub const BLOCK: usize = 128;
const FFT: usize = 2 * BLOCK;
const BINS: usize = BLOCK + 1;
/// Output trails input by one block of overlap-save and one of overlap-add.
pub const LATENCY: usize = 2 * BLOCK;
const RATE: f32 = 16_000.0;
/// Internal int16 units, where the regularisers below are the ones the literature tunes.
const SCALE: f32 = 32_768.0;
/// Far-end stamps this close to where the last push ended are taken as continuous playback.
const JITTER: usize = 160;
const FAR_RING: usize = 1 << 16;
/// The filter starts this far before the estimated direct path.
const PRE_DELAY: usize = 2 * BLOCK;
const MIN_LEAK: f32 = 0.005;
/// Two-path thresholds: update the foreground when the background is significantly better,
/// reset the background when it is significantly worse.
const VAR1_UPDATE: f64 = 0.5;
const VAR2_UPDATE: f64 = 0.25;
const VAR_BACKTRACK: f64 = 4.0;
/// The normalised step before the leak estimate can be trusted.
const INITIAL_RATE: f32 = 0.5;
/// About 100 ms of a foreground worse than no filter at all.
const RELEARN_BLOCKS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AecConfig {
    /// Room tail the adaptive filter covers after the bulk delay.
    pub tail_ms: u32,
    /// Longest speaker-to-microphone delay the estimator searches.
    pub max_delay_ms: u32,
    /// Bulk delay assumed until the estimator has locked.
    pub initial_delay_ms: u32,
    /// Residual echo suppression after the linear filter.
    pub suppress: bool,
    /// The most the suppressor cuts, in dB (negative).
    pub floor_db: f32,
}

impl Default for AecConfig {
    fn default() -> Self {
        AecConfig {
            tail_ms: 160,
            max_delay_ms: 500,
            initial_delay_ms: 0,
            suppress: true,
            floor_db: -30.0,
        }
    }
}

/// Complex spectra for `n` partitions of `BINS` bins, split re/im.
#[derive(Clone)]
struct Spectra {
    re: Vec<f32>,
    im: Vec<f32>,
}

impl Spectra {
    fn new(n: usize) -> Spectra {
        Spectra {
            re: vec![0.0; n * BINS],
            im: vec![0.0; n * BINS],
        }
    }

    fn clear(&mut self) {
        self.re.fill(0.0);
        self.im.fill(0.0);
    }

    fn bytes(&self) -> usize {
        (self.re.len() + self.im.len()) * 4
    }
}

/// Bulk delay by GCC-PHAT on 4 kHz copies of both streams.
struct DelayEstimator {
    taps: Vec<f32>,
    far: Vec<f32>,
    mic: Vec<f32>,
    mask: usize,
    /// Decimated samples written so far (both streams).
    n: usize,
    window: usize,
    max_lag: usize,
    fft: RealFft,
    buf: Vec<f32>,
    mr: Vec<f32>,
    mi: Vec<f32>,
    xr: Vec<f32>,
    xi: Vec<f32>,
    sr: Vec<f32>,
    si: Vec<f32>,
    corr: Vec<f32>,
    last_peak: Option<usize>,
    streak: usize,
    estimate: Option<usize>,
}

const DECIMATE: usize = 4;
const DECIMATOR_TAPS: usize = 32;
/// Mic window the correlation sees: 256 ms at 4 kHz.
const DELAY_WINDOW: usize = 1_024;
/// Re-estimate every 16 blocks (128 ms) while there is far-end audio.
const DELAY_HOP_BLOCKS: usize = 16;
const DELAY_SMOOTHING: f32 = 0.6;
/// A peak must stand this far above the mean correlation, twice in a row, to move the filter.
const DELAY_CONFIDENCE: f32 = 6.0;

impl DelayEstimator {
    fn new(max_delay: usize) -> DelayEstimator {
        let max_lag = max_delay.div_ceil(DECIMATE);
        let fft_n = (DELAY_WINDOW + max_lag).next_power_of_two();
        let cap = (DELAY_WINDOW + max_lag + 64).next_power_of_two();
        // A 1.6 kHz Hann-windowed sinc: below the 2 kHz Nyquist of the decimated streams.
        let fc = 1_600.0 / 8_000.0;
        let mid = (DECIMATOR_TAPS - 1) as f64 / 2.0;
        let mut taps: Vec<f32> = (0..DECIMATOR_TAPS)
            .map(|t| {
                let d = t as f64 - mid;
                let x = std::f64::consts::PI * fc * d;
                let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
                let hann = 0.5
                    - 0.5
                        * (2.0 * std::f64::consts::PI * (t as f64 + 0.5) / DECIMATOR_TAPS as f64)
                            .cos();
                (fc * sinc * hann) as f32
            })
            .collect();
        let sum: f32 = taps.iter().sum();
        taps.iter_mut().for_each(|t| *t /= sum);
        let fft = RealFft::new(fft_n);
        let bins = fft.bins();
        DelayEstimator {
            taps,
            far: vec![0.0; cap],
            mic: vec![0.0; cap],
            mask: cap - 1,
            n: 0,
            window: DELAY_WINDOW,
            max_lag,
            buf: vec![0.0; fft_n],
            mr: vec![0.0; bins],
            mi: vec![0.0; bins],
            xr: vec![0.0; bins],
            xi: vec![0.0; bins],
            sr: vec![0.0; bins],
            si: vec![0.0; bins],
            corr: vec![0.0; fft_n],
            fft,
            last_peak: None,
            streak: 0,
            estimate: None,
        }
    }

    fn reset(&mut self) {
        self.far.fill(0.0);
        self.mic.fill(0.0);
        self.n = 0;
        self.buf.fill(0.0);
        for v in [
            &mut self.mr,
            &mut self.mi,
            &mut self.xr,
            &mut self.xi,
            &mut self.sr,
            &mut self.si,
        ] {
            v.fill(0.0);
        }
        self.corr.fill(0.0);
        self.last_peak = None;
        self.streak = 0;
        self.estimate = None;
    }

    fn bytes(&self) -> usize {
        (self.taps.len() + self.far.len() + self.mic.len() + self.buf.len() + self.corr.len()) * 4
            + (self.mr.len() * 6) * 4
            + self.fft.bytes()
    }

    /// Decimate one block of each stream; `far(i)` and `mic(i)` read raw samples by absolute index.
    fn push_block(&mut self, start: usize, far: impl Fn(usize) -> f32, mic: impl Fn(usize) -> f32) {
        let mut i = start.div_ceil(DECIMATE) * DECIMATE;
        while i < start + BLOCK {
            let (mut f, mut m) = (0f32, 0f32);
            for (t, &h) in self.taps.iter().enumerate() {
                if let Some(k) = i.checked_sub(t) {
                    f += h * far(k);
                    m += h * mic(k);
                }
            }
            let j = i / DECIMATE;
            self.far[j & self.mask] = f;
            self.mic[j & self.mask] = m;
            self.n = j + 1;
            i += DECIMATE;
        }
    }

    /// One correlation update; returns the confirmed delay in 16 kHz samples when it is new.
    fn update(&mut self) -> Option<usize> {
        let (w, d) = (self.window, self.max_lag);
        if self.n < w + d {
            return None;
        }
        let first = self.n - w - d;
        let energy = |ring: &[f32], from: usize, len: usize, mask: usize| {
            (from..from + len)
                .map(|j| ring[j & mask] * ring[j & mask])
                .sum::<f32>()
                / len as f32
        };
        // About -70 dBFS: quieter than this there is nothing to correlate.
        let floor = 1e-7;
        if energy(&self.far, first, w + d, self.mask) < floor
            || energy(&self.mic, self.n - w, w, self.mask) < floor
        {
            return None;
        }
        self.buf.fill(0.0);
        for k in 0..w {
            self.buf[k] = self.mic[(self.n - w + k) & self.mask];
        }
        self.fft.forward(&self.buf, &mut self.mr, &mut self.mi);
        self.buf.fill(0.0);
        for k in 0..w + d {
            self.buf[k] = self.far[(first + k) & self.mask];
        }
        self.fft.forward(&self.buf, &mut self.xr, &mut self.xi);
        for k in 0..self.sr.len() {
            // conj(M) X, phase only.
            let re = self.mr[k] * self.xr[k] + self.mi[k] * self.xi[k];
            let im = self.mr[k] * self.xi[k] - self.mi[k] * self.xr[k];
            let mag = (re * re + im * im).sqrt() + 1e-20;
            self.sr[k] = DELAY_SMOOTHING * self.sr[k] + (1.0 - DELAY_SMOOTHING) * re / mag;
            self.si[k] = DELAY_SMOOTHING * self.si[k] + (1.0 - DELAY_SMOOTHING) * im / mag;
        }
        self.fft.inverse(&self.sr, &self.si, &mut self.corr);
        let (mut peak, mut best, mut sum) = (0usize, 0f32, 0f32);
        for k in 0..=d {
            let v = self.corr[k].abs();
            sum += v;
            if v > best {
                best = v;
                peak = k;
            }
        }
        let mean = sum / (d + 1) as f32;
        if best < DELAY_CONFIDENCE * mean {
            self.streak = 0;
            return None;
        }
        self.streak = match self.last_peak {
            Some(p) if p.abs_diff(peak) <= 2 => self.streak + 1,
            _ => 1,
        };
        self.last_peak = Some(peak);
        if self.streak < 2 {
            return None;
        }
        let delay = (d - peak) * DECIMATE;
        let changed = self
            .estimate
            .is_none_or(|e| e.abs_diff(delay) > DECIMATE * 2);
        if changed {
            self.estimate = Some(delay);
        }
        changed.then_some(delay)
    }
}

pub struct Aec {
    cfg: AecConfig,
    parts: usize,
    far: Vec<f32>,
    far_hi: usize,
    far_cursor: Option<usize>,
    mic_ring: Vec<f32>,
    pending: Vec<f32>,
    /// Blocks processed: block `k` is input samples `[k * BLOCK, (k + 1) * BLOCK)`.
    blocks: usize,
    delay: usize,
    fft: RealFft,
    x: Spectra,
    x_energy: Vec<f32>,
    x_prev: Vec<f32>,
    w: Spectra,
    f: Spectra,
    power: Vec<f32>,
    eh: Vec<f32>,
    yh: Vec<f32>,
    pey: f32,
    pyy: f32,
    leak: f32,
    adapted: bool,
    sum_adapt: f32,
    /// Consecutive blocks in which the foreground made the echo worse than doing nothing.
    worse_than_nothing: usize,
    davg1: f64,
    davg2: f64,
    dvar1: f64,
    dvar2: f64,
    prop: Vec<f32>,
    count: usize,
    t: Vec<f32>,
    er: Vec<f32>,
    ei: Vec<f32>,
    yr: Vec<f32>,
    yi: Vec<f32>,
    mu: Vec<f32>,
    d: Vec<f32>,
    xb: Vec<f32>,
    yb: Vec<f32>,
    yf: Vec<f32>,
    eb: Vec<f32>,
    ef: Vec<f32>,
    e_prev: Vec<f32>,
    y_prev: Vec<f32>,
    win: Vec<f32>,
    win_sq: Vec<f32>,
    tail: Vec<f32>,
    tail_exact: bool,
    pe_s: Vec<f32>,
    res_s: Vec<f32>,
    gain: Vec<f32>,
    frame: Vec<f32>,
    est: DelayEstimator,
}

impl Aec {
    pub fn new(cfg: AecConfig) -> Aec {
        let parts = ((cfg.tail_ms as usize * RATE as usize / 1000).div_ceil(BLOCK)).max(2);
        let max_delay = cfg.max_delay_ms as usize * RATE as usize / 1000;
        let win: Vec<f32> = (0..FFT)
            .map(|n| {
                (0.5 - 0.5 * (2.0 * std::f64::consts::PI * n as f64 / FFT as f64).cos()).sqrt()
                    as f32
            })
            .collect();
        let win_sq = win.iter().map(|w| w * w).collect();
        let mut aec = Aec {
            cfg,
            parts,
            far: vec![0.0; FAR_RING],
            far_hi: 0,
            far_cursor: None,
            mic_ring: vec![0.0; 2 * BLOCK],
            pending: Vec::with_capacity(BLOCK),
            blocks: 0,
            delay: 0,
            fft: RealFft::new(FFT),
            x: Spectra::new(parts),
            x_energy: vec![0.0; parts],
            x_prev: vec![0.0; BLOCK],
            w: Spectra::new(parts),
            f: Spectra::new(parts),
            power: vec![0.0; BINS],
            eh: vec![0.0; BINS],
            yh: vec![0.0; BINS],
            pey: 1.0,
            pyy: 1.0,
            leak: 1.0,
            adapted: false,
            sum_adapt: 0.0,
            worse_than_nothing: 0,
            davg1: 0.0,
            davg2: 0.0,
            dvar1: 0.0,
            dvar2: 0.0,
            prop: vec![0.0; parts],
            count: 0,
            t: vec![0.0; FFT],
            er: vec![0.0; BINS],
            ei: vec![0.0; BINS],
            yr: vec![0.0; BINS],
            yi: vec![0.0; BINS],
            mu: vec![0.0; BINS],
            d: vec![0.0; BLOCK],
            xb: vec![0.0; FFT],
            yb: vec![0.0; BLOCK],
            yf: vec![0.0; BLOCK],
            eb: vec![0.0; BLOCK],
            ef: vec![0.0; BLOCK],
            e_prev: vec![0.0; BLOCK],
            y_prev: vec![0.0; BLOCK],
            win,
            win_sq,
            tail: vec![0.0; BLOCK],
            tail_exact: true,
            pe_s: vec![0.0; BINS],
            res_s: vec![0.0; BINS],
            gain: vec![1.0; BINS],
            frame: vec![0.0; FFT],
            est: DelayEstimator::new(max_delay),
        };
        aec.reset();
        aec
    }

    /// Forget everything, the room included. Also writes every buffer, so the pages are resident
    /// before memory is measured.
    pub fn reset(&mut self) {
        self.far.fill(0.0);
        self.far_hi = 0;
        self.far_cursor = None;
        self.mic_ring.fill(0.0);
        self.pending.clear();
        self.blocks = 0;
        self.delay = self.initial_delay();
        for s in [&mut self.x, &mut self.w, &mut self.f] {
            s.clear();
        }
        self.x_energy.fill(0.0);
        self.x_prev.fill(0.0);
        self.power.fill(0.0);
        self.eh.fill(0.0);
        self.yh.fill(0.0);
        self.pey = 1.0;
        self.pyy = 1.0;
        self.leak = 1.0;
        self.adapted = false;
        self.sum_adapt = 0.0;
        self.worse_than_nothing = 0;
        self.reset_two_path();
        self.prop.fill(0.99 / self.parts as f32);
        self.count = 0;
        for v in [&mut self.t, &mut self.xb, &mut self.frame] {
            v.fill(0.0);
        }
        for v in [
            &mut self.er,
            &mut self.ei,
            &mut self.yr,
            &mut self.yi,
            &mut self.mu,
            &mut self.pe_s,
            &mut self.res_s,
        ] {
            v.fill(0.0);
        }
        for v in [
            &mut self.d,
            &mut self.yb,
            &mut self.yf,
            &mut self.eb,
            &mut self.ef,
            &mut self.e_prev,
            &mut self.y_prev,
            &mut self.tail,
        ] {
            v.fill(0.0);
        }
        self.gain.fill(1.0);
        self.tail_exact = true;
        self.est.reset();
    }

    fn initial_delay(&self) -> usize {
        let d = self.cfg.initial_delay_ms as usize * RATE as usize / 1000;
        d.saturating_sub(PRE_DELAY) / BLOCK * BLOCK
    }

    /// Back to the fast initial adaptation: the room changed, so the leak estimate, which cannot
    /// tell a new room from double talk, would otherwise hold the step down for seconds.
    fn relearn(&mut self) {
        self.adapted = false;
        self.sum_adapt = 0.0;
        self.pey = 1.0;
        self.pyy = 1.0;
        self.leak = 1.0;
        self.worse_than_nothing = 0;
        self.reset_two_path();
    }

    fn reset_two_path(&mut self) {
        self.davg1 = 0.0;
        self.davg2 = 0.0;
        self.dvar1 = 0.0;
        self.dvar2 = 0.0;
    }

    pub fn config(&self) -> AecConfig {
        self.cfg
    }

    /// The bulk delay the filter currently starts at, in samples.
    pub fn filter_delay(&self) -> usize {
        self.delay
    }

    /// The estimator's speaker-to-microphone delay, once it has locked.
    pub fn estimated_delay(&self) -> Option<usize> {
        self.est.estimate
    }

    pub fn bytes(&self) -> usize {
        let v = |x: &Vec<f32>| x.len() * 4;
        v(&self.far)
            + v(&self.mic_ring)
            + self.pending.capacity() * 4
            + self.x.bytes()
            + self.w.bytes()
            + self.f.bytes()
            + [
                &self.x_energy,
                &self.x_prev,
                &self.power,
                &self.eh,
                &self.yh,
                &self.prop,
                &self.t,
                &self.er,
                &self.ei,
                &self.yr,
                &self.yi,
                &self.mu,
                &self.d,
                &self.xb,
                &self.yb,
                &self.yf,
                &self.eb,
                &self.ef,
                &self.e_prev,
                &self.y_prev,
                &self.win,
                &self.win_sq,
                &self.tail,
                &self.pe_s,
                &self.res_s,
                &self.gain,
                &self.frame,
            ]
            .iter()
            .map(|x| x.len() * 4)
            .sum::<usize>()
            + self.fft.bytes()
            + self.est.bytes()
    }

    /// The far-end audio, 16 kHz, whose first sample leaves for the speaker when the microphone's
    /// clock reads `at`. It must arrive before the microphone samples it overlaps.
    pub fn play(&mut self, at: usize, pcm: &[f32]) {
        let start = match self.far_cursor {
            Some(c) if at.abs_diff(c) <= JITTER => c,
            _ => at,
        };
        let mask = FAR_RING - 1;
        if start > self.far_hi {
            let gap = (start - self.far_hi).min(FAR_RING);
            for i in start - gap..start {
                self.far[i & mask] = 0.0;
            }
        }
        for (i, &v) in pcm.iter().enumerate() {
            self.far[(start + i) & mask] = v;
        }
        let end = start + pcm.len();
        self.far_cursor = Some(end);
        self.far_hi = self.far_hi.max(end);
    }

    fn far_at(&self, i: usize) -> f32 {
        if i >= self.far_hi || i + FAR_RING < self.far_hi {
            0.0
        } else {
            self.far[i & (FAR_RING - 1)]
        }
    }

    /// Feed microphone samples; appends the echo-cancelled output, whole blocks, `LATENCY` behind.
    pub fn process(&mut self, mic: &[f32], out: &mut Vec<f32>) {
        let mut rest = mic;
        while !rest.is_empty() {
            let take = (BLOCK - self.pending.len()).min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == BLOCK {
                self.block(out);
                self.pending.clear();
            }
        }
    }

    fn block(&mut self, out: &mut Vec<f32>) {
        let start = self.blocks * BLOCK;
        let ring_len = self.mic_ring.len();
        for (i, &v) in self.pending.iter().enumerate() {
            self.mic_ring[(start + i) % ring_len] = v;
            self.d[i] = v * SCALE;
        }
        let mic_at = |ring: &[f32], i: usize| {
            if i + ring_len >= start + BLOCK {
                ring[i % ring_len]
            } else {
                0.0
            }
        };
        {
            let (far, far_hi, ring) = (&self.far, self.far_hi, &self.mic_ring);
            let far_at = |i: usize| {
                if i >= far_hi || i + FAR_RING < far_hi {
                    0.0
                } else {
                    far[i & (FAR_RING - 1)]
                }
            };
            self.est.push_block(start, far_at, |i| mic_at(ring, i));
        }
        if self.blocks.is_multiple_of(DELAY_HOP_BLOCKS) {
            if let Some(delay) = self.est.update() {
                self.retarget(delay, start);
            }
        }

        self.shift_far(start);
        let engaged = self.x_energy.iter().any(|&e| e > 0.0);
        if engaged {
            self.cancel();
        } else {
            self.ef.copy_from_slice(&self.d);
            self.yf.fill(0.0);
        }
        self.suppress(engaged, out);
        self.blocks += 1;
    }

    /// The newest far-end block for the filter, `delay` behind the microphone.
    fn shift_far(&mut self, start: usize) {
        let n = BINS;
        self.x.re.copy_within(0..(self.parts - 1) * n, n);
        self.x.im.copy_within(0..(self.parts - 1) * n, n);
        self.x_energy.copy_within(0..self.parts - 1, 1);
        let base = start as isize - self.delay as isize;
        let mut e = 0f32;
        for i in 0..BLOCK {
            let k = base + i as isize;
            let v = if k >= 0 {
                self.far_at(k as usize) * SCALE
            } else {
                0.0
            };
            self.xb[i] = self.x_prev[i];
            self.xb[BLOCK + i] = v;
            self.x_prev[i] = v;
            e += self.xb[i] * self.xb[i] + v * v;
        }
        self.x_energy[0] = e;
        if e > 0.0 {
            self.fft
                .forward(&self.xb, &mut self.x.re[..n], &mut self.x.im[..n]);
        } else {
            self.x.re[..n].fill(0.0);
            self.x.im[..n].fill(0.0);
        }
    }

    /// Move the filter to a new bulk delay: shift both filters by whole blocks so the room they
    /// learned is kept, and rebuild the far-end history at the new alignment.
    fn retarget(&mut self, estimate: usize, start: usize) {
        let near = estimate.saturating_sub(PRE_DELAY) / BLOCK * BLOCK;
        let within =
            estimate >= self.delay + BLOCK / 2 && estimate <= self.delay + PRE_DELAY + 2 * BLOCK;
        if within || near == self.delay {
            return;
        }
        let shift = (near as isize - self.delay as isize) / BLOCK as isize;
        if shift.abs() >= 2 {
            self.relearn();
        }
        let n = BINS;
        for s in [&mut self.w, &mut self.f] {
            let (re, im) = (s.re.clone(), s.im.clone());
            for p in 0..self.parts {
                let from = p as isize + shift;
                let dst = p * n..(p + 1) * n;
                if from >= 0 && (from as usize) < self.parts {
                    let src = from as usize * n..(from as usize + 1) * n;
                    s.re[dst.clone()].copy_from_slice(&re[src.clone()]);
                    s.im[dst].copy_from_slice(&im[src]);
                } else {
                    s.re[dst.clone()].fill(0.0);
                    s.im[dst].fill(0.0);
                }
            }
        }
        self.delay = near;
        // Rebuild X history at the new alignment: partition p holds blocks k - p - 1 and k - p.
        let k = start / BLOCK;
        for p in 0..self.parts {
            let mut e = 0f32;
            for (half, blk) in [(0usize, 1usize), (1, 0)] {
                let b = k as isize - p as isize - blk as isize - 1;
                for i in 0..BLOCK {
                    let idx = b * BLOCK as isize + i as isize - near as isize;
                    let v = if idx >= 0 {
                        self.far_at(idx as usize) * SCALE
                    } else {
                        0.0
                    };
                    self.xb[half * BLOCK + i] = v;
                    e += v * v;
                }
            }
            if p == 0 {
                self.x_prev.copy_from_slice(&self.xb[BLOCK..]);
            }
            self.x_energy[p] = e;
            let r = p * n..(p + 1) * n;
            self.fft
                .forward(&self.xb, &mut self.x.re[r.clone()], &mut self.x.im[r]);
        }
        self.reset_two_path();
    }

    /// `Y = sum_p S_p X_p`, back to the time domain: the last block of the overlap-save frame.
    fn echo(&mut self, foreground: bool, y: &mut [f32]) {
        let s = if foreground { &self.f } else { &self.w };
        self.yr.fill(0.0);
        self.yi.fill(0.0);
        for p in 0..self.parts {
            if self.x_energy[p] == 0.0 {
                continue;
            }
            let o = p * BINS;
            for j in 0..BINS {
                let (a, b) = (s.re[o + j], s.im[o + j]);
                let (c, d) = (self.x.re[o + j], self.x.im[o + j]);
                self.yr[j] += a * c - b * d;
                self.yi[j] += a * d + b * c;
            }
        }
        self.fft.inverse(&self.yr, &self.yi, &mut self.t);
        y.copy_from_slice(&self.t[BLOCK..]);
    }

    fn cancel(&mut self) {
        let mut yf = std::mem::take(&mut self.yf);
        let mut yb = std::mem::take(&mut self.yb);
        self.echo(true, &mut yf);
        self.echo(false, &mut yb);
        let (mut sff, mut see, mut dbf) = (0f64, 0f64, 0f64);
        for i in 0..BLOCK {
            self.ef[i] = self.d[i] - yf[i];
            self.eb[i] = self.d[i] - yb[i];
            sff += (self.ef[i] as f64).powi(2);
            see += (self.eb[i] as f64).powi(2);
            dbf += ((yf[i] - yb[i]) as f64).powi(2);
        }
        let ok = sff.is_finite()
            && see.is_finite()
            && sff < BLOCK as f64 * 1e12
            && see < BLOCK as f64 * 1e12;
        if !ok {
            // Diverged (never seen, but a NaN must not reach the wake word): start the room over.
            self.w.clear();
            self.f.clear();
            self.ef.copy_from_slice(&self.d);
            yf.fill(0.0);
            self.yf = yf;
            self.yb = yb;
            return;
        }

        let sdd: f64 = self.d.iter().map(|&v| (v as f64).powi(2)).sum();
        // A foreground that makes the microphone louder than doing nothing, block after block, is
        // a room that changed: drop it and relearn fast.
        if sdd > 1e3 * BLOCK as f64 && sff > sdd {
            self.worse_than_nothing += 1;
        } else {
            self.worse_than_nothing = 0;
        }
        if self.worse_than_nothing >= RELEARN_BLOCKS {
            self.f.clear();
            self.w.clear();
            self.relearn();
            self.ef.copy_from_slice(&self.d);
            self.eb.copy_from_slice(&self.d);
            yf.fill(0.0);
            yb.fill(0.0);
            sff = sdd;
            see = sdd;
            dbf = 0.0;
        }

        // Two-path: is the background's improvement over the foreground statistically real?
        let diff = sff - see;
        self.davg1 = 0.6 * self.davg1 + 0.4 * diff;
        self.davg2 = 0.85 * self.davg2 + 0.15 * diff;
        self.dvar1 = 0.36 * self.dvar1 + 0.16 * sff * dbf;
        self.dvar2 = 0.7225 * self.dvar2 + 0.0225 * sff * dbf;
        let better = diff * diff.abs() > sff * dbf
            || self.davg1 * self.davg1.abs() > VAR1_UPDATE * self.dvar1
            || self.davg2 * self.davg2.abs() > VAR2_UPDATE * self.dvar2;
        if better {
            self.reset_two_path();
            self.f.re.copy_from_slice(&self.w.re);
            self.f.im.copy_from_slice(&self.w.im);
            for i in 0..BLOCK {
                // Fade from the old foreground to the new one across the block, so the swap is silent.
                let a = (i as f32 + 0.5) / BLOCK as f32;
                self.ef[i] = (1.0 - a) * self.ef[i] + a * self.eb[i];
                yf[i] = (1.0 - a) * yf[i] + a * yb[i];
            }
        } else {
            let worse = -diff * diff.abs() > VAR_BACKTRACK * sff * dbf
                || -self.davg1 * self.davg1.abs() > VAR_BACKTRACK * self.dvar1
                || -self.davg2 * self.davg2.abs() > VAR_BACKTRACK * self.dvar2;
            if worse {
                self.reset_two_path();
                self.w.re.copy_from_slice(&self.f.re);
                self.w.im.copy_from_slice(&self.f.im);
                self.eb.copy_from_slice(&self.ef);
                yb.copy_from_slice(&yf);
            }
        }
        self.adapt(&yb);
        self.yf = yf;
        self.yb = yb;
    }

    /// Adapt the background filter with the learning rate of Valin (2007): per bin, the share of
    /// the error that is residual echo, so near-end speech (double talk) slows it to a halt.
    fn adapt(&mut self, yb: &[f32]) {
        let (mut sxx, mut see, mut syy, mut sey) = (0f32, 1f32, 0f32, 0f32);
        for ((&x, &e), &y) in self.x_prev.iter().zip(&self.eb).zip(yb) {
            sxx += x * x;
            see += e * e;
            syy += y * y;
            sey += e * y;
        }
        // Error and echo spectra as overlap-save frames: a zero block, then the block.
        self.t[..BLOCK].fill(0.0);
        self.t[BLOCK..].copy_from_slice(&self.eb);
        self.fft.forward(&self.t, &mut self.er, &mut self.ei);
        self.t[BLOCK..].copy_from_slice(yb);
        self.fft.forward(&self.t, &mut self.yr, &mut self.yi);

        let ss = 0.35 / self.parts as f32;
        let spec_avg = BLOCK as f32 / RATE;
        let (mut pey, mut pyy) = (0f32, 0f32);
        for j in 0..BINS {
            let xf = self.x.re[j] * self.x.re[j] + self.x.im[j] * self.x.im[j];
            self.power[j] = (1.0 - ss) * self.power[j] + 1.0 + ss * xf;
            let rf = self.er[j] * self.er[j] + self.ei[j] * self.ei[j];
            let yf = self.yr[j] * self.yr[j] + self.yi[j] * self.yi[j];
            let (eh, yh) = (rf - self.eh[j], yf - self.yh[j]);
            pey += eh * yh;
            pyy += yh * yh;
            self.eh[j] = (1.0 - spec_avg) * self.eh[j] + spec_avg * rf;
            self.yh[j] = (1.0 - spec_avg) * self.yh[j] + spec_avg * yf;
            // Reuse the echo spectrum slots for the two powers the step needs below.
            self.yr[j] = yf;
            self.yi[j] = rf;
        }
        let pyy = pyy.sqrt();
        let pey = if pyy > 0.0 { pey / pyy } else { 0.0 };
        // Leak: the regression of residual power on echo power, updated faster when echo dominates.
        let beta0 = 2.0 * BLOCK as f32 / RATE;
        let beta_max = 0.5 * BLOCK as f32 / RATE;
        let alpha = (beta0 * syy / see).min(beta_max);
        self.pey = (1.0 - alpha) * self.pey + alpha * pey;
        self.pyy = ((1.0 - alpha) * self.pyy + alpha * pyy).max(1.0);
        self.pey = self.pey.clamp(MIN_LEAK * self.pyy, self.pyy);
        self.leak = self.pey / self.pyy;

        // Residual-to-error ratio, bounded below by the part of the error correlated with the echo.
        let mut res = sxx / 8_192.0 + 3.0 * self.leak * syy;
        let bound = sey * sey / (1.0 + syy);
        if bound > see {
            res = see;
        } else if res < bound {
            res = bound;
        }
        let rer = res.min(0.5 * see) / see;

        if !self.adapted && self.sum_adapt > self.parts as f32 && self.leak > 0.03 {
            self.adapted = true;
        }
        if self.adapted {
            for j in 0..BINS {
                let (yf, rf) = (self.yr[j], self.yi[j]);
                let e = rf + 1.0;
                let r = (self.leak * yf).min(0.5 * e);
                let r = 0.7 * r + 0.3 * rer * e;
                self.mu[j] = r / (e * (self.power[j] + 10.0));
            }
        } else {
            let mut rate = 0.0;
            if sxx > BLOCK as f32 * 1_000.0 {
                rate = (INITIAL_RATE * sxx).min(INITIAL_RATE * see) / see;
            }
            for j in 0..BINS {
                self.mu[j] = rate / (self.power[j] + 10.0);
            }
            self.sum_adapt += rate;
        }

        // Proportionate: partitions that hold more of the room adapt faster.
        let mut max = 1f32;
        for p in 0..self.parts {
            let o = p * BINS;
            let e: f32 = (0..BINS)
                .map(|j| self.w.re[o + j].powi(2) + self.w.im[o + j].powi(2))
                .sum();
            self.prop[p] = (1.0 + e).sqrt();
            max = max.max(self.prop[p]);
        }
        let mut total = 0f32;
        for p in 0..self.parts {
            self.prop[p] += 0.1 * max;
            total += self.prop[p];
        }
        for p in 0..self.parts {
            self.prop[p] *= 0.99 / total;
        }

        // Gradient conj(X_p) E per partition; the circular-convolution constraint goes on
        // partition 0 and one other in rotation (alternately updated MDF).
        let rotating = 1 + self.count % (self.parts - 1);
        for p in 0..self.parts {
            if self.x_energy[p] == 0.0 {
                continue;
            }
            let o = p * BINS;
            let g = self.prop[p];
            for j in 0..BINS {
                let (xr, xi) = (self.x.re[o + j], self.x.im[o + j]);
                let (er, ei) = (self.er[j], self.ei[j]);
                let m = g * self.mu[j];
                self.w.re[o + j] += m * (xr * er + xi * ei);
                self.w.im[o + j] += m * (xr * ei - xi * er);
            }
            if p == 0 || p == rotating {
                self.fft.inverse(
                    &self.w.re[o..o + BINS],
                    &self.w.im[o..o + BINS],
                    &mut self.t,
                );
                self.t[BLOCK..].fill(0.0);
                self.fft.forward(
                    &self.t,
                    &mut self.w.re[o..o + BINS],
                    &mut self.w.im[o..o + BINS],
                );
            }
        }
        self.count += 1;
    }

    /// Residual echo suppression on a sqrt-Hann STFT, then overlap-add; when neither this frame nor
    /// the last had far-end audio, the microphone passes through bit for bit.
    fn suppress(&mut self, engaged: bool, out: &mut Vec<f32>) {
        let first = out.len();
        if self.blocks > 0 {
            out.resize(first + BLOCK, 0.0);
        }
        if !engaged {
            if self.blocks > 0 {
                let o = &mut out[first..];
                if self.tail_exact {
                    o.copy_from_slice(&self.e_prev);
                } else {
                    for (i, v) in o.iter_mut().enumerate() {
                        *v = self.tail[i] + self.win_sq[i] * self.e_prev[i];
                    }
                }
                o.iter_mut().for_each(|v| *v /= SCALE);
            }
            for i in 0..BLOCK {
                self.tail[i] = self.win_sq[BLOCK + i] * self.ef[i];
            }
            self.tail_exact = true;
        } else {
            for i in 0..BLOCK {
                self.frame[i] = self.win[i] * self.e_prev[i];
                self.frame[BLOCK + i] = self.win[BLOCK + i] * self.ef[i];
            }
            self.fft.forward(&self.frame, &mut self.er, &mut self.ei);
            if self.cfg.suppress {
                for i in 0..BLOCK {
                    self.frame[i] = self.win[i] * self.y_prev[i];
                    self.frame[BLOCK + i] = self.win[BLOCK + i] * self.yf[i];
                }
                self.fft.forward(&self.frame, &mut self.yr, &mut self.yi);
                let floor = 10f32.powf(self.cfg.floor_db / 20.0);
                for j in 0..BINS {
                    let pe = self.er[j] * self.er[j] + self.ei[j] * self.ei[j];
                    let res = self.leak * (self.yr[j] * self.yr[j] + self.yi[j] * self.yi[j]);
                    self.pe_s[j] = 0.5 * self.pe_s[j] + 0.5 * pe;
                    self.res_s[j] = 0.5 * self.res_s[j] + 0.5 * res;
                    let g = (1.0 - 2.0 * self.res_s[j] / (self.pe_s[j] + 1.0)).clamp(floor, 1.0);
                    // Down at once, back up gently: echo onsets are sudden, near-end speech is not.
                    self.gain[j] = if g < self.gain[j] {
                        g
                    } else {
                        0.5 * self.gain[j] + 0.5 * g
                    };
                    self.er[j] *= self.gain[j];
                    self.ei[j] *= self.gain[j];
                }
            }
            self.fft.inverse(&self.er, &self.ei, &mut self.frame);
            if self.blocks > 0 {
                for (i, v) in out[first..].iter_mut().enumerate() {
                    *v = (self.tail[i] + self.win[i] * self.frame[i]) / SCALE;
                }
            }
            for i in 0..BLOCK {
                self.tail[i] = self.win[BLOCK + i] * self.frame[BLOCK + i];
            }
            self.tail_exact = false;
        }
        if !engaged {
            self.gain.fill(1.0);
        }
        self.e_prev.copy_from_slice(&self.ef);
        self.y_prev.copy_from_slice(&self.yf);
    }

    /// The foreground filter in the time domain, `(bulk delay, taps)`: echo at the microphone is
    /// `sum_j taps[j] * far[n - delay - j]`.
    pub fn response(&mut self) -> (usize, Vec<f32>) {
        let mut h = vec![0f32; self.parts * BLOCK];
        for p in 0..self.parts {
            let o = p * BINS;
            self.fft.inverse(
                &self.f.re[o..o + BINS],
                &self.f.im[o..o + BINS],
                &mut self.t,
            );
            h[p * BLOCK..(p + 1) * BLOCK].copy_from_slice(&self.t[..BLOCK]);
        }
        (self.delay, h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32, amp: f32) -> Vec<f32> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((s >> 8) as f32 / 16_777_216.0 - 0.5) * 2.0 * amp
            })
            .collect()
    }

    #[test]
    fn with_nothing_playing_the_microphone_passes_through_bit_for_bit() {
        let mic = noise(16_000, 7, 0.3);
        let mut aec = Aec::new(AecConfig::default());
        let mut out = Vec::new();
        for c in mic.chunks(333) {
            aec.process(c, &mut out);
        }
        assert_eq!(out.len(), mic.len() / BLOCK * BLOCK - BLOCK);
        assert!(out
            .iter()
            .zip(&mic)
            .all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn output_lines_up_with_input_after_a_reply_ends() {
        let far = noise(4_000, 1, 0.3);
        let mut aec = Aec::new(AecConfig::default());
        aec.play(0, &far);
        let mic = noise(24_000, 9, 0.01);
        let mut out = Vec::new();
        aec.process(&mic, &mut out);
        // Long after the far end stopped, the output is the microphone again, exactly.
        let tail = 16_000..out.len();
        assert!(tail.clone().all(|i| out[i].to_bits() == mic[i].to_bits()));
    }

    #[test]
    fn jittered_stamps_stay_one_continuous_stream_and_a_jump_resyncs() {
        let mut aec = Aec::new(AecConfig::default());
        aec.play(1_000, &[1.0; 100]);
        aec.play(1_150, &[2.0; 100]);
        assert_eq!(
            (aec.far_at(1_099), aec.far_at(1_100), aec.far_at(1_199)),
            (1.0, 2.0, 2.0)
        );
        aec.play(5_000, &[3.0; 10]);
        assert_eq!(
            (aec.far_at(1_200), aec.far_at(4_999), aec.far_at(5_000)),
            (0.0, 0.0, 3.0)
        );
    }

    #[test]
    fn the_estimator_finds_a_pure_delay() {
        let far = noise(48_000, 3, 0.2);
        let delay = 1_760;
        let mic: Vec<f32> = (0..far.len())
            .map(|i| {
                if i >= delay {
                    0.5 * far[i - delay]
                } else {
                    0.0
                }
            })
            .collect();
        let mut aec = Aec::new(AecConfig::default());
        aec.play(0, &far);
        let mut out = Vec::new();
        aec.process(&mic, &mut out);
        let got = aec.estimated_delay().expect("locked");
        assert!(got.abs_diff(delay) <= 8, "estimated {got}, true {delay}");
        assert!(aec.filter_delay() <= delay && delay - aec.filter_delay() <= PRE_DELAY + BLOCK);
    }
}
