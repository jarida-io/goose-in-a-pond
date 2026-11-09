//! The Jetson's GPU ONNX Runtime that `giap.sh onnxruntime build` installs: whether it is there,
//! where its TensorRT engines are cached, and building them ahead of time (`onnx-engines`).

use std::path::{Path, PathBuf};

use pond_onnx_accel::GpuPlan;

/// The libraries an install must hold; `ORTGPU_LIBS` in `scripts/lib/onnxruntime-setup.sh` names
/// the same providers.
const PROVIDERS: &[&str] = &[
    "libonnxruntime_providers_shared.so",
    "libonnxruntime_providers_cuda.so",
    "libonnxruntime_providers_tensorrt.so",
];
/// Written last by the setup script, so an install without it is incomplete.
const MANIFEST: &str = "MANIFEST.sha256";
/// Measured on the Orin Nano: building AdaFace's engine took 2.5 GB of available memory in a
/// fresh process, and `onnx-engines` peaked at 3.2 GB RSS. Beside E4B the pond has about 1.4 GB,
/// so engines are built by the setup script with the pond stopped, not here.
pub const ENGINE_BUILD_HEADROOM_MB: u64 = 3072;
/// Measured on the Orin Nano beside E4B at 16k: with YOLOX, SCRFD and AdaFace loaded from their
/// context models the pond had 1,440 MB available against 1,636 on the CPU runtime (the three take
/// 615 MB in a process of their own). Below this the model's KV cache and a switch need the room.
pub const ENGINE_LOAD_HEADROOM_MB: u64 = 1024;

fn install_dir(data_dir: &Path, version: &str) -> PathBuf {
    data_dir.join("lib").join("onnxruntime-gpu").join(version)
}

/// The installed GPU runtime's library on a Jetson, when the install is complete.
pub fn installed_runtime(data_dir: &Path, version: &str, on_jetson: bool) -> Option<PathBuf> {
    if !on_jetson {
        return None;
    }
    let dir = install_dir(data_dir, version);
    let lib = dir.join(format!("libonnxruntime.so.{version}"));
    let complete = dir.join(MANIFEST).is_file()
        && lib.is_file()
        && PROVIDERS.iter().all(|p| dir.join(p).is_file());
    complete.then_some(lib)
}

/// `<version>-trt<tensorrt>-<sm>` from the manifest's first line: an engine is only valid for
/// the runtime and TensorRT that built it, so each pair gets its own cache directory.
pub fn cache_tag(header: &str) -> Option<String> {
    let field = |key: &str| {
        header
            .split(" | ")
            .find_map(|part| part.strip_prefix(key))
            .map(str::trim)
            .filter(|v| !v.is_empty() && !v.contains(['/', ' ']))
    };
    let version = field("onnxruntime-gpu ")?;
    let tensorrt = field("tensorrt ")?;
    let sm = header
        .split(" | ")
        .map(str::trim)
        .find(|p| p.starts_with("sm_") && !p.contains(['/', ' ']))?;
    Some(format!("{version}-trt{tensorrt}-{sm}"))
}

/// The plan for moving models to the GPU, when the runtime this process loads is the GPU install.
pub fn plan(data_dir: &Path, version: &str, on_jetson: bool) -> Option<GpuPlan> {
    let lib = installed_runtime(data_dir, version, on_jetson)?;
    let loaded = std::env::var_os("ORT_DYLIB_PATH").map(PathBuf::from)?;
    if loaded != lib {
        return None;
    }
    let header = std::fs::read_to_string(install_dir(data_dir, version).join(MANIFEST)).ok()?;
    let tag = cache_tag(header.lines().next()?)?;
    Some(GpuPlan {
        cache_dir: data_dir.join("cache").join("onnx-trt").join(tag),
        build_headroom_mb: ENGINE_BUILD_HEADROOM_MB,
        load_headroom_mb: ENGINE_LOAD_HEADROOM_MB,
    })
}

type Loader = Box<dyn FnOnce() -> anyhow::Result<Option<pond_onnx_accel::AcceleratedSession>>>;

/// The pond's GPU-bound models, each resolved and loaded as the server does when called, so only
/// one model's sessions are resident at a time.
fn loaders(data_dir: &Path) -> Vec<Loader> {
    use pond_onnx_accel::AcceleratedSession;
    #[allow(unused_mut)]
    let mut found: Vec<Loader> = Vec::new();
    #[cfg(feature = "vision-onnx")]
    {
        let path = crate::model_download::vision_models_dir(data_dir)
            .join(crate::model_download::VISION_CLASSIFIER_DEFAULT_FILE);
        found.push(Box::new(move || {
            if !path.is_file() {
                return Ok(None);
            }
            AcceleratedSession::load(path, pond_adapters_vision_onnx::model_spec()).map(Some)
        }));
    }
    #[cfg(feature = "face-onnx")]
    {
        let (embedder, kind, scrfd) = crate::face_model_paths(data_dir);
        found.push(Box::new(move || {
            if !scrfd.is_file() {
                return Ok(None);
            }
            AcceleratedSession::load(scrfd, pond_adapters_face_onnx::scrfd::model_spec()).map(Some)
        }));
        found.push(Box::new(move || {
            if !embedder.is_file() {
                return Ok(None);
            }
            AcceleratedSession::load(embedder, pond_adapters_face_onnx::embedder_spec(kind))
                .map(Some)
        }));
        for var in ["POND_FACE_ANTISPOOF_PATH", "POND_FACE_ANTISPOOF_PATH_2"] {
            found.push(Box::new(move || {
                Ok(
                    pond_adapters_face_onnx::OnnxAntispoof::try_from_env_var(var)?
                        .map(|model| model.session().clone()),
                )
            }));
        }
    }
    let _ = data_dir;
    found
}

/// `pond-server onnx-engines`: builds or loads every model's TensorRT engine now, one at a time,
/// and says how each went. The setup script runs it while the pond is stopped.
pub fn run_engines(data_dir: &Path, version: &str, on_jetson: bool) -> anyhow::Result<()> {
    // Only the installed GPU runtime, never a download: this command has nothing for the CPU.
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        if let Some(lib) = installed_runtime(data_dir, version, on_jetson) {
            pond_core::shared::services::process_env::set("ORT_DYLIB_PATH", lib);
        }
    }
    let Some(plan) = plan(data_dir, version, on_jetson) else {
        println!("  No GPU ONNX Runtime is in use here; nothing to build.");
        println!("  On a Jetson: bash scripts/giap.sh onnxruntime build");
        return Ok(());
    };
    println!("  TensorRT engines in {}", plan.cache_dir.display());
    let mut failed = 0;
    for load in loaders(data_dir) {
        let session = match load() {
            Ok(Some(s)) if s.moves_to_gpu() => s,
            Ok(Some(s)) => {
                println!("  {}: stays on the cpu, where it is as fast", s.name());
                continue;
            }
            Ok(None) => continue,
            Err(e) => {
                failed += 1;
                println!("  a model did not load: {e:#}");
                continue;
            }
        };
        let started = std::time::Instant::now();
        let cached = session.engine_cached(&plan);
        match session.move_to_gpu(&plan) {
            Ok(device) => println!(
                "  {}: {} ({} in {:.1} s)",
                session.name(),
                device.as_str(),
                if cached { "cached engine" } else { "built" },
                started.elapsed().as_secs_f64()
            ),
            Err(e) => {
                failed += 1;
                println!("  {}: stays on the cpu: {e:#}", session.name());
            }
        }
    }
    if failed > 0 {
        anyhow::bail!("{failed} model(s) could not move to the GPU");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "onnxruntime-gpu 1.24.2 | onnxruntime 058787ceead760166e3c50a0a4cba8a833a6f53f \
        | built 2026-10-26T18:11Z on linux-aarch64 | cuda 12.6 | tensorrt 10.3.0.30 | cudnn 9.3.0 \
        | sm_87 | build.sh --config Release";

    #[test]
    fn engines_are_cached_per_runtime_and_tensorrt() {
        assert_eq!(
            cache_tag(HEADER).as_deref(),
            Some("1.24.2-trt10.3.0.30-sm_87")
        );
        assert_eq!(cache_tag("onnxruntime-gpu 1.24.2 | cuda 12.6"), None);
        assert_eq!(
            cache_tag("onnxruntime-gpu ../x | tensorrt 10.3 | sm_87"),
            None,
            "a manifest can never name a directory outside the cache"
        );
    }

    fn install(data_dir: &Path, with_manifest: bool) {
        let dir = install_dir(data_dir, "1.24.2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("libonnxruntime.so.1.24.2"), b"lib").unwrap();
        for p in PROVIDERS {
            std::fs::write(dir.join(p), b"provider").unwrap();
        }
        if with_manifest {
            std::fs::write(dir.join(MANIFEST), format!("{HEADER}\n")).unwrap();
        }
    }

    #[test]
    fn only_a_complete_install_on_a_jetson_is_used() {
        let tmp = tempfile::tempdir().unwrap();
        install(tmp.path(), false);
        assert_eq!(
            installed_runtime(tmp.path(), "1.24.2", true),
            None,
            "no manifest yet"
        );
        install(tmp.path(), true);
        let lib = installed_runtime(tmp.path(), "1.24.2", true).expect("complete");
        assert!(lib.ends_with("lib/onnxruntime-gpu/1.24.2/libonnxruntime.so.1.24.2"));
        assert_eq!(
            installed_runtime(tmp.path(), "1.24.2", false),
            None,
            "not a Jetson"
        );
        assert_eq!(
            installed_runtime(tmp.path(), "1.25.0", true),
            None,
            "another version"
        );
        std::fs::remove_file(install_dir(tmp.path(), "1.24.2").join(PROVIDERS[2])).unwrap();
        assert_eq!(
            installed_runtime(tmp.path(), "1.24.2", true),
            None,
            "a provider is missing"
        );
    }
}
