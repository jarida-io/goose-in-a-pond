//! Vision-encoder (mmproj) resolution: the engine enables images only for a registry entry with
//! `mmproj_path`, which goose's featured lookup never sets for our bare stems, so we stamp it.

use goose::providers::local_inference::local_model_registry::{
    get_registry, MmprojSpec, FEATURED_MODELS,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Encoder directory: outside `models/gguf/` so `*.gguf` scans never meet an encoder, and keyed
/// by the NORMALISED name so every spelling of a model shares it.
#[must_use]
pub fn mmproj_dir(data_dir: &Path, model_name: &str) -> PathBuf {
    data_dir
        .join("models")
        .join("mmproj")
        .join(normalize_model_name(model_name))
}

/// The featured encoder spec for a stem, matched by normalised name rather than repo id.
#[must_use]
pub fn featured_mmproj_for_stem(stem: &str) -> Option<&'static MmprojSpec> {
    let wanted = normalize_model_name(stem);
    FEATURED_MODELS.iter().find_map(|m| {
        let spec = m.mmproj.as_ref()?;
        // `spec` is "owner/repo-GGUF:QUANT".
        let repo = m.spec.split(':').next().unwrap_or(m.spec);
        (normalize_model_name(repo) == wanted).then_some(spec)
    })
}

/// Comparable key: drops owner, `-GGUF`, quant suffix (`:Q4_K_M`/`-Q4_K_M`), `.gguf` and case.
/// Unlike `canonical_model_stem`, always strips the quant: encoders are per family, not quant.
fn normalize_model_name(raw: &str) -> String {
    let no_owner = raw.rsplit('/').next().unwrap_or(raw);
    let no_colon_quant = no_owner.split(':').next().unwrap_or(no_owner);
    let no_ext = no_colon_quant.trim_end_matches(".gguf");
    let lower = no_ext.to_ascii_lowercase();
    let no_repo_suffix = lower.trim_end_matches("-gguf");
    match no_repo_suffix.rsplit_once(['-', '.']) {
        Some((base, tag))
            if !base.is_empty()
                && crate::goose_agent::looks_like_quant_tag(&tag.to_uppercase()) =>
        {
            base.to_string()
        }
        _ => no_repo_suffix.to_string(),
    }
}

/// Whether the model declares an encoder, downloaded or not; gate the UI's attach button on this.
#[must_use]
pub fn declares_vision(model_name: &str) -> bool {
    featured_mmproj_for_stem(model_name).is_some()
}

/// Whether the encoder bytes are on disk, so the next turn can see images.
#[must_use]
pub fn mmproj_ready(data_dir: &Path, stem: &str) -> bool {
    resolved_mmproj_path(data_dir, stem).is_some()
}

/// On-disk encoder for a stem: GIAP's directory, then goose's, so none is fetched twice.
#[must_use]
pub fn resolved_mmproj_path(data_dir: &Path, stem: &str) -> Option<PathBuf> {
    let spec = featured_mmproj_for_stem(stem)?;
    let giap = mmproj_dir(data_dir, stem).join(spec.filename);
    if is_nonempty_file(&giap) {
        return Some(giap);
    }
    let goose_owned = spec.local_path();
    if is_nonempty_file(&goose_owned) {
        return Some(goose_owned);
    }
    None
}

fn is_nonempty_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Idempotently stamp the encoder onto `stem`'s entry; `true` if it ends up attached.
pub fn stamp_registry_entry(data_dir: &Path, stem: &str) -> bool {
    let Some(path) = resolved_mmproj_path(data_dir, stem) else {
        return false;
    };
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let Ok(mut registry) = get_registry().lock() else {
        tracing::warn!("GGUF registry lock poisoned; cannot attach vision encoder");
        return false;
    };
    let Some(entry) = registry.get_model(stem) else {
        return false;
    };
    if entry.mmproj_path.as_deref() == Some(path.as_path()) && entry.mmproj_size_bytes == size {
        return true; // already stamped
    }

    let mut updated = entry.clone();
    updated.mmproj_path = Some(path.clone());
    updated.mmproj_size_bytes = size;
    updated.mmproj_checked = true;
    updated.settings.vision_capable = true;
    // The engine budgets KV around this; zero would over-allocate and OOM on encoder load.
    updated.settings.mmproj_size_bytes = size;

    match registry.add_model(updated) {
        Ok(_) => {
            tracing::info!(
                model = %stem,
                mmproj = %path.display(),
                size_mb = size / (1024 * 1024),
                "vision encoder attached; image input is available for this model"
            );
            true
        }
        Err(e) => {
            tracing::warn!(model = %stem, error = %e, "could not attach vision encoder");
            false
        }
    }
}

/// Stems with an encoder download running, so provider builds don't start it twice.
fn in_flight() -> &'static Mutex<HashSet<String>> {
    static IN_FLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    IN_FLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Fetch `stem`'s encoder in the background if missing. Never blocks: it runs on the model-switch
/// path, and the engine re-reads the registry per generation, so the stamp lands live.
pub fn ensure_mmproj_available(data_dir: &Path, stem: &str) {
    let Some(spec) = featured_mmproj_for_stem(stem) else {
        return; // text-only model, nothing to fetch
    };
    if stamp_registry_entry(data_dir, stem) {
        return; // already on disk
    }

    {
        let mut guard = in_flight().lock().unwrap_or_else(|e| e.into_inner());
        if !guard.insert(stem.to_string()) {
            return; // a download for this stem is already running
        }
    }

    let url = format!(
        "https://huggingface.co/{}/resolve/main/{}",
        spec.repo, spec.filename
    );
    let dest_dir = mmproj_dir(data_dir, stem);
    let dest = dest_dir.join(spec.filename);
    let stem_owned = stem.to_string();
    let data_dir_owned = data_dir.to_path_buf();

    tokio::spawn(async move {
        tracing::info!(
            model = %stem_owned,
            url = %url,
            "downloading vision encoder in the background; image input becomes available when it completes"
        );
        let outcome = download_to(&url, &dest_dir, &dest).await;
        match outcome {
            Ok(bytes) => {
                tracing::info!(
                    model = %stem_owned,
                    size_mb = bytes / (1024 * 1024),
                    "vision encoder downloaded"
                );
                stamp_registry_entry(&data_dir_owned, &stem_owned);
            }
            Err(e) => tracing::warn!(
                model = %stem_owned,
                error = %e,
                "vision encoder download failed; this model stays text-only until it succeeds"
            ),
        }
        in_flight()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&stem_owned);
    });
}

/// Stream a URL to `dest` via a `.part` file, so a partial download never looks complete.
async fn download_to(url: &str, dir: &Path, dest: &Path) -> anyhow::Result<u64> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    tokio::fs::create_dir_all(dir).await?;
    let part = dest.with_extension("part");

    // Egress gate for every encoder fetch; must run in a task (`record_egress` spawns). A refusal
    // only leaves the model text-only.
    let call = pond_core::shared::services::egress::begin(url, "GET")?;
    let sent = reqwest::Client::builder()
        // No total timeout for ~1 GB on a slow link; the read timeout catches dead connections.
        .read_timeout(std::time::Duration::from_secs(120))
        .build()?
        .get(url)
        .send()
        .await;
    call.finish(sent.as_ref().ok().map(|r| r.status().as_u16()));
    let resp = sent?.error_for_status()?;

    let mut file = tokio::fs::File::create(&part).await?;
    let mut written = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        written += chunk.len() as u64;
    }
    file.flush().await?;
    drop(file);
    tokio::fs::rename(&part, dest).await?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_stem_resolves_to_its_featured_encoder() {
        // This is the exact spelling GooseAdapter registers.
        let spec = featured_mmproj_for_stem("gemma-4-E2B-it").expect("E2B declares an encoder");
        assert_eq!(spec.repo, "unsloth/gemma-4-E2B-it-GGUF");
        assert_eq!(spec.filename, "mmproj-BF16.gguf");
    }

    #[test]
    fn both_quant_spellings_resolve() {
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it-Q4_K_M").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it:Q4_K_M").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E2B-it-Q4_K_M.gguf").is_some());
        assert!(featured_mmproj_for_stem("gemma-4-E4B-it-Q5_K_M").is_some());
    }

    #[test]
    fn a_non_quant_trailing_segment_is_kept() {
        assert_eq!(normalize_model_name("gemma-4-E2B-it"), "gemma-4-e2b-it");
        // A Q-prefixed segment without a digit is not a quant.
        assert_eq!(normalize_model_name("some-model-Queen"), "some-model-queen");
    }

    #[test]
    fn the_full_featured_repo_spelling_also_resolves() {
        assert!(featured_mmproj_for_stem("unsloth/gemma-4-E2B-it-GGUF").is_some());
        assert!(featured_mmproj_for_stem("unsloth/gemma-4-E2B-it-GGUF:Q4_K_M").is_some());
    }

    #[test]
    fn e1b_declares_no_vision_even_though_it_is_a_gemma_4() {
        assert!(!declares_vision("gemma-4-E1B-it"));
        assert!(declares_vision("gemma-4-E2B-it"));
        assert!(declares_vision("gemma-4-E4B-it"));
    }

    #[test]
    fn text_only_families_declare_no_vision() {
        assert!(!declares_vision("Llama-3.2-3B-Instruct"));
        assert!(!declares_vision("Hermes-2-Pro-Mistral-7B"));
        assert!(!declares_vision("something-nobody-has-heard-of"));
    }

    #[test]
    fn every_vision_capable_featured_model_is_reachable_by_its_stem() {
        for m in FEATURED_MODELS.iter().filter(|m| m.mmproj.is_some()) {
            let repo = m.spec.split(':').next().unwrap();
            let stem = repo
                .rsplit('/')
                .next()
                .unwrap()
                .trim_end_matches("-GGUF")
                .to_string();
            assert!(
                declares_vision(&stem),
                "featured vision model {stem} is not reachable by its bare stem"
            );
        }
    }

    #[test]
    fn encoders_live_outside_the_weights_directory() {
        let dir = mmproj_dir(Path::new("/data"), "gemma-4-E2B-it");
        assert_eq!(
            dir,
            Path::new("/data/models/mmproj/gemma-4-e2b-it"),
            "an mmproj inside models/gguf would confuse resolve_gguf_filename"
        );
    }

    #[test]
    fn every_spelling_of_a_model_shares_one_encoder_directory() {
        let d = Path::new("/data");
        let canonical = mmproj_dir(d, "gemma-4-E2B-it");
        for spelling in [
            "gemma-4-E2B-it-Q4_K_M",
            "gemma-4-E2B-it:Q4_K_M",
            "gemma-4-E2B-it.gguf",
            "unsloth/gemma-4-E2B-it-GGUF:Q4_K_M",
        ] {
            assert_eq!(mmproj_dir(d, spelling), canonical, "spelling: {spelling}");
        }
        assert_ne!(mmproj_dir(d, "gemma-4-E4B-it"), canonical);
    }

    #[test]
    fn an_absent_encoder_is_not_ready() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!mmproj_ready(tmp.path(), "gemma-4-E2B-it"));
        // A text-only model is never "ready" either.
        assert!(!mmproj_ready(tmp.path(), "gemma-4-E1B-it"));
    }

    #[test]
    fn a_zero_byte_encoder_does_not_count_as_downloaded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = mmproj_dir(tmp.path(), "gemma-4-E2B-it");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("mmproj-BF16.gguf"), b"").unwrap();
        assert!(
            !mmproj_ready(tmp.path(), "gemma-4-E2B-it"),
            "a truncated or empty file must not be mistaken for an encoder"
        );

        std::fs::write(dir.join("mmproj-BF16.gguf"), b"not really a gguf").unwrap();
        assert!(mmproj_ready(tmp.path(), "gemma-4-E2B-it"));
    }
}
