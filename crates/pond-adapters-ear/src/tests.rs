//! The ear thread's rules on a scripted ear and a scripted microphone running 20 times real time.

use super::*;
use pond_audio::testing::ScriptedCapture;
use pond_core::models::ports::interrupt::InterruptReason;
use pond_ear::{Event, State};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Default)]
struct Log {
    states: Vec<State>,
    listens: Vec<usize>,
    played: Vec<(usize, usize)>,
}

/// Emits what the test injects at its next push, and moves states as the real ear does.
struct FakeEar {
    log: Arc<Mutex<Log>>,
    inbox: Arc<Mutex<VecDeque<Event>>>,
    pushed: Arc<AtomicUsize>,
    audio: Vec<f32>,
    state: State,
    household: Household,
    /// Whose voice every span is.
    voice: Arc<Mutex<Option<String>>>,
}

impl Hearing for FakeEar {
    fn identify(&mut self, _start: usize, _end: usize) -> Option<String> {
        self.voice.lock().unwrap().clone()
    }

    fn set_speaker(&mut self, _speaker: pond_ear::ear::parts::SpeakerNet) {}

    fn push_into(&mut self, mic: &[f32], out: &mut Vec<Event>) {
        self.audio.extend_from_slice(mic);
        self.pushed.fetch_add(mic.len(), Ordering::SeqCst);
        let injected: Vec<Event> = self.inbox.lock().unwrap().drain(..).collect();
        for e in injected {
            match e {
                Event::Wake { .. } => self.state = State::Listen,
                Event::Utterance { .. } | Event::Silence { .. } => self.state = State::Wait,
                _ => {}
            }
            out.push(e);
        }
    }

    fn play(&mut self, at: usize, pcm: &[f32]) {
        self.log.lock().unwrap().played.push((at, pcm.len()));
    }

    fn set_state(&mut self, to: State) -> Option<Event> {
        self.log.lock().unwrap().states.push(to);
        self.state = to;
        None
    }

    fn listen(&mut self, start: usize) -> Option<Event> {
        self.log.lock().unwrap().listens.push(start);
        self.state = State::Listen;
        None
    }

    fn audio(&self, start: usize, end: usize, out: &mut Vec<f32>) {
        out.clear();
        let end = end.min(self.audio.len());
        out.extend_from_slice(&self.audio[start.min(end)..end]);
    }

    fn state(&self) -> State {
        self.state
    }

    fn mic_samples(&self) -> usize {
        self.audio.len()
    }

    fn samples(&self) -> usize {
        self.audio.len()
    }

    fn set_household(&mut self, household: Household) {
        self.household = household;
    }

    fn household(&self) -> &Household {
        &self.household
    }
}

struct Ctl {
    log: Arc<Mutex<Log>>,
    inbox: Arc<Mutex<VecDeque<Event>>>,
    pushed: Arc<AtomicUsize>,
    mic: MicHandle,
    voice: Arc<Mutex<Option<String>>>,
}

impl Ctl {
    fn at(&self) -> usize {
        self.pushed.load(Ordering::SeqCst)
    }

    fn emit(&self, e: Event) {
        self.inbox.lock().unwrap().push_back(e);
    }

    async fn until(&self, what: &str, cond: impl Fn(&Log, usize) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !cond(&self.log.lock().unwrap(), self.at()) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    fn listens(&self) -> Vec<usize> {
        self.log.lock().unwrap().listens.clone()
    }
}

impl Drop for Ctl {
    fn drop(&mut self) {
        self.mic.shutdown();
    }
}

/// Says each scripted line in turn, then nothing; remembers how much audio it was given.
#[derive(Default)]
struct Says {
    lines: Mutex<VecDeque<String>>,
    heard: Mutex<Vec<usize>>,
}

impl Says {
    fn new(lines: &[&str]) -> Arc<Says> {
        Arc::new(Says {
            lines: Mutex::new(lines.iter().map(|s| s.to_string()).collect()),
            heard: Mutex::default(),
        })
    }

    fn calls(&self) -> usize {
        self.heard.lock().unwrap().len()
    }
}

impl Transcriber for Says {
    fn transcribe(&self, pcm: &[f32]) -> Result<String> {
        self.heard.lock().unwrap().push(pcm.len());
        Ok(self.lines.lock().unwrap().pop_front().unwrap_or_default())
    }
}

const S: usize = 16_000;

async fn rig(wake: WakeMode, says: Arc<Says>) -> (Arc<EarRig>, Ctl) {
    rig_for(wake, says, "hey goose").await
}

async fn rig_for(wake: WakeMode, says: Arc<Says>, phrase: &str) -> (Arc<EarRig>, Ctl) {
    let capture = ScriptedCapture::new(vec![0.0; S * 600], 20).paced(1);
    let (mic, _owner) = pond_audio::spawn(Box::new(capture), 16_000, 15_000, true);
    let log = Arc::new(Mutex::new(Log::default()));
    let inbox = Arc::new(Mutex::new(VecDeque::new()));
    let pushed = Arc::new(AtomicUsize::new(0));
    let voice = Arc::new(Mutex::new(None));
    let fake = FakeEar {
        log: log.clone(),
        inbox: inbox.clone(),
        pushed: pushed.clone(),
        audio: Vec::new(),
        state: State::Wait,
        household: Household::default(),
        voice: voice.clone(),
    };
    let triggers = pond_voice::text::wake_triggers(phrase, &[]);
    let cfg = EarConfig::new(wake, triggers, format!("say \"{phrase}\""));
    let rig = EarRig::with_hearing(cfg, Box::new(fake), mic.clone(), says).unwrap();
    let ctl = Ctl {
        log,
        inbox,
        pushed,
        mic,
        voice,
    };
    ctl.until("two seconds of audio", |_, at| at >= 2 * S).await;
    (rig, ctl)
}

async fn bounded<T>(what: &str, f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .unwrap_or_else(|_| panic!("{what} never finished"))
}

fn wake(start: usize, end: usize) -> Event {
    Event::Wake {
        t: 0.0,
        p: 0.99,
        start,
        end,
        who: None,
        score: f32::NAN,
    }
}

fn utterance(start: usize, end: usize) -> Event {
    Event::Utterance { t: 0.0, start, end }
}

fn segment(start: usize, end: usize) -> Event {
    Event::Segment {
        t: 0.0,
        start,
        end,
        cut: false,
    }
}

fn barge(start: usize, end: usize, who: Option<&str>) -> Event {
    Event::BargeIn {
        t: 0.0,
        start,
        end,
        who: who.map(str::to_string),
        score: f32::NAN,
    }
}

#[tokio::test]
async fn a_hey_goose_wake_hands_over_its_endpointed_request_and_records_nothing_twice() {
    let says = Says::new(&["Hey goose, what time is it?"]);
    let (rig, ctl) = rig(WakeMode::HeyGoose, says.clone()).await;
    let waiting = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_activation_with_audio().await }
    });
    let at = ctl.at();
    ctl.emit(wake(at - S, at - S / 2));
    ctl.emit(utterance(at - S, at));
    let activation = bounded("the wake", waiting).await.unwrap().unwrap();
    assert!(activation.endpointed);
    assert!(activation.speaker.is_none());
    let wav = activation.captured_audio.unwrap();
    assert_eq!(
        wav.len(),
        44 + 2 * S,
        "the endpointed utterance, as 16-bit WAV"
    );

    rig.prime_with_captured(wav);
    let heard = bounded("the listen", rig.listen()).await.unwrap();
    assert_eq!(heard.as_deref(), Some("what time is it?"));
    assert!(
        ctl.listens().is_empty(),
        "the ear was asked to listen again"
    );
    assert_eq!(
        *says.heard.lock().unwrap(),
        vec![S],
        "one transcription, of the utterance"
    );
}

#[tokio::test]
async fn a_follow_up_listens_from_now_and_ends_on_an_utterance_or_silence() {
    let says = Says::new(&["and tomorrow?"]);
    let (rig, ctl) = rig(WakeMode::HeyGoose, says).await;
    let before = ctl.at();
    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the ear to listen", |l, _| l.listens.len() == 1)
        .await;
    let from = ctl.listens()[0];
    assert!(
        from >= before && from <= ctl.at(),
        "listened from {from}, before {before}"
    );
    ctl.emit(utterance(from, ctl.at()));
    let heard = bounded("the follow-up", listening).await.unwrap().unwrap();
    assert_eq!(heard.as_deref(), Some("and tomorrow?"));

    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the second listen", |l, _| l.listens.len() == 2)
        .await;
    ctl.emit(Event::Silence { t: 0.0 });
    let heard = bounded("the silent listen", listening)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        heard.as_deref(),
        Some(""),
        "silence sends the loop back to the wake word"
    );
}

#[tokio::test]
async fn the_recording_behind_a_request_is_handed_over_once_and_never_for_silence() {
    let says = Says::new(&["and tomorrow?"]);
    let (rig, ctl) = rig(WakeMode::HeyGoose, says).await;
    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the ear to listen", |l, _| l.listens.len() == 1)
        .await;
    let from = ctl.listens()[0];
    let to = ctl.at();
    ctl.emit(utterance(from, to));
    let heard = bounded("the request", listening).await.unwrap().unwrap();
    assert_eq!(heard.as_deref(), Some("and tomorrow?"));
    let wav = rig.take_heard_audio().expect("the request's recording");
    let decoded = pond_voice::dsp::decode_wav(&wav).unwrap();
    assert_eq!(decoded.sample_rate, 16_000);
    assert_eq!(decoded.samples.len(), to - from, "the utterance, as heard");
    assert!(rig.take_heard_audio().is_none(), "handed over once");

    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the second listen", |l, _| l.listens.len() == 2)
        .await;
    ctl.emit(Event::Silence { t: 0.0 });
    let heard = bounded("the silent listen", listening)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(heard.as_deref(), Some(""));
    assert!(rig.take_heard_audio().is_none(), "nothing was said");
}

#[tokio::test]
async fn speech_the_transcriber_cannot_word_reaches_only_a_model_that_hears() {
    for forward in [true, false] {
        let (rig, ctl) = rig(WakeMode::HeyGoose, Says::new(&[""])).await;
        rig.set_forward_unworded(forward);
        let listening = tokio::spawn({
            let rig = rig.clone();
            async move { rig.listen().await }
        });
        ctl.until("the ear to listen", |l, _| l.listens.len() == 1)
            .await;
        let from = ctl.listens()[0];
        ctl.until("a second of speech", |_, at| at >= from + S)
            .await;
        ctl.emit(utterance(from, ctl.at()));
        if !forward {
            ctl.until("the listen after the noise", |l, _| l.listens.len() == 2)
                .await;
            ctl.emit(Event::Silence { t: 0.0 });
        }
        let heard = bounded("the listen", listening).await.unwrap().unwrap();
        match forward {
            true => {
                assert_eq!(heard.as_deref(), Some(UNWORDED_SPEECH));
                assert!(
                    rig.take_heard_audio().is_some(),
                    "the recording goes with it"
                );
            }
            false => {
                assert_eq!(
                    heard.as_deref(),
                    Some(""),
                    "taken for noise, and the silence after it ends the listen"
                );
                assert!(rig.take_heard_audio().is_none());
            }
        }
    }
}

#[test]
fn a_quiet_recording_is_raised_for_the_model_and_silence_is_left_alone() {
    let quiet: Vec<f32> = (0..1600).map(|i| 0.01 * ((i as f32) * 0.3).sin()).collect();
    let (raised, peak, gain) = raised_for_the_model(quiet);
    assert!((peak - 0.01).abs() < 1e-3);
    assert!((gain - 31.6).abs() < 0.1, "capped at 30 dB, got {gain}");
    assert!(raised.iter().all(|v| v.abs() <= 1.0));
    let loud: Vec<f32> = vec![0.9, -0.9];
    assert_eq!(
        raised_for_the_model(loud.clone()),
        (loud, 0.9, 1.0),
        "never turned down"
    );
    let (silent, _, gain) = raised_for_the_model(vec![0.0; 100]);
    assert_eq!((silent.len(), gain), (100, 1.0));
}

#[tokio::test]
async fn only_an_enrolled_voice_over_the_reply_interrupts_it() {
    let (rig, ctl) = rig(WakeMode::HeyGoose, Says::new(&[])).await;
    let hook = rig.state_hook();
    hook(WorkflowState::Thinking);
    hook(WorkflowState::Speak);
    ctl.until("Think then Speak", |l, _| {
        l.states == [State::Think, State::Speak]
    })
    .await;
    let mut watch = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_interrupt().await }
    });
    let at = ctl.at();
    ctl.emit(barge(at - 4_800, at, None));
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut watch)
            .await
            .is_err(),
        "a voice nobody enrolled interrupted the reply"
    );
    let at = ctl.at();
    ctl.emit(barge(at - 4_800, at, Some("p-ana")));
    let interrupt = bounded("the interrupt", watch).await.unwrap();
    assert_eq!(interrupt.why, InterruptReason::Voice);

    rig.prime_with_onset(interrupt.onset);
    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the listen from the onset", |l, _| l.listens.len() == 1)
        .await;
    assert_eq!(
        ctl.listens()[0],
        at - 4_800,
        "the follow-up starts where the voice began"
    );
    ctl.emit(Event::Silence { t: 0.0 });
    let _ = bounded("the follow-up", listening).await;
}

#[tokio::test]
async fn the_wake_word_over_the_reply_interrupts_it_and_its_request_is_the_follow_up() {
    let says = Says::new(&["hey goose stop"]);
    let (rig, ctl) = rig(WakeMode::HeyGoose, says).await;
    let hook = rig.state_hook();
    hook(WorkflowState::Thinking);
    hook(WorkflowState::Speak);
    let watch = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_interrupt().await }
    });
    ctl.until("Speak", |l, _| l.states.len() == 2).await;
    let at = ctl.at();
    ctl.emit(wake(at - 8_000, at - 1_000));
    let interrupt = bounded("the interrupt", watch).await.unwrap();
    assert_eq!(interrupt.why, InterruptReason::WakeWord);

    // A last Speak from the ended turn must not cancel the listen the wake started.
    hook(WorkflowState::Speak);
    hook(WorkflowState::Listen);
    rig.prime_with_onset(interrupt.onset);
    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    ctl.emit(utterance(at - 8_000, ctl.at()));
    let heard = bounded("the follow-up", listening).await.unwrap().unwrap();
    assert_eq!(heard.as_deref(), Some("stop"));
    assert!(
        ctl.listens().is_empty(),
        "the wake's own listen was restarted"
    );
    assert_eq!(
        ctl.log.lock().unwrap().states,
        [State::Think, State::Speak],
        "the ear left Listen after the interrupt"
    );
}

#[tokio::test]
async fn whisper_reads_each_segment_and_a_wake_in_one_hands_over_the_request() {
    let says = Says::new(&["the news at nine", "Hey goose, what's the weather?"]);
    let (rig, ctl) = rig(WakeMode::Segments, says.clone()).await;
    let mut waiting = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_activation_with_audio().await }
    });
    let at = ctl.at();
    ctl.emit(segment(at - S, at));
    let until = Instant::now() + Duration::from_secs(5);
    while says.calls() < 1 {
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut waiting)
            .await
            .is_err(),
        "a segment without the wake phrase woke the pond"
    );
    let at = ctl.at();
    ctl.emit(segment(at - 2 * S, at));
    let activation = bounded("the wake", waiting).await.unwrap().unwrap();
    assert!(activation.endpointed);
    assert_eq!(activation.captured_audio.unwrap().len(), 44 + 4 * S);

    rig.prime_with_captured(Vec::new());
    let heard = bounded("the listen", rig.listen()).await.unwrap();
    assert_eq!(heard.as_deref(), Some("what's the weather?"));
    assert_eq!(
        says.calls(),
        2,
        "the segment's own transcript is the request"
    );
    assert!(ctl.listens().is_empty());
}

/// In a room that never pauses the cap cuts the segment mid-request: the wake is heard at the cut,
/// and the request is endpointed whole from the wake rather than ending where the cut fell.
#[tokio::test]
async fn a_wake_in_a_cut_segment_hears_the_request_out() {
    let says = Says::new(&[
        "Hey goose, remind me tomorrow",
        "Hey goose, remind me tomorrow to call the plumber about the sink.",
    ]);
    let (rig, ctl) = rig(WakeMode::Segments, says.clone()).await;
    let waiting = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_activation_with_audio().await }
    });
    let at = ctl.at();
    let start = at - 2 * S;
    ctl.emit(Event::Segment {
        t: 0.0,
        start,
        end: at,
        cut: true,
    });
    let activation = bounded("the wake", waiting).await.unwrap().unwrap();
    ctl.until("the ear to listen from the wake", |l, _| {
        l.listens == [start]
    })
    .await;
    rig.prime_with_captured(activation.captured_audio.unwrap());
    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    ctl.emit(utterance(start, ctl.at()));
    let heard = bounded("the request", listening).await.unwrap().unwrap();
    assert_eq!(
        heard.as_deref(),
        Some("remind me tomorrow to call the plumber about the sink.")
    );
    assert_eq!(ctl.listens(), vec![start], "one listen, from the wake");
}

#[tokio::test]
async fn a_segment_that_is_only_the_wake_phrase_listens_for_the_request_after_it() {
    // The nano's tiny.en heard a 0.55 s wake as "Hey goose." eight times over.
    for opening in ["Hey goose.", "Hey goose. Hey goose. Hey goose."] {
        let says = Says::new(&[opening, "set a timer for ten minutes"]);
        let (rig, ctl) = rig(WakeMode::Segments, says.clone()).await;
        let waiting = tokio::spawn({
            let rig = rig.clone();
            async move { rig.wait_for_activation_with_audio().await }
        });
        let at = ctl.at();
        ctl.emit(segment(at - S, at));
        let activation = bounded("the wake", waiting).await.unwrap().unwrap();
        rig.prime_with_captured(activation.captured_audio.unwrap());
        let listening = tokio::spawn({
            let rig = rig.clone();
            async move { rig.listen().await }
        });
        ctl.until("the listen after the phrase", |l, _| l.listens.len() == 1)
            .await;
        assert_eq!(
            ctl.listens(),
            vec![at],
            "{opening}: listens from where the phrase ended"
        );
        ctl.emit(utterance(at, ctl.at()));
        let heard = bounded("the request", listening).await.unwrap().unwrap();
        assert_eq!(heard.as_deref(), Some("set a timer for ten minutes"));
        assert_eq!(says.calls(), 2);
    }
}

/// The nano woke itself five times telling a story about Goosey Goose: the canceller left
/// enough of the reply for Whisper, and these are two of the segments it transcribed.
#[tokio::test]
async fn the_ponds_own_words_heard_back_never_interrupt_its_reply() {
    let says = Says::new(&[
        "Goosey Goose left a pond.",
        "since the lock story about Goosey. Goose leaving the pilot has been delivered",
        "Goose, stop.",
    ]);
    let (rig, ctl) = rig_for(WakeMode::Segments, says.clone(), "goose").await;
    let hook = rig.state_hook();
    let spoken = rig.spoken_hook();
    hook(WorkflowState::Thinking);
    spoken("Once upon a time, Goosey Goose left the pond to see the wide world.");
    hook(WorkflowState::Speak);
    spoken("Since the long story about Goosey Goose leaving the pond has been delivered, shall I tell another?");
    ctl.until("Think then Speak", |l, _| {
        l.states == [State::Think, State::Speak]
    })
    .await;
    let mut watch = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_interrupt().await }
    });
    for n in 1..=2 {
        let at = ctl.at();
        ctl.emit(segment(at - S, at));
        let until = Instant::now() + Duration::from_secs(5);
        while says.calls() < n {
            assert!(Instant::now() < until, "segment {n} was never transcribed");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(150), &mut watch)
                .await
                .is_err(),
            "the pond's own words, heard back, interrupted its reply"
        );
    }
    let at = ctl.at();
    ctl.emit(segment(at - S, at));
    let interrupt = bounded("the person's interrupt", watch).await.unwrap();
    assert_eq!(interrupt.why, InterruptReason::WakeWord);
}

fn enrolled(ids: &[&str]) -> Household {
    Household {
        prints: ids
            .iter()
            .map(|id| pond_ear::score::Print {
                id: id.to_string(),
                name: id.to_string(),
                embs: vec![vec![1.0, 0.0]],
            })
            .collect(),
        ..Household::default()
    }
}

/// A wake recognised as `waker` (or nobody), then one follow-up heard as `voice`: what the follow-up
/// listen returns.
async fn follow_up_after(waker: Option<&str>, voice: Option<&str>) -> (Option<String>, usize) {
    let says = Says::new(&["Goose, what's the weather?", "and tomorrow"]);
    let (rig, ctl) = rig_for(WakeMode::Segments, says.clone(), "goose").await;
    rig.set_household(enrolled(&["amina", "zawadi"]));
    *ctl.voice.lock().unwrap() = waker.map(str::to_string);
    let waiting = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_activation_with_audio().await }
    });
    let at = ctl.at();
    ctl.emit(segment(at - S, at));
    let activation = bounded("the wake", waiting).await.unwrap().unwrap();
    assert_eq!(
        activation.speaker.map(|s| s.profile_id).as_deref(),
        waker,
        "the wake names its speaker"
    );
    rig.prime_with_captured(activation.captured_audio.unwrap());
    let heard = bounded("the request", rig.listen()).await.unwrap();
    assert_eq!(heard.as_deref(), Some("what's the weather?"));

    *ctl.voice.lock().unwrap() = voice.map(str::to_string);
    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the follow-up listen", |l, _| l.listens.len() == 1)
        .await;
    ctl.emit(utterance(ctl.at() - S, ctl.at()));
    let heard = bounded("the follow-up", listening).await.unwrap().unwrap();
    (heard, says.calls())
}

/// While a member teaches the pond their voice, every sentence starts with the wake phrase.
#[tokio::test]
async fn a_paused_ear_hears_no_wake_and_ends_a_waiting_follow_up() {
    let says = Says::new(&["Goose, what's the weather?"]);
    let (rig, ctl) = rig_for(WakeMode::Segments, says.clone(), "goose").await;
    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the follow-up listen", |l, _| l.listens.len() == 1)
        .await;
    rig.pause_until(Some(Instant::now() + Duration::from_secs(60)));
    let heard = bounded("the paused follow-up", listening)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(heard.as_deref(), Some(""), "the waiting follow-up ended");

    let mut waiting = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_activation_with_audio().await }
    });
    let at = ctl.at();
    ctl.emit(segment(at - S, at));
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut waiting)
            .await
            .is_err(),
        "a paused ear woke"
    );
    assert_eq!(says.calls(), 0, "a paused ear transcribed a segment");

    rig.pause_until(None);
    let at = ctl.at();
    ctl.emit(segment(at - S, at));
    bounded("the wake after the pause", waiting)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn the_wakers_own_voice_continues_the_conversation() {
    let (heard, calls) = follow_up_after(Some("amina"), Some("amina")).await;
    assert_eq!(heard.as_deref(), Some("and tomorrow"));
    assert_eq!(calls, 2);
}

/// A short follow-up is often too little for Voice Match to recognise even the waker.
#[tokio::test]
async fn a_follow_up_voice_match_does_not_recognise_continues_the_conversation() {
    let (heard, calls) = follow_up_after(Some("amina"), None).await;
    assert_eq!(heard.as_deref(), Some("and tomorrow"));
    assert_eq!(calls, 2);
}

#[tokio::test]
async fn another_members_voice_is_no_follow_up_and_is_never_transcribed() {
    let (heard, calls) = follow_up_after(Some("amina"), Some("zawadi")).await;
    assert_eq!(heard.as_deref(), Some(""));
    assert_eq!(calls, 1, "Zawadi's words were never transcribed");
}

#[tokio::test]
async fn a_wake_nobody_was_recognised_for_keeps_every_follow_up() {
    let (heard, calls) = follow_up_after(None, Some("zawadi")).await;
    assert_eq!(heard.as_deref(), Some("and tomorrow"));
    assert_eq!(calls, 2);
}

#[tokio::test]
async fn the_state_hook_moves_the_ear_with_the_loop_and_listen_is_left_to_listen() {
    let (rig, ctl) = rig(WakeMode::HeyGoose, Says::new(&[])).await;
    let hook = rig.state_hook();
    for state in [
        WorkflowState::Wait,
        WorkflowState::Listen,
        WorkflowState::Thinking,
        WorkflowState::Speak,
        WorkflowState::Wait,
    ] {
        hook(state);
    }
    ctl.until("four ear states", |l, _| l.states.len() == 4)
        .await;
    assert_eq!(
        ctl.log.lock().unwrap().states,
        [State::Wait, State::Think, State::Speak, State::Wait]
    );
    assert!(ctl.listens().is_empty());
}

#[tokio::test]
async fn the_tap_reaches_the_ear_at_16_khz_ahead_of_the_microphone_it_overlaps() {
    let (rig, ctl) = rig(WakeMode::HeyGoose, Says::new(&[])).await;
    let tap = rig.tap();
    let before = ctl.at();
    tap(&[0.1; 480], 24_000);
    tap(&[0.1; 480], 24_000);
    ctl.until("both chunks played", |l, _| l.played.len() == 2)
        .await;
    let played = ctl.log.lock().unwrap().played.clone();
    let (at, n) = played[0];
    assert!(
        at >= before,
        "stamped at {at}, before what the ear had heard ({before})"
    );
    assert!((280..=330).contains(&n), "{n} samples for 20 ms at 24 kHz");
    assert_eq!(
        played[1].0,
        at + n,
        "a burst's chunks are stamped end to end"
    );
}

/// Names each call by how it was made, so a test can see which path read the audio.
struct Paths(Mutex<Vec<&'static str>>);

impl Transcriber for Paths {
    fn transcribe(&self, _pcm: &[f32]) -> Result<String> {
        self.0.lock().unwrap().push("transcribe");
        Ok("hey goose tell me a story about a pond".into())
    }

    fn scan(&self, _pcm: &[f32]) -> Result<String> {
        self.0.lock().unwrap().push("scan");
        Ok("hey goose".into())
    }
}

#[test]
fn the_opening_is_scanned_and_only_a_long_wake_is_transcribed_in_full() {
    let triggers = pond_voice::text::wake_triggers("hey goose", &[]);
    let paths = Paths(Mutex::default());
    let short = Segment {
        start: 0,
        end: 3 * S,
        pcm: vec![0.0; 3 * S],
        cut: false,
    };
    let woke = wake_in(&paths, &triggers, short).unwrap().unwrap();
    assert_eq!(woke.transcript.as_deref(), Some("hey goose"));
    assert_eq!(*paths.0.lock().unwrap(), ["scan"]);

    let paths = Paths(Mutex::default());
    let long = Segment {
        start: 0,
        end: 20 * S,
        pcm: vec![0.0; 20 * S],
        cut: false,
    };
    let woke = wake_in(&paths, &triggers, long).unwrap().unwrap();
    assert_eq!(
        woke.transcript.as_deref(),
        Some("hey goose tell me a story about a pond")
    );
    assert_eq!(*paths.0.lock().unwrap(), ["scan", "transcribe"]);
}

#[test]
fn a_long_segment_is_matched_on_its_opening_and_transcribed_whole_only_on_a_wake() {
    let says = Says::new(&["some talk show"]);
    let long = Segment {
        start: 0,
        end: 20 * S,
        pcm: vec![0.0; 20 * S],
        cut: false,
    };
    let triggers = pond_voice::text::wake_triggers("hey goose", &[]);
    assert!(wake_in(&*says, &triggers, long).unwrap().is_none());
    assert_eq!(*says.heard.lock().unwrap(), vec![WAKE_SCAN_SECONDS * S]);

    let says = Says::new(&["hey goose", "hey goose tell me a story about a pond"]);
    let long = Segment {
        start: 0,
        end: 20 * S,
        pcm: vec![0.0; 20 * S],
        cut: false,
    };
    let woke = wake_in(&*says, &triggers, long).unwrap().unwrap();
    assert_eq!(
        woke.transcript.as_deref(),
        Some("hey goose tell me a story about a pond")
    );
    assert_eq!(
        *says.heard.lock().unwrap(),
        vec![WAKE_SCAN_SECONDS * S, 20 * S]
    );
}

/// The real ear on silence: a follow-up with nothing said ends in silence once the command
/// timeout passes. Uses the pond's Silero when it has been fetched.
#[tokio::test]
async fn the_real_ear_ends_a_quiet_follow_up_with_silence() {
    let Some(silero) = pond_ear::locations::data_dir()
        .map(|d| pond_ear::locations::silero(&d))
        .filter(|p| p.exists())
    else {
        eprintln!("no Silero in the pond's data directory; skipped");
        return;
    };
    let dir = std::env::temp_dir().join(format!("pond-adapters-ear-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let onnx = dir.join("silero_vad.onnx");
    std::fs::copy(&silero, &onnx).unwrap();
    let vad = load_vad(&onnx).unwrap();
    assert!(
        onnx.with_extension("vmw").exists(),
        "cached as .vmw beside the ONNX"
    );

    let capture = ScriptedCapture::new(vec![0.0; S * 60], 20).paced(1);
    let (mic, _owner) = pond_audio::spawn(Box::new(capture), 16_000, 15_000, true);
    let triggers = pond_voice::text::wake_triggers("hey goose", &[]);
    let cfg = EarConfig::new(WakeMode::HeyGoose, triggers, "say \"hey goose\"");
    let rig = EarRig::start(cfg, vad, mic.clone(), Says::new(&[])).unwrap();
    let heard = bounded("the quiet listen", rig.listen()).await.unwrap();
    assert_eq!(heard.as_deref(), Some(""));
    drop(rig);
    mic.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
/// On the nano the reply's tail reached the follow-up as three wordless blips 25 ms apart.
async fn the_blips_a_reply_leaves_never_close_its_follow_up() {
    let says = Says::new(&[
        "Goose, what's the weather?",
        "Thank you.",
        "",
        "Thank you.",
        "and tomorrow?",
    ]);
    let (rig, ctl) = rig_for(WakeMode::Segments, says.clone(), "goose").await;
    let waiting = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_activation_with_audio().await }
    });
    let at = ctl.at();
    ctl.emit(segment(at - S, at));
    let activation = bounded("the wake", waiting).await.unwrap().unwrap();
    rig.prime_with_captured(activation.captured_audio.unwrap());
    let heard = bounded("the request", rig.listen()).await.unwrap();
    assert_eq!(heard.as_deref(), Some("what's the weather?"));

    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    for blip in 1..=4 {
        ctl.until("the next listen", |l, _| l.listens.len() == blip)
            .await;
        ctl.emit(utterance(ctl.at() - S / 10, ctl.at()));
    }
    let heard = bounded("the follow-up", listening).await.unwrap().unwrap();
    assert_eq!(heard.as_deref(), Some("and tomorrow?"));
}

#[tokio::test]
async fn a_follow_up_that_echoes_the_ponds_reply_is_ignored() {
    let says = Says::new(&["Goose, what's the weather?", "mostly sunny."]);
    let (rig, ctl) = rig_for(WakeMode::Segments, says.clone(), "goose").await;
    let waiting = tokio::spawn({
        let rig = rig.clone();
        async move { rig.wait_for_activation_with_audio().await }
    });
    let at = ctl.at();
    ctl.emit(segment(at - S, at));
    let activation = bounded("the wake", waiting).await.unwrap().unwrap();
    rig.prime_with_captured(activation.captured_audio.unwrap());
    let heard = bounded("the request", rig.listen()).await.unwrap();
    assert_eq!(heard.as_deref(), Some("what's the weather?"));

    // The pond speaks its reply.
    rig.spoken_hook()("The forecast in Nairobi today is mostly sunny.");

    let listening = tokio::spawn({
        let rig = rig.clone();
        async move { rig.listen().await }
    });
    ctl.until("the follow-up listen", |l, _| l.listens.len() == 1)
        .await;
    ctl.emit(utterance(ctl.at() - S, ctl.at()));
    let heard = bounded("the follow-up", listening).await.unwrap().unwrap();
    assert_eq!(heard.as_deref(), Some(""));
}
