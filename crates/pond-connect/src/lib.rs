//! How a pond is found and reached (`docs/architecture/pond-setup-channel.md`). Policy stays in
//! `pond_core::network`; this crate is the mechanism. The setup channel here runs over any
//! transport that moves bytes, so the BLE and Wi-Fi adapters for each OS only move bytes.

pub mod advertising;
pub mod ble;
pub mod capabilities;
pub mod discovery;
pub mod interfaces;
pub mod setup_channel;
pub mod status;
#[cfg(test)]
mod test_support;
pub mod wifi;
