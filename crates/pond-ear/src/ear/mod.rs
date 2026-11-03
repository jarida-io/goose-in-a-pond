//! The ear itself: echo canceller, shared front end, the parts, endpointing and the state machine,
//! with every byte each part holds accounted for.

pub mod aec;
pub mod beam;
pub mod endpoint;
pub mod event;
pub mod front;
pub mod machine;
pub mod mem;
pub mod parts;

pub use aec::{Aec, AecConfig};
pub use endpoint::{Endpoint, Endpointer, Outcome};
pub use event::Event;
pub use front::Front;
pub use machine::{Config, Ear, Household, SpeakerConfig, State, WakeConfig};

/// What one part of the ear holds in memory, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Bytes {
    /// Weights as loaded (the whole `.vmw`).
    pub weights: usize,
    /// Weights this rung actually reads: what a per-rung file would hold.
    pub weights_rung: usize,
    /// Scratch the engine plans once, sized for the longest input.
    pub arena: usize,
    /// Everything else kept between calls: rings, windows, caches.
    pub state: usize,
}

impl Bytes {
    pub fn total(&self) -> usize {
        self.weights + self.arena + self.state
    }

    pub fn total_rung(&self) -> usize {
        self.weights_rung + self.arena + self.state
    }
}

/// One row of the ear's RAM and CPU table.
#[derive(Clone, Debug)]
pub struct Part {
    /// What it does: "front end", "wake", "speaker", ...
    pub role: &'static str,
    /// What does it: "wake CNN (hey goose)", "ResNet34", ...
    pub name: String,
    pub bytes: Bytes,
    /// Nanoseconds spent in this part since the stream began.
    pub ns: u64,
    pub calls: u64,
    /// Multiply-accumulates per call, where the engine knows them.
    pub macs: u64,
}
