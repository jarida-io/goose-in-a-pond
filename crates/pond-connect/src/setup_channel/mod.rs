//! The setup channel: one message protocol over any transport that moves bytes
//! (`docs/architecture/pond-setup-channel.md`).

pub mod framing;
pub mod messages;
pub mod sealing;
pub mod session;

pub use session::{
    ChannelBusy, SessionEnd, SessionEnded, SessionHandle, SessionNotifications, SessionWriter,
    SetupChannel, SetupChannelConfig, SetupChannelContext, SetupSession,
};
