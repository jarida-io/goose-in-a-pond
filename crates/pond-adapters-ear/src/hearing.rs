//! What the ear thread needs of the ear, so the thread's rules can be tested on scripted events.

use pond_ear::{Ear, Event, Household, State};

pub trait Hearing: Send {
    /// 16 kHz mono microphone audio, in any chunk size.
    fn push_into(&mut self, mic: &[f32], out: &mut Vec<Event>);
    /// The 16 kHz far end whose first sample plays when `mic_samples()` reads `at`.
    fn play(&mut self, at: usize, pcm: &[f32]);
    fn set_state(&mut self, to: State) -> Option<Event>;
    fn listen(&mut self, start: usize) -> Option<Event>;
    /// Echo-cancelled audio between absolute samples.
    fn audio(&self, start: usize, end: usize, out: &mut Vec<f32>);
    fn state(&self) -> State;
    /// Microphone samples pushed so far: the clock `play` stamps against.
    fn mic_samples(&self) -> usize;
    /// Samples the parts have seen; event indices are on this clock.
    fn samples(&self) -> usize;
    fn set_household(&mut self, household: Household);
    fn household(&self) -> &Household;
    /// The household member whose voice spoke absolute samples `[start, end)`, if Voice Match
    /// recognises one.
    fn identify(&mut self, start: usize, end: usize) -> Option<String>;
    fn set_speaker(&mut self, speaker: pond_ear::ear::parts::SpeakerNet);
}

impl Hearing for Ear {
    fn push_into(&mut self, mic: &[f32], out: &mut Vec<Event>) {
        Ear::push_into(self, mic, out);
    }

    fn play(&mut self, at: usize, pcm: &[f32]) {
        Ear::play(self, at, pcm);
    }

    fn set_state(&mut self, to: State) -> Option<Event> {
        Ear::set_state(self, to)
    }

    fn listen(&mut self, start: usize) -> Option<Event> {
        Ear::listen(self, start)
    }

    fn audio(&self, start: usize, end: usize, out: &mut Vec<f32>) {
        Ear::audio(self, start, end, out);
    }

    fn state(&self) -> State {
        Ear::state(self)
    }

    fn mic_samples(&self) -> usize {
        Ear::mic_samples(self)
    }

    fn samples(&self) -> usize {
        self.front().samples()
    }

    fn set_household(&mut self, household: Household) {
        Ear::set_household(self, household);
    }

    fn household(&self) -> &Household {
        Ear::household(self)
    }

    fn identify(&mut self, start: usize, end: usize) -> Option<String> {
        Ear::identify(self, start, end).0
    }

    fn set_speaker(&mut self, speaker: pond_ear::ear::parts::SpeakerNet) {
        Ear::set_speaker(self, speaker);
    }
}
