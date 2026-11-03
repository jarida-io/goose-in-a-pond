//! Replays a microphone recording through the room's front end as serve runs it (echo canceller,
//! Silero segments, Whisper on each segment's opening for the wake phrase) and writes one JSON
//! line per segment, plus one per clip window when a bench manifest is given. Scoring and the
//! recording protocol are in `scripts/voice-bench/`.
//!
//! ear_bench --model ggml.bin --silero silero_vad.onnx --wake "hey goose" [--variant TEXT]...
//!   [--prompt TEXT] [--gain-db DB] [--segment-max-ms MS] [--temp-step STEP]
//!   [--beam mono|left|right|das|mvdr] [--steer ff|rtf] [--noise none|sil|pre] [--loading EPS]
//!   [--pre-ms MS] [--max-lag SAMPLES] [--gate RATIO] [--min-strength S] [--hold] [--reseg]
//!   [--manifest manifest.json] --out results.jsonl RECORDING.wav
//!
//! A `--beam` other than mono reads a stereo recording. Each segment and clip window is then
//! steered on its own audio (GCC-PHAT, less the noise's cross-spectrum when there is a noise
//! estimate) and beamformed before Whisper; `--hold` also feeds the ear the beam of the last wake,
//! as a turn would hold it, and `--reseg` runs Silero again on each beamformed segment and scans
//! each piece it finds.

use std::io::Write;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use pond_adapters_whisper::WhisperRsInput;
use pond_ear::ear::beam::{self, Beam, Cov2, NoiseCov, Stft, Tdoa, Weights, C};
use pond_ear::ear::WakeConfig;
use pond_ear::{Ear, Event, Household};
use serde_json::{json, Value};

const RATE: usize = 16_000;
/// What serve transcribes of a segment to look for the wake phrase.
const WAKE_SCAN_SECONDS: usize = pond_adapters_ear::WAKE_SCAN_SECONDS;
/// The bench programs' sync chirp: 0.4 s, 500 to 4000 Hz, Hann-windowed.
const CHIRP_SECONDS: f64 = 0.4;
/// Clip windows reach this far either side of the clip as played.
const WINDOW_PAD: (f64, f64) = (0.25, 0.6);
/// Audio before a span that the beam runs over first, so the span's first frame is a full one.
const BEAM_CONTEXT: usize = beam::FFT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Mono,
    Left,
    Right,
    Das,
    Mvdr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Steer {
    FreeField,
    Rtf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NoiseFrom {
    /// No noise estimate: steer on the loudest source, MVDR against white noise.
    Nothing,
    /// Frames the ear's Silero scores under its off threshold.
    Silero,
    /// Every frame in the stretch just before the span, speech or not.
    Before,
}

#[derive(Clone, Copy, Debug)]
struct BeamArgs {
    kind: Kind,
    steer: Steer,
    noise: NoiseFrom,
    loading: f32,
    pre_ms: u32,
    /// The pair's aperture in samples: no source can sit further off broadside.
    max_lag: f32,
    /// Bins this far over the noise's power steer the beam.
    gate: f32,
    /// A steer weaker than this is not trusted: delay-and-sum toward it instead of MVDR.
    min_strength: f32,
    hold: bool,
    reseg: bool,
}

struct Args {
    model: PathBuf,
    silero: PathBuf,
    wake: String,
    variants: Vec<String>,
    prompt: Option<String>,
    gain_db: f32,
    /// In place of the ear's segment cap; 30000 is the cap before it had its own.
    segment_max_ms: Option<u32>,
    /// Whisper's retry temperature step for a segment's opening, in place of serve's 0.
    temp_step: Option<f32>,
    beam: BeamArgs,
    manifest: Option<PathBuf>,
    out: PathBuf,
    input: PathBuf,
}

fn args() -> Result<Args> {
    let mut a = std::env::args().skip(1);
    let (mut model, mut silero, mut out, mut input, mut manifest) = (None, None, None, None, None);
    let mut wake = "hey goose".to_string();
    let (mut variants, mut prompt, mut gain_db) = (Vec::new(), None, 0.0f32);
    let mut segment_max_ms = None;
    let mut temp_step = None;
    let mut beam = BeamArgs {
        kind: Kind::Mono,
        steer: Steer::FreeField,
        noise: NoiseFrom::Nothing,
        loading: 0.01,
        pre_ms: 1_500,
        max_lag: beam::MAX_LAG,
        gate: 2.0,
        min_strength: 0.0,
        hold: false,
        reseg: false,
    };
    while let Some(k) = a.next() {
        if k == "--hold" || k == "--reseg" {
            beam.hold |= k == "--hold";
            beam.reseg |= k == "--reseg";
            continue;
        }
        let mut v = || a.next().ok_or_else(|| anyhow!("{k} needs a value"));
        match k.as_str() {
            "--model" => model = Some(PathBuf::from(v()?)),
            "--silero" => silero = Some(PathBuf::from(v()?)),
            "--wake" => wake = v()?,
            "--variant" => variants.push(v()?),
            "--prompt" => prompt = Some(v()?).filter(|p| !p.is_empty()),
            "--gain-db" => gain_db = v()?.parse()?,
            "--segment-max-ms" => segment_max_ms = Some(v()?.parse()?),
            "--temp-step" => temp_step = Some(v()?.parse()?),
            "--beam" => {
                beam.kind = match v()?.as_str() {
                    "mono" => Kind::Mono,
                    "left" => Kind::Left,
                    "right" => Kind::Right,
                    "das" => Kind::Das,
                    "mvdr" => Kind::Mvdr,
                    other => bail!("unknown beam {other}"),
                }
            }
            "--steer" => {
                beam.steer = match v()?.as_str() {
                    "ff" => Steer::FreeField,
                    "rtf" => Steer::Rtf,
                    other => bail!("unknown steer {other}"),
                }
            }
            "--noise" => {
                beam.noise = match v()?.as_str() {
                    "none" => NoiseFrom::Nothing,
                    "sil" => NoiseFrom::Silero,
                    "pre" => NoiseFrom::Before,
                    other => bail!("unknown noise {other}"),
                }
            }
            "--loading" => beam.loading = v()?.parse()?,
            "--pre-ms" => beam.pre_ms = v()?.parse()?,
            "--max-lag" => beam.max_lag = v()?.parse()?,
            "--gate" => beam.gate = v()?.parse()?,
            "--min-strength" => beam.min_strength = v()?.parse()?,
            "--manifest" => manifest = Some(PathBuf::from(v()?)),
            "--out" => out = Some(PathBuf::from(v()?)),
            _ if !k.starts_with("--") => input = Some(PathBuf::from(k)),
            _ => bail!("unknown argument {k}"),
        }
    }
    if (beam.hold || beam.reseg) && beam.kind == Kind::Mono {
        bail!("--hold and --reseg need a --beam");
    }
    Ok(Args {
        model: model.context("--model")?,
        silero: silero.context("--silero")?,
        wake,
        variants,
        prompt,
        gain_db,
        segment_max_ms,
        temp_step,
        beam,
        manifest,
        out: out.context("--out")?,
        input: input.context("a recording")?,
    })
}

fn ear(silero: &std::path::Path) -> Result<Ear> {
    let mut cfg = pond_ear::Config::new(
        WakeConfig::hey_goose().map_err(|e| anyhow!("{e}"))?,
        pond_adapters_ear::load_vad(silero)?,
    );
    cfg.segments = true;
    Ear::new(cfg, Household::default()).map_err(|e| anyhow!("{e}"))
}

fn chirp() -> Vec<f32> {
    let n = (CHIRP_SECONDS * RATE as f64) as usize;
    let (f0, f1) = (500.0, 4000.0);
    (0..n)
        .map(|i| {
            let t = i as f64 / RATE as f64;
            let ph =
                2.0 * std::f64::consts::PI * (f0 * t + (f1 - f0) / (2.0 * CHIRP_SECONDS) * t * t);
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
            (ph.sin() * w) as f32
        })
        .collect()
}

/// Where the chirp starts in `x[from..to]`, by normalised cross-correlation, and how well it matched.
fn find_chirp(x: &[f32], from: usize, to: usize) -> Option<(usize, f32)> {
    let c = chirp();
    let to = to.min(x.len().saturating_sub(c.len()));
    let energy: f32 = c.iter().map(|v| v * v).sum();
    let mut best = (0f32, None);
    for lag in from..to {
        let w = &x[lag..lag + c.len()];
        let dot: f32 = w.iter().zip(&c).map(|(a, b)| a * b).sum();
        let e: f32 = w.iter().map(|v| v * v).sum();
        let score = dot / (e * energy).sqrt().max(1e-9);
        if score > best.0 {
            best = (score, Some(lag));
        }
    }
    best.1.filter(|_| best.0 > 0.1).map(|lag| (lag, best.0))
}

/// whisper.cpp's own account of what it allocated, on stderr.
struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Info
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("{}", r.args());
        }
    }
    fn flush(&self) {}
}

/// The recording as the bench reads it: the mono mix the ear has always had, and the two
/// channels when the beam needs them.
struct Recording {
    mono: Vec<f32>,
    left: Vec<f32>,
    right: Vec<f32>,
}

fn read_recording(bytes: &[u8], gain_db: f32, need_stereo: bool) -> Result<Recording> {
    let (rate, ch, frames) = pond_ear::wav::decode_wav_channels(bytes).map_err(|e| anyhow!(e))?;
    if need_stereo && ch != 2 {
        bail!("a beam needs a two-channel recording, this one has {ch}");
    }
    let to16 = |x: Vec<f32>| match rate as usize {
        RATE => x,
        _ => pond_ear::dsp::resample_to_16k(&x, rate),
    };
    let gain = 10f32.powf(gain_db / 20.0);
    // The sound server's software gain saturates at full scale; so does this.
    let gained = |mut x: Vec<f32>| {
        if gain_db != 0.0 {
            x.iter_mut().for_each(|v| *v = (*v * gain).clamp(-1.0, 1.0));
        }
        x
    };
    let channel = |c: usize| gained(to16(frames.iter().skip(c).step_by(ch).copied().collect()));
    let (left, right) = if ch == 2 {
        (channel(0), channel(1))
    } else {
        (Vec::new(), Vec::new())
    };
    let mono = if ch == 2 {
        left.iter()
            .zip(&right)
            .map(|(a, b)| (a + b) * 0.5)
            .collect()
    } else {
        gained(to16(
            frames
                .chunks_exact(ch)
                .map(|f| f.iter().sum::<f32>() / ch as f32)
                .collect(),
        ))
    };
    Ok(Recording { mono, left, right })
}

/// What a span was steered to.
struct Steered {
    weights: Weights,
    tdoa: Option<Tdoa>,
    noise_frames: usize,
}

fn identity_noise() -> Vec<Cov2> {
    vec![Cov2::identity(); beam::BINS]
}

/// Weights for one span: its own direction, and for MVDR the noise as configured.
fn steer(b: &BeamArgs, rec: &Recording, span: Range<usize>, learned: &NoiseCov) -> Steered {
    let n = rec.left.len();
    let span = span.start.min(n)..span.end.min(n);
    let (l, r) = (&rec.left[span.clone()], &rec.right[span.clone()]);
    let fixed = |w| Steered {
        weights: w,
        tdoa: None,
        noise_frames: 0,
    };
    match b.kind {
        Kind::Mono => return fixed(Weights::mean()),
        Kind::Left => return fixed(Weights::channel(0)),
        Kind::Right => return fixed(Weights::channel(1)),
        Kind::Das | Kind::Mvdr => {}
    }
    let noise = match b.noise {
        NoiseFrom::Nothing => None,
        NoiseFrom::Silero => learned
            .ready()
            .then(|| (learned.cov().to_vec(), learned.frames() as usize)),
        NoiseFrom::Before => {
            let pre = b.pre_ms as usize * RATE / 1000;
            let from = span.start.saturating_sub(pre);
            let (cov, frames) =
                beam::span_cov(&rec.left[from..span.start], &rec.right[from..span.start]);
            (frames as u64 >= beam::NOISE_MIN_FRAMES).then_some((cov, frames))
        }
    };
    let x = match &noise {
        Some((nc, _)) if b.gate > 0.0 => beam::span_cov_masked(l, r, nc, b.gate),
        _ => beam::span_cov(l, r).0,
    };
    let tdoa = match &noise {
        Some((nc, _)) => beam::tdoa_against(&x, nc, b.max_lag),
        None => beam::phat_peak(&x.iter().map(|c| c.lr).collect::<Vec<_>>(), b.max_lag),
    };
    let lag = tdoa.map_or(0.0, |t| t.lag);
    let weak = tdoa.is_none_or(|t| t.strength < b.min_strength);
    let noise_frames = noise.as_ref().map_or(0, |n| n.1);
    let weights = match b.kind {
        Kind::Das if tdoa.is_none() => Weights::mean(),
        Kind::Das => Weights::delay_and_sum(lag),
        Kind::Mvdr if weak => Weights::delay_and_sum(lag),
        _ => {
            let nc = noise.map_or_else(identity_noise, |n| n.0);
            let steer = match b.steer {
                Steer::FreeField => beam::free_field(lag),
                Steer::Rtf => beam::relative_transfer(&x, &nc, b.loading, lag),
            };
            Weights::mvdr(&steer, &nc, b.loading)
        }
    };
    Steered {
        weights,
        tdoa,
        noise_frames,
    }
}

/// Silero's segments of a beamformed span, as ranges of it: the room's own segment rule run again
/// on the beam, where a television the beam has turned down no longer joins speech up.
fn resegment(re: &mut Ear, pcm: &[f32]) -> Vec<Range<usize>> {
    re.reset();
    let mut events = Vec::new();
    re.push_into(pcm, &mut events);
    re.push_into(&vec![0f32; 2 * RATE], &mut events);
    let lat = pond_ear::ear::aec::LATENCY;
    events
        .iter()
        .filter_map(|e| match *e {
            Event::Segment { start, end, .. } => Some(
                start.saturating_sub(lat).min(pcm.len())..end.saturating_sub(lat).min(pcm.len()),
            ),
            _ => None,
        })
        .filter(|r| !r.is_empty())
        .collect()
}

/// `span` of the recording through `w`, with a frame of context before it.
fn beamform_span(rec: &Recording, span: Range<usize>, w: &Weights) -> Vec<f32> {
    let n = rec.left.len();
    let (s, e) = (span.start.min(n), span.end.min(n));
    let from = s.saturating_sub(BEAM_CONTEXT);
    let mut y = beam::beamform(&rec.left[from..e], &rec.right[from..e], w);
    y.drain(..s - from);
    y
}

/// The noise covariance learned as the stream goes: a frame is noise when every Silero chunk over
/// it scored under the off threshold. Hops are `beam::HOP` samples of recording time.
struct NoiseLearner {
    cov: NoiseCov,
    hops: Vec<Option<bool>>,
    next_frame: usize,
    sl: Stft,
    sr: Stft,
    l: Vec<C>,
    r: Vec<C>,
    ns: u64,
}

impl NoiseLearner {
    fn new() -> NoiseLearner {
        NoiseLearner {
            cov: NoiseCov::new(beam::NOISE_MEMORY),
            hops: Vec::new(),
            next_frame: 0,
            sl: Stft::new(),
            sr: Stft::new(),
            l: vec![C::ZERO; beam::BINS],
            r: vec![C::ZERO; beam::BINS],
            ns: 0,
        }
    }

    /// Silero's verdict on recording samples `[start, end)`.
    fn mark(&mut self, start: usize, end: usize, noise: bool) {
        let (h0, h1) = (start / beam::HOP, end.div_ceil(beam::HOP));
        if self.hops.len() < h1 {
            self.hops.resize(h1, None);
        }
        for h in &mut self.hops[h0..h1] {
            *h = Some(h.unwrap_or(true) && noise);
        }
    }

    fn learn(&mut self, rec: &Recording) {
        let t = Instant::now();
        let per = beam::FFT / beam::HOP;
        while self.next_frame + per <= self.hops.len() {
            let f = self.next_frame;
            let hops = &self.hops[f..f + per];
            if hops.iter().any(Option::is_none) {
                break;
            }
            let s = f * beam::HOP;
            if hops.iter().all(|h| *h == Some(true)) && s + beam::FFT <= rec.left.len() {
                self.sl.analyse(&rec.left[s..s + beam::FFT], &mut self.l);
                self.sr.analyse(&rec.right[s..s + beam::FFT], &mut self.r);
                self.cov.learn(&self.l, &self.r);
            }
            self.next_frame += 1;
        }
        self.ns += t.elapsed().as_nanos() as u64;
    }
}

fn steer_json(s: &Steered) -> Value {
    let wng = s.weights.white_noise_power();
    let wng_db = 10.0 * (wng.iter().sum::<f32>() / wng.len() as f32 / 0.5).log10();
    json!({"lag": s.tdoa.map(|t| t.lag), "strength": s.tdoa.map(|t| t.strength),
           "wng_db": wng_db, "noise_frames": s.noise_frames})
}

fn main() -> Result<()> {
    let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Info));
    let a = args()?;
    let b = a.beam;
    let stereo = b.kind != Kind::Mono;
    let bytes = std::fs::read(&a.input).with_context(|| a.input.display().to_string())?;
    let rec = read_recording(&bytes, a.gain_db, stereo)?;
    let mic = &rec.mono;
    let silero = a.silero.as_path();
    let triggers = pond_voice::text::wake_triggers(&a.wake, &a.variants);

    let before = pond_adapters_ear::process_memory();
    let (mic_handle, _owner) = pond_audio::spawn(
        Box::new(pond_audio::testing::ScriptedCapture::silence(0, 20)),
        pond_audio::CAPTURE_RATE_HZ,
        5_000,
        true,
    );
    let loaded = Instant::now();
    let whisper = WhisperRsInput::new(a.model.clone(), mic_handle)?;
    let load_ms = loaded.elapsed().as_millis();
    let after = pond_adapters_ear::process_memory();
    let prompt = a.prompt.as_deref();
    let accurate = pond_adapters_whisper::TranscribeOpts::accurate();
    let mut scan_opts = pond_adapters_whisper::TranscribeOpts::segment_scan();
    if let Some(step) = a.temp_step {
        scan_opts.temperature_step = step;
    }
    let run = |pcm: &[f32], opts| -> Result<(String, u128)> {
        let t = Instant::now();
        let text = whisper.transcribe_pcm_with(pcm, opts, prompt)?;
        Ok((text, t.elapsed().as_millis()))
    };
    let transcribe = |pcm: &[f32]| run(pcm, accurate);
    let opening_of = |pcm: &[f32]| run(pcm, scan_opts);

    let mut seg = ear(silero)?;
    if let Some(ms) = a.segment_max_ms {
        let mut e = seg.endpoint();
        e.segment_max_ms = ms;
        seg.set_endpoint(e);
    }
    let off = seg.endpoint().off;
    // The canceller passes the microphone through 16 ms late while nothing plays; a held beam adds
    // its own frame.
    let delay = pond_ear::ear::aec::LATENCY + if b.hold { beam::LATENCY } else { 0 };

    // The stream beam runs whenever there is a beam, so its cost is measured; the ear hears it
    // only with --hold.
    let mut stream = stereo.then(|| Beam::new(Weights::mean()));
    let mut learner = (stereo && b.noise == NoiseFrom::Silero).then(NoiseLearner::new);
    let (mut steer_ns, mut segment_beam_ns, mut window_beam_ns) = (0u64, 0u64, 0u64);
    let mut held = 0usize;
    let mut re_ear = match b.reseg && stereo {
        true => Some(ear(silero)?),
        false => None,
    };
    if let (Some(re), Some(ms)) = (re_ear.as_mut(), a.segment_max_ms) {
        let mut e = re.endpoint();
        e.segment_max_ms = ms;
        re.set_endpoint(e);
    }
    let (mut reseg_ns, mut reseg_dropped) = (0u64, 0usize);

    let mut out = std::io::BufWriter::new(std::fs::File::create(&a.out)?);
    let mut events = Vec::new();
    let mut whole: Vec<f32> = Vec::with_capacity(mic.len());
    let mut buf = Vec::new();
    let mut beamed = Vec::new();
    let started = Instant::now();
    let mut segments = 0usize;
    let step = RATE / 10;
    for at in (0..mic.len()).step_by(step) {
        let range = at..(at + step).min(mic.len());
        events.clear();
        let before_len = seg.front().samples();
        match stream.as_mut() {
            Some(beam) => {
                beamed.clear();
                beam.push(
                    &rec.left[range.clone()],
                    &rec.right[range.clone()],
                    &mut beamed,
                );
                if b.hold {
                    seg.push_into(&beamed, &mut events);
                } else {
                    seg.push_into(&mic[range.clone()], &mut events);
                }
            }
            None => seg.push_into(&mic[range.clone()], &mut events),
        }
        seg.audio(before_len, seg.front().samples(), &mut buf);
        whole.extend_from_slice(&buf);
        let vad = seg.take_vad();
        if let Some(l) = learner.as_mut() {
            for (t, p) in vad {
                let end = (t * RATE as f64).round() as usize;
                let start = end.saturating_sub(pond_ear::engine::silero::CHUNK);
                if end > delay {
                    l.mark(start.saturating_sub(delay), end - delay, p < off);
                }
            }
            l.learn(&rec);
        }
        for ev in events.drain(..) {
            let Event::Segment { start, end, .. } = ev else {
                continue;
            };
            let (s, e) = (start, end);
            let (pcm, steered): (Vec<f32>, Option<Steered>) = if stereo {
                let span = s.saturating_sub(delay)..e.saturating_sub(delay);
                let t = Instant::now();
                let st = steer(
                    &b,
                    &rec,
                    span.clone(),
                    learner.as_ref().map_or(&NoiseCov::new(1), |l| &l.cov),
                );
                steer_ns += t.elapsed().as_nanos() as u64;
                let t = Instant::now();
                let y = beamform_span(&rec, span, &st.weights);
                segment_beam_ns += t.elapsed().as_nanos() as u64;
                (y, Some(st))
            } else {
                (whole[s.min(whole.len())..e.min(whole.len())].to_vec(), None)
            };
            let pieces = match re_ear.as_mut() {
                Some(re) if steered.is_some() => {
                    let t = Instant::now();
                    let p = resegment(re, &pcm);
                    reseg_ns += t.elapsed().as_nanos() as u64;
                    reseg_dropped += p.is_empty() as usize;
                    p
                }
                _ => std::iter::once(0..pcm.len()).collect(),
            };
            for piece in pieces {
                let pcm = &pcm[piece.start.min(pcm.len())..piece.end.min(pcm.len())];
                let scan = pcm.len().min(WAKE_SCAN_SECONDS * RATE);
                let (opening, ms_opening) = opening_of(&pcm[..scan])?;
                let wake = pond_voice::text::heard_wake(&opening, &triggers);
                let (full, ms_full) = if scan < pcm.len() {
                    transcribe(pcm)?
                } else {
                    (opening.clone(), 0)
                };
                if wake && b.hold {
                    if let (Some(beam), Some(st)) = (stream.as_mut(), steered.as_ref()) {
                        beam.set_weights(st.weights.clone());
                        held += 1;
                    }
                }
                let t = |i: usize| (i as f64 - delay as f64) / RATE as f64;
                writeln!(
                    out,
                    "{}",
                    json!({"kind": "segment", "start": t(s + piece.start),
                           "end": t(s + piece.start + pcm.len()), "opening": opening,
                           "transcript": full, "wake": wake, "ms_opening": ms_opening,
                           "ms_full": ms_full, "beam": steered.as_ref().map(steer_json)})
                )?;
                segments += 1;
            }
        }
    }
    let replay_s = started.elapsed().as_secs_f64();

    if let Some(path) = &a.manifest {
        let m: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        // Chirps are found in the recording itself: program time maps to recording time linearly.
        let (first, first_match) = find_chirp(mic, 0, 15 * RATE).context("no first chirp")?;
        let end_prog = m["end_chirp"].as_f64().context("end_chirp")?;
        let first_prog = m["first_chirp"].as_f64().context("first_chirp")?;
        let guess = first as f64 / RATE as f64 + end_prog - first_prog;
        let (last, last_match) = find_chirp(
            mic,
            ((guess - 3.0) * RATE as f64).max(0.0) as usize,
            ((guess + 3.0) * RATE as f64) as usize,
        )
        .context("no end chirp")?;
        let (r0, r1) = (first as f64 / RATE as f64, last as f64 / RATE as f64);
        let scale = (r1 - r0) / (end_prog - first_prog);
        let at = |p: f64| r0 + (p - first_prog) * scale;
        writeln!(
            out,
            "{}",
            json!({"kind": "sync", "first": r0, "last": r1, "scale": scale,
                   "match": [first_match, last_match]})
        )?;
        for clip in m["clips"].as_array().context("clips")? {
            let (s, e) = (
                at(clip["start"].as_f64().unwrap_or(0.0)) - WINDOW_PAD.0,
                at(clip["end"].as_f64().unwrap_or(0.0)) + WINDOW_PAD.1,
            );
            let (pcm, steered) = if stereo {
                let sample = |t: f64| (t * RATE as f64).max(0.0) as usize;
                let span = sample(s)..sample(e);
                let t = Instant::now();
                let st = steer(
                    &b,
                    &rec,
                    span.clone(),
                    learner.as_ref().map_or(&NoiseCov::new(1), |l| &l.cov),
                );
                let y = beamform_span(&rec, span, &st.weights);
                window_beam_ns += t.elapsed().as_nanos() as u64;
                (y, Some(st))
            } else {
                let idx = |t: f64| ((t * RATE as f64) as isize + delay as isize).max(0) as usize;
                let w = &whole[idx(s).min(whole.len())..idx(e).min(whole.len())];
                (w.to_vec(), None)
            };
            let (text, ms) = transcribe(&pcm)?;
            writeln!(
                out,
                "{}",
                json!({"kind": "window", "id": clip["id"], "start": s, "end": e,
                       "transcript": text, "ms": ms, "beam": steered.as_ref().map(steer_json)})
            )?;
        }
    }
    let audio_s = mic.len() as f64 / RATE as f64;
    let beam_json = stream.as_ref().map(|s| {
        let noise = learner.as_ref();
        json!({"kind": format!("{:?}", b.kind), "steer": format!("{:?}", b.steer),
               "noise": format!("{:?}", b.noise), "loading": b.loading, "pre_ms": b.pre_ms,
               "max_lag": b.max_lag, "gate": b.gate, "min_strength": b.min_strength,
               "hold": b.hold, "held": held, "reseg": b.reseg, "reseg_ns": reseg_ns,
               "reseg_dropped": reseg_dropped,
               "stream_ns": s.ns, "stream_bytes": s.bytes(),
               "noise_ns": noise.map(|l| l.ns), "noise_frames": noise.map(|l| l.cov.frames()),
               "steer_ns": steer_ns, "segment_beam_ns": segment_beam_ns,
               "window_beam_ns": window_beam_ns,
               "cpu_pct_stream": s.ns as f64 / (audio_s * 1e7),
               "cpu_pct_turn_path": (s.ns + noise.map_or(0, |l| l.ns) + steer_ns + segment_beam_ns + reseg_ns) as f64 / (audio_s * 1e7)})
    });
    let mb = |m: Option<pond_adapters_ear::ProcessMemory>| m.map(|m| m.footprint as f64 / 1e6);
    writeln!(
        out,
        "{}",
        json!({"kind": "run", "input": a.input, "model": a.model, "prompt": a.prompt,
               "gain_db": a.gain_db, "segments": segments,
               "segment_max_ms": a.segment_max_ms, "temp_step": a.temp_step,
               "audio_s": audio_s, "replay_s": replay_s,
               "load_ms": load_ms, "footprint_mb_before": mb(before),
               "footprint_mb_after_load": mb(after), "footprint_mb_end": mb(pond_adapters_ear::process_memory()),
               "triggers": triggers, "beam": beam_json,
               "parts": seg.parts().iter().map(|p| json!({"role": p.role, "name": p.name, "bytes": p.bytes.total(), "ns": p.ns})).collect::<Vec<_>>()})
    )?;
    out.flush()?;
    eprintln!(
        "{}: {segments} segments, {replay_s:.1} s",
        a.input.display()
    );
    Ok(())
}
