//! Which device this process should believe it is running on: a profile lets the Mac take the
//! decisions the board takes, since `apply_jetson_settings` is `#[cfg(feature = "cuda")]` and
//! only ever ran on the Orin. Opt-in via `POND_DEVICE_PROFILE`; unset, every accessor returns
//! `None`. Emulates decisions, not survival: no KV or perf numbers, see scripts/jetson-emu.sh.

use std::sync::OnceLock;

/// Environment variable naming the profile to emulate. Unset means "be honest".
pub const PROFILE_ENV: &str = "POND_DEVICE_PROFILE";
/// Override for [`DeviceProfile::total_ram_mb`], in MB.
pub const TOTAL_RAM_ENV: &str = "POND_DEVICE_TOTAL_RAM_MB";
/// Override for [`DeviceProfile::pretend_cuda`]: `0`/`false` forces the
/// CPU-build-on-an-accelerated-host case, which is the one thing
/// `pond_core::models::domain::acceleration` exists to shout about.
pub const PRETEND_CUDA_ENV: &str = "POND_DEVICE_PRETEND_CUDA";

/// A device this process can be asked to believe it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    /// The profile's own name, as spelled in `POND_DEVICE_PROFILE`.
    pub name: String,
    /// Total RAM **as the kernel reports it**, not as the box is marketed.
    ///
    /// The distinction has already cost a board: an Orin Nano 8 GB reports 7,620
    /// MB, and the missing 572 MB is spent on carveouts before Linux sees it.
    pub total_ram_mb: u64,
    /// What `/proc/device-tree/model` would contain.
    pub device_tree_model: Option<String>,
    /// Whether `/etc/nv_tegra_release` would exist.
    pub has_tegra_release: bool,
    /// Whether to answer the acceleration probe as a CUDA build.
    ///
    /// A CPU build on a Jetson is a real and silent failure mode —
    /// `build-docker.sh` produces one — so it has to be reachable here too.
    pub pretend_cuda: bool,
    /// Whether the model registry should be stamped with this device's
    /// settings rather than the host platform's.
    pub stamp_device_model_settings: bool,
}

impl DeviceProfile {
    /// Look up a built-in profile by name. `None` for anything unrecognised.
    ///
    /// Pure, so the table is testable without a Jetson. An unrecognised name returns `None`
    /// rather than a host default: a typo in `POND_DEVICE_PROFILE` must be loud, not inert.
    pub fn builtin(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            // The deployment. Every figure here was read off the board; see
            // `device_budget::JETSON_TOTAL_RAM_MB` for why the total is 7620
            // and not 8192.
            "orin-nano-8gb" | "orin-nano" | "jetson" => Some(Self {
                name: "orin-nano-8gb".to_string(),
                total_ram_mb: super::device_budget::JETSON_TOTAL_RAM_MB,
                device_tree_model: Some(
                    // The exact string read from the device on 2026-08-16.
                    "NVIDIA Jetson Orin Nano Engineering Reference Developer Kit Super".to_string(),
                ),
                has_tegra_release: true,
                pretend_cuda: true,
                stamp_device_model_settings: true,
            }),
            // The same board with the binary `build-docker.sh` produces: a
            // Jetson that cannot reach its own accelerator. Exists so the
            // warning path can be exercised on a Mac.
            "orin-nano-8gb-cpu" | "orin-nano-cpu" => Some(Self {
                name: "orin-nano-8gb-cpu".to_string(),
                total_ram_mb: super::device_budget::JETSON_TOTAL_RAM_MB,
                device_tree_model: Some(
                    "NVIDIA Jetson Orin Nano Engineering Reference Developer Kit Super".to_string(),
                ),
                has_tegra_release: true,
                pretend_cuda: false,
                stamp_device_model_settings: true,
            }),
            // Not a board we own. It is here because the Gemma-4 MTP work
            // concluded that the 8 GB NvMap wall, not the arithmetic, is what
            // blocks it — so "what would the derivation choose with twice the
            // RAM" is a question worth being able to ask without buying one.
            "orin-nx-16gb" | "orin-nx" => Some(Self {
                name: "orin-nx-16gb".to_string(),
                total_ram_mb: 15564,
                device_tree_model: Some("NVIDIA Jetson Orin NX Developer Kit".to_string()),
                has_tegra_release: true,
                pretend_cuda: true,
                stamp_device_model_settings: true,
            }),
            _ => None,
        }
    }

    /// Every built-in profile name, for a `--help` that cannot go stale.
    pub fn builtin_names() -> &'static [&'static str] {
        &["orin-nano-8gb", "orin-nano-8gb-cpu", "orin-nx-16gb"]
    }

    /// Apply the per-field environment overrides to a base profile.
    ///
    /// Takes its lookups as a closure so override precedence is testable without mutating the
    /// process environment, which is a data race in a threaded test binary.
    pub fn with_overrides(mut self, lookup: impl Fn(&str) -> Option<String>) -> Self {
        if let Some(mb) = lookup(TOTAL_RAM_ENV).and_then(|v| v.trim().parse::<u64>().ok()) {
            self.total_ram_mb = mb;
        }
        if let Some(v) = lookup(PRETEND_CUDA_ENV) {
            self.pretend_cuda = matches!(v.trim(), "1" | "true" | "yes" | "on");
        }
        self
    }

    /// Resolve a profile from an arbitrary environment. Pure; see
    /// [`Self::with_overrides`] for why the lookup is injected.
    pub fn resolve(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let name = lookup(PROFILE_ENV)?;
        let name = name.trim();
        if name.is_empty() || name.eq_ignore_ascii_case("none") || name.eq_ignore_ascii_case("host")
        {
            return None;
        }
        match Self::builtin(name) {
            Some(p) => Some(p.with_overrides(lookup)),
            None => {
                tracing::error!(
                    profile = name,
                    known = ?Self::builtin_names(),
                    "{PROFILE_ENV} names a profile that does not exist; running as this host. \
                     Nothing else will report this, and a run that silently declined to emulate \
                     is worse than one that failed to start."
                );
                None
            }
        }
    }
}

/// The profile this process is emulating, or `None` to be honest about the host.
///
/// Read from the environment once: emulation is a property of a run, or the model registry
/// could be stamped for one device while the memory budget is computed for another.
pub fn active() -> Option<&'static DeviceProfile> {
    static ACTIVE: OnceLock<Option<DeviceProfile>> = OnceLock::new();
    ACTIVE
        .get_or_init(|| {
            let resolved = DeviceProfile::resolve(|k| std::env::var(k).ok());
            if let Some(p) = &resolved {
                tracing::warn!(
                    profile = %p.name,
                    total_ram_mb = p.total_ram_mb,
                    pretend_cuda = p.pretend_cuda,
                    "DEVICE EMULATION ACTIVE — this process is answering hardware questions as a \
                     {} and is NOT a source of performance or memory-ceiling numbers.",
                    p.name
                );
            }
            resolved
        })
        .as_ref()
}

/// Whether a device profile is emulating a board whose model settings should be
/// stamped into the registry — i.e. whether the non-CUDA build should take the
/// device's model-settings branch rather than the host platform's.
pub fn stamping_device_model_settings() -> bool {
    active().is_some_and(|p| p.stamp_device_model_settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(pk, _)| *pk == k)
                .map(|(_, v)| v.to_string())
        }
    }

    /// The safety property, and the only test here that would matter if the
    /// rest were deleted: an unset environment emulates nothing.
    #[test]
    fn no_profile_means_no_emulation() {
        assert_eq!(DeviceProfile::resolve(env(&[])), None);
        assert_eq!(DeviceProfile::resolve(env(&[(PROFILE_ENV, "")])), None);
        assert_eq!(DeviceProfile::resolve(env(&[(PROFILE_ENV, "none")])), None);
        assert_eq!(DeviceProfile::resolve(env(&[(PROFILE_ENV, "host")])), None);
    }

    /// A typo must not quietly run as the host. This is the failure mode the
    /// whole module is built to avoid, applied to itself.
    #[test]
    fn an_unknown_profile_does_not_silently_emulate() {
        assert_eq!(
            DeviceProfile::resolve(env(&[(PROFILE_ENV, "orrin-nano")])),
            None,
            "an unrecognised name must not resolve to some other board"
        );
    }

    /// The RAM figure is the one this table can get wrong in a way that reaches
    /// the context derivation, so it is asserted against the kernel's number
    /// rather than the marketing one.
    #[test]
    fn the_orin_profile_carries_the_kernels_ram_and_not_the_marketing_figure() {
        let p = DeviceProfile::builtin("orin-nano-8gb").expect("the deployment profile exists");
        assert_eq!(
            p.total_ram_mb, 7620,
            "8192 is what the box says; 7620 is what `free -m` says, and the 572 MB gap has \
             already been spent silently once"
        );
        assert!(p.has_tegra_release);
        assert!(p.pretend_cuda);
    }

    /// Both halves of the acceleration table have to be reachable from a Mac,
    /// or the emulator can only ever produce the passing case.
    #[test]
    fn the_cpu_build_on_a_jetson_is_a_profile_of_its_own() {
        let good = DeviceProfile::builtin("orin-nano-8gb").unwrap();
        let bad = DeviceProfile::builtin("orin-nano-8gb-cpu").unwrap();
        assert_eq!(good.device_tree_model, bad.device_tree_model);
        assert!(good.pretend_cuda && !bad.pretend_cuda);
    }

    /// The profile's device-tree string has to satisfy the matcher that will
    /// actually see it. Asserting the two agree here means a change to either
    /// one fails a test rather than producing an emulator that quietly reads as
    /// an ordinary host.
    #[test]
    fn every_profiles_device_tree_string_is_recognised_as_accelerated() {
        for name in DeviceProfile::builtin_names() {
            let p = DeviceProfile::builtin(name).expect("named in builtin_names");
            assert!(
                super::super::acceleration::host_is_accelerated(
                    p.device_tree_model.as_deref(),
                    false
                ),
                "{name}: the device tree string must be recognised WITHOUT leaning on the \
                 tegra-release file, or the profile is only half emulated"
            );
        }
    }

    #[test]
    fn overrides_win_over_the_builtin() {
        let p = DeviceProfile::resolve(env(&[
            (PROFILE_ENV, "orin-nano-8gb"),
            (TOTAL_RAM_ENV, "4096"),
            (PRETEND_CUDA_ENV, "0"),
        ]))
        .expect("a known profile with overrides still resolves");
        assert_eq!(p.total_ram_mb, 4096);
        assert!(!p.pretend_cuda);
    }

    /// An unparseable override leaves the measured figure alone rather than
    /// falling to zero, which would hand the context derivation a budget of
    /// nothing and clamp every model to MIN_CTX.
    #[test]
    fn a_junk_ram_override_is_ignored_rather_than_zeroed() {
        let p = DeviceProfile::resolve(env(&[
            (PROFILE_ENV, "orin-nano-8gb"),
            (TOTAL_RAM_ENV, "lots"),
        ]))
        .unwrap();
        assert_eq!(p.total_ram_mb, 7620);
    }

    /// `scripts/jetson-emu.sh` carries its own copy of the profile table because it has to size
    /// a container before any Rust has run. A profile added on one side and not the other fails
    /// here rather than producing an emulator that refuses a known profile or sizes a container
    /// for the wrong memory. Text-matching is weak, but `pond-core` cannot depend on the script.
    #[test]
    fn shell_and_rust_agree_about_the_profiles() {
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/jetson-emu.sh");
        let Ok(text) = std::fs::read_to_string(&script) else {
            // Absent in a vendored or partial checkout. Nothing to guard.
            return;
        };
        let table = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("EMU_PROFILES="))
            .expect("jetson-emu.sh must define EMU_PROFILES")
            .trim_matches('"');

        let shell: Vec<(&str, u64)> = table
            .split_whitespace()
            .map(|e| {
                let (name, mb) = e.split_once(':').expect("EMU_PROFILES entries are NAME:MB");
                (name, mb.parse::<u64>().expect("MB is a number"))
            })
            .collect();

        for (name, mb) in &shell {
            let p = DeviceProfile::builtin(name).unwrap_or_else(|| {
                panic!("jetson-emu.sh offers '{name}', which Rust does not know")
            });
            assert_eq!(
                p.total_ram_mb, *mb,
                "{name}: the script would give a container {mb} MB while the binary budgets for \
                 {} MB. The container tier's whole value is that its ceiling is REAL, and a \
                 ceiling that disagrees with the budget tests nothing.",
                p.total_ram_mb
            );
        }

        for name in DeviceProfile::builtin_names() {
            assert!(
                shell.iter().any(|(n, _)| n == name),
                "Rust knows profile '{name}' but jetson-emu.sh will refuse it; add it to \
                 EMU_PROFILES"
            );
        }
    }

    #[test]
    fn every_name_in_the_help_text_actually_resolves() {
        for name in DeviceProfile::builtin_names() {
            assert!(
                DeviceProfile::builtin(name).is_some(),
                "{name} is advertised by builtin_names but does not exist"
            );
        }
    }
}
