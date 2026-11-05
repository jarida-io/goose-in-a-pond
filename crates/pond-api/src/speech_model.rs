//! The running speech model follows the ASR role without a restart, through the switch serve
//! installs; with none installed (tests, the CLI) a choice waits for the next start.

use std::sync::{Arc, RwLock};

use pond_core::models::ports::speech_model::SpeechModelSwitch;

static SWITCH: RwLock<Option<Arc<dyn SpeechModelSwitch>>> = RwLock::new(None);

/// Install serve's switch. Call once at startup, in the process that serves the API.
pub fn install(switch: Arc<dyn SpeechModelSwitch>) {
    *SWITCH.write().unwrap_or_else(|e| e.into_inner()) = Some(switch);
}

/// Start moving to `model` in the background: a reply never waits on a download.
pub fn switch_soon(model: &str) -> bool {
    let model = model.trim().to_string();
    let switch = SWITCH.read().unwrap_or_else(|e| e.into_inner()).clone();
    let (Some(switch), false) = (switch, model.is_empty()) else {
        return false;
    };
    tokio::spawn(async move {
        let outcome = switch.switch_to(&model).await;
        tracing::info!(kind = "speech_model_switch", model = %model, outcome = ?outcome, "speech model");
    });
    true
}
