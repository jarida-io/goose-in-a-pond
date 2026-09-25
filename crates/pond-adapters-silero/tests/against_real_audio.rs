//! What the detector does to sound, measured rather than described.
//!
//! The unit tests in `lib.rs` cover the pieces — window arithmetic, the
//! missing-model error, holding a verdict across a short read. None of them
//! plays the detector any speech, so none of them would notice a model that
//! had quietly stopped firing. These do.
//!
//! `#[ignore]` because the 2 MB model is not in the repo:
//!
//! ```text
//! ORT_DYLIB_PATH=... SILERO_MODEL=<data_dir>/models/silero/silero_vad.onnx \
//!   cargo test -p pond-adapters-silero --test against_real_audio -- --ignored --nocapture
//! ```
//!
//! The speech IS in the repo — two `say`-generated utterances at 16 kHz mono,
//! 0.7 s and 5.4 s. Synthetic, and that is a real limit: they are clean, read,
//! US-accented, so they answer "does it fire on speech at all" and say nothing
//! about how it behaves on an accent or in a room. That second question needs
//! recordings of the person who will use it.

use pond_adapters_silero::{SileroDetector, WINDOW};
use pond_voice::dsp::{decode_wav, RmsDetector, SpeechDetector};

/// The capture loop's per-poll size (~30 ms at 16 kHz); deliberately not a multiple of 512.
const CAPTURE_CHUNK: usize = 480;

/// `END_OF_SPEECH_RMS` in `WhisperRsInput` — the gate Silero was added beside.
const RMS_THRESHOLD: f32 = 0.005;

fn model() -> std::path::PathBuf {
    match std::env::var("SILERO_MODEL") {
        Ok(p) => std::path::PathBuf::from(p),
        Err(_) => panic!(
            "set SILERO_MODEL to <data_dir>/models/silero/silero_vad.onnx \
             (any `pond-server chat --voice` run fetches it)"
        ),
    }
}

/// Percentage of capture-sized frames each detector called speech.
fn sweep(samples: &[f32]) -> (f32, f32) {
    let mut silero = SileroDetector::new(model()).expect("load the model");
    let mut rms = RmsDetector::new(RMS_THRESHOLD);
    let (mut s, mut r, mut n) = (0usize, 0usize, 0usize);
    for chunk in samples.chunks(CAPTURE_CHUNK) {
        if silero.is_speech(chunk) {
            s += 1;
        }
        if rms.is_speech(chunk) {
            r += 1;
        }
        n += 1;
    }
    let pct = |k: usize| 100.0 * k as f32 / n as f32;
    (pct(s), pct(r))
}

fn wav(bytes: &[u8]) -> Vec<f32> {
    let d = decode_wav(bytes).expect("fixture must decode");
    assert_eq!(d.sample_rate, 16_000, "fixture must already be at 16 kHz");
    d.samples
}

/// Steady broadband noise at 0.01 (a fan), twice the energy threshold.
fn fan_noise() -> Vec<f32> {
    let mut seed = 1u32;
    (0..WINDOW * 60)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            ((seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.01
        })
        .collect()
}

/// A 120 Hz tone at 0.02 (a fridge compressor): periodic, unlike the fan.
fn fridge_hum() -> Vec<f32> {
    (0..WINDOW * 60)
        .map(|i| (2.0 * std::f32::consts::PI * 120.0 * i as f32 / 16_000.0).sin() * 0.02)
        .collect()
}

#[test]
#[ignore = "needs the model; set SILERO_MODEL"]
fn it_fires_on_speech() {
    for (name, bytes) in [
        (
            "speech-short (0.7 s)",
            &include_bytes!("fixtures/speech-short.wav")[..],
        ),
        (
            "speech-long (5.4 s)",
            &include_bytes!("fixtures/speech-long.wav")[..],
        ),
    ] {
        let (silero, rms) = sweep(&wav(bytes));
        println!("{name:<22} silero {silero:>5.1}%   rms {rms:>5.1}%");
        // Measured ~92%; the floor allows for the clips' silent edges and the first window.
        assert!(
            silero >= 75.0,
            "{name}: silero called only {silero:.1}% of frames speech"
        );
    }
}

#[test]
#[ignore = "needs the model; set SILERO_MODEL"]
fn it_is_not_fooled_by_the_room() {
    for (name, samples) in [
        ("fan @0.01", fan_noise()),
        ("fridge hum @0.02", fridge_hum()),
    ] {
        let (silero, rms) = sweep(&samples);
        println!("{name:<22} silero {silero:>5.1}%   rms {rms:>5.1}%");

        // Measured 0.0% on both.
        assert!(
            silero <= 5.0,
            "{name}: silero called {silero:.1}% of steady noise speech"
        );

        // The control (measured 100%): if the gate stops failing here, the premise has moved.
        assert!(
            rms >= 95.0,
            "{name}: the energy gate called only {rms:.1}% speech — it used to \
             call 100%, so the failure this detector was added to fix has changed"
        );
    }
}

#[test]
#[ignore = "needs the model; set SILERO_MODEL"]
fn both_agree_that_silence_is_silence() {
    let (silero, rms) = sweep(&vec![0.0; WINDOW * 60]);
    println!(
        "{:<22} silero {silero:>5.1}%   rms {rms:>5.1}%",
        "digital silence"
    );
    assert_eq!(silero, 0.0);
    assert_eq!(rms, 0.0);
}
