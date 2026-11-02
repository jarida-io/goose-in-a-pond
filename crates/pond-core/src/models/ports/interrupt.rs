use async_trait::async_trait;

pub use crate::shared::domain::agent::InterruptReason;

/// A turn was asked to end early.
#[derive(Debug, Clone, Copy)]
pub struct Interrupt {
    /// When the interrupting speech began; the follow-up capture starts there.
    pub onset: std::time::Instant,
    pub why: InterruptReason,
}

/// What may cut a voice turn short besides the energy barge-in, for a loop with a front end that
/// tells a household voice and the wake word from the reply's echo.
#[async_trait]
pub trait InterruptWatch: Send + Sync {
    /// The first interrupt after this call; never resolves while nothing interrupts. Dropping the
    /// future stops watching.
    async fn wait_for_interrupt(&self) -> Interrupt;
}
