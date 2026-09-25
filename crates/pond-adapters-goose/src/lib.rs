pub mod extension_manager;
pub mod giap_registration;
pub mod goose_agent;
#[cfg(test)]
mod goose_nudges;
pub mod logging;
pub mod mesh_provider;
pub mod model_traits;
pub mod mtp_drafter;
pub mod orchestrator;
pub mod provider_adapter;
pub mod provider_shim;
pub mod token_counter;
pub mod vision_encoder;

pub use extension_manager::GiapGooseExtensionManager;
pub use giap_registration::{register_giap_extensions, registered_extensions};
pub use goose_agent::GooseAdapter;
pub use orchestrator::{GooseChildRunner, GooseOrchestrator};
pub use provider_adapter::GooseProviderAdapter;
