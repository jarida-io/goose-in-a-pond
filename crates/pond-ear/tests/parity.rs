//! The hand-written engine reproduces Kaldi and ONNX Runtime, rung by rung, on formula inputs
//! (goldens from oracle/golden.py). Models the repo does not carry are used when the pond has them.

mod common;

use common::*;
use pond_ear::engine::exec;
use pond_ear::engine::fbank::{self, NUM_BINS, SHIFT};
use pond_ear::engine::model::Arch;
use pond_ear::engine::silero::{step, Scratch, State, CHUNK};
use pond_ear::{ModelFile, Rung};

fn rungs() -> Vec<Rung> {
    Rung::ALL.iter().copied().filter(Rung::available).collect()
}

fn is_int8(r: Rung) -> bool {
    r.name().contains("i8")
}

#[test]
fn fbank_matches_kaldi() {
    let clip = fbank_clip();
    let want = golden("fbank.bin");
    let n = fbank::num_frames(clip.len());
    assert_eq!(n * NUM_BINS, want.len());
    let mut worst = 0f32;
    let mut got = [0f32; NUM_BINS];
    for t in 0..n {
        fbank::frame(&clip[t * SHIFT..], &mut got);
        for (g, w) in got.iter().zip(&want[t * NUM_BINS..(t + 1) * NUM_BINS]) {
            worst = worst.max((g - w).abs());
        }
    }
    eprintln!("fbank vs kaldi-native-fbank: max|err| {worst:.2e} over {n} frames");
    assert!(worst < 1e-3, "fbank parity {worst}");
}

#[test]
fn the_embedded_wake_model_matches_onnx_runtime_on_every_rung() {
    let file = pond_ear::wake::hey_goose().unwrap();
    let model = file.model();
    let plan = exec::plan(&model, 120).unwrap();
    let want = golden("wake.bin");
    for rung in rungs() {
        let mut arena = vec![0f32; plan.arena];
        let mut worst = 0f32;
        for (w, &y) in wake_windows().iter().zip(&want) {
            let mut out = [0f32];
            exec::run(&model, &plan, rung, w, &mut arena, &mut out, None).unwrap();
            worst = worst.max((out[0] - y).abs());
            if y.abs() > 0.5 {
                assert_eq!(
                    out[0] >= 0.0,
                    y >= 0.0,
                    "{}: a confident decision flipped",
                    rung.name()
                );
            }
        }
        eprintln!(
            "hey goose {:<13} worst |logit err| {worst:.2e} (logits {want:?})",
            rung.name()
        );
        let bound = if is_int8(rung) { 0.5 } else { 1e-3 };
        assert!(worst <= bound, "{}: logit error {worst}", rung.name());
    }
}

#[test]
fn rungs_agree_on_the_wake_model_whatever_the_input() {
    let file = pond_ear::wake::hey_goose().unwrap();
    let model = file.model();
    let plan = exec::plan(&model, 120).unwrap();
    let mut arena = vec![0f32; plan.arena];
    for seed in 0..8 {
        let w = lcg_features(120, 100 + seed);
        let mut reference = [0f32];
        exec::run(
            &model,
            &plan,
            Rung::Scalar,
            &w,
            &mut arena,
            &mut reference,
            None,
        )
        .unwrap();
        for rung in rungs() {
            let mut out = [0f32];
            exec::run(&model, &plan, rung, &w, &mut arena, &mut out, None).unwrap();
            let bound = if is_int8(rung) {
                0.5
            } else {
                1e-4 * reference[0].abs().max(1.0)
            };
            assert!(
                (out[0] - reference[0]).abs() <= bound,
                "{} seed {seed}: {} vs {}",
                rung.name(),
                out[0],
                reference[0]
            );
        }
    }
}

#[test]
fn silero_matches_onnx_runtime_with_the_official_context() {
    let Some(path) = silero() else { return };
    let file = ModelFile::read(&path).unwrap();
    let model = file.model();
    assert_eq!(model.arch, Arch::Vad);
    let audio = silero_stream();
    let want = golden("silero.bin");
    assert_eq!(audio.len() / CHUNK, want.len());
    for rung in rungs() {
        let (mut st, mut s) = (State::new(), Box::new(Scratch::new()));
        let (mut worst, mut flips) = (0f32, 0);
        for (i, &w) in want.iter().enumerate() {
            let p = step(
                rung,
                model.w32,
                &mut st,
                &mut s,
                &audio[i * CHUNK..(i + 1) * CHUNK],
            );
            worst = worst.max((p - w).abs());
            flips += ((p >= 0.5) != (w >= 0.5)) as usize;
        }
        let speech = want.iter().filter(|&&p| p >= 0.5).count();
        eprintln!("silero {:<13} worst |p err| {worst:.2e}, decision flips {flips}/{} ({speech} speech chunks)", rung.name(), want.len());
        assert!(worst <= 1e-4, "{}: worst {worst}", rung.name());
        assert_eq!(flips, 0);
    }
}

#[test]
fn resnet34_matches_onnx_runtime() {
    let Some(path) = speaker() else { return };
    let file = ModelFile::read(&path).unwrap();
    let model = file.model();
    assert_eq!(model.arch, Arch::ResNet);
    let feats = resnet_features();
    let want = golden("resnet34.bin");
    let plan = exec::plan(&model, 200).unwrap();
    for rung in rungs() {
        let mut arena = vec![0f32; plan.arena];
        let mut emb = vec![0f32; model.emb_dim];
        exec::run(&model, &plan, rung, &feats, &mut arena, &mut emb, None).unwrap();
        let cos = cosine(&emb, &want);
        eprintln!(
            "resnet34 {:<13} 1 - cos {:.2e} ({:.2} GMAC)",
            rung.name(),
            1.0 - cos,
            plan.macs as f64 / 1e9
        );
        let gate = if is_int8(rung) { 0.98 } else { 0.9999 };
        assert!(cos >= gate, "{}: cosine {cos}", rung.name());
    }
}
