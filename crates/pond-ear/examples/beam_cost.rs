//! What the beam costs per second of audio on this machine, next to the echo canceller it sits
//! beside: the stream, the noise estimate, and the per-segment steer and beam a wake scan adds.
//!
//! beam_cost [SECONDS] [SEGMENT_SECONDS]

use std::hint::black_box;
use std::time::Instant;

use pond_ear::ear::aec::{Aec, AecConfig};
use pond_ear::ear::beam::{self, Beam, NoiseCov, Stft, Weights, C};

const RATE: usize = 16_000;
const CHUNK: usize = RATE / 100;

fn noise(n: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (((s >> 16) as f32 / 32768.0) - 1.0) * 0.1
        })
        .collect()
}

fn main() {
    let args: Vec<f32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let seconds = args.first().copied().unwrap_or(60.0);
    let seg_s = args.get(1).copied().unwrap_or(3.0);
    let n = (seconds * RATE as f32) as usize;
    let (l, r) = (noise(n, 1), noise(n, 2));
    let far = noise(n, 3);
    let pct = |ns: u128, audio_s: f32| ns as f64 / (audio_s as f64 * 1e7);

    let mut b = Beam::new(Weights::delay_and_sum(1.0));
    let mut out = Vec::with_capacity(n);
    let t = Instant::now();
    for s in (0..n).step_by(CHUNK) {
        b.push(&l[s..s + CHUNK], &r[s..s + CHUNK], &mut out);
    }
    let stream = t.elapsed().as_nanos();
    black_box(&out);

    let (mut sl, mut sr) = (Stft::new(), Stft::new());
    let (mut fl, mut fr) = (vec![C::ZERO; beam::BINS], vec![C::ZERO; beam::BINS]);
    let mut nc = NoiseCov::new(beam::NOISE_MEMORY);
    let t = Instant::now();
    for s in (0..n - beam::FFT).step_by(beam::HOP) {
        sl.analyse(&l[s..s + beam::FFT], &mut fl);
        sr.analyse(&r[s..s + beam::FFT], &mut fr);
        nc.learn(&fl, &fr);
    }
    let learn = t.elapsed().as_nanos();
    let t = Instant::now();
    for s in (0..n - beam::FFT).step_by(beam::HOP) {
        black_box(s);
        nc.learn(&fl, &fr);
    }
    let learn_only = t.elapsed().as_nanos();

    let m = (seg_s * RATE as f32) as usize;
    let reps = 20;
    let t = Instant::now();
    for i in 0..reps {
        let s = (i * RATE) % (n - m);
        let (x, _) = beam::span_cov(&l[s..s + m], &r[s..s + m]);
        let tdoa = beam::tdoa_against(&x, nc.cov(), beam::MAX_LAG);
        let steer = beam::relative_transfer(&x, nc.cov(), 0.01, tdoa.map_or(0.0, |t| t.lag));
        let w = Weights::mvdr(&steer, nc.cov(), 0.01);
        black_box(beam::beamform(&l[s..s + m], &r[s..s + m], &w));
    }
    let per_segment = t.elapsed().as_nanos() / reps as u128;

    let mut aec = Aec::new(AecConfig::default());
    let mut y = Vec::with_capacity(n);
    let delay = RATE / 20;
    let mic: Vec<f32> = (0..n)
        .map(|i| {
            l[i] + if i >= delay {
                far[i - delay] * 0.3
            } else {
                0.0
            }
        })
        .collect();
    let t = Instant::now();
    for s in (0..n).step_by(CHUNK) {
        // The far end goes in as the tap delivers it, just ahead of the microphone.
        aec.play(s, &far[s..s + CHUNK]);
        aec.process(&mic[s..s + CHUNK], &mut y);
    }
    let aec_ns = t.elapsed().as_nanos();
    black_box(&y);

    println!("{seconds} s of stereo audio, one core:");
    println!(
        "  stream beam (2 FFT in, 1 out per 16 ms)  {:6.3} %",
        pct(stream, seconds)
    );
    println!(
        "  noise covariance, STFT + update           {:6.3} %  (update alone {:.3} %)",
        pct(learn, seconds),
        pct(learn_only, seconds)
    );
    println!(
        "  steer + MVDR-RTF beam of a {seg_s} s segment  {:6.2} ms  ({:.3} % of the segment's own audio)",
        per_segment as f64 / 1e6,
        pct(per_segment, seg_s)
    );
    println!(
        "  echo canceller, far end playing           {:6.3} %  (x2 for one per microphone)",
        pct(aec_ns, seconds)
    );
    println!(
        "  beam state {} bytes, noise covariance {} bytes",
        b.bytes(),
        nc.bytes()
    );
}
