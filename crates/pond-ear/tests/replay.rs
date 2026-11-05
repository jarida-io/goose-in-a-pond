//! The ear gives the same events bit for bit whatever the chunk size, scores as the reference
//! detector does, and endpoints and reports barge-ins on the echo-cancelled signal. Synthetic audio.

mod common;

use common::*;
use pond_ear::ear::{AecConfig, SpeakerConfig, WakeConfig};
use pond_ear::score::{normalized, Print};
use pond_ear::wake::detector::Detector;
use pond_ear::{Config, Ear, Event, Household, ModelFile, Rung, State};

const S: usize = RATE;

fn vad_model() -> ModelFile {
    match silero() {
        Some(p) => ModelFile::read(&p).unwrap(),
        None => synthetic_silero(7),
    }
}

fn config(threshold: f32, aec: bool) -> Config {
    let mut wake = WakeConfig::hey_goose().unwrap();
    wake.threshold = threshold;
    let mut cfg = Config::new(wake, vad_model());
    cfg.speaker = Some(SpeakerConfig {
        model: synthetic_speaker(42),
        label: "synthetic".into(),
        max_seconds: 3.0,
    });
    cfg.aec = aec.then(AecConfig::default);
    cfg
}

/// What the host does, at fixed microphone positions so every chunking sees it at the same sample.
#[derive(Clone, Copy, Debug)]
enum Act {
    Listen,
    Set(State),
}

/// Thirty seconds of mixed voices, noise and quiet, with the pond's reply playing from 14 to 22 s
/// and its echo (through a small room) in the microphone, and someone talking over it at 18 s.
struct Stream {
    mic: Vec<f32>,
    far: Vec<f32>,
    acts: Vec<(usize, Act)>,
}

fn stream() -> Stream {
    let a = voice(4 * S, 51, 115.0, [650.0, 1_150.0, 2_450.0]);
    let b = voice(3 * S, 52, 210.0, [820.0, 1_450.0, 2_950.0]);
    let c = voice(6 * S, 53, 140.0, [700.0, 1_250.0, 2_600.0]);
    let mut near = [
        a,
        silence(S),
        b,
        noise(S, 54, 0.02),
        silence(5 * S),
        c.clone(),
        silence(4 * S),
        c[..6 * S].to_vec(),
    ]
    .concat();
    near.resize(30 * S, 0.0);
    // The talker over the reply is quiet until 18 s.
    for v in near[14 * S..18 * S].iter_mut() {
        *v = 0.0;
    }
    let reply = voice(8 * S, 55, 180.0, [760.0, 1_300.0, 2_800.0]);
    let mut far = vec![0f32; 30 * S];
    far[14 * S..22 * S].copy_from_slice(&reply);
    let h = room(
        2_400,
        480,
        0.2,
        56,
        [(41, 0.25), (97, -0.15), (150, 0.1), (210, -0.08)],
    );
    let echo = convolve(&far, &h);
    let mic = (0..30 * S).map(|i| near[i] + echo[i]).collect();
    let acts = vec![
        (5 * S, Act::Listen),
        (13 * S, Act::Set(State::Think)),
        (14 * S, Act::Set(State::Speak)),
        (22 * S, Act::Set(State::Wait)),
    ];
    Stream { mic, far, acts }
}

/// The stream through `ear` in `chunk`-sized pushes cut at the host's actions, the far end 100 ms
/// ahead; events `Debug`-printed so floats compare bit for bit.
fn replay(ear: &mut Ear, st: &Stream, chunk: usize) -> Vec<String> {
    let mut events = Vec::new();
    let (mut m, mut played) = (0, 0);
    let mut acts = st.acts.iter().peekable();
    while m < st.mic.len() {
        let mut end = (m + chunk).min(st.mic.len());
        if let Some(&&(at, _)) = acts.peek() {
            end = end.min(at.max(m + 1));
        }
        let want = (end + 1_600).min(st.far.len());
        if want > played {
            ear.play(played, &st.far[played..want]);
            played = want;
        }
        events.extend(ear.push(&st.mic[m..end]));
        m = end;
        while let Some(&&(at, act)) = acts.peek() {
            if at > m {
                break;
            }
            acts.next();
            let e = match act {
                Act::Listen => ear.listen(ear.front().samples()),
                Act::Set(s) => ear.set_state(s),
            };
            events.extend(e);
        }
    }
    events.iter().map(|e| format!("{e:?}")).collect()
}

/// A threshold that the synthetic stream crosses a few times, so there are wakes and cooldowns.
fn busy_threshold(st: &Stream) -> f32 {
    let mut ear = Ear::new(config(2.0, false), Household::default()).unwrap();
    ear.push(&st.mic);
    let mut p: Vec<f32> = ear.take_scores().iter().map(|s| s.1).collect();
    p.sort_by(|a, b| b.total_cmp(a));
    p[30]
}

fn household(st: &Stream) -> Household {
    let mut ear = Ear::new(config(2.0, false), Household::default()).unwrap();
    ear.push(&st.mic[..8 * S]);
    let embs = [(0, S), (S, 2 * S), (2 * S, 3 * S)]
        .iter()
        .map(|&(a, b)| normalized(&ear.embed(a, b).unwrap()))
        .collect();
    Household {
        prints: vec![Print {
            id: "a".into(),
            name: "A".into(),
            embs,
        }],
        ..Household::default()
    }
}

#[test]
fn a_replay_gives_the_same_events_bit_for_bit_whatever_the_chunking() {
    let st = stream();
    let threshold = busy_threshold(&st);
    let home = household(&st);
    let mut runs = Vec::new();
    for chunk in [1usize, 7, 511, 1_600, 50_000] {
        let mut ear = Ear::new(config(threshold, true), home.clone()).unwrap();
        runs.push((chunk, replay(&mut ear, &st, chunk)));
    }
    let (_, first) = &runs[0];
    for (chunk, ev) in &runs[1..] {
        assert_eq!(
            ev.len(),
            first.len(),
            "chunk {chunk}: {} events vs {}",
            ev.len(),
            first.len()
        );
        for (a, b) in ev.iter().zip(first) {
            assert_eq!(a, b, "chunk {chunk}");
        }
    }
    let count = |k: &str| first.iter().filter(|e| e.starts_with(k)).count();
    eprintln!(
        "{} events identical across 5 chunkings: {} wake, {} state, {} speech, {} utterance, {} silence, {} barge-in",
        first.len(),
        count("Wake"),
        count("State"),
        count("Speech"),
        count("Utterance"),
        count("Silence"),
        count("BargeIn")
    );
    assert!(count("Wake") >= 2 && count("State") >= 6);
}

#[test]
fn a_replay_is_the_same_after_reset() {
    let st = stream();
    let threshold = busy_threshold(&st);
    let mut ear = Ear::new(config(threshold, true), household(&st)).unwrap();
    let a = replay(&mut ear, &st, 1_600);
    ear.reset();
    let b = replay(&mut ear, &st, 1_600);
    assert_eq!(a, b);
}

#[test]
fn the_ear_scores_and_triggers_exactly_as_the_reference_detector() {
    let st = stream();
    let threshold = busy_threshold(&st);
    let rung = Rung::fastest();
    let mut det = Detector::new(pond_ear::wake::hey_goose().unwrap(), rung, threshold).unwrap();
    let (mut want_scores, mut want_trig) = (Vec::new(), Vec::new());
    for c in st.mic.chunks(1_600) {
        let (s, t) = det.push(c);
        want_scores.extend(s.iter().map(|x| (x.t, x.p.to_bits())));
        want_trig.extend(t.iter().map(|x| (x.t, x.start, x.end)));
    }
    for aec in [false, true] {
        let mut cfg = config(threshold, aec);
        cfg.rung = rung;
        let mut ear = Ear::new(cfg, Household::default()).unwrap();
        let (mut scores, mut trig) = (Vec::new(), Vec::new());
        // No far end: with the canceller in the path, it must pass the microphone through untouched.
        for c in st.mic.chunks(997) {
            for e in ear.push(c) {
                if let Event::Wake { t, start, end, .. } = e {
                    trig.push((t, start, end));
                }
            }
            scores.extend(ear.take_scores().iter().map(|s| (s.0, s.1.to_bits())));
        }
        let n = scores.len();
        assert!(
            n + 2 >= want_scores.len(),
            "aec {aec}: {n} hops vs {}",
            want_scores.len()
        );
        assert_eq!(scores[..], want_scores[..n], "aec {aec}");
        let m = trig.len();
        assert_eq!(trig[..], want_trig[..m], "aec {aec}");
        eprintln!(
            "aec {aec}: {n} hops and {m} triggers identical to the detector over {:.0} s",
            st.mic.len() as f64 / S as f64
        );
    }
}

#[test]
fn loading_costs_what_the_ear_says_it_holds() {
    let dir = std::env::temp_dir().join(format!("pond-ear-measure-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // .vmw files, as the pond will load them: an in-process ONNX parse fragments the heap it frees.
    let vmw = |name: &str, onnx: Option<std::path::PathBuf>, fallback: ModelFile| {
        let p = dir.join(name);
        let bytes = match onnx {
            Some(o) => pond_ear::convert::convert_onnx(&std::fs::read(o).unwrap()).unwrap(),
            None => fallback.bytes().to_vec(),
        };
        std::fs::write(&p, bytes).unwrap();
        p
    };
    let vad = vmw("silero.vmw", silero(), synthetic_silero(7));
    let speaker = vmw("speaker.vmw", common::speaker(), synthetic_speaker(42));
    // A fresh process: this one has loaded and dropped ears, and its allocator would recycle pages.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_pond-ear"))
        .args(["measure", "--vad"])
        .arg(&vad)
        .arg("--speaker")
        .arg(&speaker)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let line = String::from_utf8(out.stdout).unwrap();
    let num = |key: &str| -> Vec<f64> {
        line.split(&format!("\"{key}\":"))
            .skip(1)
            .map(|rest| rest.split([',', '}']).next().unwrap().parse().unwrap())
            .collect()
    };
    let grew = num("footprint_after")[0] - num("footprint_before")[0];
    let accounted: f64 = ["weights", "arena", "state"]
        .iter()
        .map(|k| num(k).iter().sum::<f64>())
        .sum();
    let ratio = grew / accounted;
    eprintln!(
        "accounted {:.2} MB, the OS saw the footprint grow {:.2} MB ({:.0}%)",
        accounted / 1e6,
        grew / 1e6,
        ratio * 100.0
    );
    // A few hundred KB of allocator and conversion slack is noise against tens of MB, not against 6.
    let slack = 0.1 + 0.6e6 / accounted;
    assert!(
        (1.0 - slack..=1.0 + slack).contains(&ratio),
        "footprint grew {:.0}% of the accounted bytes",
        ratio * 100.0
    );
}

#[test]
fn an_utterance_ends_800_ms_after_the_speech_and_a_lone_listen_times_out() {
    let Some(vad) = silero() else { return };
    let mut wake = WakeConfig::hey_goose().unwrap();
    wake.threshold = 2.0;
    let mut cfg = Config::new(wake, ModelFile::read(&vad).unwrap());
    cfg.aec = None;
    let mut ear = Ear::new(cfg, Household::default()).unwrap();
    let command = voice(2 * S, 61, 125.0, [680.0, 1_200.0, 2_500.0]);
    let speech_end = S / 2 + pond_ear::dsp::quality(&command).speech_end;
    let audio = [silence(S / 2), command, silence(2 * S)].concat();
    let mut ev: Vec<Event> = ear.listen(0).into_iter().collect();
    for c in audio.chunks(1_600) {
        ev.extend(ear.push(c));
    }
    let utterances: Vec<&Event> = ev
        .iter()
        .filter(|e| matches!(e, Event::Utterance { .. }))
        .collect();
    let [Event::Utterance { t, start, end }] = utterances[..] else {
        panic!("{ev:?}")
    };
    assert_eq!(*start, 0);
    assert!(
        end.abs_diff(speech_end) <= 1_600,
        "end {end}, speech ended at {speech_end}"
    );
    let quiet_ms = (t - speech_end as f64 / S as f64) * 1e3;
    eprintln!("endpoint fired {quiet_ms:.0} ms after the speech ended (rule: 800 ms of quiet); utterance ends {} ms from the speech's end", (*end as f64 - speech_end as f64) / 16.0);
    assert!((780.0..=900.0).contains(&quiet_ms), "{quiet_ms} ms");
    assert_eq!(ear.state(), State::Wait);

    let mut ev: Vec<Event> = ear.listen(ear.front().samples()).into_iter().collect();
    for c in silence(6 * S).chunks(1_600) {
        ev.extend(ear.push(c));
    }
    assert_eq!(
        ev.iter()
            .filter(|e| matches!(e, Event::Silence { .. }))
            .count(),
        1,
        "{ev:?}"
    );
}

#[test]
fn the_replys_echo_does_not_barge_in_but_a_voice_over_it_does() {
    let Some(vad) = silero() else { return };
    let reply = |seed| voice(8 * S, seed, 175.0, [740.0, 1_280.0, 2_750.0]);
    let (r1, r2) = (reply(71), reply(72));
    let talker = voice(5 * S, 73, 235.0, [880.0, 1_650.0, 3_050.0]);
    // Reply 1 teaches the canceller the room; reply 2 is talked over 3 s in.
    let far = [r1, silence(S), r2].concat();
    let h = room(
        2_800,
        720,
        0.25,
        74,
        [(45, 0.3), (88, -0.2), (170, 0.12), (240, -0.08)],
    );
    let echo: Vec<f32> = convolve(&far, &h).iter().map(|v| v * 1.4).collect();
    let over = 9 * S + 3 * S;
    let mic: Vec<f32> = (0..far.len())
        .map(|i| {
            echo[i]
                + if i >= over && i - over < talker.len() {
                    talker[i - over]
                } else {
                    0.0
                }
        })
        .collect();

    for aec in [true, false] {
        let mut wake = WakeConfig::hey_goose().unwrap();
        wake.threshold = 2.0;
        let mut cfg = Config::new(wake, ModelFile::read(&vad).unwrap());
        cfg.aec = aec.then(AecConfig::default);
        let mut ear = Ear::new(cfg, Household::default()).unwrap();
        let mut barges = Vec::new();
        let mut fired = None;
        let mut played = 0;
        for (k, c) in mic.chunks(160).enumerate() {
            let m = k * 160;
            if m == 0 || m == 9 * S {
                ear.set_state(State::Speak);
            } else if m == 8 * S {
                ear.set_state(State::Wait);
            }
            let want = (m + 160 + 1_600).min(far.len());
            if want > played {
                ear.play(played, &far[played..want]);
                played = want;
            }
            for e in ear.push(c) {
                if let Event::BargeIn { start, t, .. } = e {
                    barges.push(start);
                    if start >= over && fired.is_none() {
                        fired = Some(t - over as f64 / S as f64);
                    }
                    ear.set_state(State::Speak);
                }
            }
        }
        let early: Vec<f64> = barges
            .iter()
            .filter(|&&s| (9 * S..over).contains(&s))
            .map(|&s| s as f64 / S as f64)
            .collect();
        let first_over = barges
            .iter()
            .find(|&&s| s >= over - S / 10)
            .map(|&s| (s as f64 - over as f64) / S as f64);
        let learning: Vec<f64> = barges
            .iter()
            .filter(|&&s| s < 8 * S)
            .map(|&s| s as f64 / S as f64)
            .collect();
        eprintln!("aec {aec}: barge-ins while learning the room {learning:?} s; on the echo alone {early:?} s; first over the talker starts {first_over:?} s into it, reported {fired:?} s in");
        if aec {
            assert!(
                early.is_empty(),
                "the reply's own echo barged in at {early:?} s"
            );
            // Silero needs about two syllables of this synthetic talker, then 300 ms of speech.
            assert!(
                first_over.is_some_and(|d| d < 1.0),
                "no timely barge-in for the talker"
            );
        } else {
            assert!(
                !early.is_empty(),
                "without the canceller the echo should look like a voice"
            );
        }
    }
}

#[test]
fn with_segments_on_the_ear_reports_speech_outside_listen_and_never_runs_the_cnn() {
    let st = stream();
    let mut runs = Vec::new();
    for chunk in [1usize, 511, 1_600, 50_000] {
        // At threshold 0 the CNN would fire on its first hop, if it ran at all.
        let mut cfg = config(0.0, true);
        cfg.segments = true;
        let mut ear = Ear::new(cfg, Household::default()).unwrap();
        assert!(ear.segments());
        let ev = replay(&mut ear, &st, chunk);
        assert!(
            ear.take_scores().is_empty(),
            "chunk {chunk}: the wake CNN ran"
        );
        runs.push((chunk, ev));
    }
    let (_, first) = &runs[0];
    for (chunk, ev) in &runs[1..] {
        assert_eq!(ev, first, "chunk {chunk}");
    }
    assert!(!first.iter().any(|e| e.starts_with("Wake")), "{first:?}");

    let mut cfg = config(0.0, true);
    cfg.segments = true;
    let mut ear = Ear::new(cfg, Household::default()).unwrap();
    let mut segments = Vec::new();
    for (k, c) in st.mic.chunks(1_600).enumerate() {
        let m = k * 1_600;
        if let Some(&(_, act)) = st.acts.iter().find(|(at, _)| *at == m) {
            match act {
                Act::Listen => {
                    ear.listen(ear.front().samples());
                }
                Act::Set(s) => {
                    ear.set_state(s);
                }
            }
        }
        for e in ear.push(c) {
            if let Event::Segment { start, end, .. } = e {
                assert!(
                    ear.state() != State::Listen,
                    "a segment was reported while listening"
                );
                assert!(start < end && end <= ear.front().samples());
                segments.push((start, end));
            }
        }
    }
    eprintln!(
        "segments {:?} s",
        segments
            .iter()
            .map(|&(s, e)| (s as f64 / S as f64, e as f64 / S as f64))
            .collect::<Vec<_>>()
    );
    if silero().is_some() {
        assert!(
            segments.iter().any(|&(s, e)| s < S / 2 && e > 3 * S),
            "the first voice (0 to 4 s) was not one segment: {segments:?}"
        );
    }
}
