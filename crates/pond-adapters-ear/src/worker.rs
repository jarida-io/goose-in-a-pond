//! The ear's own thread: the only reader of its microphone claim and the only caller of the ear,
//! so every rule about wakes, turns and interrupts is decided here, in sample order.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use pond_audio::{MicHandle, MicReader, MicState};
use pond_core::models::ports::interrupt::{Interrupt, InterruptReason};
use pond_core::models::services::voice::quiet;
use pond_core::shared::domain::agent::{ThrottledAudioLevelSink, VoiceSpeaker, WorkflowState};
use pond_ear::{Event, Household, State};
use tokio::sync::oneshot;

use crate::echo::Spoken;
use crate::hearing::Hearing;
use crate::reference::{Reference, RATE};
use crate::Cue;

/// How long the thread waits for a command before it reads the microphone again.
const POLL: Duration = Duration::from_millis(10);
/// No microphone audio for this long ends a listen with silence and fails a wake wait, so a
/// lost device is reported instead of waited on forever.
const MIC_STALL: Duration = Duration::from_secs(3);
/// A wake or an interrupt nobody was waiting for is kept this long for the next one who asks.
const PENDING_FOR: Duration = Duration::from_secs(2);
/// A listen from sample `s` takes speech already endpointed from this far before `s`: an onset
/// converted back from an `Instant` is a few milliseconds out.
const STASH_TOLERANCE: usize = RATE as usize / 20;
/// Other audio stays paused while the words after a wake are captured, renewed as capture goes
/// on, and after the capture for the reply's turn to take over.
const QUIET_WHILE_LISTENING: Duration = Duration::from_secs(2);
const QUIET_UNTIL_THE_TURN: Duration = Duration::from_secs(5);
const QUIET_RENEW: Duration = Duration::from_millis(500);

/// What a listen heard.
#[derive(Clone, Debug)]
pub(crate) enum Heard {
    Utterance {
        start: usize,
        end: usize,
        pcm: Vec<f32>,
        /// Already transcribed by the segment matcher.
        transcript: Option<String>,
    },
    Silence,
}

/// A wake and the request it opened, endpointed.
#[derive(Clone, Debug)]
pub(crate) struct Woke {
    pub start: usize,
    pub end: usize,
    pub pcm: Vec<f32>,
    pub speaker: Option<VoiceSpeaker>,
    pub transcript: Option<String>,
}

pub(crate) enum ListenFrom {
    Now,
    At(usize),
    Since(Instant),
}

/// A segment for the matcher to transcribe.
pub(crate) struct Segment {
    pub start: usize,
    pub end: usize,
    pub pcm: Vec<f32>,
}

pub(crate) struct RefChunk {
    pub at: Instant,
    pub rate: u32,
    pub pcm: Vec<f32>,
}

pub(crate) enum Cmd {
    Loop(WorkflowState),
    Listen {
        from: ListenFrom,
        reply: oneshot::Sender<Heard>,
    },
    Wake {
        reply: oneshot::Sender<Result<Woke, String>>,
    },
    Interrupt {
        reply: oneshot::Sender<Interrupt>,
    },
    /// The matcher heard the wake phrase in a segment.
    SegmentWake(Woke),
    /// The pond is about to say this.
    Spoken(String),
    Household(Household),
    Shutdown,
}

pub(crate) struct Worker {
    pub hearing: Box<dyn Hearing>,
    pub mic: MicHandle,
    pub cmds: Receiver<Cmd>,
    pub refs: Receiver<RefChunk>,
    pub segments: Option<tokio::sync::mpsc::Sender<Segment>>,
    pub latency: usize,
    pub cue: Option<Cue>,
    pub audio_level: Option<Arc<ThrottledAudioLevelSink>>,
    pub state: WorkerState,
}

/// Everything the thread remembers between pushes.
pub(crate) struct WorkerState {
    reference: Reference,
    host: WorkflowState,
    /// An interrupt ended the turn: the loop's last Think or Speak must not cancel the listen the
    /// interrupt started.
    interrupted: bool,
    listen: Option<oneshot::Sender<Heard>>,
    wakes: Vec<oneshot::Sender<Result<Woke, String>>>,
    interrupts: Vec<oneshot::Sender<Interrupt>>,
    /// A wake heard while the loop waited: its request is still being endpointed.
    wake_turn: Option<Option<VoiceSpeaker>>,
    pending_woke: Option<(Woke, Instant)>,
    pending_interrupt: Option<(Interrupt, Instant)>,
    /// Endpointed with nobody listening yet, and the sample its turn began at.
    stash: Option<(usize, Heard)>,
    turn_start: usize,
    last_interrupt: Option<(Instant, usize)>,
    last_audio: Instant,
    /// When the last push ended, and the parts' clock after it.
    last_push: (Instant, usize),
    dropped: u64,
    quiet_listening: bool,
    quiet_renewed: Instant,
    /// What the pond said lately, to tell its echo from a person.
    spoken: Spoken,
    /// When the turn under way began thinking; what it says is kept for the whole turn.
    turn_began: Option<Instant>,
}

impl WorkerState {
    pub fn new() -> WorkerState {
        let now = Instant::now();
        WorkerState {
            reference: Reference::new(),
            host: WorkflowState::Wait,
            interrupted: false,
            listen: None,
            wakes: Vec::new(),
            interrupts: Vec::new(),
            wake_turn: None,
            pending_woke: None,
            pending_interrupt: None,
            stash: None,
            turn_start: 0,
            last_interrupt: None,
            last_audio: now,
            last_push: (now, 0),
            dropped: 0,
            quiet_listening: false,
            quiet_renewed: now,
            spoken: Spoken::default(),
            turn_began: None,
        }
    }
}

fn speaker_named(household: &Household, who: Option<String>) -> Option<VoiceSpeaker> {
    let id = who?;
    let name = household
        .prints
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| id.clone());
    Some(VoiceSpeaker {
        profile_id: id,
        name,
    })
}

fn linger(span: Duration) {
    if let Some(quiet) = quiet::installed() {
        quiet.linger(span);
    }
}

impl Worker {
    pub fn run(mut self) {
        let session = self.mic.open_session();
        let mut reader = MicReader::new(self.mic.shared().clone());
        let mut events = Vec::new();
        'run: loop {
            match self.cmds.recv_timeout(POLL) {
                Ok(cmd) => {
                    if !self.command(cmd) {
                        break 'run;
                    }
                    while let Ok(cmd) = self.cmds.try_recv() {
                        if !self.command(cmd) {
                            break 'run;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break 'run,
            }
            self.hear(&mut reader, &mut events);
            self.watch();
        }
        self.mic.close_session(session);
    }

    /// One read of the microphone: the playback stamped first, then the push, then its events.
    fn hear(&mut self, reader: &mut MicReader, events: &mut Vec<Event>) {
        let fresh = reader.drain();
        let lost = (reader.dropped() - self.state.dropped) as usize;
        self.state.dropped = reader.dropped();
        let now = Instant::now();
        if !fresh.is_empty() {
            self.state.last_audio = now;
        }
        // Before the push: a reference must reach the canceller ahead of the samples it overlaps.
        let floor = self.hearing.mic_samples() + lost;
        let end = floor + fresh.len();
        while let Ok(chunk) = self.refs.try_recv() {
            let age = now.saturating_duration_since(chunk.at).as_secs_f64() * RATE as f64;
            let natural = end.saturating_sub(age as usize) + self.latency;
            let (at, pcm) = self
                .state
                .reference
                .stamp(chunk.rate, &chunk.pcm, natural, floor);
            if !pcm.is_empty() {
                self.hearing.play(at, pcm);
            }
        }
        if lost > 0 {
            // Silence for what a slow read lost, so the ear's clock stays the microphone's.
            self.hearing.push_into(&vec![0.0; lost], events);
        }
        if fresh.is_empty() && events.is_empty() {
            return;
        }
        self.hearing.push_into(&fresh, events);
        self.state.last_push = (now, self.hearing.samples());
        if let Some(sink) = &self.audio_level {
            if matches!(self.state.host, WorkflowState::Wait | WorkflowState::Listen) {
                sink.maybe_emit(pond_voice::dsp::rms(&fresh));
            }
        }
        for event in std::mem::take(events) {
            self.event(event);
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Wake { start, who, .. } => {
                let speaker = speaker_named(self.hearing.household(), who);
                self.cue();
                self.state.turn_start = start;
                match self.state.host {
                    WorkflowState::Thinking | WorkflowState::Speak => {
                        self.state.wake_turn = None;
                        self.interrupt(start, InterruptReason::WakeWord);
                    }
                    _ => self.state.wake_turn = Some(speaker),
                }
            }
            Event::Utterance { start, end, .. } => {
                let mut pcm = Vec::new();
                self.hearing.audio(start, end, &mut pcm);
                self.heard(Heard::Utterance {
                    start,
                    end,
                    pcm,
                    transcript: None,
                });
            }
            Event::Silence { .. } => self.heard(Heard::Silence),
            Event::Segment { start, end, .. } => {
                let mut pcm = Vec::new();
                self.hearing.audio(start, end, &mut pcm);
                if let Some(segments) = &self.segments {
                    // Full while one is still being transcribed: a backlog is not worth decoding.
                    let _ = segments.try_send(Segment {
                        start,
                        end,
                        pcm: pcm.clone(),
                    });
                }
                self.state.stash = Some((
                    start,
                    Heard::Utterance {
                        start,
                        end,
                        pcm,
                        transcript: None,
                    },
                ));
            }
            Event::BargeIn {
                start,
                who: Some(_),
                ..
            } if self.state.host == WorkflowState::Speak => {
                self.interrupt(start, InterruptReason::Voice);
            }
            _ => {}
        }
    }

    fn cue(&mut self) {
        if let Some(cue) = &self.cue {
            cue();
        }
        linger(QUIET_WHILE_LISTENING);
        self.state.quiet_listening = true;
        self.state.quiet_renewed = Instant::now();
    }

    fn instant_of(&self, sample: usize) -> Instant {
        let (at, clock) = self.state.last_push;
        let back = clock.saturating_sub(sample) as f64 / RATE as f64;
        at.checked_sub(Duration::from_secs_f64(back)).unwrap_or(at)
    }

    fn sample_of(&self, instant: Instant) -> usize {
        if let Some((at, sample)) = self.state.last_interrupt {
            if at == instant {
                return sample;
            }
        }
        let (at, clock) = self.state.last_push;
        let back = at.saturating_duration_since(instant).as_secs_f64() * RATE as f64;
        clock.saturating_sub(back as usize)
    }

    fn interrupt(&mut self, start: usize, why: InterruptReason) {
        let onset = self.instant_of(start);
        self.state.last_interrupt = Some((onset, start));
        self.state.interrupted = true;
        let interrupt = Interrupt { onset, why };
        let mut taken = false;
        for waiter in self.state.interrupts.drain(..) {
            taken |= waiter.send(interrupt).is_ok();
        }
        tracing::info!(why = ?why, taken, "ear: interrupt");
        if !taken {
            self.state.pending_interrupt = Some((interrupt, Instant::now()));
        }
    }

    fn heard(&mut self, heard: Heard) {
        if self.state.quiet_listening && matches!(heard, Heard::Utterance { .. }) {
            linger(QUIET_UNTIL_THE_TURN);
        }
        self.state.quiet_listening = false;
        let mut heard = heard;
        if let Some(waiter) = self.state.listen.take() {
            match waiter.send(heard) {
                Ok(()) => {
                    self.state.wake_turn = None;
                    return;
                }
                Err(back) => heard = back,
            }
        }
        if let Some(speaker) = self.state.wake_turn.take() {
            // A wake then silence was no wake at all; the wait goes on.
            if let Heard::Utterance {
                start, end, pcm, ..
            } = heard
            {
                self.woke(Woke {
                    start,
                    end,
                    pcm,
                    speaker,
                    transcript: None,
                });
            }
            return;
        }
        let start = match &heard {
            Heard::Utterance { start, .. } => *start,
            Heard::Silence => self.state.turn_start,
        };
        self.state.stash = Some((start, heard));
    }

    fn woke(&mut self, woke: Woke) {
        let mut taken = false;
        for waiter in self.state.wakes.drain(..) {
            taken |= waiter.send(Ok(woke.clone())).is_ok();
        }
        if !taken {
            self.state.pending_woke = Some((woke, Instant::now()));
        }
    }

    /// Returns false to stop the thread.
    fn command(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Loop(state) => self.follow(state),
            Cmd::Listen { from, reply } => {
                let start = match from {
                    ListenFrom::Now => self.hearing.samples(),
                    ListenFrom::At(sample) => sample,
                    ListenFrom::Since(instant) => self.sample_of(instant),
                };
                self.state.interrupted = false;
                if let Some((began, _)) = &self.state.stash {
                    if began + STASH_TOLERANCE >= start {
                        if let Some((_, heard)) = self.state.stash.take() {
                            let _ = reply.send(heard);
                        }
                        return true;
                    }
                }
                self.state.stash = None;
                if self.hearing.state() != State::Listen {
                    self.hearing.listen(start);
                    self.state.turn_start = start;
                }
                self.state.listen = Some(reply);
            }
            Cmd::Wake { reply } => match self.mic.state() {
                MicState::Denied => {
                    let _ = reply.send(Err(
                        "microphone is disabled in Settings (mic_enabled = false)".into(),
                    ));
                }
                MicState::Failed(e) => {
                    let _ = reply.send(Err(format!("microphone could not be opened: {e}")));
                }
                _ => match self.state.pending_woke.take() {
                    Some((woke, at)) if at.elapsed() < PENDING_FOR => {
                        let _ = reply.send(Ok(woke));
                    }
                    _ => self.state.wakes.push(reply),
                },
            },
            Cmd::Interrupt { reply } => {
                let turn = matches!(
                    self.state.host,
                    WorkflowState::Thinking | WorkflowState::Speak
                );
                match self.state.pending_interrupt.take() {
                    Some((interrupt, at)) if turn && at.elapsed() < PENDING_FOR => {
                        let _ = reply.send(interrupt);
                    }
                    _ => self.state.interrupts.push(reply),
                }
            }
            Cmd::SegmentWake(woke) => {
                let now = Instant::now();
                self.state.spoken.forget_old(now, self.state.turn_began);
                let echo = woke
                    .transcript
                    .as_deref()
                    .and_then(|heard| self.state.spoken.echo_of(heard));
                if let Some(share) = echo {
                    tracing::info!(
                        heard = woke.transcript.as_deref().unwrap_or_default(),
                        share,
                        state = ?self.state.host,
                        "ear: own echo ignored"
                    );
                    return true;
                }
                self.cue();
                linger(QUIET_UNTIL_THE_TURN);
                self.state.quiet_listening = false;
                match self.state.host {
                    WorkflowState::Thinking | WorkflowState::Speak => {
                        let start = woke.start;
                        self.state.stash = Some((
                            start,
                            Heard::Utterance {
                                start,
                                end: woke.end,
                                pcm: woke.pcm,
                                transcript: woke.transcript,
                            },
                        ));
                        self.interrupt(start, InterruptReason::WakeWord);
                    }
                    _ => self.woke(woke),
                }
            }
            Cmd::Spoken(text) => {
                let now = Instant::now();
                self.state.spoken.forget_old(now, self.state.turn_began);
                self.state.spoken.say(&text, now);
            }
            Cmd::Household(household) => self.hearing.set_household(household),
            Cmd::Shutdown => return false,
        }
        true
    }

    /// The loop's own state, mapped onto the ear's.
    fn follow(&mut self, state: WorkflowState) {
        self.state.host = state;
        match state {
            WorkflowState::Thinking | WorkflowState::Speak => {
                self.state.turn_began.get_or_insert_with(Instant::now);
            }
            WorkflowState::Wait | WorkflowState::Listen => self.state.turn_began = None,
        }
        match state {
            WorkflowState::Wait => {
                self.state.interrupted = false;
                self.state.pending_interrupt = None;
                self.state.stash = None;
                // A wake said as the loop came back to Wait is for the wait about to start.
                let waking =
                    self.state.wake_turn.is_some() && self.hearing.state() == State::Listen;
                if !waking {
                    self.hearing.set_state(State::Wait);
                }
            }
            WorkflowState::Thinking if !self.state.interrupted => {
                self.hearing.set_state(State::Think);
            }
            WorkflowState::Speak if !self.state.interrupted => {
                self.hearing.set_state(State::Speak);
            }
            WorkflowState::Listen => {
                self.state.interrupted = false;
                self.state.pending_interrupt = None;
            }
            _ => {}
        }
    }

    /// Waiters that went away are dropped; a silent microphone ends what waits on it.
    fn watch(&mut self) {
        self.state.wakes.retain(|w| !w.is_closed());
        self.state.interrupts.retain(|w| !w.is_closed());
        if self.state.listen.as_ref().is_some_and(|w| w.is_closed()) {
            self.state.listen = None;
        }
        if self.state.quiet_listening
            && self.hearing.state() == State::Listen
            && self.state.quiet_renewed.elapsed() >= QUIET_RENEW
        {
            linger(QUIET_WHILE_LISTENING);
            self.state.quiet_renewed = Instant::now();
        }
        if self.state.last_audio.elapsed() < MIC_STALL {
            return;
        }
        if let Some(waiter) = self.state.listen.take() {
            tracing::warn!("ear: no microphone audio; ending the listen");
            let _ = waiter.send(Heard::Silence);
        }
        for waiter in self.state.wakes.drain(..) {
            let _ = waiter.send(Err("the microphone stopped delivering audio".into()));
        }
    }
}

/// The segment matcher: transcribes the opening of each segment and posts a wake when it holds
/// the wake phrase. Runs on the runtime so the ear thread never waits on Whisper.
pub(crate) async fn match_segments(
    mut segments: tokio::sync::mpsc::Receiver<Segment>,
    cmds: Sender<Cmd>,
    transcriber: Arc<dyn crate::Transcriber>,
    triggers: Vec<String>,
) {
    while let Some(segment) = segments.recv().await {
        let transcriber = transcriber.clone();
        let triggers = triggers.clone();
        let matched =
            tokio::task::spawn_blocking(move || crate::wake_in(&*transcriber, &triggers, segment))
                .await;
        match matched {
            Ok(Ok(Some(woke))) => {
                if cmds.send(Cmd::SegmentWake(woke)).is_err() {
                    return;
                }
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => tracing::debug!(error = %e, "ear: segment not transcribed"),
            Err(e) => tracing::warn!(error = %e, "ear: segment transcription panicked"),
        }
    }
}
