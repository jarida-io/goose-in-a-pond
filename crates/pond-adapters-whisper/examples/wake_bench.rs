//! Personal wake templates against Whisper's wake, offline: each voice teaches the phrase with five
//! takes as `/voice/takes` would (Whisper's word times find the phrase, the ear's fbank makes the
//! template), then every other clip of every voice is mixed into rooms (quiet, babble at two
//! levels, babble alone, near silence) and replayed through the ear, scoring four rules per segment.
//!
//! wake_bench --model ggml.bin --silero silero_vad.onnx --bench DIR [--prompt TEXT]
//!   [--takes w01,w02,w03,w04,w05] [--variant TEXT]... [--reverb] --out result.json
//! DIR holds sentences.tsv, tts/<voice>/<id>.wav, babble/*.wav and negatives/*.wav.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use pond_adapters_whisper::WhisperRsInput;
use pond_ear::ear::{Front, WakeConfig};
use pond_ear::engine::fbank::{NUM_BINS, SHIFT};
use pond_ear::wake::personal::{self, Member, PersonalWake, Query, Template};
use pond_ear::{Ear, Event, Household};
use serde_json::json;

const RATE: usize = 16_000;
const GAP_S: f32 = 2.6;
/// A take's opening as a segment would give it: Silero's 200 ms pre-roll before the phrase.
const PRE_ROLL_S: f32 = 0.2;
/// Whisper's word times are rough; the span keeps this much either side.
const SPAN_PAD_S: f32 = 0.05;

struct Args {
    model: PathBuf,
    silero: PathBuf,
    bench: PathBuf,
    prompt: Option<String>,
    takes: Vec<String>,
    variants: Vec<String>,
    reverb: bool,
    out: PathBuf,
    /// Write the non-wake references as the ear embeds them, then stop.
    write_references: Option<PathBuf>,
}

fn args() -> Result<Args> {
    let mut a = std::env::args().skip(1);
    let (mut model, mut silero, mut bench, mut out) = (None, None, None, None);
    let mut prompt = None;
    let mut takes: Vec<String> = "w01,w02,w03,w04,w05".split(',').map(String::from).collect();
    let mut variants = Vec::new();
    let mut reverb = false;
    let mut write_references = None;
    while let Some(k) = a.next() {
        let mut v = || a.next().ok_or_else(|| anyhow!("{k} needs a value"));
        match k.as_str() {
            "--model" => model = Some(PathBuf::from(v()?)),
            "--silero" => silero = Some(PathBuf::from(v()?)),
            "--bench" => bench = Some(PathBuf::from(v()?)),
            "--prompt" => prompt = Some(v()?),
            "--takes" => takes = v()?.split(',').map(String::from).collect(),
            "--variant" => variants.push(v()?),
            "--reverb" => reverb = true,
            "--out" => out = Some(PathBuf::from(v()?)),
            "--write-references" => write_references = Some(PathBuf::from(v()?)),
            _ => bail!("unknown argument {k}"),
        }
    }
    Ok(Args {
        model: model.context("--model")?,
        silero: silero.context("--silero")?,
        bench: bench.context("--bench")?,
        prompt,
        takes,
        variants,
        reverb,
        out: out.unwrap_or_default(),
        write_references,
    })
}

fn read_16k(path: &Path) -> Result<Vec<f32>> {
    let (rate, pcm) = pond_ear::wav::decode_wav(&std::fs::read(path)?).map_err(|e| anyhow!(e))?;
    if pcm.iter().any(|v| !v.is_finite()) {
        bail!("{} holds samples that are not numbers", path.display());
    }
    let mut out = Vec::new();
    let mut r = pond_ear::dsp::Resampler::new(rate, RATE as u32);
    r.process(&pcm, &mut out);
    r.process(&vec![0.0; 256], &mut out);
    Ok(out)
}

fn fbank(pcm: &[f32]) -> Vec<f32> {
    let mut front = Front::new(pcm.len() as f32 / RATE as f32 + 1.0);
    let range = front.push(pcm);
    let mut out = vec![0.0; range.len() * NUM_BINS];
    let n = front.frames(range.start, range.end, &mut out);
    out.truncate(n * NUM_BINS);
    out
}

/// Frames `[from_s, to_s)` of a clip's fbank.
fn frames_between(all: &[f32], from_s: f32, to_s: f32) -> Vec<f32> {
    let n = all.len() / NUM_BINS;
    let f = |s: f32| ((s.max(0.0) * RATE as f32) as usize / SHIFT).min(n);
    all[f(from_s) * NUM_BINS..f(to_s).max(f(from_s)) * NUM_BINS].to_vec()
}

fn active_rms(x: &[f32]) -> f32 {
    let frames: Vec<f32> = x
        .chunks(320)
        .map(|c| (c.iter().map(|v| v * v).sum::<f32>() / c.len() as f32).sqrt())
        .collect();
    let peak = frames.iter().copied().fold(0.0f32, f32::max);
    let loud: Vec<f32> = frames.into_iter().filter(|&r| r > peak * 0.03).collect();
    (loud.iter().map(|r| r * r).sum::<f32>() / loud.len().max(1) as f32).sqrt()
}

/// Seeded xorshift in [-0.5, 0.5).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 as f64 / u64::MAX as f64) as f32 - 0.5
    }
}

/// A small room: the direct sound and a 0.4 s exponentially decaying tail.
fn room(x: &[f32]) -> Vec<f32> {
    let len = RATE * 2 / 5;
    let mut rng = Rng(7);
    let tail: Vec<f32> = (0..len)
        .map(|i| rng.next() * 0.35 * (-6.9 * i as f32 / len as f32).exp())
        .collect();
    let mut y = x.to_vec();
    for (i, &v) in x.iter().enumerate() {
        if v == 0.0 {
            continue;
        }
        for (k, &h) in tail.iter().enumerate().step_by(4) {
            if let Some(o) = y.get_mut(i + k + 160) {
                *o += v * h * 2.0;
            }
        }
    }
    y
}

/// Six talkers from the babble clips, end to end, overlapped.
fn babble_bed(clips: &[Vec<f32>], len: usize) -> Vec<f32> {
    let mut bed = vec![0.0; len];
    let mut rng = Rng(11);
    for _ in 0..6 {
        let mut pos = ((rng.next() + 0.5) * RATE as f32) as usize;
        while pos < len {
            let c = &clips[((rng.next() + 0.5) * clips.len() as f32) as usize % clips.len()];
            let g = 0.1 / active_rms(c).max(1e-6);
            for (k, &v) in c.iter().enumerate() {
                if let Some(o) = bed.get_mut(pos + k) {
                    *o += v * g;
                }
            }
            pos += c.len() + ((rng.next() + 0.5) * 0.3 * RATE as f32) as usize;
        }
    }
    bed
}

struct Clip {
    voice: String,
    id: String,
    wake: bool,
    start: usize,
    end: usize,
}

/// What a rule decided on one segment.
#[derive(Clone, Copy, Default)]
struct Calls {
    whisper: bool,
    strict: bool,
    template: bool,
}

type Rule = (&'static str, fn(Calls) -> bool);
/// A segment as replayed: its span, the rules' calls, Whisper's opening, the nearest member.
type Replayed = (usize, usize, Calls, String, Option<(String, f32)>, f32);

fn rules() -> [Rule; 4] {
    [
        ("whisper", |c| c.whisper),
        ("templates", |c| c.template),
        ("either", |c| c.whisper || c.template),
        ("strict_or_template", |c| c.strict || c.template),
    ]
}

fn main() -> Result<()> {
    let a = args()?;
    let sentences: BTreeMap<String, String> =
        std::fs::read_to_string(a.bench.join("sentences.tsv"))?
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
    let voices: Vec<String> = std::fs::read_dir(a.bench.join("tts"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let triggers = pond_voice::text::wake_triggers("hey goose", &a.variants);
    let strict = vec!["hey goose".to_string()];

    let (mic, _owner) = pond_audio::spawn(
        Box::new(pond_audio::testing::ScriptedCapture::silence(0, 20)),
        pond_audio::CAPTURE_RATE_HZ,
        5_000,
        true,
    );
    let whisper = WhisperRsInput::new(a.model.clone(), mic)?;
    let prompt = a.prompt.as_deref();

    // Non-wake references, also impostors for calibration: rendered once, in voices under test.
    let mut references = Vec::new();
    let mut generic_impostors = Vec::new();
    for e in std::fs::read_dir(a.bench.join("negatives"))?.filter_map(|e| e.ok()) {
        if e.path().extension().is_none_or(|x| x != "wav") {
            continue;
        }
        let pcm = read_16k(&e.path())?;
        let f = fbank(&pcm);
        references.push(Template::from_fbank(&trim_frames(&f)));
        generic_impostors.push(Query::from_fbank(&f));
    }

    if let Some(path) = &a.write_references {
        std::fs::write(path, personal::encode_references(&references))?;
        eprintln!("{} references to {}", references.len(), path.display());
        return Ok(());
    }
    // The ear's own copy, so the bench scores what serve will.
    let references = personal::references();
    if references.is_empty() {
        bail!("the ear embeds no references; write them with --write-references");
    }

    // Enrolment: five takes per voice, as `/voice/takes` cuts them.
    let mut members = Vec::new();
    let mut enrol_report = Vec::new();
    for voice in &voices {
        let mut takes = Vec::new();
        let mut own = Vec::new();
        for id in &a.takes {
            let pcm = read_16k(&a.bench.join("tts").join(voice).join(format!("{id}.wav")))?;
            let words = whisper.transcribe_words(&pcm, prompt)?;
            let timed: Vec<(&str, f32, f32)> = words
                .iter()
                .map(|w| (w.text.as_str(), w.start, w.end))
                .collect();
            let Some((s, e)) = pond_voice::text::wake_span(&timed, &triggers) else {
                eprintln!("{voice}/{id}: no wake phrase in {words:?}");
                continue;
            };
            if !(personal::MIN_PHRASE_S..=personal::MAX_PHRASE_S).contains(&(e - s)) {
                eprintln!(
                    "{voice}/{id}: a phrase {:.2} s long is a mistimed word",
                    e - s
                );
                continue;
            }
            let all = fbank(&pcm);
            let span = frames_between(&all, s - SPAN_PAD_S, e + SPAN_PAD_S);
            let opening = frames_between(&all, s - PRE_ROLL_S, 30.0);
            takes.push((Query::from_fbank(&opening), Template::from_fbank(&span)));
            own.push(Query::from_fbank(&frames_between(&all, e + 0.1, 30.0)));
            enrol_report.push(json!({"voice": voice, "take": id, "span": [s, e]}));
        }
        let mut impostors = own;
        impostors.extend(generic_impostors.iter().cloned());
        let thresholds = personal::calibrate(&takes, &impostors, &references);
        eprintln!("{voice}: {} takes, thresholds {thresholds:?}", takes.len());
        members.push(Member {
            id: voice.clone(),
            templates: takes.into_iter().map(|(_, t)| t).collect(),
            thresholds,
        });
    }
    let household = PersonalWake {
        members,
        references,
    };

    // Rooms: every clip not used as a take, in a seeded order with gaps.
    let mut rng = Rng(42);
    let mut items: Vec<(String, String)> = voices
        .iter()
        .flat_map(|v| sentences.keys().map(move |id| (v.clone(), id.clone())))
        .filter(|(_, id)| !a.takes.contains(id))
        .collect();
    for i in (1..items.len()).rev() {
        let j = ((rng.next() + 0.5) * (i + 1) as f32) as usize % (i + 1);
        items.swap(i, j);
    }
    let mut speech = vec![0.0; RATE];
    let mut clips = Vec::new();
    for (voice, id) in &items {
        let pcm = read_16k(&a.bench.join("tts").join(voice).join(format!("{id}.wav")))?;
        let g = 0.05 / active_rms(&pcm).max(1e-6);
        let start = speech.len();
        speech.extend(pcm.iter().map(|v| v * g));
        clips.push(Clip {
            voice: voice.clone(),
            id: id.clone(),
            wake: id.starts_with('w'),
            start,
            end: speech.len(),
        });
        speech.extend(vec![0.0; (GAP_S * RATE as f32) as usize]);
    }
    if a.reverb {
        speech = room(&speech);
    }
    let babble: Vec<Vec<f32>> = std::fs::read_dir(a.bench.join("babble"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| read_16k(&e.path()).ok())
        .collect();
    let bed = babble_bed(&babble, speech.len().max(300 * RATE));
    let level = active_rms(&speech);
    let bed_level = active_rms(&bed);
    let mut quiet_noise = Rng(5);
    let mixed = |snr_db: Option<f32>, with_speech: bool, len: usize| -> Vec<f32> {
        (0..len)
            .map(|i| {
                let s = if with_speech { speech[i] } else { 0.0 };
                let b = snr_db.map_or(0.0, |snr| {
                    bed[i] * level / bed_level * 10f32.powf(-snr / 20.0)
                });
                s + b
            })
            .collect()
    };
    let mut floor: Vec<f32> = (0..300 * RATE)
        .map(|_| quiet_noise.next() * 0.0006)
        .collect();
    let rooms: Vec<(&str, Vec<f32>, bool)> = vec![
        ("quiet", mixed(None, true, speech.len()), true),
        ("babble10", mixed(Some(10.0), true, speech.len()), true),
        ("babble3", mixed(Some(3.0), true, speech.len()), true),
        ("babble_only", mixed(Some(3.0), false, 300 * RATE), false),
        ("near_silence", std::mem::take(&mut floor), false),
    ];

    let mut report = BTreeMap::new();
    let mut judge_ns = Vec::new();
    for (name, audio, has_clips) in &rooms {
        let mut cfg = pond_ear::Config::new(
            WakeConfig::hey_goose().map_err(|e| anyhow!("{e}"))?,
            pond_adapters_ear::load_vad(&a.silero)?,
        );
        cfg.segments = true;
        let mut ear = Ear::new(cfg, Household::default()).map_err(|e| anyhow!("{e}"))?;
        let delay = pond_ear::ear::aec::LATENCY;
        let mut segs: Vec<Replayed> = Vec::new();
        let mut events = Vec::new();
        let mut pcm = Vec::new();
        let mut frames = vec![0.0; 1000 * NUM_BINS];
        for chunk in audio.chunks(RATE / 10) {
            events.clear();
            ear.push_into(chunk, &mut events);
            for ev in events.drain(..) {
                let Event::Segment { start, end, .. } = ev else {
                    continue;
                };
                ear.audio(start, end, &mut pcm);
                let scan = pcm.len().min(pond_adapters_ear::WAKE_SCAN_SECONDS * RATE);
                let opening = whisper.transcribe_pcm_with(
                    &pcm[..scan],
                    pond_adapters_whisper::TranscribeOpts::segment_scan(),
                    prompt,
                )?;
                let f0 = start / SHIFT;
                let n = ear.front().frames(f0, f0 + 1000, &mut frames);
                let t = Instant::now();
                let verdict = household.judge(&Query::from_fbank(&frames[..n * NUM_BINS]));
                judge_ns.push(t.elapsed().as_nanos() as u64);
                let calls = Calls {
                    whisper: pond_voice::text::heard_wake(&opening, &triggers),
                    strict: pond_voice::text::heard_wake(&opening, &strict),
                    template: verdict.accepted.is_some(),
                };
                let s = start.saturating_sub(delay);
                let e = end.saturating_sub(delay);
                segs.push((
                    s,
                    e,
                    calls,
                    opening,
                    verdict.nearest.clone(),
                    verdict.margin,
                ));
            }
        }
        let mut by_rule = BTreeMap::new();
        for (rule, fires) in rules() {
            let mut recall: BTreeMap<String, (usize, usize)> = BTreeMap::new();
            let (mut false_plain, mut stray) = (0usize, 0usize);
            let near = |s: usize, e: usize, c: &Clip| {
                s < c.end + RATE / 3 && e > c.start.saturating_sub(RATE / 3)
            };
            for c in clips.iter().filter(|_| *has_clips) {
                let woke = segs.iter().any(|(s, e, calls, ..)| {
                    near(*s, *e, c)
                        && fires(*calls)
                        // A segment that ran over a wake clip too is that wake, not this clip's.
                        && (c.wake || !clips.iter().any(|k| k.wake && near(*s, *e, k)))
                });
                if c.wake {
                    let r = recall.entry(c.voice.clone()).or_default();
                    r.0 += woke as usize;
                    r.1 += 1;
                } else if woke {
                    false_plain += 1;
                }
            }
            for (s, e, calls, ..) in &segs {
                let on_clip = *has_clips
                    && clips
                        .iter()
                        .any(|c| *s < c.end + RATE / 3 && *e > c.start.saturating_sub(RATE / 3));
                if fires(*calls) && !on_clip {
                    stray += 1;
                }
            }
            let hits: usize = recall.values().map(|r| r.0).sum();
            let total: usize = recall.values().map(|r| r.1).sum();
            by_rule.insert(
                rule,
                json!({"wake_hits": hits, "wake_clips": total, "per_voice": recall,
                       "false_on_plain": false_plain, "stray": stray}),
            );
        }
        let detail: Vec<_> = segs
            .iter()
            .map(|(s, e, c, opening, nearest, margin)| {
                let clip = clips.iter().find(|k| *s < k.end && *e > k.start);
                json!({"start": *s as f32 / RATE as f32, "end": *e as f32 / RATE as f32,
                       "clip": clip.map(|k| format!("{}/{}", k.voice, k.id)),
                       "whisper": c.whisper, "strict": c.strict, "template": c.template,
                       "opening": opening, "nearest": nearest, "margin": margin})
            })
            .collect();
        eprintln!("{name}: {} segments", segs.len());
        report.insert(
            name.to_string(),
            json!({"segments": segs.len(), "seconds": audio.len() as f32 / RATE as f32,
                   "rules": by_rule, "detail": detail}),
        );
    }
    judge_ns.sort();
    let thresholds: Vec<_> = household
        .members
        .iter()
        .map(|m| {
            json!({"member": m.id, "distance": m.thresholds.distance, "margin": m.thresholds.margin,
                        "templates": m.templates.iter().map(Template::len).collect::<Vec<_>>()})
        })
        .collect();
    let clip_rows: Vec<_> = clips
        .iter()
        .map(|c| {
            json!({"voice": c.voice, "id": c.id, "wake": c.wake,
                        "start": c.start as f32 / RATE as f32, "end": c.end as f32 / RATE as f32})
        })
        .collect();
    let out = json!({
        "clips": clip_rows,
        "rooms": report,
        "enrolment": enrol_report,
        "thresholds": thresholds,
        "references": household.references.len(),
        "judge_ms_median": judge_ns.get(judge_ns.len() / 2).map(|n| *n as f64 / 1e6),
        "judge_ms_p95": judge_ns.get(judge_ns.len() * 95 / 100).map(|n| *n as f64 / 1e6),
        "reverb": a.reverb,
        "prompt": a.prompt,
    });
    std::fs::write(&a.out, serde_json::to_string_pretty(&out)?)?;
    Ok(())
}

/// A reference is the phrase alone: the frames between the first and last loud ones.
fn trim_frames(f: &[f32]) -> Vec<f32> {
    let n = f.len() / NUM_BINS;
    let energy: Vec<f32> = (0..n)
        .map(|i| f[i * NUM_BINS..(i + 1) * NUM_BINS].iter().sum::<f32>() / NUM_BINS as f32)
        .collect();
    let peak = energy.iter().copied().fold(f32::MIN, f32::max);
    let on = |e: &f32| *e > peak - 6.0;
    let first = energy.iter().position(on).unwrap_or(0);
    let last = energy.iter().rposition(on).unwrap_or(n.saturating_sub(1));
    f[first.saturating_sub(5) * NUM_BINS..((last + 5).min(n)) * NUM_BINS].to_vec()
}
