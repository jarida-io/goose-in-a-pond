//! The front end's window, twiddle and mel tables, computed once here in f64 and rounded once
//! (torchaudio's f32 tables to within an ulp), so the engine never needs libm at run time.

use std::f64::consts::PI;
use std::fmt::Write as _;

const FRAME: usize = 400;
const NFFT: usize = 512;
// The complex FFT's size after even/odd packing.
const HALF: usize = NFFT / 2;
const NUM_BINS: usize = 80;

fn mel(f: f64) -> f64 {
    1127.0 * (1.0 + f / 700.0).ln()
}

fn array(out: &mut String, name: &str, ty: &str, v: &[String]) {
    writeln!(out, "pub static {name}: [{ty}; {}] = [", v.len()).unwrap();
    for chunk in v.chunks(8) {
        writeln!(out, "    {},", chunk.join(", ")).unwrap();
    }
    writeln!(out, "];").unwrap();
}

fn f32s(v: impl Iterator<Item = f64>) -> Vec<String> {
    v.map(|x| format!("{:?}f32", x as f32)).collect()
}

fn f64s(v: impl Iterator<Item = f64>) -> Vec<String> {
    v.map(|x| format!("{:?}f64", x)).collect()
}

fn main() {
    let mut out = String::new();
    let a = 2.0 * PI / (FRAME - 1) as f64;

    array(
        &mut out,
        "HAMMING",
        "f32",
        &f32s((0..FRAME).map(|i| 0.54 - 0.46 * (a * i as f64).cos())),
    );

    // Twiddles for the 256-point complex FFT: W256^k = exp(-2 pi i k / 256), k < 128.
    array(
        &mut out,
        "TW256_RE",
        "f64",
        &f64s((0..HALF / 2).map(|k| (-2.0 * PI * k as f64 / HALF as f64).cos())),
    );
    array(
        &mut out,
        "TW256_IM",
        "f64",
        &f64s((0..HALF / 2).map(|k| (-2.0 * PI * k as f64 / HALF as f64).sin())),
    );
    // Real-split twiddles: W512^k, k < 256.
    array(
        &mut out,
        "TW512_RE",
        "f64",
        &f64s((0..HALF).map(|k| (-2.0 * PI * k as f64 / NFFT as f64).cos())),
    );
    array(
        &mut out,
        "TW512_IM",
        "f64",
        &f64s((0..HALF).map(|k| (-2.0 * PI * k as f64 / NFFT as f64).sin())),
    );

    let bits = HALF.trailing_zeros();
    let rev: Vec<String> = (0..HALF)
        .map(|i| format!("{}", (i as u32).reverse_bits() >> (32 - bits)))
        .collect();
    array(&mut out, "BITREV256", "u16", &rev);

    // Kaldi mel banks (torchaudio get_mel_banks, vtln off), sparse: each bin is one contiguous run.
    let (lo, hi) = (mel(20.0), mel(8000.0));
    let delta = (hi - lo) / (NUM_BINS + 1) as f64;
    let (mut starts, mut lens, mut weights) = (Vec::new(), Vec::new(), Vec::new());
    for b in 0..NUM_BINS {
        let (left, center, right) = (
            lo + b as f64 * delta,
            lo + (b + 1) as f64 * delta,
            lo + (b + 2) as f64 * delta,
        );
        let w: Vec<(usize, f64)> = (0..HALF)
            .map(|k| {
                let m = mel(k as f64 * 16000.0 / NFFT as f64);
                (
                    k,
                    ((m - left) / (center - left))
                        .min((right - m) / (right - center))
                        .max(0.0),
                )
            })
            .filter(|(_, w)| *w > 0.0)
            .collect();
        starts.push(format!("{}", w[0].0));
        lens.push(format!("{}", w.len()));
        assert!(
            w.windows(2).all(|p| p[1].0 == p[0].0 + 1),
            "mel bin {b} is not contiguous"
        );
        weights.extend(w.iter().map(|(_, x)| format!("{:?}f32", *x as f32)));
    }
    array(&mut out, "MEL_START", "u16", &starts);
    array(&mut out, "MEL_LEN", "u16", &lens);
    array(&mut out, "MEL_W", "f32", &weights);

    let dst = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("tables.rs");
    std::fs::write(dst, out).unwrap();
    println!("cargo:rerun-if-changed=build.rs");
}
