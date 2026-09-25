//! Live Kokoro synthesis, `#[ignore]`d; also benches native `ort` RTF. `KOKORO_DIR` must hold
//! the model, `tokenizer.json` and `voices/af_heart.bin`; run with `-- --ignored --nocapture`.

use pond_adapters_kokoro::{engine::duration_secs, tokenizer, Engine, StyleTable, Vocab};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn model_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var("KOKORO_DIR").ok()?);
    dir.join("tokenizer.json").exists().then_some(dir)
}

/// Intra-op threads: `pond-server`'s own default, so the bench measures what ships;
/// `KOKORO_THREADS` overrides it for sweeps.
fn intra_threads() -> Option<usize> {
    match std::env::var("KOKORO_THREADS") {
        Ok(s) => s.parse().ok(),
        Err(_) => Some(pond_adapters_kokoro::default_intra_threads()),
    }
}

/// Weights file: this host's default tier unless `KOKORO_MODEL` overrides it, since file size
/// doesn't predict speed (q4f16 is larger than q8 yet far faster on aarch64).
fn model_file() -> String {
    std::env::var("KOKORO_MODEL").unwrap_or_else(|_| {
        pond_adapters_kokoro::model_filename(pond_adapters_kokoro::host_default_quality())
            .to_owned()
    })
}

/// Any installed non-default voice; only `DEFAULT_VOICE` is guaranteed to be installed.
fn second_voice(voices: &Path) -> Option<String> {
    let mut names: Vec<String> = std::fs::read_dir(voices)
        .ok()?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_suffix(".bin").map(str::to_owned)
        })
        .filter(|n| n != pond_adapters_kokoro::DEFAULT_VOICE)
        .collect();
    names.sort();
    names.into_iter().next()
}

#[test]
#[ignore = "needs Kokoro weights; set KOKORO_DIR"]
fn synthesizes_real_audio() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR to a directory holding tokenizer.json + model + voices/");
    };

    let vocab = Vocab::load(&dir.join("tokenizer.json")).expect("vocab");
    assert_eq!(vocab.len(), 115, "shipped Kokoro vocab is 115 symbols");

    let style = StyleTable::load(&dir.join("voices"), "af_heart").expect("voice");
    let mut engine = Engine::load(&dir.join(model_file()), intra_threads()).expect("engine");

    let text = pond_adapters_kokoro::PREVIEW_SENTENCE;
    let (chunks, dropped) = tokenizer::chunk(text, &vocab).expect("chunk");
    assert!(!chunks.is_empty(), "preview sentence produced no chunks");
    assert_eq!(
        dropped, 0,
        "preview sentence lost phonemes to the vocab filter"
    );

    let t0 = Instant::now();
    let mut samples = Vec::new();
    for c in &chunks {
        samples.extend(
            engine
                .synthesize(c, style.style_for(c.tokens.len()), 1.0)
                .expect("synthesis"),
        );
    }
    let elapsed = t0.elapsed().as_secs_f32();
    let audio = duration_secs(&samples);

    assert!(
        audio > 1.0,
        "expected more than a second of audio, got {audio}s"
    );
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "output is silence (peak {peak})");
    assert!(peak <= 1.5, "output is wildly out of range (peak {peak})");

    println!(
        "kokoro {}: {audio:.2}s audio in {elapsed:.2}s  RTF={:.3}  chunks={}  peak={peak:.3}",
        model_file(),
        elapsed / audio,
        chunks.len()
    );
}

#[test]
#[ignore = "needs Kokoro weights; set KOKORO_DIR"]
fn pace_changes_duration() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR");
    };
    let vocab = Vocab::load(&dir.join("tokenizer.json")).unwrap();
    let style = StyleTable::load(&dir.join("voices"), "af_heart").unwrap();
    let mut engine = Engine::load(&dir.join(model_file()), intra_threads()).unwrap();

    let (chunks, _) = tokenizer::chunk("The pond is awake and listening.", &vocab).unwrap();
    let c = &chunks[0];
    let row = style.style_for(c.tokens.len());

    let slow = engine.synthesize(c, row, 0.75).unwrap().len();
    let normal = engine.synthesize(c, row, 1.0).unwrap().len();
    let fast = engine.synthesize(c, row, 1.5).unwrap().len();

    println!("pace samples: 0.75x={slow} 1.0x={normal} 1.5x={fast}");
    assert!(slow > normal, "0.75x should be longer than 1.0x");
    assert!(fast < normal, "1.5x should be shorter than 1.0x");
}

#[test]
#[ignore = "needs Kokoro weights + a second voice; set KOKORO_DIR"]
fn different_voices_produce_different_audio() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR");
    };
    let voices = dir.join("voices");
    let Some(other) = second_voice(&voices) else {
        // A fresh pond has only one voice; nothing to compare.
        println!(
            "only {} installed; skipping",
            pond_adapters_kokoro::DEFAULT_VOICE
        );
        return;
    };
    let vocab = Vocab::load(&dir.join("tokenizer.json")).unwrap();
    let mut engine = Engine::load(&dir.join(model_file()), intra_threads()).unwrap();
    let (chunks, _) = tokenizer::chunk("Good morning.", &vocab).unwrap();
    let c = &chunks[0];

    let a = StyleTable::load(&voices, pond_adapters_kokoro::DEFAULT_VOICE).unwrap();
    let b = StyleTable::load(&voices, &other).unwrap();

    let wave_a = engine
        .synthesize(c, a.style_for(c.tokens.len()), 1.0)
        .unwrap();
    let wave_b = engine
        .synthesize(c, b.style_for(c.tokens.len()), 1.0)
        .unwrap();

    let n = wave_a.len().min(wave_b.len());
    assert!(n > 0);
    let diff: f32 = wave_a[..n]
        .iter()
        .zip(&wave_b[..n])
        .map(|(x, y)| (x - y).abs())
        .sum::<f32>()
        / n as f32;
    println!("mean abs difference af_heart vs {other}: {diff:.5}");
    assert!(
        diff > 1e-4,
        "af_heart and {other} produced near-identical audio"
    );
}

#[test]
#[ignore = "needs KOKORO_DIR for the real vocab"]
fn ordinary_text_loses_no_phonemes() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR");
    };
    let vocab = Vocab::load(&dir.join("tokenizer.json")).unwrap();

    let corpus = [
        "The pond is awake. I have your calendar for tomorrow.",
        "Your meeting is at 3:45 PM on March 2nd, 2026.",
        "The total came to $1,247.83, up 12.5% from last quarter.",
        "Check the REST API over HTTP, then the SQLite DB.",
        "She sells thirty-three shiny thistles by the northern thoroughfare.",
        "Jerry asked about Nairobi, Kisumu, and the Jetson Orin Nano.",
        "It's 72 degrees and clear until about four in the afternoon.",
    ];

    let mut total_dropped = 0;
    for text in corpus {
        let (_chunks, dropped) = tokenizer::chunk(text, &vocab).unwrap();
        if dropped > 0 {
            println!("DROPPED {dropped} in: {text}");
        }
        total_dropped += dropped;
    }
    assert_eq!(
        total_dropped, 0,
        "espeak emitted phonemes outside Kokoro's vocab — words will sound wrong"
    );
}

/// Checked here because the unit tests' vocab includes `PROSODY_PUNCT` by construction.
#[test]
#[ignore = "needs Kokoro weights; set KOKORO_DIR"]
fn every_preserved_mark_is_in_the_real_vocab() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR to a directory holding tokenizer.json + model + voices/")
    };
    let vocab = Vocab::load(&dir.join("tokenizer.json")).expect("vocab");

    // Spelled out, not imported, so editing `PROSODY_PUNCT` can't redefine what is checked.
    for c in ['.', ',', '!', '?', ';', ':', '"', '(', ')'] {
        assert!(
            vocab.contains(c),
            "{c:?} is preserved by the tokenizer but is not in the model's vocab"
        );
    }

    let (_, _, dropped) = vocab.encode(",.!?;:\"()");
    assert_eq!(dropped, 0, "a preserved mark was dropped by the real vocab");
}

/// Measured as duration: prosody is mostly pause, so the marked text must take longer to say.
#[test]
#[ignore = "needs Kokoro weights; set KOKORO_DIR"]
fn punctuation_changes_the_audio() {
    let Some(dir) = model_dir() else {
        panic!("set KOKORO_DIR to a directory holding tokenizer.json + model + voices/");
    };

    let vocab = Vocab::load(&dir.join("tokenizer.json")).expect("vocab");
    let style = StyleTable::load(&dir.join("voices"), "af_heart").expect("voice");
    let mut engine = Engine::load(&dir.join(model_file()), intra_threads()).expect("engine");

    const BARE: &str = "Hello world Are you sure Yes really";
    const MARKED: &str = "Hello, world! Are you sure? Yes; really.";

    let mut say = |text: &str| -> (Vec<f32>, String, usize) {
        let (chunks, dropped) = tokenizer::chunk(text, &vocab).expect("chunk");
        let phonemes = chunks
            .iter()
            .map(|c| c.phonemes.as_str())
            .collect::<Vec<_>>()
            .join("");
        let mut samples = Vec::new();
        for c in &chunks {
            samples.extend(
                engine
                    .synthesize(c, style.style_for(c.tokens.len()), 1.0)
                    .expect("synthesis"),
            );
        }
        (samples, phonemes, dropped)
    };

    let (bare, bare_ipa, bare_dropped) = say(BARE);
    let (marked, marked_ipa, marked_dropped) = say(MARKED);

    println!("bare    {bare_ipa:?}");
    println!("marked  {marked_ipa:?}");

    assert_eq!(bare_dropped, 0, "unpunctuated text lost phonemes");
    assert_eq!(
        marked_dropped, 0,
        "punctuated text lost phonemes — a mark is outside the vocab"
    );

    for mark in ['.', ',', '!', '?', ';'] {
        assert!(
            marked_ipa.contains(mark),
            "{mark:?} never reached the model: {marked_ipa:?}"
        );
    }
    assert!(
        !bare_ipa.contains(','),
        "unpunctuated text somehow grew a comma: {bare_ipa:?}"
    );

    let bare_secs = duration_secs(&bare);
    let marked_secs = duration_secs(&marked);
    println!("bare {bare_secs:.2}s   marked {marked_secs:.2}s");

    assert!(
        marked_secs > bare_secs,
        "punctuation did not lengthen the audio ({marked_secs:.2}s vs {bare_secs:.2}s) — \
         the marks are reaching the tokenizer but not changing the voice"
    );

    // Write both so the difference can be heard, not just measured.
    if let Ok(out) = std::env::var("KOKORO_OUT") {
        write_wav(&format!("{out}/bare.wav"), &bare);
        write_wav(&format!("{out}/marked.wav"), &marked);
        println!("wrote {out}/bare.wav and {out}/marked.wav");
    }
}

/// Minimal 16-bit mono WAV, so a listener does not need the rest of the stack.
fn write_wav(path: &str, samples: &[f32]) {
    const RATE: u32 = 24_000;
    let bytes: Vec<u8> = samples
        .iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
        .collect();
    let mut wav = Vec::with_capacity(44 + bytes.len());
    wav.extend(b"RIFF");
    wav.extend(((36 + bytes.len()) as u32).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(RATE.to_le_bytes());
    wav.extend((RATE * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend((bytes.len() as u32).to_le_bytes());
    wav.extend(bytes);
    std::fs::write(path, wav).expect("write wav");
}
