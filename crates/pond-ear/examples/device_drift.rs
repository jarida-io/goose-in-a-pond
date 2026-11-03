//! Whether each device's echo canceller tracks the clock drift between two USB microphones: both
//! recordings of the pond's reply get the far end stamped at the same nominal sample, as the
//! playback tap would, and the difference of the two cancellers' bulk delays is compared with what
//! the sync chirps say.
//!
//! device_drift A.wav B.wav FAR.wav [--far-chirp S]

use pond_ear::dsp::Resampler;
use pond_ear::ear::aec::{Aec, AecConfig};

const RATE: usize = 16_000;
const LEAD: usize = RATE / 10;
const CHUNK: usize = RATE / 100;

fn chirp() -> Vec<f32> {
    let n = (0.4 * RATE as f64) as usize;
    (0..n)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            let ph = 2.0 * std::f64::consts::PI * (500.0 * t + 3500.0 / 0.8 * t * t);
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
            (ph.sin() * w) as f32
        })
        .collect()
}

/// The chirp's start in `x[from..to]`, refined to a fraction of a sample.
fn find_chirp(x: &[f32], from: usize, to: usize) -> Option<f64> {
    let c = chirp();
    let to = to.min(x.len().saturating_sub(c.len() + 1));
    let ncc = |l: usize| -> f64 {
        let w = &x[l..l + c.len()];
        let dot: f64 = w.iter().zip(&c).map(|(a, b)| (*a * *b) as f64).sum();
        let e: f64 = w.iter().map(|v| (*v as f64).powi(2)).sum();
        dot / e.sqrt().max(1e-12)
    };
    let (mut best, mut at) = (0.0, None);
    for l in from.max(1)..to {
        let v = ncc(l);
        if v > best {
            (best, at) = (v, Some(l));
        }
    }
    let l = at?;
    let (a, b, cc) = (ncc(l - 1), ncc(l), ncc(l + 1));
    let den = a - 2.0 * b + cc;
    Some(
        l as f64
            + if den.abs() > 1e-12 {
                (0.5 * (a - cc) / den).clamp(-0.5, 0.5)
            } else {
                0.0
            },
    )
}

fn mono(path: &str) -> Vec<f32> {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let (rate, x) = pond_ear::wav::decode_wav(&b).unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(rate as usize, RATE, "{path}: 16 kHz");
    x
}

/// The canceller's bulk delay, sampled once a second, in samples.
fn delays(mic: &[f32], far: &[f32], at: isize) -> Vec<Option<usize>> {
    let mut a = Aec::new(AecConfig::default());
    let (mut out, mut fed, mut got) = (Vec::new(), 0usize, Vec::new());
    for s in (0..mic.len()).step_by(CHUNK) {
        let e = (s + CHUNK).min(mic.len());
        while (fed as isize) < (e + RATE) as isize - at && fed < far.len() {
            let n = 1_600.min(far.len() - fed);
            if at + fed as isize >= 0 {
                a.play((at + fed as isize) as usize, &far[fed..fed + n]);
            }
            fed += n;
        }
        a.process(&mic[s..e], &mut out);
        if e % RATE < CHUNK {
            got.push(a.estimated_delay());
        }
    }
    got
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (a, b) = (mono(&args[0]), mono(&args[1]));
    let far_chirp: f32 = args
        .iter()
        .position(|x| x == "--far-chirp")
        .map_or(1.0, |i| args[i + 1].parse().expect("--far-chirp"));
    let fb = std::fs::read(&args[2]).expect("far");
    let (frate, fx) = pond_ear::wav::decode_wav(&fb).expect("far wav");
    let mut rs = Resampler::new(frate, RATE as u32);
    let mut far = Vec::new();
    rs.process(&fx, &mut far);
    let fc = (far_chirp * RATE as f32) as usize;
    let far_at = find_chirp(&far, fc.saturating_sub(RATE / 2), fc + RATE / 2).expect("far chirp");

    let edge = 15 * RATE;
    let (a0, a1) = (
        find_chirp(&a, 0, edge).unwrap(),
        find_chirp(&a, a.len() - edge, a.len()).unwrap(),
    );
    let (b0, b1) = (
        find_chirp(&b, 0, edge).unwrap(),
        find_chirp(&b, b.len() - edge, b.len()).unwrap(),
    );
    let ppm = ((a1 - a0) / (b1 - b0) - 1.0) * 1e6;
    println!(
        "chirps: A at {:.2} ms, B at {:.2} ms: offset {:.3} ms; over {:.1} s, A's clock runs {ppm:+.1} ppm against B's",
        a0 / 16.0,
        b0 / 16.0,
        (a0 - b0) / 16.0,
        (a1 - a0) / RATE as f64
    );
    // The same nominal stamp for both, as one playback tap stamps every device it feeds.
    let at = a0.round() as isize - far_at.round() as isize - LEAD as isize;
    let (da, db) = (delays(&a, &far, at), delays(&b, &far, at));
    let mut pts = Vec::new();
    print!("delay A-B by second (ms):");
    for (i, (x, y)) in da.iter().zip(&db).enumerate() {
        if let (Some(x), Some(y)) = (x, y) {
            let d = (*x as f64 - *y as f64) / 16.0;
            pts.push((i as f64, d));
            if i % 5 == 0 {
                print!(" {i}s:{d:+.2}");
            }
        }
    }
    println!();
    if pts.len() > 2 {
        let n = pts.len() as f64;
        let (mx, my) = (
            pts.iter().map(|p| p.0).sum::<f64>() / n,
            pts.iter().map(|p| p.1).sum::<f64>() / n,
        );
        let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
        let sxx: f64 = pts.iter().map(|p| (p.0 - mx).powi(2)).sum();
        let slope = sxy / sxx;
        println!(
            "cancellers: A-B delay {my:+.2} ms on average, drifting {:+.1} ppm ({} seconds locked); the chirps said {:+.3} ms and {ppm:+.1} ppm",
            slope * 1e3,
            pts.len(),
            (a0 - b0) / 16.0
        );
    }
}
