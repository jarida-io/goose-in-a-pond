//! Whether this binary can use the host's accelerator; a CPU build silently prefills a Jetson at
//! 26 tok/s instead of 696. Warn, never refuse: a CPU pond beats no pond.

/// What this binary can do with this host's accelerator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acceleration {
    /// Built with CUDA. Nothing to say.
    CudaBuild,
    /// An accelerated host and a binary that cannot reach it: the one case worth warning about.
    CpuOnAcceleratedHost,
    /// No accelerator expected here — a Mac, a laptop, a CI runner.
    CpuElsewhere,
}

pub fn classify(host_is_accelerated: bool, cuda_build: bool) -> Acceleration {
    match (host_is_accelerated, cuda_build) {
        (_, true) => Acceleration::CudaBuild,
        (true, false) => Acceleration::CpuOnAcceleratedHost,
        (false, false) => Acceleration::CpuElsewhere,
    }
}

/// What to tell the operator, with the rebuild command: nothing else leads to the feature chain.
pub fn warning(acceleration: Acceleration) -> Option<&'static str> {
    match acceleration {
        Acceleration::CudaBuild | Acceleration::CpuElsewhere => None,
        Acceleration::CpuOnAcceleratedHost => Some(
            "This host has an NVIDIA accelerator and this binary was built WITHOUT CUDA. \
             Local inference will run on the CPU: measured on an Orin Nano, 26 tok/s of \
             prefill against 696 with CUDA, so a single turn takes tens of seconds instead \
             of one or two. Nothing else will report this. Rebuild with: \
             cargo build --release -p pond-server \
             --features pond-adapters-local-inference/cuda,pond-adapters-whisper/cuda \
             (this is what scripts/jetson/deploy.sh does; scripts/jetson/build-docker.sh \
             does NOT).",
        ),
    }
}

/// Whether the host has an NVIDIA accelerator. `model` is `/proc/device-tree/model` and
/// `has_tegra_release` is whether `/etc/nv_tegra_release` exists; either suffices.
pub fn host_is_accelerated(model: Option<&str>, has_tegra_release: bool) -> bool {
    if has_tegra_release {
        return true;
    }
    let Some(model) = model else {
        return false;
    };
    // Vendor-supplied and inconsistently cased ("Jetson-AGX", "nvidia,p3768").
    let model = model.to_ascii_lowercase();
    model.contains("jetson") || model.contains("tegra") || model.contains("nvidia")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `if !cuda { warn }` would shout at every laptop and be muted within a week.
    #[test]
    fn only_a_cpu_build_on_an_accelerated_host_is_a_problem() {
        assert_eq!(classify(true, true), Acceleration::CudaBuild);
        assert_eq!(classify(false, true), Acceleration::CudaBuild);
        assert_eq!(classify(false, false), Acceleration::CpuElsewhere);
        assert_eq!(
            classify(true, false),
            Acceleration::CpuOnAcceleratedHost,
            "a Jetson running a CPU binary is the one case nothing else reports"
        );
    }

    #[test]
    fn only_the_problem_case_says_anything() {
        assert!(warning(Acceleration::CudaBuild).is_none());
        assert!(warning(Acceleration::CpuElsewhere).is_none());
        assert!(warning(Acceleration::CpuOnAcceleratedHost).is_some());
    }

    #[test]
    fn the_warning_names_the_rebuild_and_the_script_that_omits_it() {
        let w = warning(Acceleration::CpuOnAcceleratedHost).expect("the problem case warns");
        assert!(
            w.contains("pond-adapters-local-inference/cuda"),
            "the warning must carry the feature flag that actually matters: {w}"
        );
        assert!(
            w.contains("build-docker.sh"),
            "the warning must name the build path that omits CUDA: {w}"
        );
    }

    #[test]
    fn a_jetson_is_recognised_however_its_device_tree_spells_it() {
        for model in [
            // Read off the deployed device; the strings below are invented variants.
            "NVIDIA Jetson Orin Nano Engineering Reference Developer Kit Super",
            "NVIDIA Jetson Orin Nano Developer Kit",
            "Jetson-AGX",
            "nvidia,p3768-0000+p3767-0005",
            "NVIDIA Tegra",
        ] {
            assert!(
                host_is_accelerated(Some(model), false),
                "should recognise {model:?}"
            );
        }
    }

    /// Containers routinely lack the device tree, and that is where a CPU-only image hides.
    #[test]
    fn a_container_without_a_device_tree_is_still_recognised_by_jetpack() {
        assert!(host_is_accelerated(None, true));
        assert!(!host_is_accelerated(None, false));
    }

    /// The control, and without it the matcher could return `true` for anything.
    #[test]
    fn an_ordinary_host_is_not_mistaken_for_an_accelerated_one() {
        for model in [
            "Apple M4 Pro",
            "Raspberry Pi 5 Model B",
            "",
            "Generic x86_64",
        ] {
            assert!(
                !host_is_accelerated(Some(model), false),
                "should NOT claim {model:?} is accelerated"
            );
        }
    }
}
