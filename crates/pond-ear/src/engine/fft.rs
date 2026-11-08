//! Two real FFTs: the front end's 512-point power spectrum in f64 (f32 rounding swamps weak bins
//! beside a strong one: 7e-3 log-mel error), and a general f32 one for the echo canceller.

use super::tables::{BITREV256, TW256_IM, TW256_RE, TW512_IM, TW512_RE};

const N: usize = 256;

fn fft256(re: &mut [f64; N], im: &mut [f64; N]) {
    for (i, &j) in BITREV256.iter().enumerate() {
        let j = j as usize;
        if j > i {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= N {
        let half = len / 2;
        let step = N / len;
        let mut start = 0;
        while start < N {
            for j in 0..half {
                let (wr, wi) = (TW256_RE[j * step], TW256_IM[j * step]);
                let (a, b) = (start + j, start + j + half);
                let tr = wr * re[b] - wi * im[b];
                let ti = wr * im[b] + wi * re[b];
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
            start += len;
        }
        len *= 2;
    }
}

/// Power spectrum |X[k]|^2, k < 256, of the 512 real samples in `x`; Kaldi's mel banks give the
/// Nyquist bin zero weight, so it is not produced.
pub fn power_spectrum_512(x: &[f32; 2 * N], power: &mut [f32; N]) {
    let mut re = [0f64; N];
    let mut im = [0f64; N];
    for n in 0..N {
        re[n] = x[2 * n] as f64;
        im[n] = x[2 * n + 1] as f64;
    }
    fft256(&mut re, &mut im);
    for k in 0..N {
        let m = (N - k) % N;
        // E = (Z[k] + conj Z[N-k]) / 2,  O = (Z[k] - conj Z[N-k]) / 2i
        let (er, ei) = (0.5 * (re[k] + re[m]), 0.5 * (im[k] - im[m]));
        let (dr, di) = (0.5 * (re[k] - re[m]), 0.5 * (im[k] + im[m]));
        let (or, oi) = (di, -dr);
        let (wr, wi) = (TW512_RE[k], TW512_IM[k]);
        let xr = er + wr * or - wi * oi;
        let xi = ei + wr * oi + wi * or;
        power[k] = (xr * xr + xi * xi) as f32;
    }
}

/// A real FFT of any power-of-two size in f32: an `n/2`-point complex FFT on the even/odd-packed
/// signal, then a split pass. Owns its scratch, so a transform never allocates.
#[derive(Clone)]
pub struct RealFft {
    n: usize,
    bitrev: Vec<u32>,
    tw_re: Vec<f32>,
    tw_im: Vec<f32>,
    split_re: Vec<f32>,
    split_im: Vec<f32>,
    zr: Vec<f32>,
    zi: Vec<f32>,
}

impl RealFft {
    /// `n` must be a power of two, at least 4.
    pub fn new(n: usize) -> RealFft {
        assert!(
            n >= 4 && n.is_power_of_two(),
            "RealFft size {n} is not a power of two >= 4"
        );
        let half = n / 2;
        let bits = half.trailing_zeros();
        let bitrev = (0..half as u32)
            .map(|i| {
                if bits == 0 {
                    0
                } else {
                    i.reverse_bits() >> (32 - bits)
                }
            })
            .collect();
        let ang = |k: usize, m: usize| -2.0 * std::f64::consts::PI * k as f64 / m as f64;
        RealFft {
            n,
            bitrev,
            tw_re: (0..half / 2).map(|k| ang(k, half).cos() as f32).collect(),
            tw_im: (0..half / 2).map(|k| ang(k, half).sin() as f32).collect(),
            split_re: (0..=half).map(|k| ang(k, n).cos() as f32).collect(),
            split_im: (0..=half).map(|k| ang(k, n).sin() as f32).collect(),
            zr: vec![0.0; half],
            zi: vec![0.0; half],
        }
    }

    /// Bins of a transform: `n / 2 + 1`, DC to Nyquist.
    pub fn bins(&self) -> usize {
        self.n / 2 + 1
    }

    pub fn bytes(&self) -> usize {
        self.bitrev.len() * 4
            + (self.tw_re.len() + self.tw_im.len() + self.split_re.len() + self.split_im.len()) * 4
            + (self.zr.len() + self.zi.len()) * 4
    }

    fn complex(&mut self, inverse: bool) {
        let half = self.n / 2;
        let (zr, zi) = (&mut self.zr, &mut self.zi);
        for i in 0..half {
            let j = self.bitrev[i] as usize;
            if j > i {
                zr.swap(i, j);
                zi.swap(i, j);
            }
        }
        let sign = if inverse { -1.0 } else { 1.0 };
        let mut len = 2;
        while len <= half {
            let h = len / 2;
            let step = half / len;
            let mut start = 0;
            while start < half {
                for j in 0..h {
                    let (wr, wi) = (self.tw_re[j * step], sign * self.tw_im[j * step]);
                    let (a, b) = (start + j, start + j + h);
                    let tr = wr * zr[b] - wi * zi[b];
                    let ti = wr * zi[b] + wi * zr[b];
                    zr[b] = zr[a] - tr;
                    zi[b] = zi[a] - ti;
                    zr[a] += tr;
                    zi[a] += ti;
                }
                start += len;
            }
            len *= 2;
        }
    }

    /// `X[k] = sum x[t] e^(-2 pi i k t / n)` for `k <= n / 2`.
    pub fn forward(&mut self, x: &[f32], re: &mut [f32], im: &mut [f32]) {
        let half = self.n / 2;
        debug_assert!(x.len() >= self.n && re.len() > half && im.len() > half);
        for m in 0..half {
            self.zr[m] = x[2 * m];
            self.zi[m] = x[2 * m + 1];
        }
        self.complex(false);
        for k in 0..=half {
            let (a, b) = (k % half, (half - k) % half);
            let (er, ei) = (
                0.5 * (self.zr[a] + self.zr[b]),
                0.5 * (self.zi[a] - self.zi[b]),
            );
            let (dr, di) = (
                0.5 * (self.zr[a] - self.zr[b]),
                0.5 * (self.zi[a] + self.zi[b]),
            );
            let (or, oi) = (di, -dr);
            let (wr, wi) = (self.split_re[k], self.split_im[k]);
            re[k] = er + wr * or - wi * oi;
            im[k] = ei + wr * oi + wi * or;
        }
    }

    /// The inverse of `forward`, scaled so that `inverse(forward(x)) == x`.
    pub fn inverse(&mut self, re: &[f32], im: &[f32], x: &mut [f32]) {
        let half = self.n / 2;
        debug_assert!(x.len() >= self.n && re.len() > half && im.len() > half);
        for k in 0..half {
            let m = half - k;
            let (er, ei) = (0.5 * (re[k] + re[m]), 0.5 * (im[k] - im[m]));
            let (dr, di) = (0.5 * (re[k] - re[m]), 0.5 * (im[k] + im[m]));
            // O = conj(W^k) * D, then Z = E + i O.
            let (wr, wi) = (self.split_re[k], -self.split_im[k]);
            let (or, oi) = (wr * dr - wi * di, wr * di + wi * dr);
            self.zr[k] = er - oi;
            self.zi[k] = ei + or;
        }
        self.complex(true);
        let scale = 1.0 / half as f32;
        for m in 0..half {
            x[2 * m] = self.zr[m] * scale;
            x[2 * m + 1] = self.zi[m] * scale;
        }
    }
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

    #[test]
    fn matches_a_direct_dft() {
        let mut x = [0f32; 512];
        x[..400].copy_from_slice(&noise(400, 12345));
        let mut p = [0f32; 256];
        power_spectrum_512(&x, &mut p);
        for (k, &got) in p.iter().enumerate() {
            let (mut r, mut i) = (0f64, 0f64);
            for (n, &v) in x.iter().enumerate() {
                let a = -2.0 * core::f64::consts::PI * (k * n) as f64 / 512.0;
                r += v as f64 * a.cos();
                i += v as f64 * a.sin();
            }
            let want = (r * r + i * i) as f32;
            assert!(
                (got - want).abs() <= 1e-4 * want.max(1.0),
                "bin {k}: {got} vs {want}"
            );
        }
    }

    #[test]
    fn real_fft_matches_a_direct_dft_and_inverts() {
        for n in [4usize, 8, 64, 256, 4096] {
            let x = noise(n, n as u32);
            let mut f = RealFft::new(n);
            let (mut re, mut im) = (vec![0f32; f.bins()], vec![0f32; f.bins()]);
            f.forward(&x, &mut re, &mut im);
            for k in (0..f.bins()).step_by((n / 64).max(1)) {
                let (mut r, mut i) = (0f64, 0f64);
                for (t, &v) in x.iter().enumerate() {
                    let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                    r += v as f64 * a.cos();
                    i += v as f64 * a.sin();
                }
                let tol = 2e-5 * (n as f64).sqrt() * (n as f64).log2();
                assert!(
                    (re[k] as f64 - r).abs() < tol && (im[k] as f64 - i).abs() < tol,
                    "n {n} bin {k}"
                );
            }
            let mut y = vec![0f32; n];
            f.inverse(&re, &im, &mut y);
            let err = x
                .iter()
                .zip(&y)
                .map(|(a, b)| (a - b).abs())
                .fold(0f32, f32::max);
            assert!(err < 1e-5, "n {n}: round trip {err}");
        }
    }
}
