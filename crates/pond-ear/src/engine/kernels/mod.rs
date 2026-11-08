//! Compute kernels, one module per rung of the ladder. Every rung computes the same function and
//! must pass the same parity gate; only speed differs.

#[cfg(target_arch = "aarch64")]
pub mod neon;
#[cfg(target_arch = "aarch64")]
pub mod neon_i8;
pub mod scalar;

/// Which implementation of the convolution runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    /// Plain loops: the reference every faster rung is checked against.
    Scalar,
    /// Register-blocked NEON f32 micro-kernel (the Mac and the Orin).
    #[cfg(target_arch = "aarch64")]
    NeonF32,
    /// int8 weights and activations through `SDOT` (FEAT_DotProd: the M4 and the Orin's A78AE).
    /// Convs with no int8 weights (the one-channel stem) run on `NeonF32`.
    #[cfg(target_arch = "aarch64")]
    I8Sdot,
}

impl Rung {
    #[cfg(target_arch = "aarch64")]
    pub const ALL: &'static [Rung] = &[Rung::Scalar, Rung::NeonF32, Rung::I8Sdot];
    #[cfg(not(target_arch = "aarch64"))]
    pub const ALL: &'static [Rung] = &[Rung::Scalar];

    pub fn name(&self) -> &'static str {
        match self {
            Rung::Scalar => "scalar-f32",
            #[cfg(target_arch = "aarch64")]
            Rung::NeonF32 => "neon-f32",
            #[cfg(target_arch = "aarch64")]
            Rung::I8Sdot => "neon-i8-sdot",
        }
    }

    pub fn from_name(name: &str) -> Option<Rung> {
        Rung::ALL.iter().copied().find(|r| r.name() == name)
    }

    /// Whether this CPU can run the rung: `SDOT` is an extension, not part of baseline aarch64.
    pub fn available(&self) -> bool {
        match self {
            Rung::Scalar => true,
            #[cfg(target_arch = "aarch64")]
            Rung::NeonF32 => true,
            #[cfg(target_arch = "aarch64")]
            Rung::I8Sdot => std::arch::is_aarch64_feature_detected!("dotprod"),
        }
    }

    /// The fastest rung this CPU runs.
    pub fn fastest() -> Rung {
        Rung::ALL
            .iter()
            .rev()
            .copied()
            .find(Rung::available)
            .unwrap_or(Rung::Scalar)
    }
}

/// One convolution. `src`/`dst` are halo-layout activations `[(h + 2) * (w + 2) * c]`; `w` is
/// packed `[cout/8][k*k][cin][8]`.
pub struct ConvArgs<'a> {
    pub src: &'a [f32],
    pub hi: usize,
    pub wi: usize,
    pub cin: usize,
    pub ho: usize,
    pub wo: usize,
    pub cout: usize,
    pub k: usize,
    pub stride: usize,
    pub pad: usize,
    pub w: &'a [f32],
    pub b: &'a [f32],
    pub relu: bool,
    pub accum: bool,
}

pub fn conv2d(rung: Rung, a: &ConvArgs, dst: &mut [f32]) {
    match rung {
        Rung::Scalar => scalar::conv2d(a, dst),
        #[cfg(target_arch = "aarch64")]
        Rung::NeonF32 | Rung::I8Sdot => neon::conv2d(a, dst),
    }
}

/// One depthwise convolution (cin == cout == c), halo layouts as for `ConvArgs`; `w` is packed
/// `[k*k][c]`.
pub struct DwArgs<'a> {
    pub src: &'a [f32],
    pub wi: usize,
    pub c: usize,
    pub ho: usize,
    pub wo: usize,
    pub k: usize,
    pub stride: usize,
    pub pad: usize,
    pub w: &'a [f32],
    pub b: &'a [f32],
    pub relu: bool,
}

/// Depthwise has no reduction over channels for SDOT to exploit, so every non-scalar rung runs
/// it in NEON f32.
pub fn depthwise(rung: Rung, a: &DwArgs, dst: &mut [f32]) {
    match rung {
        Rung::Scalar => scalar::depthwise(a, dst),
        #[cfg(target_arch = "aarch64")]
        Rung::NeonF32 | Rung::I8Sdot => neon::depthwise(a, dst),
    }
}

/// Mean over every (frequency, time) position, per channel.
pub fn gap_pool(src: &[f32], h: usize, w: usize, c: usize, out: &mut [f32]) {
    let row = (w + 2) * c;
    out[..c].fill(0.0);
    for y in 0..h {
        for x in 0..w {
            let px = &src[(y + 1) * row + (x + 1) * c..][..c];
            for (o, v) in out.iter_mut().zip(px) {
                *o += v;
            }
        }
    }
    let n = (h * w) as f32;
    for o in out[..c].iter_mut() {
        *o /= n;
    }
}

/// Temporal statistics pooling (WeSpeaker TSTP) exactly as its ONNX graph computes it: unbiased
/// variance, std = sqrt(var + eps). Output `[c * h + f]`: all means, then all stds.
pub fn tstp_pool(src: &[f32], h: usize, w: usize, c: usize, eps: f32, out: &mut [f32]) {
    let row = (w + 2) * c;
    let n = w as f32;
    let (means, stds) = out.split_at_mut(h * c);
    for f in 0..h {
        for ch in 0..c {
            let at = |t: usize| src[(f + 1) * row + (t + 1) * c + ch];
            let mut sum = 0f32;
            for t in 0..w {
                sum += at(t);
            }
            let mean = sum / n;
            let mut sq = 0f32;
            for t in 0..w {
                let d = at(t) - mean;
                sq += d * d;
            }
            let var = (sq / n) * n / (n - 1.0);
            means[ch * h + f] = mean;
            stds[ch * h + f] = crate::engine::math::sqrt(var + eps);
        }
    }
}

/// `out = b + W x`, W row-major `[out][in]`.
pub fn linear(w: &[f32], b: &[f32], x: &[f32], out: &mut [f32]) {
    let n = x.len();
    for (o, (y, &bias)) in out.iter_mut().zip(b).enumerate() {
        let row = &w[o * n..(o + 1) * n];
        let mut acc = 0f32;
        for (a, v) in row.iter().zip(x) {
            acc += a * v;
        }
        *y = bias + acc;
    }
}
