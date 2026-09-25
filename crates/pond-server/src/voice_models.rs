//! Turns `Settings` into voice model paths, accepting a catalog name, a filename, or a
//! hand-placed file; an unresolvable value is `None`, never a synthesised path.

use std::path::{Path, PathBuf};

use pond_core::models::domain::model_record::{ModelCategory, ModelRecord};
use pond_core::models::ports::model_repository::ModelRepository;
use pond_core::user_data::domain::settings::Settings;

/// Where to fetch a model that is configured but not yet on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperDownload {
    pub url: String,
    pub size_mb: u64,
}

/// A piper voice's `.onnx` weights and `.onnx.json` config; it can't load without both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiperDownload {
    pub onnx_filename: String,
    pub config_filename: String,
    pub onnx_url: String,
    pub config_url: String,
    pub size_mb: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperModel {
    pub path: PathBuf,
    /// `None` for a file on disk the catalog doesn't describe (usable, not re-downloadable).
    pub download: Option<WhisperDownload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PiperVoice {
    pub onnx: PathBuf,
    pub config: PathBuf,
    pub download: Option<PiperDownload>,
}

impl PiperVoice {
    #[cfg(test)]
    pub fn is_installed(&self) -> bool {
        self.onnx.exists() && self.config.exists()
    }
}

/// Everything the voice stack needs from `Settings`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VoiceModels {
    pub whisper: Option<WhisperModel>,
    pub piper: Option<PiperVoice>,
}

impl VoiceModels {
    /// Piper iff a voice resolved, not by how `active_tts_model` is spelled.
    #[cfg(test)]
    pub fn tts_is_piper(&self) -> bool {
        self.piper.is_some()
    }
}

/// Resolve by catalog name/id, then catalog filename, then a literal file in `models/`.
pub fn resolve_whisper(
    value: &str,
    catalog: &[ModelRecord],
    models_dir: &Path,
) -> Option<WhisperModel> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    // 1. Catalog name, e.g. "base" — the shape sync_assignments_to_settings writes.
    if let Some(rec) = catalog.iter().find(|m| m.name == value || m.id == value) {
        if let Some(filename) = rec.filename.as_deref() {
            return Some(WhisperModel {
                path: models_dir.join(filename),
                download: download_of(rec),
            });
        }
    }

    // 2. Filename, e.g. "ggml-base.bin" — the shape both settings UIs write.
    if let Some(rec) = catalog
        .iter()
        .find(|m| m.filename.as_deref() == Some(value))
    {
        return Some(WhisperModel {
            path: models_dir.join(value),
            download: download_of(rec),
        });
    }

    // 3. A hand-placed file the catalog doesn't know: usable, not re-downloadable.
    let literal = models_dir.join(value);
    if literal.is_file() {
        return Some(WhisperModel {
            path: literal,
            download: None,
        });
    }

    // Deliberately not a synthesised path.
    None
}

/// Resolve by catalog filename (the UIs' shape), then name/id, then a file in `models/tts/`.
pub fn resolve_piper(value: &str, catalog: &[ModelRecord], tts_dir: &Path) -> Option<PiperVoice> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let matched = catalog
        .iter()
        .find(|m| m.filename.as_deref() == Some(value))
        .or_else(|| catalog.iter().find(|m| m.name == value || m.id == value));

    if let Some(rec) = matched {
        if let Some(onnx_filename) = rec.filename.as_deref() {
            let config_filename = rec
                .config_filename
                .clone()
                .unwrap_or_else(|| format!("{onnx_filename}.json"));
            return Some(PiperVoice {
                onnx: tts_dir.join(onnx_filename),
                config: tts_dir.join(&config_filename),
                download: piper_download_of(rec, onnx_filename, &config_filename),
            });
        }
    }

    // Hand-placed voice: the weights must exist; the config is assumed at `<onnx>.json`.
    let literal = tts_dir.join(value);
    if literal.is_file() {
        return Some(PiperVoice {
            config: PathBuf::from(format!("{}.json", literal.display())),
            onnx: literal,
            download: None,
        });
    }

    None
}

/// The sole complete voice in `tts_dir`, if unambiguous; fallback for a setting like `amy`.
pub fn any_installed_piper_voice(tts_dir: &Path) -> Option<PiperVoice> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(tts_dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
        .filter(|p| PathBuf::from(format!("{}.json", p.display())).is_file())
        .collect();
    found.sort();

    match found.len() {
        1 => {
            let onnx = found.remove(0);
            Some(PiperVoice {
                config: PathBuf::from(format!("{}.json", onnx.display())),
                onnx,
                download: None,
            })
        }
        _ => None,
    }
}

fn download_of(rec: &ModelRecord) -> Option<WhisperDownload> {
    let url = rec.url.clone()?;
    if url.is_empty() {
        return None;
    }
    Some(WhisperDownload {
        url,
        size_mb: rec.size_mb,
    })
}

fn piper_download_of(
    rec: &ModelRecord,
    onnx_filename: &str,
    config_filename: &str,
) -> Option<PiperDownload> {
    let onnx_url = rec.url.clone()?;
    let config_url = rec.config_url.clone()?;
    if onnx_url.is_empty() || config_url.is_empty() {
        return None;
    }
    Some(PiperDownload {
        onnx_filename: onnx_filename.to_string(),
        config_filename: config_filename.to_string(),
        onnx_url,
        config_url,
        size_mb: rec.size_mb,
    })
}

/// Resolve both voice models with one catalog fetch (whisper: `models/`, piper: `models/tts/`).
pub async fn resolve_voice_models(
    settings: &Settings,
    repo: &dyn ModelRepository,
    data_dir: &Path,
) -> VoiceModels {
    let models_dir = data_dir.join("models");
    let tts_dir = crate::model_download::tts_models_dir(data_dir);

    let whisper_catalog = repo
        .list_by_category(&ModelCategory::Whisper)
        .await
        .unwrap_or_default();
    let piper_catalog = repo
        .list_by_category(&ModelCategory::TtsPiper)
        .await
        .unwrap_or_default();

    // Fall back to a lone installed voice, but warn: the setting is still wrong.
    let piper = resolve_piper(&settings.voice_tts_voice, &piper_catalog, &tts_dir).or_else(|| {
        let fallback = any_installed_piper_voice(&tts_dir)?;
        tracing::warn!(
            configured = %settings.voice_tts_voice,
            using = %fallback.onnx.display(),
            "voice_tts_voice does not resolve; falling back to the one installed voice"
        );
        Some(fallback)
    });

    VoiceModels {
        whisper: resolve_whisper(
            &settings.active_whisper_model,
            &whisper_catalog,
            &models_dir,
        ),
        piper,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ModelRecord` has no `Default`; tests override only the fields resolution reads.
    fn bare(id: &str, category: ModelCategory, name: &str) -> ModelRecord {
        ModelRecord {
            id: id.to_string(),
            category,
            name: name.to_string(),
            filename: None,
            description: String::new(),
            size_mb: 0,
            url: None,
            hf_id: None,
            ram_estimate_mb: None,
            recommended_role: None,
            context_length: None,
            quantization: None,
            asr_language: None,
            asr_size: None,
            tts_engine: None,
            tts_voice_name: None,
            config_filename: None,
            config_url: None,
            tts_url: None,
            sample_rate: None,
            downloaded: false,
            is_custom: false,
        }
    }

    fn whisper_rec(name: &str, filename: &str) -> ModelRecord {
        let mut r = bare(&format!("whisper/{name}"), ModelCategory::Whisper, name);
        r.filename = Some(filename.to_string());
        r.url = Some(format!("https://example.test/{filename}"));
        r.size_mb = 142;
        r
    }

    fn piper_rec(name: &str, onnx: &str) -> ModelRecord {
        let mut r = bare(&format!("tts_piper/{name}"), ModelCategory::TtsPiper, name);
        r.filename = Some(onnx.to_string());
        r.config_filename = Some(format!("{onnx}.json"));
        r.url = Some(format!("https://example.test/{onnx}"));
        r.config_url = Some(format!("https://example.test/{onnx}.json"));
        r.size_mb = 63;
        r
    }

    fn whisper_catalog() -> Vec<ModelRecord> {
        vec![
            whisper_rec("base", "ggml-base.bin"),
            whisper_rec("base.en", "ggml-base.en.bin"),
            whisper_rec("tiny.en", "ggml-tiny.en.bin"),
        ]
    }

    fn piper_catalog() -> Vec<ModelRecord> {
        vec![
            piper_rec("en-lessac-medium", "en_US-lessac-medium.onnx"),
            piper_rec("en-ryan-medium", "en_US-ryan-medium.onnx"),
        ]
    }

    // ── shape 1: catalog name ────────────────────────────────────────────

    #[test]
    fn whisper_resolves_a_catalog_name() {
        let got = resolve_whisper("base", &whisper_catalog(), Path::new("/d/models")).unwrap();
        assert_eq!(got.path, Path::new("/d/models/ggml-base.bin"));
        assert!(got.download.is_some(), "catalog hit must stay downloadable");
    }

    #[test]
    fn whisper_dotted_catalog_name_does_not_double_the_suffix() {
        let got = resolve_whisper("base.en", &whisper_catalog(), Path::new("/d/models")).unwrap();
        assert_eq!(got.path, Path::new("/d/models/ggml-base.en.bin"));
    }

    // ── shape 2: filename ────────────────────────────────────────────────

    #[test]
    fn whisper_resolves_a_filename_without_double_prefixing() {
        let got =
            resolve_whisper("ggml-base.bin", &whisper_catalog(), Path::new("/d/models")).unwrap();
        assert_eq!(got.path, Path::new("/d/models/ggml-base.bin"));
        assert!(!got.path.to_string_lossy().contains("ggml-ggml"));
    }

    #[test]
    fn piper_resolves_a_filename_and_pairs_the_config() {
        let got = resolve_piper(
            "en_US-lessac-medium.onnx",
            &piper_catalog(),
            Path::new("/d/models/tts"),
        )
        .unwrap();
        assert_eq!(
            got.onnx,
            Path::new("/d/models/tts/en_US-lessac-medium.onnx")
        );
        assert_eq!(
            got.config,
            Path::new("/d/models/tts/en_US-lessac-medium.onnx.json")
        );
        let dl = got.download.unwrap();
        assert!(dl.config_url.ends_with(".json"), "config must be fetchable");
    }

    #[test]
    fn piper_resolves_a_catalog_name() {
        let got = resolve_piper(
            "en-lessac-medium",
            &piper_catalog(),
            Path::new("/d/models/tts"),
        )
        .unwrap();
        assert_eq!(
            got.onnx,
            Path::new("/d/models/tts/en_US-lessac-medium.onnx")
        );
    }

    // ── shape 3: the onboarding values that never existed ────────────────

    #[test]
    fn onboarding_voices_resolve_to_none_not_a_bogus_path() {
        for bogus in ["amy", "kathleen", "libritts"] {
            assert_eq!(
                resolve_piper(bogus, &piper_catalog(), Path::new("/d/models/tts")),
                None,
                "{bogus} must not resolve"
            );
        }
    }

    #[test]
    fn unknown_whisper_value_resolves_to_none() {
        assert_eq!(
            resolve_whisper("no-such-model", &whisper_catalog(), Path::new("/d/models")),
            None
        );
    }

    #[test]
    fn empty_and_whitespace_values_resolve_to_none() {
        assert_eq!(
            resolve_whisper("", &whisper_catalog(), Path::new("/d/models")),
            None
        );
        assert_eq!(
            resolve_whisper("   ", &whisper_catalog(), Path::new("/d/models")),
            None
        );
        assert_eq!(
            resolve_piper("", &piper_catalog(), Path::new("/d/models/tts")),
            None
        );
    }

    #[test]
    fn values_are_trimmed_before_matching() {
        assert!(resolve_whisper("  base  ", &whisper_catalog(), Path::new("/d/models")).is_some());
    }

    // ── the piper gate ───────────────────────────────────────────────────

    #[test]
    fn tts_is_piper_follows_resolution_not_the_setting_spelling() {
        let resolved = VoiceModels {
            whisper: None,
            piper: resolve_piper(
                "en-lessac-medium",
                &piper_catalog(),
                Path::new("/d/models/tts"),
            ),
        };
        assert!(resolved.tts_is_piper());

        let unresolved = VoiceModels {
            whisper: None,
            piper: resolve_piper("amy", &piper_catalog(), Path::new("/d/models/tts")),
        };
        assert!(!unresolved.tts_is_piper());
    }

    // ── catalog entries missing download metadata ────────────────────────

    #[test]
    fn a_catalog_entry_without_a_url_still_resolves_but_is_not_downloadable() {
        let mut rec = whisper_rec("base", "ggml-base.bin");
        rec.url = None;
        let got = resolve_whisper("base", &[rec], Path::new("/d/models")).unwrap();
        assert_eq!(got.path, Path::new("/d/models/ggml-base.bin"));
        assert_eq!(got.download, None);
    }

    #[test]
    fn a_piper_entry_without_a_config_url_is_not_downloadable() {
        let mut rec = piper_rec("en-lessac-medium", "en_US-lessac-medium.onnx");
        rec.config_url = None;
        let got = resolve_piper("en-lessac-medium", &[rec], Path::new("/d/models/tts")).unwrap();
        assert_eq!(got.download, None, "half a voice is not a fetchable voice");
    }

    #[test]
    fn piper_config_filename_defaults_to_the_onnx_sibling() {
        let mut rec = piper_rec("en-lessac-medium", "en_US-lessac-medium.onnx");
        rec.config_filename = None;
        let got = resolve_piper("en-lessac-medium", &[rec], Path::new("/d/models/tts")).unwrap();
        assert_eq!(
            got.config,
            Path::new("/d/models/tts/en_US-lessac-medium.onnx.json")
        );
    }

    // ── shape 4: hand-placed files the catalog has never heard of ────────

    #[test]
    fn a_hand_placed_whisper_file_resolves_from_disk() {
        let dir = std::env::temp_dir().join(format!("giap-vm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("ggml-custom.bin");
        std::fs::write(&f, b"x").unwrap();

        let got = resolve_whisper("ggml-custom.bin", &whisper_catalog(), &dir).unwrap();
        assert_eq!(got.path, f);
        assert_eq!(got.download, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── the disk fallback: audible despite a broken setting ──────────────

    /// A scratch dir holding `names` as complete .onnx/.onnx.json pairs.
    struct Voices(PathBuf);
    impl Voices {
        fn new(tag: &str, names: &[&str]) -> Self {
            let dir =
                std::env::temp_dir().join(format!("giap-voices-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for n in names {
                std::fs::write(dir.join(format!("{n}.onnx")), b"weights").unwrap();
                std::fs::write(dir.join(format!("{n}.onnx.json")), b"{}").unwrap();
            }
            Self(dir)
        }
    }
    impl Drop for Voices {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn a_lone_installed_voice_is_used_when_the_setting_is_bogus() {
        let v = Voices::new("lone", &["en_US-lessac-medium"]);
        assert_eq!(resolve_piper("amy", &piper_catalog(), &v.0), None);

        let got = any_installed_piper_voice(&v.0).unwrap();
        assert_eq!(got.onnx, v.0.join("en_US-lessac-medium.onnx"));
        assert_eq!(got.config, v.0.join("en_US-lessac-medium.onnx.json"));
        assert!(got.is_installed());
    }

    #[test]
    fn the_fallback_declines_when_the_choice_is_ambiguous() {
        let v = Voices::new("two", &["en_US-lessac-medium", "en_US-ryan-medium"]);
        assert_eq!(any_installed_piper_voice(&v.0), None);
    }

    #[test]
    fn the_fallback_ignores_a_voice_missing_its_config() {
        let v = Voices::new("halfvoice", &[]);
        std::fs::write(v.0.join("orphan.onnx"), b"weights").unwrap();
        assert_eq!(any_installed_piper_voice(&v.0), None);
    }

    #[test]
    fn the_fallback_is_none_on_a_missing_or_empty_dir() {
        assert_eq!(
            any_installed_piper_voice(Path::new("/nonexistent/giap/tts")),
            None
        );
        let v = Voices::new("empty", &[]);
        assert_eq!(any_installed_piper_voice(&v.0), None);
    }

    #[test]
    fn a_missing_literal_file_does_not_resolve() {
        let dir = std::env::temp_dir().join(format!("giap-vm-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(
            resolve_whisper("not-there.bin", &whisper_catalog(), &dir),
            None
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
