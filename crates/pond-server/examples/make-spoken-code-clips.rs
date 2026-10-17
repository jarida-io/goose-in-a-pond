//! Regenerates the clips a pond with no voice installed reads a pairing code with
//! (`crates/pond-server/assets/spoken-code/`), from a local Kokoro-82M. Read-only on the data
//! directory: it loads the model and the voice from there and writes only into `--out`.
//!
//! ```text
//! cargo run -p pond-server --example make-spoken-code-clips -- \
//!     [--data-dir DIR] [--out DIR] [--voice af_heart] [--quality fp32]
//! ```

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pond_adapters_kokoro::{model_filename, tokenizer, Engine, StyleTable, Vocab, SAMPLE_RATE};
use pond_voice::dsp::{encode_wav_pcm16, f32_to_pcm16, resample_to_16k};

/// File stem, then what Kokoro says. The announcer plays them as: intro, the six digits,
/// again, the six digits.
const CLIPS: &[(&str, &str)] = &[
    ("intro", "Your Goose pairing code is"),
    ("again", "Once more"),
    ("0", "zero"),
    ("1", "one"),
    ("2", "two"),
    ("3", "three"),
    ("4", "four"),
    ("5", "five"),
    ("6", "six"),
    ("7", "seven"),
    ("8", "eight"),
    ("9", "nine"),
];

/// The whole set must stay small enough to compile into every binary.
const BUDGET_BYTES: usize = 500 * 1024;
/// A sample quieter than this share of the clip's peak is silence for trimming.
const TRIM_FLOOR: f32 = 0.02;
/// Kept either side of the speech, so a trimmed word does not start or end with a click.
const TRIM_MARGIN_MS: usize = 25;
/// Peak level each clip is normalised to, so the digits are equally loud.
const PEAK: f32 = 0.9;

struct Args {
    data_dir: PathBuf,
    out: PathBuf,
    voice: String,
    quality: String,
}

fn args() -> Result<Args> {
    let mut args = Args {
        data_dir: std::env::var_os("POND_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::data_dir().map(|d| d.join("goose-in-a-pond")))
            .context("no data directory; pass --data-dir")?,
        out: Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/spoken-code"),
        voice: pond_adapters_kokoro::DEFAULT_VOICE.to_string(),
        quality: "fp32".to_string(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = || it.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--data-dir" => args.data_dir = value()?.into(),
            "--out" => args.out = value()?.into(),
            "--voice" => args.voice = value()?,
            "--quality" => args.quality = value()?,
            other => bail!("unknown option {other}"),
        }
    }
    Ok(args)
}

/// ort is linked to load its runtime at startup; point it at the pond's own copy when the shell
/// has not.
fn find_onnx_runtime(data_dir: &Path) {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    let lib = data_dir.join("lib");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&lib)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("libonnxruntime"))
        })
        .collect();
    found.sort();
    if let Some(newest) = found.pop() {
        std::env::set_var("ORT_DYLIB_PATH", newest);
    }
}

fn trim(samples: &[f32]) -> &[f32] {
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    let floor = peak * TRIM_FLOOR;
    let margin = SAMPLE_RATE as usize * TRIM_MARGIN_MS / 1000;
    let first = samples.iter().position(|s| s.abs() > floor).unwrap_or(0);
    let last = samples
        .iter()
        .rposition(|s| s.abs() > floor)
        .unwrap_or(samples.len().saturating_sub(1));
    &samples[first.saturating_sub(margin)..(last + margin + 1).min(samples.len())]
}

fn normalise(samples: &[f32]) -> Vec<f32> {
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    if peak <= f32::EPSILON {
        return samples.to_vec();
    }
    samples.iter().map(|s| s * PEAK / peak).collect()
}

fn main() -> Result<()> {
    let args = args()?;
    find_onnx_runtime(&args.data_dir);
    let espeak = args.data_dir.join("bin").join("espeak-ng-data");
    if espeak.is_dir() {
        // The name espeak-rs reads; see pond-adapters-kokoro.
        std::env::set_var("PIPER_ESPEAKNG_DATA_DIRECTORY", &espeak);
    }

    let kokoro = args.data_dir.join("models").join("kokoro");
    let vocab = Vocab::load(&kokoro.join("tokenizer.json"))?;
    let style = StyleTable::load(&kokoro.join("voices"), &args.voice)?;
    let mut engine = Engine::load(&kokoro.join(model_filename(&args.quality)), None)?;

    let mut voiced = Vec::with_capacity(CLIPS.len());
    for (stem, text) in CLIPS {
        let (chunks, dropped) = tokenizer::chunk(text, &vocab)?;
        if dropped > 0 {
            bail!("Kokoro dropped {dropped} phonemes of {text:?}; the clip would sound wrong");
        }
        let mut samples = Vec::new();
        for chunk in &chunks {
            samples.extend(engine.synthesize(chunk, style.style_for(chunk.tokens.len()), 1.0)?);
        }
        voiced.push((*stem, normalise(trim(&samples))));
    }

    let encode = |rate: u32| -> Vec<(&str, Vec<u8>)> {
        voiced
            .iter()
            .map(|(stem, samples)| {
                let samples = if rate == SAMPLE_RATE {
                    samples.clone()
                } else {
                    resample_to_16k(samples, SAMPLE_RATE)
                };
                (*stem, encode_wav_pcm16(&f32_to_pcm16(&samples), rate))
            })
            .collect()
    };
    let mut wavs = encode(SAMPLE_RATE);
    let total = |wavs: &[(&str, Vec<u8>)]| wavs.iter().map(|(_, w)| w.len()).sum::<usize>();
    if total(&wavs) > BUDGET_BYTES {
        wavs = encode(16_000);
    }
    if total(&wavs) > BUDGET_BYTES {
        bail!(
            "the clips come to {} bytes even at 16 kHz, over the {BUDGET_BYTES} budget",
            total(&wavs)
        );
    }

    std::fs::create_dir_all(&args.out)?;
    for (stem, wav) in &wavs {
        let path = args.out.join(format!("{stem}.wav"));
        std::fs::write(&path, wav).with_context(|| format!("writing {}", path.display()))?;
        println!("{} ({} bytes)", path.display(), wav.len());
    }
    println!(
        "{} clips, {} bytes, voice {}, model {}",
        wavs.len(),
        total(&wavs),
        args.voice,
        model_filename(&args.quality)
    );
    Ok(())
}
