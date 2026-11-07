//! The "hey goose" CNN beside Whisper's segment wake, on the same audio: does the CNN hear what
//! Whisper hears, and nothing more? Both run as serve runs them: Whisper reads the opening of each
//! Silero segment, the CNN scores every 80 ms.
//!
//! wake_bench record DIR [--device NAME]    say scripted trials into this computer's microphone
//! wake_bench score DIR|WAV...              both detectors on recordings (a DIR from `record`)
//! wake_bench listen [--minutes M] [--save WAV]   both detectors live on the microphone
//!   [--model ggml.bin] [--silero silero_vad.onnx] [--names "Ann,Bo"] [--rung neon-f32]
//!   [--greedy]   decode each opening greedily over the clip's own length, not beam-5 over 30 s
//!   [--whisper-log]   print whisper.cpp's own account of its buffers
//!
//! Whisper defaults to base.en, the nano's, from the pond's data directory.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use pond_adapters_whisper::{TranscribeOpts, WhisperRsInput};
use pond_ear::ear::WakeConfig;
use pond_ear::{Ear, Event, Household};

const RATE: usize = 16_000;
const WAKE_SCAN_SECONDS: usize = pond_adapters_ear::WAKE_SCAN_SECONDS;
const THRESHOLDS: [f32; 7] = [0.97, 0.95, 0.90, 0.80, 0.70, 0.50, 0.30];
/// A segment's CNN score is the best hop from its start to this long after its end: the CNN
/// scores a window ending at the hop, so the phrase's last hop lands after the speech.
const HOP_REACH: (f64, f64) = (0.1, 0.6);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Label {
    Wake,
    Near,
    Other,
}

impl Label {
    fn name(self) -> &'static str {
        match self {
            Label::Wake => "wake",
            Label::Near => "near",
            Label::Other => "other",
        }
    }

    fn parse(s: &str) -> Option<Label> {
        match s {
            "wake" => Some(Label::Wake),
            "near" => Some(Label::Near),
            "other" => Some(Label::Other),
            _ => None,
        }
    }
}

/// A clip's label, whether Whisper woke on it, and the CNN's best score in it.
type Row = (Option<Label>, bool, f32);

struct Trial {
    label: Label,
    say: &'static str,
    how: &'static str,
    seconds: f32,
}

const fn t(label: Label, say: &'static str, how: &'static str, seconds: f32) -> Trial {
    Trial {
        label,
        say,
        how,
        seconds,
    }
}

use Label::{Near, Other, Wake};

/// "Near" lines sound like the phrase without saying it. Whisper's triggers for "hey goose"
/// include "goose" alone, so it wakes on some of them by design; they are reported apart.
const TRIALS: &[Trial] = &[
    t(Wake, "Hey goose.", "", 4.0),
    t(Other, "What's for dinner tonight?", "", 4.0),
    t(Wake, "Hey goose, what time is it?", "", 4.0),
    t(Near, "The goose is in the garden.", "", 4.0),
    t(Wake, "Hey goose, turn off the kitchen lights.", "", 4.5),
    t(Other, "Can you pass me the remote?", "", 4.0),
    t(Near, "Hey, good news!", "", 4.0),
    t(
        Wake,
        "Hey goose ... set a timer for ten minutes.",
        "pause after \"hey goose\"",
        6.0,
    ),
    t(Other, "I'll be back in five minutes.", "", 4.0),
    t(Wake, "Hey goose.", "quietly", 4.0),
    t(Near, "Hey Bruce, how's it going?", "", 4.0),
    t(Wake, "Hey goose, what's the weather tomorrow?", "", 4.5),
    t(Other, "The meeting moved to Thursday.", "", 4.0),
    t(
        Wake,
        "Hey goose, play some music.",
        "from across the room",
        6.0,
    ),
    t(Near, "I saw a goose by the lake.", "", 4.0),
    t(
        Wake,
        "Hey goose what's on my calendar",
        "fast, as one word",
        4.0,
    ),
    t(Other, "Did you feed the cat?", "", 4.0),
    t(Near, "Hey Gus, come here.", "", 4.0),
    t(Wake, "So, hey goose, add milk to the list.", "", 4.5),
    t(Other, "Where did I leave my keys?", "", 4.0),
    t(Wake, "Hey goose!", "loudly", 4.0),
    t(Near, "Hey, juice please.", "", 4.0),
    t(Wake, "Hey goose, how are you today?", "", 4.5),
    t(Other, "It's going to rain this afternoon.", "", 4.0),
    t(Near, "Okay goose, what time is it?", "", 4.0),
    t(Wake, "Hey goose, stop.", "", 4.0),
    t(Other, "Turn the volume down a bit.", "", 4.0),
    t(Near, "What a wild goose chase.", "", 4.0),
    t(Wake, "Hey goose, remind me to call mum at six.", "", 5.0),
    t(Other, "Let's go for a walk later.", "", 4.0),
    t(Near, "Hey, you goofs.", "", 4.0),
    t(Wake, "Hey goose.", "with your back to the computer", 5.0),
    t(Other, "That song is really catchy.", "", 4.0),
    t(Wake, "Hey goose, what did I ask you yesterday?", "", 5.0),
    t(Near, "Hey Ruth, close the door.", "", 4.0),
    t(Wake, "Hey goose, good night.", "", 4.0),
];

struct Opts {
    model: PathBuf,
    silero: PathBuf,
    names: Vec<String>,
    device: Option<String>,
    rung: Option<pond_ear::Rung>,
    greedy: bool,
    minutes: Option<f64>,
    save: Option<PathBuf>,
    rest: Vec<String>,
}

fn opts(args: &[String]) -> Result<Opts> {
    let data = pond_ear::locations::data_dir().context("no data directory; set POND_DATA_DIR")?;
    let mut o = Opts {
        model: data.join("models").join("ggml-base.en.bin"),
        silero: pond_ear::locations::silero(&data),
        names: Vec::new(),
        device: None,
        rung: None,
        greedy: false,
        minutes: None,
        save: None,
        rest: Vec::new(),
    };
    let mut a = args.iter();
    while let Some(k) = a.next() {
        let mut v = || {
            a.next()
                .cloned()
                .ok_or_else(|| anyhow!("{k} needs a value"))
        };
        match k.as_str() {
            "--model" => o.model = PathBuf::from(v()?),
            "--silero" => o.silero = PathBuf::from(v()?),
            "--names" => {
                o.names = v()?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            "--device" => o.device = Some(v()?),
            "--rung" => {
                let name = v()?;
                o.rung = Some(
                    pond_ear::Rung::from_name(&name)
                        .filter(pond_ear::Rung::available)
                        .ok_or_else(|| anyhow!("no rung {name} on this CPU"))?,
                );
            }
            "--greedy" => o.greedy = true,
            "--whisper-log" => {}
            "--minutes" => o.minutes = Some(v()?.parse()?),
            "--save" => o.save = Some(PathBuf::from(v()?)),
            _ if k.starts_with("--") => bail!("unknown option {k}"),
            _ => o.rest.push(k.clone()),
        }
    }
    for (what, p) in [("Whisper model", &o.model), ("Silero", &o.silero)] {
        if !p.exists() {
            bail!("{what} not found at {}", p.display());
        }
    }
    Ok(o)
}

/// One stretch of speech as both detectors judged it.
struct Seg {
    start: f64,
    end: f64,
    opening: String,
    whisper: bool,
    whisper_ms: u128,
    cnn: f32,
}

/// Both detectors fed in lockstep, so a recording and the live microphone are judged alike.
struct Pair<'a> {
    bench: &'a Bench,
    segmenter: Ear,
    cnn: Ear,
    whole: Vec<f32>,
    buf: Vec<f32>,
    events: Vec<Event>,
    hops: Vec<(f64, f32)>,
    pending: Vec<(usize, usize)>,
    segs: Vec<Seg>,
}

impl<'a> Pair<'a> {
    fn new(bench: &'a Bench) -> Result<Pair<'a>> {
        Ok(Pair {
            bench,
            segmenter: bench.ear(true)?,
            cnn: bench.ear(false)?,
            whole: Vec::new(),
            buf: Vec::new(),
            events: Vec::new(),
            hops: Vec::new(),
            pending: Vec::new(),
            segs: Vec::new(),
        })
    }

    /// Feed `pcm`; returns how many segments were judged.
    fn push(&mut self, pcm: &[f32]) -> Result<usize> {
        for chunk in pcm.chunks(RATE / 10) {
            self.events.clear();
            let from = self.segmenter.front().samples();
            self.segmenter.push_into(chunk, &mut self.events);
            self.segmenter
                .audio(from, self.segmenter.front().samples(), &mut self.buf);
            self.whole.extend_from_slice(&self.buf);
            self.cnn.push(chunk);
            self.hops.extend(self.cnn.take_scores());
            for ev in self.events.drain(..) {
                if let Event::Segment { start, end, .. } = ev {
                    self.pending.push((start, end));
                }
            }
        }
        let now = self.hops.last().map(|h| h.0).unwrap_or(0.0);
        let ready: Vec<(usize, usize)> = self
            .pending
            .iter()
            .copied()
            .filter(|&(_, e)| secs(e) + HOP_REACH.1 <= now)
            .collect();
        self.pending.retain(|p| !ready.contains(p));
        for &(s, e) in &ready {
            let seg = self.judge(s, e)?;
            self.segs.push(seg);
        }
        Ok(ready.len())
    }

    /// Flush the last segment with a quiet tail, as a speaker falling silent would.
    fn finish(&mut self) -> Result<()> {
        self.push(&vec![0.0; RATE * 2])?;
        let rest: Vec<(usize, usize)> = self.pending.drain(..).collect();
        for (s, e) in rest {
            let seg = self.judge(s, e)?;
            self.segs.push(seg);
        }
        Ok(())
    }

    fn judge(&self, s: usize, e: usize) -> Result<Seg> {
        let pcm = &self.whole[s.min(self.whole.len())..e.min(self.whole.len())];
        let scan = pcm.len().min(WAKE_SCAN_SECONDS * RATE);
        let started = Instant::now();
        let opening = self.bench.whisper.transcribe_pcm_with(
            &pcm[..scan],
            self.bench.scan,
            self.bench.prompt.as_deref(),
        )?;
        let whisper_ms = started.elapsed().as_millis();
        let (from, to) = (secs(s) - HOP_REACH.0, secs(e) + HOP_REACH.1);
        Ok(Seg {
            start: secs(s),
            end: secs(e),
            whisper: pond_voice::text::heard_wake(&opening, &self.bench.triggers),
            opening,
            whisper_ms,
            cnn: peak(&self.hops, from, to),
        })
    }

    fn cnn_rung(&self) -> &'static str {
        self.bench
            .rung
            .unwrap_or_else(pond_ear::Rung::fastest)
            .name()
    }

    fn cnn_peak(&self) -> f32 {
        peak(&self.hops, f64::MIN, f64::MAX)
    }
}

fn secs(sample: usize) -> f64 {
    sample as f64 / RATE as f64
}

fn peak(hops: &[(f64, f32)], from: f64, to: f64) -> f32 {
    hops.iter()
        .filter(|h| h.0 >= from && h.0 <= to)
        .map(|h| h.1)
        .fold(0.0, f32::max)
}

struct Bench {
    whisper: WhisperRsInput,
    triggers: Vec<String>,
    prompt: Option<String>,
    silero: PathBuf,
    rung: Option<pond_ear::Rung>,
    scan: TranscribeOpts,
    model: String,
    load_ms: u128,
    whisper_mb: Option<f64>,
}

impl Bench {
    fn new(o: &Opts) -> Result<Bench> {
        let mb = || pond_adapters_ear::process_memory().map(|m| m.footprint as f64 / 1e6);
        let before = mb();
        let (mic, _owner) = pond_audio::spawn(
            Box::new(pond_audio::testing::ScriptedCapture::silence(0, 20)),
            pond_audio::CAPTURE_RATE_HZ,
            5_000,
            true,
        );
        let started = Instant::now();
        let whisper = WhisperRsInput::new(o.model.clone(), mic)?;
        let load_ms = started.elapsed().as_millis();
        let whisper_mb = before.zip(mb()).map(|(b, a)| a - b);
        Ok(Bench {
            whisper,
            triggers: pond_voice::text::wake_triggers("hey goose", &[]),
            prompt: pond_core::models::domain::speech_prompt::room_prompt("hey goose", &o.names),
            silero: o.silero.clone(),
            rung: o.rung,
            scan: match o.greedy {
                true => TranscribeOpts::wake_word(),
                false => TranscribeOpts::segment_scan(),
            },
            model: o
                .model
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            load_ms,
            whisper_mb,
        })
    }

    fn ear(&self, segments: bool) -> Result<Ear> {
        let mut cfg = pond_ear::Config::new(
            WakeConfig::hey_goose().map_err(|e| anyhow!("{e}"))?,
            pond_adapters_ear::load_vad(&self.silero)?,
        );
        cfg.segments = segments;
        if let Some(rung) = self.rung {
            cfg.rung = rung;
        }
        Ear::new(cfg, Household::default()).map_err(|e| anyhow!("{e}"))
    }

    fn costs(&self, pair: &Pair, whisper_ms: &[u128], audio_s: f64) {
        let parts = pair.cnn.parts();
        let bytes: usize = parts.iter().map(|p| p.bytes.total()).sum();
        let ns: u64 = parts.iter().map(|p| p.ns).sum();
        let wake_ns: u64 = parts
            .iter()
            .filter(|p| p.role == "wake")
            .map(|p| p.ns)
            .sum();
        let mut ms = whisper_ms.to_vec();
        ms.sort_unstable();
        let median = ms.get(ms.len() / 2).copied().unwrap_or(0);
        // The process footprint also holds Metal's own setup; `--whisper-log` gives Whisper's buffers.
        println!(
            "\nCost on this computer, {audio_s:.0} s of audio, CNN on {}:\n  ear with the CNN: {:.1} MB, {:.2}% of one core (the CNN alone {:.2}%)\n  Whisper {} ({}): {} process footprint at load, loaded in {} ms, {median} ms median per segment ({} segments)",
            pair.cnn_rung(),
            bytes as f64 / 1e6,
            ns as f64 / 1e9 / audio_s.max(1e-9) * 100.0,
            wake_ns as f64 / 1e9 / audio_s.max(1e-9) * 100.0,
            self.model,
            match self.scan.beam_size {
                Some(_) => "beam search over 30 s",
                None => "greedy over the clip",
            },
            self.whisper_mb
                .map(|m| format!("+{m:.0} MB"))
                .unwrap_or_else(|| "unmeasured".into()),
            self.load_ms,
            ms.len()
        );
    }
}

/// whisper.cpp reports each buffer it allocates through `log`; `--whisper-log` prints them.
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
    if std::env::args().any(|a| a == "--whisper-log") {
        let _ = log::set_logger(&Stderr).map(|()| log::set_max_level(log::LevelFilter::Info));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        bail!(
            "usage: wake_bench record DIR | score DIR|WAV... | listen [--minutes M] [--save WAV]"
        );
    };
    let o = opts(&args[1..])?;
    match cmd.as_str() {
        "record" => {
            let dir = PathBuf::from(o.rest.first().context("record needs a DIR")?);
            record(&o, &dir)?;
            score(&o, &[dir])
        }
        "score" => {
            let inputs: Vec<PathBuf> = o.rest.iter().map(PathBuf::from).collect();
            if inputs.is_empty() {
                bail!("score needs a DIR or WAV");
            }
            score(&o, &inputs)
        }
        "listen" => listen(&o),
        other => bail!("unknown command {other}"),
    }
}

fn open_mic(o: &Opts) -> Result<pond_audio::MicHandle> {
    let device = match &o.device {
        Some(name) => pond_audio::CpalCapture::with_device(name.clone()),
        None => pond_audio::CpalCapture::new(),
    };
    let (mic, _owner) =
        pond_audio::spawn(Box::new(device), pond_audio::CAPTURE_RATE_HZ, 30_000, true);
    mic.open();
    mic.wait_for(
        |s| !matches!(s, pond_audio::MicState::Closed),
        Duration::from_secs(5),
    );
    match mic.state() {
        pond_audio::MicState::Open => Ok(mic),
        other => bail!("the microphone did not open: {other:?}"),
    }
}

fn record(o: &Opts, dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let mic = open_mic(o)?;
    let mut index = std::fs::File::create(dir.join("trials.tsv"))?;
    println!(
        "{} trials, about {:.0} minutes. Speak as you would to the pond, from where you sit.\nEach line is said once, when \"now\" appears.",
        TRIALS.len(),
        TRIALS.iter().map(|t| t.seconds + 2.5).sum::<f32>() / 60.0
    );
    std::thread::sleep(Duration::from_secs(3));
    for (i, trial) in TRIALS.iter().enumerate() {
        let how = match trial.how {
            "" => String::new(),
            h => format!("  ({h})"),
        };
        println!("\n[{}/{}]  {}{how}", i + 1, TRIALS.len(), trial.say);
        for n in ["3", "2", "1"] {
            print!("{n} ");
            std::io::stdout().flush()?;
            std::thread::sleep(Duration::from_millis(600));
        }
        // A little before "now", so an early start is not clipped.
        let mut reader = pond_audio::MicReader::starting_back(mic.shared().clone(), RATE / 4);
        println!("now");
        std::thread::sleep(Duration::from_secs_f32(trial.seconds));
        let pcm = reader.drain();
        let loudest = pcm.iter().fold(0f32, |m, v| m.max(v.abs()));
        if loudest < 0.003 {
            println!("  (silent: is the microphone allowed for this terminal in System Settings, Privacy?)");
        }
        let id = format!("{:02}", i + 1);
        std::fs::write(
            dir.join(format!("{id}.wav")),
            pond_ear::wav::encode_wav16(&pcm, RATE as u32),
        )?;
        writeln!(index, "{id}\t{}\t{}", trial.label.name(), trial.say)?;
    }
    println!("\nRecorded to {}. Scoring...", dir.display());
    Ok(())
}

struct Clip {
    id: String,
    label: Option<Label>,
    say: String,
    pcm: Vec<f32>,
}

fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    let (rate, pcm) = pond_ear::wav::decode_wav(&bytes).map_err(|e| anyhow!(e))?;
    Ok(match rate as usize {
        RATE => pcm,
        _ => pond_ear::dsp::resample_to_16k(&pcm, rate),
    })
}

fn clips(inputs: &[PathBuf]) -> Result<Vec<Clip>> {
    let mut out = Vec::new();
    for input in inputs {
        if input.is_dir() {
            let index = std::fs::read_to_string(input.join("trials.tsv"))
                .with_context(|| format!("{} has no trials.tsv", input.display()))?;
            for line in index.lines().filter(|l| !l.trim().is_empty()) {
                let f: Vec<&str> = line.split('\t').collect();
                let id = f[0].to_string();
                out.push(Clip {
                    pcm: read_wav(&input.join(format!("{id}.wav")))?,
                    label: f.get(1).and_then(|l| Label::parse(l)),
                    say: f.get(2).unwrap_or(&"").to_string(),
                    id,
                });
            }
        } else {
            out.push(Clip {
                id: input.display().to_string(),
                label: None,
                say: String::new(),
                pcm: read_wav(input)?,
            });
        }
    }
    Ok(out)
}

fn score(o: &Opts, inputs: &[PathBuf]) -> Result<()> {
    let bench = Bench::new(o)?;
    let clips = clips(inputs)?;
    let mut rows: Vec<Row> = Vec::new();
    let mut last: Option<Pair> = None;
    let mut audio_s = 0.0;
    let mut whisper_ms = Vec::new();
    println!(
        "\n{:<6} {:<6} {:>6}  {:<8} {:<40} said",
        "clip", "label", "CNN", "Whisper", "Whisper heard"
    );
    for clip in &clips {
        let mut pair = Pair::new(&bench)?;
        // Lead-in quiet: Silero and the CNN need a moment before speech, as in a room.
        pair.push(&vec![0.0; RATE / 2])?;
        pair.push(&clip.pcm)?;
        pair.finish()?;
        audio_s += clip.pcm.len() as f64 / RATE as f64;
        let whisper = pair.segs.iter().any(|s| s.whisper);
        let cnn = pair.cnn_peak();
        let heard: Vec<&str> = pair.segs.iter().map(|s| s.opening.trim()).collect();
        println!(
            "{:<6} {:<6} {:>6.3}  {:<8} {:<40} {}",
            short(&clip.id, 6),
            clip.label.map(Label::name).unwrap_or("-"),
            cnn,
            if whisper { "wake" } else { "-" },
            short(&heard.join(" | "), 40),
            clip.say
        );
        if clip.label.is_none() && clips.len() == 1 {
            for s in &pair.segs {
                println!(
                    "    {:>7.2}-{:<7.2} CNN {:.3}  Whisper {}  \"{}\"",
                    s.start,
                    s.end,
                    s.cnn,
                    if s.whisper { "wake" } else { "-" },
                    s.opening.trim()
                );
            }
        }
        whisper_ms.extend(pair.segs.iter().map(|s| s.whisper_ms));
        rows.push((clip.label, whisper, cnn));
        last = Some(pair);
    }
    summary(&rows);
    if let Some(pair) = &last {
        bench.costs(pair, &whisper_ms, audio_s);
    }
    Ok(())
}

fn short(s: &str, n: usize) -> String {
    match s.chars().count() > n {
        true => format!("{}~", s.chars().take(n - 1).collect::<String>()),
        false => s.to_string(),
    }
}

fn summary(rows: &[Row]) {
    let count = |l: Label| rows.iter().filter(|r| r.0 == Some(l)).count();
    let (nw, nn, no) = (count(Wake), count(Near), count(Other));
    if nw + nn + no == 0 {
        let whisper = rows.iter().filter(|r| r.1).count();
        println!(
            "\nUnlabelled: Whisper woke on {whisper} of {} clips.",
            rows.len()
        );
        for th in THRESHOLDS {
            let cnn = rows.iter().filter(|r| r.2 >= th).count();
            let both = rows.iter().filter(|r| r.1 && r.2 >= th).count();
            println!("  CNN at {th:.2}: {cnn}, both {both}");
        }
        return;
    }
    let hits =
        |l: Label, f: &dyn Fn(&Row) -> bool| rows.iter().filter(|r| r.0 == Some(l) && f(r)).count();
    println!(
        "\n{:<14} {:>14} {:>16} {:>16}",
        "detector",
        format!("wake heard /{nw}"),
        format!("near woke /{nn}"),
        format!("other woke /{no}")
    );
    let line = |name: String, f: &dyn Fn(&Row) -> bool| {
        println!(
            "{:<14} {:>14} {:>16} {:>16}",
            name,
            hits(Wake, f),
            hits(Near, f),
            hits(Other, f)
        )
    };
    line("Whisper".into(), &|r| r.1);
    for th in THRESHOLDS {
        line(format!("CNN at {th:.2}"), &|r| r.2 >= th);
    }
    let mut wake: Vec<f32> = rows
        .iter()
        .filter(|r| r.0 == Some(Wake))
        .map(|r| r.2)
        .collect();
    wake.sort_by(|a, b| a.total_cmp(b));
    let other_max = rows
        .iter()
        .filter(|r| r.0 == Some(Other))
        .map(|r| r.2)
        .fold(0.0, f32::max);
    let near_max = rows
        .iter()
        .filter(|r| r.0 == Some(Near))
        .map(|r| r.2)
        .fold(0.0, f32::max);
    let whisper_only = hits(Wake, &|r| r.1 && r.2 < 0.97);
    let cnn_only = hits(Wake, &|r| !r.1 && r.2 >= 0.97);
    println!(
        "\nCNN scores on the wake lines, lowest first: {}",
        wake.iter()
            .map(|p| format!("{p:.3}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "Highest CNN score on an ordinary line {other_max:.3}, on a near line {near_max:.3}.\nAt the shipped 0.97: Whisper heard {whisper_only} wake lines the CNN missed; the CNN heard {cnn_only} Whisper missed."
    );
}

fn listen(o: &Opts) -> Result<()> {
    let bench = Bench::new(o)?;
    let mic = open_mic(o)?;
    let mut reader = pond_audio::MicReader::new(mic.shared().clone());
    let mut pair = Pair::new(&bench)?;
    let mut saved: Vec<f32> = Vec::new();
    let started = Instant::now();
    let limit = o.minutes.map(|m| Duration::from_secs_f64(m * 60.0));
    println!(
        "Listening{}. Each stretch of speech prints the CNN's score and what Whisper heard. Ctrl-C ends it.",
        limit
            .map(|l| format!(" for {:.0} min", l.as_secs_f64() / 60.0))
            .unwrap_or_default()
    );
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc_flag(stop);
    }
    let mut shown = 0;
    while !stop.load(std::sync::atomic::Ordering::Relaxed)
        && limit.is_none_or(|l| started.elapsed() < l)
    {
        std::thread::sleep(Duration::from_millis(100));
        let pcm = reader.drain();
        if o.save.is_some() {
            saved.extend_from_slice(&pcm);
        }
        pair.push(&pcm)?;
        for s in &pair.segs[shown..] {
            println!(
                "{:>7.1}s  {:>4.1}s  CNN {:.3} {:<5} Whisper {:<5} \"{}\"",
                s.start,
                s.end - s.start,
                s.cnn,
                if s.cnn >= 0.97 { "wake" } else { "-" },
                if s.whisper { "wake" } else { "-" },
                s.opening.trim()
            );
        }
        shown = pair.segs.len();
    }
    let audio_s = started.elapsed().as_secs_f64();
    let whisper = pair.segs.iter().filter(|s| s.whisper).count();
    println!("\n{} segments; Whisper woke on {whisper}.", pair.segs.len());
    for th in THRESHOLDS {
        let cnn = pair.segs.iter().filter(|s| s.cnn >= th).count();
        let both = pair
            .segs
            .iter()
            .filter(|s| s.whisper && s.cnn >= th)
            .count();
        println!("  CNN at {th:.2}: {cnn} (both {both})");
    }
    let ms: Vec<u128> = pair.segs.iter().map(|s| s.whisper_ms).collect();
    bench.costs(&pair, &ms, audio_s);
    if let Some(path) = &o.save {
        std::fs::write(path, pond_ear::wav::encode_wav16(&saved, RATE as u32))?;
        println!("Saved the audio to {}", path.display());
    }
    Ok(())
}

/// Ctrl-C ends `listen` with its summary instead of killing it; no handler crate for one example.
fn ctrlc_flag(stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    std::thread::spawn(move || {
        rt.block_on(async {
            if tokio::signal::ctrl_c().await.is_ok() {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
    });
    Ok(())
}
