//! `pond-ear measure|replay|convert|personal-cost`: what the ear costs in a fresh process, a
//! recording run through it as if it were the microphone, ONNX to `.vmw`, and what matching a
//! household's wake templates costs per segment. Usage is printed without arguments.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pond_ear::ear::{AecConfig, SpeakerConfig, WakeConfig};
use pond_ear::{locations, Config, Ear, Household, ModelFile, Rung};

const USAGE: &str = "\
pond-ear measure [--wake PATH] [--vad PATH] [--speaker PATH] [--rung NAME] [--no-aec]
pond-ear replay <in.wav> [--far PLAYED.wav] [--speak] [--wake PATH] [--vad PATH] [--speaker PATH] [--threshold T] [--rung NAME] [--chunk N] [--no-aec]
pond-ear convert <in.onnx> <out.vmw>
pond-ear personal-cost [--members N] [--templates N] [--frames N] [--runs N]
--far plays a file sample-aligned with the microphone, --speak holds the ear in Speak.
The wake model defaults to the embedded hey goose, Silero to the pond's own copy; Voice Match
loads only with --speaker.";

fn arg(args: &[String], key: &str) -> Option<String> {
    args.iter()
        .position(|a| a == key)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn fail(msg: impl std::fmt::Display) -> ExitCode {
    eprintln!("{msg}");
    ExitCode::FAILURE
}

fn config(args: &[String]) -> Result<Config, String> {
    let vad = match arg(args, "--vad") {
        Some(p) => PathBuf::from(p),
        None => locations::data_dir()
            .map(|d| locations::silero(&d))
            .filter(|p| p.exists())
            .ok_or("no Silero model in the pond's data directory; pass --vad PATH")?,
    };
    let mut wake = WakeConfig::hey_goose().map_err(|e| e.to_string())?;
    if let Some(p) = arg(args, "--wake") {
        wake.model = ModelFile::read(Path::new(&p)).map_err(|e| e.to_string())?;
    }
    let mut cfg = Config::new(wake, ModelFile::read(&vad).map_err(|e| e.to_string())?);
    if let Some(p) = arg(args, "--speaker") {
        let label = Path::new(&p)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        cfg.speaker = Some(SpeakerConfig {
            model: ModelFile::read(Path::new(&p)).map_err(|e| e.to_string())?,
            label,
            max_seconds: pond_ear::ear::machine::SPEAKER_MAX_SECONDS,
        });
    }
    if let Some(name) = arg(args, "--rung") {
        cfg.rung = Rung::from_name(&name)
            .filter(Rung::available)
            .ok_or(format!("rung {name} is not available on this CPU"))?;
    }
    if let Some(t) = arg(args, "--threshold") {
        cfg.wake.threshold = t.parse().map_err(|_| format!("bad threshold {t}"))?;
    }
    cfg.aec = (!args.iter().any(|a| a == "--no-aec")).then(AecConfig::default);
    Ok(cfg)
}

fn measure(args: &[String]) -> ExitCode {
    pond_ear::ear::mem::release();
    let before = pond_ear::ear::mem::now().unwrap_or_default();
    let cfg = match config(args) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let rung = cfg.rung;
    let ear = match Ear::new(cfg, Household::default()) {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    pond_ear::ear::mem::release();
    let after = pond_ear::ear::mem::now().unwrap_or_default();
    let parts: Vec<String> = ear
        .parts()
        .iter()
        .map(|p| {
            let b = p.bytes;
            format!(
                "{{\"role\":\"{}\",\"name\":\"{}\",\"weights\":{},\"weights_rung\":{},\"arena\":{},\"state\":{},\"macs\":{}}}",
                p.role, p.name, b.weights, b.weights_rung, b.arena, b.state, p.macs
            )
        })
        .collect();
    println!(
        "{{\"rung\":\"{}\",\"footprint_before\":{},\"footprint_after\":{},\"resident_after\":{},\"parts\":[{}]}}",
        rung.name(),
        before.footprint,
        after.footprint,
        after.resident,
        parts.join(",")
    );
    ExitCode::SUCCESS
}

fn replay(args: &[String]) -> ExitCode {
    let Some(wav) = args.get(2) else {
        return fail(USAGE);
    };
    let cfg = match config(args) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let rung = cfg.rung;
    let chunk: usize = arg(args, "--chunk")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_600);
    let mut ear = match Ear::new(cfg, Household::default()) {
        Ok(e) => e,
        Err(e) => return fail(e),
    };
    let read = |path: &str| {
        std::fs::read(path)
            .map_err(|e| format!("{path}: {e}"))
            .and_then(|b| pond_ear::wav::decode_wav(&b))
            .map(|(rate, pcm)| pond_ear::dsp::resample_to_16k(&pcm, rate))
    };
    let pcm = match read(wav) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let far = match arg(args, "--far").map(|p| read(&p)).transpose() {
        Ok(f) => f.unwrap_or_default(),
        Err(e) => return fail(e),
    };
    if args.iter().any(|a| a == "--speak") {
        ear.set_state(pond_ear::State::Speak);
    }
    let chunk = chunk.max(1);
    for (k, c) in pcm.chunks(chunk).enumerate() {
        let at = k * chunk;
        // A mixer runs ahead of the microphone: give the canceller the far end before its echo.
        if at < far.len() {
            ear.play(at, &far[at..(at + chunk).min(far.len())]);
        }
        for e in ear.push(c) {
            println!("{}", e.json());
        }
    }
    let secs = ear.audio_seconds().max(1e-9);
    let parts: Vec<String> = ear
        .parts()
        .iter()
        .map(|p| {
            format!(
                "{{\"role\":\"{}\",\"bytes\":{},\"bytes_rung\":{},\"cpu_pct\":{:.3}}}",
                p.role,
                p.bytes.total(),
                p.bytes.total_rung(),
                p.ns as f64 / 1e9 / secs * 100.0
            )
        })
        .collect();
    println!(
        "{{\"summary\":{{\"seconds\":{secs:.2},\"rung\":\"{}\",\"parts\":[{}]}}}}",
        rung.name(),
        parts.join(",")
    );
    ExitCode::SUCCESS
}

fn convert(args: &[String]) -> ExitCode {
    let (Some(src), Some(dst)) = (args.get(2), args.get(3)) else {
        return fail(USAGE);
    };
    let bytes = std::fs::read(src)
        .map_err(|e| format!("{src}: {e}"))
        .and_then(|b| pond_ear::convert::convert_onnx(&b));
    match bytes.and_then(|b| std::fs::write(dst, b).map_err(|e| format!("{dst}: {e}"))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(e),
    }
}

/// Per-segment cost of the personal wake match: a household of seeded templates and the embedded
/// references, timed on segments that miss (no reference needed) and that come near (all of them).
fn personal_cost(args: &[String]) -> ExitCode {
    use pond_ear::engine::fbank::NUM_BINS;
    use pond_ear::wake::personal::{self, Member, PersonalWake, Query, Template, Thresholds};
    let num = |key: &str, default: usize| {
        arg(args, key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    };
    let (members, templates, frames, runs) = (
        num("--members", 2),
        num("--templates", 8),
        num("--frames", 70),
        num("--runs", 300),
    );
    let seed = std::cell::Cell::new(0x9e37_79b9_7f4a_7c15u64);
    let noise = |n: usize| -> Vec<f32> {
        (0..n * NUM_BINS)
            .map(|_| {
                let mut s = seed.get();
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                seed.set(s);
                (s >> 40) as f32 / (1u64 << 24) as f32
            })
            .collect()
    };
    let household = |distance: f32| PersonalWake {
        members: (0..members)
            .map(|m| Member {
                id: format!("m{m}"),
                templates: (0..templates)
                    .map(|_| Template::from_fbank(&noise(frames)))
                    .collect(),
                thresholds: Thresholds {
                    distance,
                    margin: 0.0,
                },
            })
            .collect(),
        references: personal::references(),
    };
    let timed = |wake: &PersonalWake| -> (f64, f64) {
        let mut ms: Vec<f64> = (0..runs)
            .map(|_| {
                let q = Query::from_fbank(&noise(personal::QUERY_FRAMES));
                let t = std::time::Instant::now();
                std::hint::black_box(wake.judge(&q));
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        (ms[ms.len() / 2], ms[ms.len() * 95 / 100])
    };
    let miss = timed(&household(0.0));
    let near = timed(&household(2.0));
    let wake = household(0.0);
    println!(
        "personal wake: {members} members x {templates} templates of {frames} frames, {} references, \
         {:.2} MB held",
        wake.references.len(),
        wake.bytes() as f64 / 1e6
    );
    println!(
        "  a segment that misses: p50 {:.2} ms, p95 {:.2} ms",
        miss.0, miss.1
    );
    println!(
        "  a segment that comes near: p50 {:.2} ms, p95 {:.2} ms",
        near.0, near.1
    );
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("measure") => measure(&args),
        Some("replay") => replay(&args),
        Some("convert") => convert(&args),
        Some("personal-cost") => personal_cost(&args),
        _ => fail(USAGE),
    }
}
