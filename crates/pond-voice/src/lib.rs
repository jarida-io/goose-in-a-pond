//! Voice primitives shared by every GIAP surface.
//!
//! A deliberate leaf crate: no `pond-core`, tokio, cpal or anyhow (`Cargo.toml` says why).

pub mod barge;
pub mod control;
pub mod dsp;
pub mod text;
pub mod turn;
