//! Matter backend: [`DeviceControlPort`] over GIAP's own `matter-server/` (matter.js) on the
//! loopback `giap-matter` WebSocket. The domain-level protocol is in `docs/matter-protocol.md`.

mod bridge;
mod client;
mod commissioning;
mod control;
mod notify;
mod protocol;
mod runtime;
mod server_setup;

pub use bridge::{run_matter_bridge, run_matter_supervisor, SupervisorConfig};
pub use client::{code_of, MatterClient, MatterEvent};
pub use commissioning::MatterCommissioner;
pub use control::{MatterDeviceControl, SharedMatterClient};
pub use notify::MatterNotifier;
pub use protocol::{
    is_matter_device_id, matter_bridged_endpoint, matter_device_id, matter_node_id,
    redact_setup_code, WireDevice, WireReading,
};
pub use runtime::{MatterRuntime, SwitchableDeviceControl};
pub use server_setup::{
    ensure_running as ensure_matter_server, local_port_from_ws_url, revive_local_controller,
    Revival, SharedServerChild,
};

#[cfg(test)]
mod tests;
