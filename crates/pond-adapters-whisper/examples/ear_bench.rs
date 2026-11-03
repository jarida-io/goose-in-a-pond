//! Replays a microphone recording through the room's front end as serve runs it (echo canceller,
//! Silero segments, Whisper on each segment's opening for the wake phrase) and writes one JSON
//! line per segment, plus one per clip window when a bench manifest is given. Scoring and the
//! recording protocol are in `scripts/voice-bench/`.
//!
//! ear_bench --model ggml.bin --silero silero_vad.onnx --wake "hey goose" [--variant TEXT]...
//!   [--prompt TEXT] [--gain-db DB] [--segment-max-ms MS] [--temp-step STEP]
//!   [--manifest manifest.json] --out results.jsonl RECORDING.wav

use std::io::Write;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use pond_adapters_whisper::WhisperRsInput;
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
    while let Some(k) = a.next() {
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
            "--manifest" => manifest = Some(PathBuf::from(v()?)),
            "--out" => out = Some(PathBuf::from(v()?)),
            _ if !k.starts_with("--") => input = Some(PathBuf::from(k)),
            _ => bail!("unknown argument {k}"),
        }
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

fn main() -> Result<()> {
    let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Info));
    let a = args()?;
    let bytes = std::fs::read(&a.input).with_context(|| a.input.display().to_string())?;
    let (rate, mut mic) = pond_ear::wav::decode_wav(&bytes).map_err(|e| anyhow!(e))?;
    if rate as usize != RATE {
        mic = pond_ear::dsp::resample_to_16k(&mic, rate);
    }
    let gain = 10f32.powf(a.gain_db / 20.0);
    if a.gain_db != 0.0 {
        // The sound server's software gain saturates at full scale; so does this.
        mic.iter_mut()
            .for_each(|v| *v = (*v * gain).clamp(-1.0, 1.0));
    }
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
    // The canceller passes the microphone through 16 ms late while nothing plays.
    let delay = pond_ear::ear::aec::LATENCY;

    let mut out = std::io::BufWriter::new(std::fs::File::create(&a.out)?);
    let mut events = Vec::new();
    let mut whole: Vec<f32> = Vec::with_capacity(mic.len());
    let mut buf = Vec::new();
    let started = Instant::now();
    let mut segments = 0usize;
    for chunk in mic.chunks(RATE / 10) {
        events.clear();
        let before_len = seg.front().samples();
        seg.push_into(chunk, &mut events);
        seg.audio(before_len, seg.front().samples(), &mut buf);
        whole.extend_from_slice(&buf);
        for ev in events.drain(..) {
            let Event::Segment { start, end, .. } = ev else {
                continue;
            };
            let (s, e) = (start, end);
            let pcm = &whole[s.min(whole.len())..e.min(whole.len())];
            let scan = pcm.len().min(WAKE_SCAN_SECONDS * RATE);
            let (opening, ms_opening) = opening_of(&pcm[..scan])?;
            let wake = pond_voice::text::heard_wake(&opening, &triggers);
            let (full, ms_full) = if scan < pcm.len() {
                transcribe(pcm)?
            } else {
                (opening.clone(), 0)
            };
            let t = |i: usize| (i as f64 - delay as f64) / RATE as f64;
            writeln!(
                out,
                "{}",
                json!({"kind": "segment", "start": t(s), "end": t(e), "opening": opening,
                       "transcript": full, "wake": wake, "ms_opening": ms_opening,
                       "ms_full": ms_full})
            )?;
            segments += 1;
        }
    }
    let replay_s = started.elapsed().as_secs_f64();

    if let Some(path) = &a.manifest {
        let m: Value = serde_json::from_slice(&std::fs::read(path)?)?;
        // Chirps are found in the recording itself: program time maps to recording time linearly.
        let (first, first_match) = find_chirp(&mic, 0, 15 * RATE).context("no first chirp")?;
        let end_prog = m["end_chirp"].as_f64().context("end_chirp")?;
        let first_prog = m["first_chirp"].as_f64().context("first_chirp")?;
        let guess = first as f64 / RATE as f64 + end_prog - first_prog;
        let (last, last_match) = find_chirp(
            &mic,
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
            let idx = |t: f64| ((t * RATE as f64) as isize + delay as isize).max(0) as usize;
            let pcm = &whole[idx(s).min(whole.len())..idx(e).min(whole.len())];
            let (text, ms) = transcribe(pcm)?;
            writeln!(
                out,
                "{}",
                json!({"kind": "window", "id": clip["id"], "start": s, "end": e,
                       "transcript": text, "ms": ms})
            )?;
        }
    }
    let mb = |m: Option<pond_adapters_ear::ProcessMemory>| m.map(|m| m.footprint as f64 / 1e6);
    writeln!(
        out,
        "{}",
        json!({"kind": "run", "input": a.input, "model": a.model, "prompt": a.prompt,
               "gain_db": a.gain_db, "segments": segments,
               "segment_max_ms": a.segment_max_ms, "temp_step": a.temp_step,
               "audio_s": mic.len() as f64 / RATE as f64, "replay_s": replay_s,
               "load_ms": load_ms, "footprint_mb_before": mb(before),
               "footprint_mb_after_load": mb(after), "footprint_mb_end": mb(pond_adapters_ear::process_memory()),
               "triggers": triggers, "parts": seg.parts().iter().map(|p| json!({"role": p.role, "name": p.name, "bytes": p.bytes.total(), "ns": p.ns})).collect::<Vec<_>>()})
    )?;
    out.flush()?;
    eprintln!(
        "{}: {segments} segments, {replay_s:.1} s",
        a.input.display()
    );
    Ok(())
}
