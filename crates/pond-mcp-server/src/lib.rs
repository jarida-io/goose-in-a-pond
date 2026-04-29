pub mod giap_server;
pub mod registry;

pub use giap_server::GiapMcpServer;
pub use registry::{init_giap_services, set_last_user_message, spawn_giap_server, try_tool_agent, try_wikipedia_lookup, GiapServiceHandles};
