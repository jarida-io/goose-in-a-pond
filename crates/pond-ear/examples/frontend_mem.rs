//! What a front end over every connected microphone adds to today's one-microphone ear, measured
//! by the OS in a fresh process: an echo canceller per extra microphone, each device's resampler,
//! the multi-channel ring a segment's beam reads, the stream beam and the noise covariance.
//!
//! frontend_mem [MICS] [RING_SECONDS]

use std::hint::black_box;

use pond_ear::dsp::Resampler;
use pond_ear::ear::aec::{Aec, AecConfig};
use pond_ear::ear::beam::{self, Beam, CovN, Stft, Weights, C};
use pond_ear::ear::mem;

const RATE: usize = 16_000;

fn main() {
    let args: Vec<f32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let mics = args.first().map_or(3, |&m| m as usize);
    let ring_s = args.get(1).copied().unwrap_or(10.0);
    mem::release();
    let before = mem::now().unwrap_or_default();

    let mut aecs: Vec<Aec> = (1..mics).map(|_| Aec::new(AecConfig::default())).collect();
    let aec_bytes: usize = aecs.iter().map(|a| a.bytes()).sum();
    let resamplers = [
        Resampler::new(32_000, 16_000),
        Resampler::new(44_100, 16_000),
    ];
    let rs_bytes: usize = resamplers.iter().map(|r| r.bytes()).sum();
    let mut ring = vec![0f32; mics * (ring_s * RATE as f32) as usize];
    ring.iter_mut()
        .enumerate()
        .for_each(|(i, v)| *v = (i % 7) as f32);
    let mut stream = Beam::new(Weights::mean());
    let extra_stft: Vec<Stft> = (2..mics).map(|_| Stft::new()).collect();
    let mut extra_spec = vec![vec![C::ZERO; beam::BINS]; mics.saturating_sub(2)];
    let mut noise = vec![CovN::identity(mics); beam::BINS];
    let mut weights = vec![vec![C::ONE; mics]; beam::BINS];
    let mut out = Vec::with_capacity(RATE);
    let x = vec![0.01f32; RATE];
    stream.push(&x, &x, &mut out);
    for a in aecs.iter_mut() {
        a.play(0, &x);
        let mut y = Vec::new();
        a.process(&x, &mut y);
    }
    extra_spec.iter_mut().for_each(|s| s.fill(C::ONE));
    noise.iter_mut().for_each(|c| c.m.fill(C::ONE));
    weights.iter_mut().for_each(|w| w.fill(C::ONE));

    mem::release();
    let after = mem::now().unwrap_or_default();
    let computed = aec_bytes
        + rs_bytes
        + ring.len() * 4
        + stream.bytes()
        + extra_stft.iter().map(|s| s.bytes()).sum::<usize>()
        + extra_spec.len() * beam::BINS * 8
        + noise.len() * mics * mics * 8
        + weights.len() * mics * 8;
    let mb = |b: f64| b / 1e6;
    println!("{mics} microphones, {ring_s} s ring:");
    println!(
        "  echo cancellers beyond the first ({})  {:6.2} MB",
        mics - 1,
        mb(aec_bytes as f64)
    );
    println!(
        "  multi-channel ring                     {:6.2} MB",
        mb((ring.len() * 4) as f64)
    );
    println!(
        "  stream beam, STFTs, noise, weights     {:6.2} MB",
        mb((computed - aec_bytes - ring.len() * 4 - rs_bytes) as f64)
    );
    println!(
        "  resamplers (32 and 44.1 kHz to 16)     {:6.2} MB",
        mb(rs_bytes as f64)
    );
    println!(
        "  computed total                         {:6.2} MB",
        mb(computed as f64)
    );
    println!(
        "  the OS: footprint grew {:.2} MB, resident {:.2} MB",
        mb(after.footprint as f64 - before.footprint as f64),
        mb(after.resident as f64 - before.resident as f64)
    );
    black_box((
        &aecs,
        &ring,
        &stream,
        &extra_stft,
        &noise,
        &weights,
        &resamplers,
        &out,
    ));
}
