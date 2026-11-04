//! Starting the audio device supervisor with the household's settings behind it.

use std::path::Path;
use std::sync::Arc;

use pond_adapters_audio_devices::{
    AudioPrefs, AudioSupervisor, ChangeFn, CpalProber, Role, SupervisorConfig,
};
use pond_audio::MicHandle;
use pond_core::user_data::domain::settings::Settings;
use pond_core::user_data::ports::settings::SettingsRepository;

fn pin(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.eq_ignore_ascii_case("auto")).then(|| value.to_string())
}

pub fn prefs_of(settings: &Settings) -> AudioPrefs {
    AudioPrefs {
        input_pin: pin(&settings.audio_input_device),
        output_pin: pin(&settings.audio_output_device),
        mic_enabled: settings.mic_enabled,
        sound_test: settings.audio_sound_test,
        manage_system: settings.audio_manage_system,
    }
}

/// Supervise this process's devices. Every pass reads the settings afresh, so a pin saved from
/// either app applies at the next test without a restart.
pub fn start(
    role: Role,
    data_dir: &Path,
    mic: Option<MicHandle>,
    settings: Arc<dyn SettingsRepository + Send + Sync>,
    on_change: Option<ChangeFn>,
) -> AudioSupervisor {
    let runtime = tokio::runtime::Handle::current();
    let fallback = Settings::default();
    let prefs = Arc::new(move || {
        // Called from the supervisor's own thread or a blocking task, never a runtime worker.
        let read = runtime.block_on(settings.get());
        prefs_of(read.as_ref().unwrap_or(&fallback))
    });
    AudioSupervisor::spawn(
        SupervisorConfig {
            role,
            data_dir: data_dir.to_path_buf(),
            mic: mic.clone(),
            prefs,
            on_change,
        },
        Arc::new(CpalProber::new(mic)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blank_pins_mean_automatic() {
        let mut s = Settings::default();
        s.audio_input_device = "  ".into();
        s.audio_output_device = "AirPods".into();
        s.audio_sound_test = false;
        let p = prefs_of(&s);
        assert_eq!(p.input_pin, None);
        assert_eq!(p.output_pin.as_deref(), Some("AirPods"));
        assert!(!p.sound_test);
        assert!(p.mic_enabled);
        assert!(p.manage_system, "on unless the household turns it off");
    }
}
