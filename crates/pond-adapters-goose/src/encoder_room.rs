//! Room for work that needs the multimodal encoder's memory.

/// Releases a loaded encoder no turn is using, for `wanted_for`, and says whether there was one.
/// The next spoken or picture turn loads it again.
pub fn release_idle_encoder(wanted_for: &'static str) -> bool {
    goose::providers::local_inference::release_idle_encoders(wanted_for) > 0
}
