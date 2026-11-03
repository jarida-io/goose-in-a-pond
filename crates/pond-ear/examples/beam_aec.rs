//! Where the echo canceller goes beside a beam, measured on a stereo recording of the pond's own
//! speaker: the canceller on the mono mix, beam then canceller, or a canceller per microphone then
//! the beam. The beam's steer switches once mid-reply, as a new talker's wake would switch it.
//!
//! beam_aec RECORDING.wav FAR.wav [--far-chirp S] [--lag L] [--switch S]
//!
//! FAR is the program the speaker played; its sync chirp (at --far-chirp seconds, default 1) and
//! the recording's are matched to stamp the far end on the microphone's clock.

use std::time::Instant;

use pond_ear::dsp::Resampler;
use pond_ear::ear::aec::{self, Aec, AecConfig};
use pond_ear::ear::beam::{self, Beam, Weights};

const RATE: usize = 16_000;
/// The far end is stamped this far ahead of where the chirps say it arrives, inside the
/// canceller's delay search.
const LEAD: usize = RATE / 10;
/// Far-end audio handed over ahead of the microphone: under the canceller's 4 s ring.
const FAR_AHEAD: usize = RATE;
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

fn find_chirp(x: &[f32], from: usize, to: usize) -> Option<usize> {
    let c = chirp();
    let to = to.min(x.len().saturating_sub(c.len()));
    let energy: f32 = c.iter().map(|v| v * v).sum();
    let mut best = (0f32, None);
    for lag in from..to {
        let w = &x[lag..lag + c.len()];
        let dot: f32 = w.iter().zip(&c).map(|(a, b)| a * b).sum();
        let e: f32 = w.iter().map(|v| v * v).sum();
        let s = dot / (e * energy).sqrt().max(1e-9);
        if s > best.0 {
            best = (s, Some(lag));
        }
    }
    best.1.filter(|_| best.0 > 0.1)
}

fn read(path: &str) -> (u32, usize, Vec<f32>) {
    let b = std::fs::read(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    pond_ear::wav::decode_wav_channels(&b).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn opt(args: &[String], name: &str, default: f32) -> f32 {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map_or(default, |v| v.parse().expect(name))
}

/// The far end, fed to every canceller just ahead of the microphone.
struct Far {
    pcm: Vec<f32>,
    at: isize,
    fed: usize,
}

impl Far {
    fn feed(&mut self, mic_end: usize, shift: usize, aecs: &mut [&mut Aec]) {
        let want = (mic_end + FAR_AHEAD) as isize - self.at;
        while (self.fed as isize) < want && self.fed < self.pcm.len() {
            let n = 1_600.min(self.pcm.len() - self.fed);
            let at = self.at + self.fed as isize + shift as isize;
            if at >= 0 {
                for a in aecs.iter_mut() {
                    a.play(at as usize, &self.pcm[self.fed..self.fed + n]);
                }
            }
            self.fed += n;
        }
    }
}

struct Chain {
    name: &'static str,
    out: Vec<f32>,
    /// Samples the output trails the microphone by.
    delay: usize,
    ns: u64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (rec_path, far_path) = (&args[0], &args[1]);
    let far_chirp = opt(&args, "--far-chirp", 1.0);
    let lag = opt(&args, "--lag", 2.0);
    let switch_s = opt(&args, "--switch", 30.0);

    let (rate, ch, x) = read(rec_path);
    assert!(
        rate as usize == RATE && ch == 2,
        "a 16 kHz stereo recording"
    );
    let left: Vec<f32> = x.iter().step_by(2).copied().collect();
    let right: Vec<f32> = x.iter().skip(1).step_by(2).copied().collect();
    let mono: Vec<f32> = left
        .iter()
        .zip(&right)
        .map(|(a, b)| (a + b) * 0.5)
        .collect();
    let (frate, fch, fx) = read(far_path);
    let fmono: Vec<f32> = fx
        .chunks_exact(fch)
        .map(|f| f.iter().sum::<f32>() / fch as f32)
        .collect();
    let mut rs = Resampler::new(frate, RATE as u32);
    let mut far16 = Vec::new();
    rs.process(&fmono, &mut far16);
    rs.process(&vec![0.0; rs.latency() * 4], &mut far16);
    let skip = rs.latency() * RATE / frate as usize;
    far16.drain(..skip.min(far16.len()));

    let fc = (far_chirp * RATE as f32) as usize;
    let far_at = find_chirp(&far16, fc.saturating_sub(RATE / 2), fc + RATE / 2).expect("far chirp");
    let rec_at = find_chirp(&mono, 0, 15 * RATE).expect("recording chirp");
    let at = rec_at as isize - far_at as isize - LEAD as isize;
    let switch = (switch_s * RATE as f32) as usize + rec_at;

    let (w1, w2) = (Weights::delay_and_sum(lag), Weights::delay_and_sum(-lag));
    let cfg = AecConfig::default();
    let n = mono.len();
    let mut chains = Vec::new();

    {
        let mut a = Aec::new(cfg);
        let mut far = Far {
            pcm: far16.clone(),
            at,
            fed: 0,
        };
        let (mut out, t) = (Vec::with_capacity(n), Instant::now());
        for s in (0..n).step_by(CHUNK) {
            let e = (s + CHUNK).min(n);
            far.feed(e, 0, &mut [&mut a]);
            a.process(&mono[s..e], &mut out);
        }
        chains.push(Chain {
            name: "mono -> aec",
            out,
            delay: aec::LATENCY,
            ns: t.elapsed().as_nanos() as u64,
        });
    }
    {
        let mut a = Aec::new(cfg);
        let mut b = Beam::new(w1.clone());
        let mut far = Far {
            pcm: far16.clone(),
            at,
            fed: 0,
        };
        let (mut out, mut y, t) = (Vec::with_capacity(n), Vec::new(), Instant::now());
        for s in (0..n).step_by(CHUNK) {
            let e = (s + CHUNK).min(n);
            if s <= switch && switch < e {
                b.set_weights(w2.clone());
            }
            y.clear();
            b.push(&left[s..e], &right[s..e], &mut y);
            far.feed(e, beam::LATENCY, &mut [&mut a]);
            a.process(&y, &mut out);
        }
        chains.push(Chain {
            name: "beam -> aec",
            out,
            delay: aec::LATENCY + beam::LATENCY,
            ns: t.elapsed().as_nanos() as u64,
        });
    }
    {
        let (mut al, mut ar) = (Aec::new(cfg), Aec::new(cfg));
        let mut b = Beam::new(w1.clone());
        let mut far = Far {
            pcm: far16.clone(),
            at,
            fed: 0,
        };
        let (mut cl, mut cr, mut out, t) = (
            Vec::new(),
            Vec::new(),
            Vec::with_capacity(n),
            Instant::now(),
        );
        for s in (0..n).step_by(CHUNK) {
            let e = (s + CHUNK).min(n);
            if s <= switch && switch < e {
                b.set_weights(w2.clone());
            }
            far.feed(e, 0, &mut [&mut al, &mut ar]);
            cl.clear();
            cr.clear();
            al.process(&left[s..e], &mut cl);
            ar.process(&right[s..e], &mut cr);
            b.push(&cl, &cr, &mut out);
        }
        chains.push(Chain {
            name: "aec x2 -> beam",
            out,
            delay: aec::LATENCY + beam::LATENCY,
            ns: t.elapsed().as_nanos() as u64,
        });
    }

    let p = |x: &[f32]| x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64;
    let db = |a: f64, b: f64| 10.0 * (a.max(1e-20) / b.max(1e-20)).log10();
    let rel = |s: f64| rec_at + (s * RATE as f64) as usize;
    let windows = [
        ("0.5-3 s", 0.5, 3.0),
        ("3-10 s", 3.0, 10.0),
        ("20-30 s", 20.0, 30.0),
        ("after switch 0-3 s", switch_s as f64, switch_s as f64 + 3.0),
        (
            "after switch 3-10 s",
            switch_s as f64 + 3.0,
            switch_s as f64 + 10.0,
        ),
        ("50-58 s", 50.0, 58.0),
    ];
    println!(
        "chirp at {:.3} s; far end stamped {:.0} ms ahead; switch to the other steer at {:.1} s; lag {lag}",
        rec_at as f64 / RATE as f64,
        LEAD as f64 * 1e3 / RATE as f64,
        switch_s
    );
    print!("{:16}", "residual, dB vs mono mic");
    for w in &windows {
        print!(" {:>20}", w.0);
    }
    println!(" {:>10}", "cpu %");
    let audio_s = n as f64 / RATE as f64;
    for c in &chains {
        print!("{:16}", c.name);
        for &(_, a, b) in &windows {
            let (s, e) = (rel(a), rel(b).min(n));
            let mic = p(&mono[s..e]);
            let (os, oe) = (
                (s + c.delay).min(c.out.len()),
                (e + c.delay).min(c.out.len()),
            );
            print!(" {:>20.1}", db(p(&c.out[os..oe]), mic));
        }
        println!(" {:>10.3}", c.ns as f64 / (audio_s * 1e7));
    }
}
